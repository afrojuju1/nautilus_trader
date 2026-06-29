//! Alpaca live data client.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use nautilus_common::{
    clients::DataClient,
    live::{runner::get_data_event_sender, runtime::get_runtime},
    messages::{
        DataEvent,
        data::{
            BarsResponse, DataResponse, ForwardPricesResponse, InstrumentResponse,
            InstrumentsResponse, RequestBars, RequestForwardPrices, RequestInstrument,
            RequestInstruments, SubscribeInstrument, SubscribeInstrumentStatus,
            SubscribeInstruments, SubscribeOptionGreeks, SubscribeQuotes, SubscribeTrades,
            UnsubscribeInstrument, UnsubscribeInstrumentStatus, UnsubscribeInstruments,
            UnsubscribeOptionGreeks, UnsubscribeQuotes, UnsubscribeTrades,
        },
    },
};
use nautilus_core::{
    AtomicMap, MUTEX_POISONED, Params, UnixNanos,
    datetime::datetime_to_unix_nanos,
    time::{AtomicTime, get_atomic_clock_realtime},
};
use nautilus_model::{
    data::{
        Bar, BarType, Data, ForwardPrice, QuoteTick, TradeTick, greeks::OptionGreekValues,
        option_chain::OptionGreeks,
    },
    enums::{AggressorSide, BarAggregation, GreeksConvention, PriceType},
    identifiers::{ClientId, InstrumentId, Symbol, TradeId, Venue},
    instruments::{Equity, Instrument, InstrumentAny},
    types::{Currency, Price},
};
use nautilus_network::websocket::{
    TransportBackend, WebSocketClient, WebSocketConfig, channel_message_handler,
};
use rust_decimal::Decimal;
use serde_json::json;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    common::{
        consts::{
            ALPACA_OPEN_INTEREST_INFO_KEY, ALPACA_OPTION_CHAIN_EXPIRATION_PARAM,
            ALPACA_OPTION_CHAIN_MAX_EXPIRATION_PARAM, ALPACA_OPTION_CHAIN_MIN_EXPIRATION_PARAM,
            ALPACA_OPTION_CHAIN_TYPE_PARAM, ALPACA_OPTION_CHAIN_UNDERLYING_PARAM, ALPACA_VENUE,
        },
        credentials::AlpacaCredential,
    },
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaOptionContract, AlpacaOptionQuote, AlpacaOptionSnapshot, AlpacaOptionType,
            OptionBarsRequest, OptionSnapshotsRequest, StockBarsRequest, StockSnapshotsRequest,
        },
    },
    parse::{
        AlpacaOptionSymbolParts, canonical_alpaca_option_symbol, parse_alpaca_option_instrument_id,
    },
    providers::AlpacaOptionContractProvider,
    runtime::emit_operator_event,
    websocket::market_data::{
        AlpacaOptionMarketDataMessage, AlpacaOptionMarketDataSubscriptionRequest,
        AlpacaOptionStreamQuote, AlpacaOptionStreamTrade, parse_option_market_data_message,
    },
};

const DEFAULT_OPTION_SNAPSHOT_POLL_SECS: u64 = 5;
const OPTION_MARKET_DATA_WS_HEARTBEAT_SECS: u64 = 30;
const OPTION_MARKET_DATA_RECONNECT_TIMEOUT_MS: u64 = 10_000;
const OPTION_MARKET_DATA_RECONNECT_DELAY_INITIAL_MS: u64 = 1_000;
const OPTION_MARKET_DATA_RECONNECT_DELAY_MAX_MS: u64 = 30_000;
const OPTION_MARKET_DATA_RECONNECT_BACKOFF_FACTOR: f64 = 2.0;
const OPTION_MARKET_DATA_RECONNECT_JITTER_MS: u64 = 250;

#[derive(Debug, Default)]
struct OptionSnapshotSubscriptions {
    quote_instrument_ids: BTreeSet<InstrumentId>,
    greeks_instrument_ids: BTreeSet<InstrumentId>,
    is_polling: bool,
}

impl OptionSnapshotSubscriptions {
    fn is_empty(&self) -> bool {
        self.quote_instrument_ids.is_empty() && self.greeks_instrument_ids.is_empty()
    }

    fn instrument_ids(&self, include_quote_snapshots: bool) -> Vec<InstrumentId> {
        if include_quote_snapshots {
            self.quote_instrument_ids
                .union(&self.greeks_instrument_ids)
                .copied()
                .collect()
        } else {
            self.greeks_instrument_ids.iter().copied().collect()
        }
    }

    fn should_emit_quote(&self, instrument_id: &InstrumentId) -> bool {
        self.quote_instrument_ids.contains(instrument_id)
    }

    fn should_emit_greeks(&self, instrument_id: &InstrumentId) -> bool {
        self.greeks_instrument_ids.contains(instrument_id)
    }
}

#[derive(Debug, Default)]
struct OptionMarketDataSubscriptions {
    quote_instrument_ids: BTreeSet<InstrumentId>,
    trade_instrument_ids: BTreeSet<InstrumentId>,
    is_streaming: bool,
    cmd_tx: Option<mpsc::UnboundedSender<OptionMarketDataCommand>>,
}

impl OptionMarketDataSubscriptions {
    fn is_empty(&self) -> bool {
        self.quote_instrument_ids.is_empty() && self.trade_instrument_ids.is_empty()
    }

    fn quote_symbols(&self) -> Vec<String> {
        instrument_symbols_for_stream(&self.quote_instrument_ids)
    }

    fn trade_symbols(&self) -> Vec<String> {
        instrument_symbols_for_stream(&self.trade_instrument_ids)
    }

    fn instrument_id_for_symbol(&self, symbol: &str) -> Option<InstrumentId> {
        self.quote_instrument_ids
            .iter()
            .chain(self.trade_instrument_ids.iter())
            .copied()
            .find(|instrument_id| {
                alpaca_symbol_from_instrument_id(*instrument_id).eq_ignore_ascii_case(symbol)
            })
    }
}

#[derive(Debug)]
enum OptionMarketDataCommand {
    Subscribe {
        quotes: Vec<String>,
        trades: Vec<String>,
    },
    Unsubscribe {
        quotes: Vec<String>,
        trades: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionMarketDataSource {
    Unknown,
    Stream,
    SnapshotFallback,
    Error,
}

impl OptionMarketDataSource {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Stream => 1,
            Self::SnapshotFallback => 2,
            Self::Error => 3,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Stream => "stream",
            Self::SnapshotFallback => "snapshot_fallback",
            Self::Error => "error",
        }
    }
}

#[derive(Debug)]
struct OptionMarketDataRuntimeState {
    source: AtomicU8,
    last_stream_quote_init_ns: AtomicU64,
}

impl Default for OptionMarketDataRuntimeState {
    fn default() -> Self {
        Self {
            source: AtomicU8::new(OptionMarketDataSource::Unknown.as_u8()),
            last_stream_quote_init_ns: AtomicU64::new(0),
        }
    }
}

impl OptionMarketDataRuntimeState {
    fn is_quote_stream_fresh(
        &self,
        clock: &'static AtomicTime,
        quote_snapshot_fallback_after: Duration,
    ) -> bool {
        if self.source.load(Ordering::Acquire) != OptionMarketDataSource::Stream.as_u8() {
            return false;
        }

        let last_stream_quote_init_ns = self.last_stream_quote_init_ns.load(Ordering::Acquire);
        last_stream_quote_init_ns > 0
            && clock
                .get_time_ns()
                .as_u64()
                .saturating_sub(last_stream_quote_init_ns)
                <= quote_snapshot_fallback_after.as_nanos() as u64
    }

    fn reset(&self) {
        self.source
            .store(OptionMarketDataSource::Unknown.as_u8(), Ordering::Release);
        self.last_stream_quote_init_ns.store(0, Ordering::Release);
    }

    fn mark_stream_quote(
        &self,
        feed: &str,
        quote_subscriptions: usize,
        trade_subscriptions: usize,
        clock: &'static AtomicTime,
    ) {
        self.last_stream_quote_init_ns
            .store(clock.get_time_ns().as_u64(), Ordering::Release);
        self.mark_source(
            OptionMarketDataSource::Stream,
            feed,
            "quote",
            quote_subscriptions,
            trade_subscriptions,
        );
    }

    fn mark_source(
        &self,
        source: OptionMarketDataSource,
        feed: &str,
        reason: &str,
        quote_subscriptions: usize,
        trade_subscriptions: usize,
    ) {
        let previous = self.source.swap(source.as_u8(), Ordering::AcqRel);
        if previous == source.as_u8() {
            return;
        }

        emit_operator_event(
            "option_market_data_stream",
            json!({
                "source": source.as_str(),
                "feed": feed,
                "reason": reason,
                "quote_subscriptions": quote_subscriptions,
                "trade_subscriptions": trade_subscriptions,
            }),
        );
    }
}

#[derive(Clone, Debug)]
struct AlpacaOptionInstrumentsRequest {
    underlying_symbol: String,
    min_expiration: String,
    max_expiration: String,
    option_type: Option<AlpacaOptionType>,
}

/// Live data client for Alpaca instruments.
#[derive(Debug)]
pub struct AlpacaDataClient {
    clock: &'static AtomicTime,
    client_id: ClientId,
    config: AlpacaDataClientConfig,
    http_client: AlpacaHttpClient,
    is_connected: AtomicBool,
    pending_tasks: Mutex<Vec<JoinHandle<()>>>,
    data_sender: tokio::sync::mpsc::UnboundedSender<DataEvent>,
    instruments: Arc<AtomicMap<InstrumentId, InstrumentAny>>,
    option_snapshot_subscriptions: Arc<Mutex<OptionSnapshotSubscriptions>>,
    option_market_data_subscriptions: Arc<Mutex<OptionMarketDataSubscriptions>>,
    option_market_data_state: Arc<OptionMarketDataRuntimeState>,
}

impl AlpacaDataClient {
    /// Creates a new [`AlpacaDataClient`] instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client fails to initialize.
    pub fn new(client_id: ClientId, config: AlpacaDataClientConfig) -> anyhow::Result<Self> {
        let http_client = AlpacaHttpClient::from_data_config(&config)
            .context("failed to initialize Alpaca data HTTP client")?;

        Ok(Self {
            clock: get_atomic_clock_realtime(),
            client_id,
            config,
            http_client,
            is_connected: AtomicBool::new(false),
            pending_tasks: Mutex::new(Vec::new()),
            data_sender: get_data_event_sender(),
            instruments: Arc::new(AtomicMap::new()),
            option_snapshot_subscriptions: Arc::new(Mutex::new(
                OptionSnapshotSubscriptions::default(),
            )),
            option_market_data_subscriptions: Arc::new(Mutex::new(
                OptionMarketDataSubscriptions::default(),
            )),
            option_market_data_state: Arc::new(OptionMarketDataRuntimeState::default()),
        })
    }

    fn venue(&self) -> Venue {
        Venue::new(ALPACA_VENUE)
    }

    fn spawn_task<F>(&self, description: &'static str, fut: F)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let handle = get_runtime().spawn(async move {
            if let Err(e) = fut.await {
                log::warn!("{description} failed: {e:?}");
            }
        });

        let mut tasks = self.pending_tasks.lock().expect(MUTEX_POISONED);
        tasks.retain(|handle| !handle.is_finished());
        tasks.push(handle);
    }

    fn abort_pending_tasks(&self) {
        let mut tasks = self.pending_tasks.lock().expect(MUTEX_POISONED);
        for handle in tasks.drain(..) {
            handle.abort();
        }
    }

    fn option_snapshot_poll_interval(&self) -> Duration {
        Duration::from_secs(
            self.config
                .snapshot_greeks_poll_secs
                .unwrap_or(DEFAULT_OPTION_SNAPSHOT_POLL_SECS)
                .max(1),
        )
    }

    fn ensure_option_snapshot_poller(&self) {
        if !self.is_connected() {
            return;
        }

        let mut subscriptions = self
            .option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED);
        if subscriptions.is_polling || subscriptions.is_empty() {
            return;
        }

        subscriptions.is_polling = true;
        drop(subscriptions);

        let http_client = self.http_client.clone();
        let sender = self.data_sender.clone();
        let instruments = self.instruments.clone();
        let subscriptions = self.option_snapshot_subscriptions.clone();
        let market_data_state = self.option_market_data_state.clone();
        let interval = self.option_snapshot_poll_interval();
        let quote_snapshot_fallback_after = interval + interval;
        let feed = self.config.option_feed.as_str().to_string();
        let clock = self.clock;

        self.spawn_task("option_snapshot_poller", async move {
            loop {
                match poll_option_snapshots(
                    &http_client,
                    &sender,
                    &instruments,
                    &subscriptions,
                    &market_data_state,
                    quote_snapshot_fallback_after,
                    &feed,
                    clock,
                )
                .await
                {
                    Ok(true) => tokio::time::sleep(interval).await,
                    Ok(false) => {
                        subscriptions.lock().expect(MUTEX_POISONED).is_polling = false;
                        break;
                    }
                    Err(e) => {
                        log::warn!("Alpaca option snapshot polling failed: {e:?}");
                        tokio::time::sleep(interval).await;
                    }
                }
            }

            Ok(())
        });
    }

    fn mark_option_snapshot_poller_stopped(&self) {
        self.option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED)
            .is_polling = false;
    }

    fn ensure_option_market_data_stream(&self) {
        if !self.is_connected() {
            return;
        }

        let Some(credential) =
            AlpacaCredential::resolve(self.config.api_key.clone(), self.config.api_secret.clone())
        else {
            let (quote_count, trade_count) = self.option_market_data_subscription_counts();
            self.option_market_data_state.mark_source(
                OptionMarketDataSource::SnapshotFallback,
                self.config.option_feed.as_str(),
                "missing_credentials",
                quote_count,
                trade_count,
            );
            self.ensure_option_snapshot_poller();
            return;
        };

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        {
            let mut subscriptions = self
                .option_market_data_subscriptions
                .lock()
                .expect(MUTEX_POISONED);
            if subscriptions.is_streaming || subscriptions.is_empty() {
                return;
            }

            subscriptions.is_streaming = true;
            subscriptions.cmd_tx = Some(cmd_tx);
        }

        let url = self.config.resolved_option_market_data_ws_url();
        let sender = self.data_sender.clone();
        let instruments = self.instruments.clone();
        let subscriptions = self.option_market_data_subscriptions.clone();
        let market_data_state = self.option_market_data_state.clone();
        let feed = self.config.option_feed.as_str().to_string();
        let clock = self.clock;

        self.spawn_task("option_market_data_stream", async move {
            let result = stream_option_market_data(
                url,
                credential,
                sender,
                instruments,
                subscriptions.clone(),
                market_data_state.clone(),
                feed.clone(),
                clock,
                cmd_rx,
            )
            .await;

            {
                let mut subscriptions = subscriptions.lock().expect(MUTEX_POISONED);
                subscriptions.is_streaming = false;
                subscriptions.cmd_tx = None;
            }

            if let Err(error) = &result {
                let (quote_count, trade_count) = option_market_data_subscription_counts(
                    &subscriptions.lock().expect(MUTEX_POISONED),
                );
                market_data_state.mark_source(
                    OptionMarketDataSource::Error,
                    &feed,
                    &error.to_string(),
                    quote_count,
                    trade_count,
                );
            }

            result
        });
    }

    fn mark_option_market_data_stream_stopped(&self) {
        let mut subscriptions = self
            .option_market_data_subscriptions
            .lock()
            .expect(MUTEX_POISONED);
        subscriptions.is_streaming = false;
        subscriptions.cmd_tx = None;
        self.option_market_data_state.reset();
    }

    fn option_market_data_subscription_counts(&self) -> (usize, usize) {
        let subscriptions = self
            .option_market_data_subscriptions
            .lock()
            .expect(MUTEX_POISONED);
        option_market_data_subscription_counts(&subscriptions)
    }

    async fn load_option_instrument(
        provider: &AlpacaOptionContractProvider,
        instrument_id: InstrumentId,
    ) -> anyhow::Result<InstrumentAny> {
        let parts = parse_alpaca_option_instrument_id(instrument_id)?;
        let contracts = provider
            .load_active_contracts(
                parts.underlying_symbol.clone(),
                parts.expiration_date.clone(),
                parts.expiration_date.clone(),
                Some(parts.option_type),
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load active Alpaca option contracts for {} {}",
                    parts.underlying_symbol, parts.expiration_date
                )
            })?;

        let contract = contracts
            .iter()
            .find(|contract| option_contract_matches(contract, &parts))
            .ok_or_else(|| {
                anyhow!(
                    "Alpaca option contract not found for {}",
                    parts.symbol.as_str()
                )
            })?;

        let option = crate::parse::parse_option_contract(contract).with_context(|| {
            format!("failed to parse Alpaca option contract {}", contract.symbol)
        })?;

        Ok(option.into_any())
    }

    fn send_instrument(
        sender: &tokio::sync::mpsc::UnboundedSender<DataEvent>,
        instrument: InstrumentAny,
    ) {
        if let Err(e) = sender.send(DataEvent::Instrument(instrument)) {
            log::error!("Failed to send Alpaca instrument: {e}");
        }
    }

    fn subscribe_option_snapshot_quotes(&self, instrument_id: InstrumentId) {
        let mut subscriptions = self
            .option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED);
        subscriptions.quote_instrument_ids.insert(instrument_id);
        drop(subscriptions);
        self.ensure_option_snapshot_poller();
    }

    fn subscribe_option_market_data_quotes(&self, instrument_id: InstrumentId) {
        let symbol = alpaca_symbol_from_instrument_id(instrument_id);
        let inserted = {
            let mut subscriptions = self
                .option_market_data_subscriptions
                .lock()
                .expect(MUTEX_POISONED);
            subscriptions.quote_instrument_ids.insert(instrument_id)
        };

        if inserted {
            self.send_option_market_data_command(OptionMarketDataCommand::Subscribe {
                quotes: vec![symbol],
                trades: Vec::new(),
            });
        }
        self.ensure_option_market_data_stream();
    }

    fn unsubscribe_option_market_data_quotes(&self, instrument_id: InstrumentId) {
        let symbol = alpaca_symbol_from_instrument_id(instrument_id);
        let removed = {
            let mut subscriptions = self
                .option_market_data_subscriptions
                .lock()
                .expect(MUTEX_POISONED);
            subscriptions.quote_instrument_ids.remove(&instrument_id)
        };

        if removed {
            self.send_option_market_data_command(OptionMarketDataCommand::Unsubscribe {
                quotes: vec![symbol],
                trades: Vec::new(),
            });
        }
    }

    fn subscribe_option_market_data_trades(&self, instrument_id: InstrumentId) {
        let symbol = alpaca_symbol_from_instrument_id(instrument_id);
        let inserted = {
            let mut subscriptions = self
                .option_market_data_subscriptions
                .lock()
                .expect(MUTEX_POISONED);
            subscriptions.trade_instrument_ids.insert(instrument_id)
        };

        if inserted {
            self.send_option_market_data_command(OptionMarketDataCommand::Subscribe {
                quotes: Vec::new(),
                trades: vec![symbol],
            });
        }
        self.ensure_option_market_data_stream();
    }

    fn unsubscribe_option_market_data_trades(&self, instrument_id: InstrumentId) {
        let symbol = alpaca_symbol_from_instrument_id(instrument_id);
        let removed = {
            let mut subscriptions = self
                .option_market_data_subscriptions
                .lock()
                .expect(MUTEX_POISONED);
            subscriptions.trade_instrument_ids.remove(&instrument_id)
        };

        if removed {
            self.send_option_market_data_command(OptionMarketDataCommand::Unsubscribe {
                quotes: Vec::new(),
                trades: vec![symbol],
            });
        }
    }

    fn send_option_market_data_command(&self, command: OptionMarketDataCommand) {
        let cmd_tx = self
            .option_market_data_subscriptions
            .lock()
            .expect(MUTEX_POISONED)
            .cmd_tx
            .clone();
        if let Some(cmd_tx) = cmd_tx
            && let Err(error) = cmd_tx.send(command)
        {
            log::debug!("Alpaca option market-data stream command channel closed: {error}");
        }
    }

    fn subscribe_option_snapshot_greeks(&self, instrument_id: InstrumentId) {
        let mut subscriptions = self
            .option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED);
        subscriptions.greeks_instrument_ids.insert(instrument_id);
        drop(subscriptions);
        self.ensure_option_snapshot_poller();
    }
}

#[async_trait::async_trait(?Send)]
impl DataClient for AlpacaDataClient {
    fn client_id(&self) -> ClientId {
        self.client_id
    }

    fn venue(&self) -> Option<Venue> {
        Some(self.venue())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        log::info!(
            "Starting Alpaca data client: client_id={}, environment={:?}, data_base_url={}, trading_base_url={}",
            self.client_id,
            self.config.environment,
            self.config.resolved_data_base_url(),
            self.config.resolved_trading_base_url(),
        );
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        log::info!("Stopping Alpaca data client {}", self.client_id);
        self.abort_pending_tasks();
        self.mark_option_snapshot_poller_stopped();
        self.mark_option_market_data_stream_stopped();
        self.is_connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        log::debug!("Resetting Alpaca data client {}", self.client_id);
        self.abort_pending_tasks();
        self.instruments.rcu(|instruments| instruments.clear());
        let mut subscriptions = self
            .option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED);
        subscriptions.quote_instrument_ids.clear();
        subscriptions.greeks_instrument_ids.clear();
        subscriptions.is_polling = false;
        let mut market_data_subscriptions = self
            .option_market_data_subscriptions
            .lock()
            .expect(MUTEX_POISONED);
        market_data_subscriptions.quote_instrument_ids.clear();
        market_data_subscriptions.trade_instrument_ids.clear();
        market_data_subscriptions.is_streaming = false;
        market_data_subscriptions.cmd_tx = None;
        self.option_market_data_state.reset();
        self.is_connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn dispose(&mut self) -> anyhow::Result<()> {
        log::debug!("Disposing Alpaca data client {}", self.client_id);
        self.stop()
    }

    fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Acquire)
    }

    fn is_disconnected(&self) -> bool {
        !self.is_connected()
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.is_connected() {
            return Ok(());
        }

        self.is_connected.store(true, Ordering::Release);
        self.ensure_option_snapshot_poller();
        self.ensure_option_market_data_stream();
        log::info!("Connected: client_id={}", self.client_id);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if !self.is_connected() {
            return Ok(());
        }

        self.abort_pending_tasks();
        self.mark_option_snapshot_poller_stopped();
        self.mark_option_market_data_stream_stopped();
        self.is_connected.store(false, Ordering::Release);
        log::info!("Disconnected: client_id={}", self.client_id);
        Ok(())
    }

    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        log::debug!("Subscribing to Alpaca option quotes: {}", cmd.instrument_id,);
        anyhow::ensure!(
            cmd.instrument_id.venue == self.venue(),
            "expected Alpaca venue {}, got {}",
            self.venue(),
            cmd.instrument_id.venue,
        );

        let instrument_id = cmd.instrument_id;
        self.subscribe_instrument(SubscribeInstrument::new(
            instrument_id,
            cmd.client_id,
            cmd.venue,
            cmd.command_id,
            cmd.ts_init,
            cmd.correlation_id,
            cmd.params,
        ))?;
        self.subscribe_option_snapshot_quotes(instrument_id);
        self.subscribe_option_market_data_quotes(instrument_id);

        Ok(())
    }

    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        log::debug!(
            "Unsubscribing from Alpaca option quotes: {}",
            cmd.instrument_id,
        );
        self.option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED)
            .quote_instrument_ids
            .remove(&cmd.instrument_id);
        self.unsubscribe_option_market_data_quotes(cmd.instrument_id);
        Ok(())
    }

    fn subscribe_trades(&mut self, cmd: SubscribeTrades) -> anyhow::Result<()> {
        log::debug!("Subscribing to Alpaca option trades: {}", cmd.instrument_id);
        anyhow::ensure!(
            cmd.instrument_id.venue == self.venue(),
            "expected Alpaca venue {}, got {}",
            self.venue(),
            cmd.instrument_id.venue,
        );

        let instrument_id = cmd.instrument_id;
        self.subscribe_instrument(SubscribeInstrument::new(
            instrument_id,
            cmd.client_id,
            cmd.venue,
            cmd.command_id,
            cmd.ts_init,
            cmd.correlation_id,
            cmd.params,
        ))?;
        self.subscribe_option_market_data_trades(instrument_id);

        Ok(())
    }

    fn unsubscribe_trades(&mut self, cmd: &UnsubscribeTrades) -> anyhow::Result<()> {
        log::debug!(
            "Unsubscribing from Alpaca option trades: {}",
            cmd.instrument_id
        );
        self.unsubscribe_option_market_data_trades(cmd.instrument_id);
        Ok(())
    }

    fn subscribe_option_greeks(&mut self, cmd: SubscribeOptionGreeks) -> anyhow::Result<()> {
        log::debug!(
            "Subscribing to Alpaca option snapshot Greeks: {}",
            cmd.instrument_id,
        );
        anyhow::ensure!(
            cmd.instrument_id.venue == self.venue(),
            "expected Alpaca venue {}, got {}",
            self.venue(),
            cmd.instrument_id.venue,
        );

        let instrument_id = cmd.instrument_id;
        self.subscribe_instrument(SubscribeInstrument::new(
            instrument_id,
            cmd.client_id,
            cmd.venue,
            cmd.command_id,
            cmd.ts_init,
            cmd.correlation_id,
            cmd.params,
        ))?;
        self.subscribe_option_snapshot_greeks(instrument_id);

        Ok(())
    }

    fn unsubscribe_option_greeks(&mut self, cmd: &UnsubscribeOptionGreeks) -> anyhow::Result<()> {
        log::debug!(
            "Unsubscribing from Alpaca option snapshot Greeks: {}",
            cmd.instrument_id,
        );
        self.option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED)
            .greeks_instrument_ids
            .remove(&cmd.instrument_id);
        Ok(())
    }

    fn subscribe_instruments(&mut self, cmd: SubscribeInstruments) -> anyhow::Result<()> {
        log::debug!("Subscribing to cached Alpaca instruments for {}", cmd.venue);
        let sender = self.data_sender.clone();
        for instrument in self.instruments.load().values().cloned() {
            Self::send_instrument(&sender, instrument);
        }
        Ok(())
    }

    fn subscribe_instrument(&mut self, cmd: SubscribeInstrument) -> anyhow::Result<()> {
        log::debug!("Subscribing to Alpaca instrument: {}", cmd.instrument_id);

        if let Some(instrument) = self.instruments.get_cloned(&cmd.instrument_id) {
            Self::send_instrument(&self.data_sender, instrument);
            return Ok(());
        }

        let provider = AlpacaOptionContractProvider::new(self.http_client.clone());
        let instruments = self.instruments.clone();
        let sender = self.data_sender.clone();
        let instrument_id = cmd.instrument_id;

        self.spawn_task("subscribe_instrument", async move {
            let instrument = Self::load_option_instrument(&provider, instrument_id).await?;
            instruments.insert(instrument.id(), instrument.clone());
            Self::send_instrument(&sender, instrument);
            Ok(())
        });

        Ok(())
    }

    fn unsubscribe_instruments(&mut self, cmd: &UnsubscribeInstruments) -> anyhow::Result<()> {
        log::debug!(
            "Unsubscribing from cached Alpaca instruments for {}",
            cmd.venue
        );
        Ok(())
    }

    fn unsubscribe_instrument(&mut self, cmd: &UnsubscribeInstrument) -> anyhow::Result<()> {
        log::debug!(
            "Unsubscribing from Alpaca instrument: {}",
            cmd.instrument_id
        );
        Ok(())
    }

    fn subscribe_instrument_status(
        &mut self,
        cmd: SubscribeInstrumentStatus,
    ) -> anyhow::Result<()> {
        log::debug!(
            "Accepting Alpaca instrument status subscription as no-op: {}",
            cmd.instrument_id,
        );
        Ok(())
    }

    fn unsubscribe_instrument_status(
        &mut self,
        cmd: &UnsubscribeInstrumentStatus,
    ) -> anyhow::Result<()> {
        log::debug!(
            "Accepting Alpaca instrument status unsubscription as no-op: {}",
            cmd.instrument_id,
        );
        Ok(())
    }

    fn request_instruments(&self, request: RequestInstruments) -> anyhow::Result<()> {
        let request_id = request.request_id;
        let client_id = request.client_id.unwrap_or(self.client_id);
        let venue = request.venue.unwrap_or_else(|| self.venue());
        anyhow::ensure!(
            venue == self.venue(),
            "expected Alpaca venue {}, got {}",
            self.venue(),
            venue,
        );
        let start_nanos = datetime_to_unix_nanos(request.start);
        let end_nanos = datetime_to_unix_nanos(request.end);
        let params = request.params;

        if let Some(option_request) = option_instruments_request(params.as_ref())? {
            let provider = AlpacaOptionContractProvider::new(self.http_client.clone());
            let instruments = self.instruments.clone();
            let sender = self.data_sender.clone();
            let clock = self.clock;

            self.spawn_task("request_option_instruments", async move {
                let data = provider
                    .load_active_instruments(
                        option_request.underlying_symbol.clone(),
                        option_request.min_expiration.clone(),
                        option_request.max_expiration.clone(),
                        option_request.option_type,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load active Alpaca option instruments for {} {}..{}",
                            option_request.underlying_symbol,
                            option_request.min_expiration,
                            option_request.max_expiration,
                        )
                    })?
                    .into_iter()
                    .map(|instrument| instrument.into_any())
                    .collect::<Vec<_>>();

                for instrument in &data {
                    instruments.insert(instrument.id(), instrument.clone());
                }

                let response = DataResponse::Instruments(InstrumentsResponse::new(
                    request_id,
                    client_id,
                    venue,
                    data,
                    start_nanos,
                    end_nanos,
                    clock.get_time_ns(),
                    params,
                ));

                if let Err(e) = sender.send(DataEvent::Response(response)) {
                    log::error!("Failed to send Alpaca option instruments response: {e}");
                }

                Ok(())
            });

            return Ok(());
        }

        let instruments = self.instruments.load().values().cloned().collect();

        let response = DataResponse::Instruments(InstrumentsResponse::new(
            request_id,
            client_id,
            venue,
            instruments,
            start_nanos,
            end_nanos,
            self.clock.get_time_ns(),
            params,
        ));

        if let Err(e) = self.data_sender.send(DataEvent::Response(response)) {
            log::error!("Failed to send Alpaca instruments response: {e}");
        }

        Ok(())
    }

    fn request_instrument(&self, request: RequestInstrument) -> anyhow::Result<()> {
        log::debug!("Requesting Alpaca instrument: {}", request.instrument_id);

        let instrument_id = request.instrument_id;
        let request_id = request.request_id;
        let client_id = request.client_id.unwrap_or(self.client_id);
        let start_nanos = datetime_to_unix_nanos(request.start);
        let end_nanos = datetime_to_unix_nanos(request.end);
        let params = request.params;
        let provider = AlpacaOptionContractProvider::new(self.http_client.clone());
        let instruments = self.instruments.clone();
        let sender = self.data_sender.clone();
        let clock = self.clock;

        self.spawn_task("request_instrument", async move {
            let instrument = match instruments.get_cloned(&instrument_id) {
                Some(instrument) => instrument,
                None => {
                    let instrument = Self::load_option_instrument(&provider, instrument_id).await?;
                    instruments.insert(instrument.id(), instrument.clone());
                    instrument
                }
            };

            let response = DataResponse::Instrument(Box::new(InstrumentResponse::new(
                request_id,
                client_id,
                instrument.id(),
                instrument,
                start_nanos,
                end_nanos,
                clock.get_time_ns(),
                params,
            )));

            if let Err(e) = sender.send(DataEvent::Response(response)) {
                log::error!("Failed to send Alpaca instrument response: {e}");
            }

            Ok(())
        });

        Ok(())
    }

    fn request_bars(&self, request: RequestBars) -> anyhow::Result<()> {
        log::debug!("Requesting Alpaca bars: {}", request.bar_type);

        let http_client = self.http_client.clone();
        let sender = self.data_sender.clone();
        let instruments = self.instruments.clone();
        let clock = self.clock;
        let bar_type = request.bar_type;
        let start = request.start;
        let end = request.end;
        let limit = request.limit.map(std::num::NonZeroUsize::get);
        let request_id = request.request_id;
        let client_id = request.client_id.unwrap_or(self.client_id);
        let params = request.params;
        let start_nanos = datetime_to_unix_nanos(start);
        let end_nanos = datetime_to_unix_nanos(end);
        let option_feed = self.config.option_feed.as_str().to_string();
        let stock_feed = self.config.stock_feed.as_str().to_string();

        self.spawn_task("request_bars", async move {
            let bars = match request_bars_from_http(
                &http_client,
                &instruments,
                bar_type,
                start,
                end,
                limit,
                &option_feed,
                &stock_feed,
                clock,
            )
            .await
            {
                Ok(bars) => bars,
                Err(error) => {
                    log::warn!("Alpaca bar request failed: {error:#}");
                    Vec::new()
                }
            };

            let response = DataResponse::Bars(BarsResponse::new(
                request_id,
                client_id,
                bar_type,
                bars,
                start_nanos,
                end_nanos,
                clock.get_time_ns(),
                params,
            ));

            if let Err(error) = sender.send(DataEvent::Response(response)) {
                log::error!("Failed to send Alpaca bars response: {error}");
            }

            Ok(())
        });

        Ok(())
    }

    fn request_forward_prices(&self, request: RequestForwardPrices) -> anyhow::Result<()> {
        let http_client = self.http_client.clone();
        let sender = self.data_sender.clone();
        let clock = self.clock;
        let request_id = request.request_id;
        let client_id = request.client_id.unwrap_or(self.client_id);
        let venue = request.venue;
        let underlying = request.underlying;
        let instrument_id = request.instrument_id;
        let params = request.params;
        let stock_feed = self.config.stock_feed.as_str().to_string();

        self.spawn_task("request_forward_prices", async move {
            let ts_init = clock.get_time_ns();
            let forward_prices = match stock_snapshot_price(
                &http_client,
                underlying.as_str(),
                &stock_feed,
            )
            .await
            {
                Ok(Some(price)) => match instrument_id {
                    Some(instrument_id) => match Decimal::from_str(&price.to_string()) {
                        Ok(decimal) => vec![ForwardPrice::new(
                            instrument_id,
                            decimal,
                            Some(underlying.to_string()),
                            ts_init,
                            ts_init,
                        )],
                        Err(e) => {
                            log::warn!(
                                "Failed to parse Alpaca forward price {price} for {underlying}: {e}",
                            );
                            Vec::new()
                        }
                    },
                    None => {
                        log::warn!(
                            "Alpaca forward price request for {underlying} had no sample instrument; emitting empty response",
                        );
                        Vec::new()
                    }
                },
                Ok(None) => {
                    log::warn!(
                        "Alpaca stock snapshots had no latest price for {underlying}; emitting empty forward prices",
                    );
                    Vec::new()
                }
                Err(e) => {
                    log::warn!(
                        "Failed to request Alpaca forward price for {underlying}: {e:?}; emitting empty response",
                    );
                    Vec::new()
                }
            };

            let response = DataResponse::ForwardPrices(ForwardPricesResponse::new(
                request_id,
                client_id,
                venue,
                forward_prices,
                clock.get_time_ns(),
                params,
            ));

            if let Err(e) = sender.send(DataEvent::Response(response)) {
                log::error!("Failed to send Alpaca forward prices response: {e}");
            }

            Ok(())
        });

        Ok(())
    }
}

fn option_instruments_request(
    params: Option<&Params>,
) -> anyhow::Result<Option<AlpacaOptionInstrumentsRequest>> {
    let Some(params) = params else {
        return Ok(None);
    };

    let Some(underlying_symbol) = params.get_str(ALPACA_OPTION_CHAIN_UNDERLYING_PARAM) else {
        return Ok(None);
    };
    anyhow::ensure!(
        !underlying_symbol.trim().is_empty(),
        "{ALPACA_OPTION_CHAIN_UNDERLYING_PARAM} cannot be empty",
    );

    let expiration = params
        .get_str(ALPACA_OPTION_CHAIN_EXPIRATION_PARAM)
        .map(str::to_string);
    let min_expiration = params
        .get_str(ALPACA_OPTION_CHAIN_MIN_EXPIRATION_PARAM)
        .map(str::to_string)
        .or_else(|| expiration.clone())
        .ok_or_else(|| {
            anyhow!(
                "{ALPACA_OPTION_CHAIN_MIN_EXPIRATION_PARAM} or \
                 {ALPACA_OPTION_CHAIN_EXPIRATION_PARAM} is required"
            )
        })?;
    let max_expiration = params
        .get_str(ALPACA_OPTION_CHAIN_MAX_EXPIRATION_PARAM)
        .map(str::to_string)
        .or(expiration)
        .unwrap_or_else(|| min_expiration.clone());
    let option_type = params
        .get_str(ALPACA_OPTION_CHAIN_TYPE_PARAM)
        .map(parse_option_type_param)
        .transpose()?
        .flatten();

    Ok(Some(AlpacaOptionInstrumentsRequest {
        underlying_symbol: underlying_symbol.trim().to_ascii_uppercase(),
        min_expiration,
        max_expiration,
        option_type,
    }))
}

fn parse_option_type_param(value: &str) -> anyhow::Result<Option<AlpacaOptionType>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "all" | "both" | "any" => Ok(None),
        "call" | "calls" | "c" => Ok(Some(AlpacaOptionType::Call)),
        "put" | "puts" | "p" => Ok(Some(AlpacaOptionType::Put)),
        other => Err(anyhow!(
            "unsupported {ALPACA_OPTION_CHAIN_TYPE_PARAM}={other:?}, expected all, call, or put"
        )),
    }
}

async fn poll_option_snapshots(
    http_client: &AlpacaHttpClient,
    sender: &tokio::sync::mpsc::UnboundedSender<DataEvent>,
    instruments: &Arc<AtomicMap<InstrumentId, InstrumentAny>>,
    subscriptions: &Arc<Mutex<OptionSnapshotSubscriptions>>,
    market_data_state: &Arc<OptionMarketDataRuntimeState>,
    quote_snapshot_fallback_after: Duration,
    feed: &str,
    clock: &'static AtomicTime,
) -> anyhow::Result<bool> {
    let include_quote_snapshots =
        !market_data_state.is_quote_stream_fresh(clock, quote_snapshot_fallback_after);
    let instrument_ids = {
        let subscriptions = subscriptions.lock().expect(MUTEX_POISONED);
        if subscriptions.is_empty() {
            return Ok(false);
        }
        subscriptions.instrument_ids(include_quote_snapshots)
    };
    if instrument_ids.is_empty() {
        return Ok(true);
    }

    let mut request = OptionSnapshotsRequest::for_symbols(
        instrument_ids
            .iter()
            .map(|instrument_id| alpaca_symbol_from_instrument_id(*instrument_id)),
    );
    request.feed = Some(feed.to_string());

    let response = http_client
        .option_snapshots(&request)
        .await
        .context("failed to request Alpaca option snapshots")?;

    for instrument_id in instrument_ids {
        let Some(snapshot) = option_snapshot_for_instrument(&response.snapshots, instrument_id)
        else {
            log::debug!("Alpaca option snapshot missing for {instrument_id}");
            continue;
        };
        let Some(instrument) = instruments.get_cloned(&instrument_id) else {
            log::debug!("Alpaca instrument metadata missing for snapshot {instrument_id}");
            continue;
        };
        let (emit_quote, emit_greeks) = {
            let subscriptions = subscriptions.lock().expect(MUTEX_POISONED);
            (
                subscriptions.should_emit_quote(&instrument_id),
                subscriptions.should_emit_greeks(&instrument_id),
            )
        };

        if emit_quote
            && let Some(quote) = option_quote_tick(
                instrument_id,
                &instrument,
                snapshot.latest_quote.as_ref(),
                clock,
            )
        {
            let quote_count = subscriptions
                .lock()
                .expect(MUTEX_POISONED)
                .quote_instrument_ids
                .len();
            market_data_state.mark_source(
                OptionMarketDataSource::SnapshotFallback,
                feed,
                "snapshot_quote",
                quote_count,
                0,
            );
            send_data_event(sender, DataEvent::Data(Data::Quote(quote)), "option quote");
        }
        if emit_greeks
            && let Some(greeks) = option_greeks(instrument_id, &instrument, snapshot, clock)
        {
            send_data_event(sender, DataEvent::OptionGreeks(greeks), "option greeks");
        }
    }

    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn stream_option_market_data(
    url: String,
    credential: AlpacaCredential,
    sender: tokio::sync::mpsc::UnboundedSender<DataEvent>,
    instruments: Arc<AtomicMap<InstrumentId, InstrumentAny>>,
    subscriptions: Arc<Mutex<OptionMarketDataSubscriptions>>,
    market_data_state: Arc<OptionMarketDataRuntimeState>,
    feed: String,
    clock: &'static AtomicTime,
    mut cmd_rx: mpsc::UnboundedReceiver<OptionMarketDataCommand>,
) -> anyhow::Result<()> {
    let (message_handler, mut raw_rx) = channel_message_handler();
    let cfg = WebSocketConfig {
        url,
        headers: vec![
            (
                "APCA-API-KEY-ID".to_string(),
                credential.api_key().to_string(),
            ),
            (
                "APCA-API-SECRET-KEY".to_string(),
                credential.api_secret().to_string(),
            ),
            (
                "Content-Type".to_string(),
                "application/msgpack".to_string(),
            ),
        ],
        heartbeat: Some(OPTION_MARKET_DATA_WS_HEARTBEAT_SECS),
        heartbeat_msg: None,
        reconnect_timeout_ms: Some(OPTION_MARKET_DATA_RECONNECT_TIMEOUT_MS),
        reconnect_delay_initial_ms: Some(OPTION_MARKET_DATA_RECONNECT_DELAY_INITIAL_MS),
        reconnect_delay_max_ms: Some(OPTION_MARKET_DATA_RECONNECT_DELAY_MAX_MS),
        reconnect_backoff_factor: Some(OPTION_MARKET_DATA_RECONNECT_BACKOFF_FACTOR),
        reconnect_jitter_ms: Some(OPTION_MARKET_DATA_RECONNECT_JITTER_MS),
        reconnect_max_attempts: None,
        idle_timeout_ms: None,
        backend: TransportBackend::default(),
        proxy_url: None,
    };

    let client = WebSocketClient::connect(cfg, Some(message_handler), None, None, Vec::new(), None)
        .await
        .context("failed to connect Alpaca option market-data stream")?;
    send_current_option_market_data_subscription(&client, &subscriptions).await?;

    loop {
        tokio::select! {
            Some(command) = cmd_rx.recv() => {
                send_option_market_data_command(&client, command).await?;
            }
            Some(raw) = raw_rx.recv() => {
                match raw {
                    Message::Ping(data) => {
                        if let Err(error) = client.send_pong(data.to_vec()).await {
                            log::warn!("Failed to send Alpaca option market-data pong: {error}");
                        }
                    }
                    Message::Close(_) => {
                        client.notify_closed();
                        return Err(anyhow!("Alpaca option market-data stream closed"));
                    }
                    other => {
                        let messages = parse_option_market_data_message(&other)?;
                        for message in messages {
                            handle_option_market_data_message(
                                message,
                                &client,
                                &sender,
                                &instruments,
                                &subscriptions,
                                &market_data_state,
                                &feed,
                                clock,
                            )
                            .await?;
                        }
                    }
                }
            }
            else => {
                client.notify_closed();
                return Err(anyhow!("Alpaca option market-data stream channel closed"));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_option_market_data_message(
    message: AlpacaOptionMarketDataMessage,
    client: &WebSocketClient,
    sender: &tokio::sync::mpsc::UnboundedSender<DataEvent>,
    instruments: &Arc<AtomicMap<InstrumentId, InstrumentAny>>,
    subscriptions: &Arc<Mutex<OptionMarketDataSubscriptions>>,
    market_data_state: &Arc<OptionMarketDataRuntimeState>,
    feed: &str,
    clock: &'static AtomicTime,
) -> anyhow::Result<()> {
    match message {
        AlpacaOptionMarketDataMessage::Quote(quote) => {
            if let Some((instrument_id, instrument)) =
                stream_instrument(&quote.symbol, subscriptions, instruments)
                && let Some(tick) =
                    option_stream_quote_tick(instrument_id, &instrument, &quote, clock)
            {
                let (quote_count, trade_count) = option_market_data_subscription_counts(
                    &subscriptions.lock().expect(MUTEX_POISONED),
                );
                market_data_state.mark_stream_quote(feed, quote_count, trade_count, clock);
                send_data_event(
                    sender,
                    DataEvent::Data(Data::Quote(tick)),
                    "option stream quote",
                );
            }
        }
        AlpacaOptionMarketDataMessage::Trade(trade) => {
            if let Some((instrument_id, instrument)) =
                stream_instrument(&trade.symbol, subscriptions, instruments)
                && let Some(tick) =
                    option_stream_trade_tick(instrument_id, &instrument, &trade, clock)
            {
                let (quote_count, trade_count) = option_market_data_subscription_counts(
                    &subscriptions.lock().expect(MUTEX_POISONED),
                );
                market_data_state.mark_source(
                    OptionMarketDataSource::Stream,
                    feed,
                    "trade",
                    quote_count,
                    trade_count,
                );
                send_data_event(
                    sender,
                    DataEvent::Data(Data::Trade(tick)),
                    "option stream trade",
                );
            }
        }
        AlpacaOptionMarketDataMessage::Subscription(subscription) => {
            let quote_count = subscription.quotes.len();
            let trade_count = subscription.trades.len();
            log::info!(
                "Alpaca option market-data subscriptions: quotes={} trades={}",
                quote_count,
                trade_count
            );
        }
        AlpacaOptionMarketDataMessage::Success(success) => {
            log::info!(
                "Alpaca option market-data stream success: {}",
                success.msg.as_deref().unwrap_or("ok")
            );
        }
        AlpacaOptionMarketDataMessage::Error(error) => {
            let reason = format!(
                "code={} msg={}",
                error
                    .code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string()),
                error.msg.as_deref().unwrap_or("unknown")
            );
            let (quote_count, trade_count) = option_market_data_subscription_counts(
                &subscriptions.lock().expect(MUTEX_POISONED),
            );
            market_data_state.mark_source(
                OptionMarketDataSource::Error,
                feed,
                &reason,
                quote_count,
                trade_count,
            );
            return Err(anyhow!("Alpaca option market-data stream error: {reason}"));
        }
        AlpacaOptionMarketDataMessage::Reconnected => {
            log::info!("Alpaca option market-data stream reconnected");
            send_current_option_market_data_subscription(client, subscriptions).await?;
        }
    }

    Ok(())
}

async fn send_current_option_market_data_subscription(
    client: &WebSocketClient,
    subscriptions: &Arc<Mutex<OptionMarketDataSubscriptions>>,
) -> anyhow::Result<()> {
    let (quotes, trades) = {
        let subscriptions = subscriptions.lock().expect(MUTEX_POISONED);
        (subscriptions.quote_symbols(), subscriptions.trade_symbols())
    };
    send_option_market_data_request(
        client,
        AlpacaOptionMarketDataSubscriptionRequest::subscribe(quotes, trades),
    )
    .await
}

async fn send_option_market_data_command(
    client: &WebSocketClient,
    command: OptionMarketDataCommand,
) -> anyhow::Result<()> {
    let request = match command {
        OptionMarketDataCommand::Subscribe { quotes, trades } => {
            AlpacaOptionMarketDataSubscriptionRequest::subscribe(quotes, trades)
        }
        OptionMarketDataCommand::Unsubscribe { quotes, trades } => {
            AlpacaOptionMarketDataSubscriptionRequest::unsubscribe(quotes, trades)
        }
    };
    send_option_market_data_request(client, request).await
}

async fn send_option_market_data_request(
    client: &WebSocketClient,
    request: AlpacaOptionMarketDataSubscriptionRequest,
) -> anyhow::Result<()> {
    if request.is_empty() {
        return Ok(());
    }

    let payload = request.to_msgpack_bytes()?;
    client
        .send_bytes(payload, None)
        .await
        .context("failed to send Alpaca option market-data subscription request")
}

fn stream_instrument(
    symbol: &str,
    subscriptions: &Arc<Mutex<OptionMarketDataSubscriptions>>,
    instruments: &Arc<AtomicMap<InstrumentId, InstrumentAny>>,
) -> Option<(InstrumentId, InstrumentAny)> {
    let instrument_id = subscriptions
        .lock()
        .expect(MUTEX_POISONED)
        .instrument_id_for_symbol(symbol)?;
    let instrument = instruments.get_cloned(&instrument_id)?;
    Some((instrument_id, instrument))
}

async fn request_bars_from_http(
    http_client: &AlpacaHttpClient,
    instruments: &Arc<AtomicMap<InstrumentId, InstrumentAny>>,
    bar_type: BarType,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    limit: Option<usize>,
    option_feed: &str,
    stock_feed: &str,
    clock: &'static AtomicTime,
) -> anyhow::Result<Vec<Bar>> {
    if parse_alpaca_option_instrument_id(bar_type.instrument_id()).is_ok() {
        request_option_bars_from_http(
            http_client,
            instruments,
            bar_type,
            start,
            end,
            limit,
            option_feed,
            clock,
        )
        .await
    } else {
        request_stock_bars_from_http(
            http_client,
            instruments,
            bar_type,
            start,
            end,
            limit,
            stock_feed,
            clock,
        )
        .await
    }
}

async fn request_option_bars_from_http(
    http_client: &AlpacaHttpClient,
    instruments: &Arc<AtomicMap<InstrumentId, InstrumentAny>>,
    bar_type: BarType,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    limit: Option<usize>,
    feed: &str,
    clock: &'static AtomicTime,
) -> anyhow::Result<Vec<Bar>> {
    anyhow::ensure!(
        bar_type.is_standard(),
        "Alpaca option bars require a standard bar type, got {bar_type}",
    );
    anyhow::ensure!(
        bar_type.is_externally_aggregated(),
        "Alpaca option bars require EXTERNAL aggregation, got {bar_type}",
    );
    anyhow::ensure!(
        bar_type.spec().price_type == PriceType::Last,
        "Alpaca option bars require LAST price type, got {}",
        bar_type.spec().price_type,
    );

    let instrument_id = bar_type.instrument_id();
    let start = start.ok_or_else(|| anyhow!("Alpaca option bar requests require a start time"))?;
    let instrument = match instruments.get_cloned(&instrument_id) {
        Some(instrument) => instrument,
        None => {
            let provider = AlpacaOptionContractProvider::new(http_client.clone());
            let instrument = AlpacaDataClient::load_option_instrument(&provider, instrument_id)
                .await
                .with_context(|| format!("failed to load Alpaca instrument {instrument_id}"))?;
            instruments.insert(instrument.id(), instrument.clone());
            instrument
        }
    };

    let symbol = alpaca_symbol_from_instrument_id(instrument_id);
    let mut request = OptionBarsRequest::for_symbols(
        [symbol.clone()],
        alpaca_bar_timeframe(bar_type, "option")?,
        start.to_rfc3339(),
    );
    request.end = end.map(|end| end.to_rfc3339());
    request.feed = Some(feed.to_string());
    request.limit = limit.unwrap_or(10_000).clamp(1, 10_000);

    let response = http_client
        .option_bars(&request)
        .await
        .context("failed to request Alpaca option bars")?;
    let bars = bars_for_symbol(&response.bars, &symbol)
        .iter()
        .filter_map(|bar| {
            alpaca_historical_bar(
                bar_type,
                &instrument,
                bar.timestamp.as_deref(),
                bar.open,
                bar.high,
                bar.low,
                bar.close,
                bar.volume,
                clock,
            )
        })
        .collect::<Vec<_>>();
    Ok(sort_and_limit_bars(bars, limit))
}

async fn request_stock_bars_from_http(
    http_client: &AlpacaHttpClient,
    instruments: &Arc<AtomicMap<InstrumentId, InstrumentAny>>,
    bar_type: BarType,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    limit: Option<usize>,
    feed: &str,
    clock: &'static AtomicTime,
) -> anyhow::Result<Vec<Bar>> {
    anyhow::ensure!(
        bar_type.is_standard(),
        "Alpaca stock bars require a standard bar type, got {bar_type}",
    );
    anyhow::ensure!(
        bar_type.is_externally_aggregated(),
        "Alpaca stock bars require EXTERNAL aggregation, got {bar_type}",
    );
    anyhow::ensure!(
        bar_type.spec().price_type == PriceType::Last,
        "Alpaca stock bars require LAST price type, got {}",
        bar_type.spec().price_type,
    );

    let instrument_id = bar_type.instrument_id();
    anyhow::ensure!(
        instrument_id.venue == Venue::new(ALPACA_VENUE),
        "expected Alpaca venue {}, got {}",
        ALPACA_VENUE,
        instrument_id.venue,
    );
    let start = start.ok_or_else(|| anyhow!("Alpaca stock bar requests require a start time"))?;
    let instrument = match instruments.get_cloned(&instrument_id) {
        Some(instrument) => instrument,
        None => {
            let instrument = stock_bar_instrument(instrument_id, clock.get_time_ns())?;
            instruments.insert(instrument.id(), instrument.clone());
            instrument
        }
    };
    let symbol = instrument_id.symbol.as_str().to_string();
    let mut request = StockBarsRequest::for_symbols(
        [symbol.clone()],
        alpaca_bar_timeframe(bar_type, "stock")?,
        start.to_rfc3339(),
    );
    request.end = end.map(|end| end.to_rfc3339());
    request.feed = Some(feed.to_string());
    request.limit = limit.unwrap_or(10_000).clamp(1, 10_000);

    let response = http_client
        .stock_bars(&request)
        .await
        .context("failed to request Alpaca stock bars")?;
    let bars = bars_for_symbol(&response.bars, &symbol)
        .iter()
        .filter_map(|bar| {
            alpaca_historical_bar(
                bar_type,
                &instrument,
                bar.timestamp.as_deref(),
                bar.open,
                bar.high,
                bar.low,
                bar.close,
                bar.volume,
                clock,
            )
        })
        .collect::<Vec<_>>();
    Ok(sort_and_limit_bars(bars, limit))
}

fn alpaca_bar_timeframe(bar_type: BarType, instrument_kind: &str) -> anyhow::Result<String> {
    let spec = bar_type.spec();
    let step = spec.step.get();
    match spec.aggregation {
        BarAggregation::Minute => Ok(format!("{step}Min")),
        BarAggregation::Hour => Ok(format!("{step}Hour")),
        BarAggregation::Day => Ok(format!("{step}Day")),
        other => Err(anyhow!(
            "unsupported Alpaca {instrument_kind} bar aggregation {other}; expected minute, hour, or day"
        )),
    }
}

fn alpaca_historical_bar(
    bar_type: BarType,
    instrument: &InstrumentAny,
    timestamp: Option<&str>,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    volume: Option<u64>,
    clock: &'static AtomicTime,
) -> Option<Bar> {
    let ts_event = timestamp.and_then(parse_rfc3339_timestamp)?;
    let ts_init = clock.get_time_ns();
    let open = instrument.try_make_price(open?).ok()?;
    let high = instrument.try_make_price(high?).ok()?;
    let low = instrument.try_make_price(low?).ok()?;
    let close = instrument.try_make_price(close?).ok()?;
    let volume = instrument.try_make_qty(volume? as f64, None).ok()?;

    Bar::new_checked(bar_type, open, high, low, close, volume, ts_event, ts_init).ok()
}

fn bars_for_symbol<T: Clone>(bars: &BTreeMap<String, Vec<T>>, symbol: &str) -> Vec<T> {
    bars.get(symbol)
        .or_else(|| {
            bars.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(symbol))
                .map(|(_, bars)| bars)
        })
        .cloned()
        .unwrap_or_default()
}

fn sort_and_limit_bars(mut bars: Vec<Bar>, limit: Option<usize>) -> Vec<Bar> {
    bars.sort_by_key(|bar| bar.ts_event);
    if let Some(limit) = limit
        && bars.len() > limit
    {
        bars.truncate(limit);
    }
    bars
}

fn stock_bar_instrument(
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> anyhow::Result<InstrumentAny> {
    Ok(Equity::new_checked(
        instrument_id,
        Symbol::from(instrument_id.symbol.as_str()),
        None,
        Currency::USD(),
        4,
        Price::from("0.0001"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        ts_init,
        ts_init,
    )?
    .into_any())
}

async fn stock_snapshot_price(
    http_client: &AlpacaHttpClient,
    underlying: &str,
    feed: &str,
) -> anyhow::Result<Option<f64>> {
    let mut request = StockSnapshotsRequest::for_symbols([underlying.to_string()]);
    request.feed = Some(feed.to_string());
    let response = http_client.stock_snapshots(&request).await?;
    Ok(snapshot_for_symbol(&response.snapshots, underlying)
        .and_then(|snapshot| snapshot.latest_price()))
}

fn option_snapshot_for_instrument<'a>(
    snapshots: &'a BTreeMap<String, AlpacaOptionSnapshot>,
    instrument_id: InstrumentId,
) -> Option<&'a AlpacaOptionSnapshot> {
    let raw_symbol = instrument_id.symbol.as_str();
    let canonical_symbol = raw_symbol.strip_prefix("O:").unwrap_or(raw_symbol);
    snapshot_for_symbol(snapshots, canonical_symbol)
        .or_else(|| snapshot_for_symbol(snapshots, raw_symbol))
}

fn snapshot_for_symbol<'a, T>(snapshots: &'a BTreeMap<String, T>, symbol: &str) -> Option<&'a T> {
    snapshots.get(symbol).or_else(|| {
        snapshots
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(symbol))
            .map(|(_, snapshot)| snapshot)
    })
}

fn option_quote_tick(
    instrument_id: InstrumentId,
    instrument: &InstrumentAny,
    quote: Option<&AlpacaOptionQuote>,
    clock: &'static AtomicTime,
) -> Option<QuoteTick> {
    let quote = quote?;
    if !quote.is_valid() {
        return None;
    }

    let bid = quote.bid_price?;
    let ask = quote.ask_price?;
    let bid_size = quote.bid_size?;
    let ask_size = quote.ask_size?;
    let ts_init = clock.get_time_ns();
    let ts_event = quote
        .timestamp
        .as_deref()
        .and_then(parse_rfc3339_timestamp)
        .unwrap_or(ts_init);

    let bid_price = instrument.try_make_price(bid).ok()?;
    let ask_price = instrument.try_make_price(ask).ok()?;
    let bid_size = instrument.try_make_qty(bid_size as f64, None).ok()?;
    let ask_size = instrument.try_make_qty(ask_size as f64, None).ok()?;

    QuoteTick::new_checked(
        instrument_id,
        bid_price,
        ask_price,
        bid_size,
        ask_size,
        ts_event,
        ts_init,
    )
    .ok()
}

fn option_stream_quote_tick(
    instrument_id: InstrumentId,
    instrument: &InstrumentAny,
    quote: &AlpacaOptionStreamQuote,
    clock: &'static AtomicTime,
) -> Option<QuoteTick> {
    if quote.bid_price <= 0.0 || quote.ask_price <= 0.0 || quote.ask_price < quote.bid_price {
        return None;
    }

    let ts_init = clock.get_time_ns();
    let ts_event = parse_rfc3339_timestamp(&quote.timestamp).unwrap_or(ts_init);
    let bid_price = instrument.try_make_price(quote.bid_price).ok()?;
    let ask_price = instrument.try_make_price(quote.ask_price).ok()?;
    let bid_size = instrument.try_make_qty(quote.bid_size as f64, None).ok()?;
    let ask_size = instrument.try_make_qty(quote.ask_size as f64, None).ok()?;

    QuoteTick::new_checked(
        instrument_id,
        bid_price,
        ask_price,
        bid_size,
        ask_size,
        ts_event,
        ts_init,
    )
    .ok()
}

fn option_stream_trade_tick(
    instrument_id: InstrumentId,
    instrument: &InstrumentAny,
    trade: &AlpacaOptionStreamTrade,
    clock: &'static AtomicTime,
) -> Option<TradeTick> {
    if trade.price <= 0.0 || trade.size == 0 {
        return None;
    }

    let ts_init = clock.get_time_ns();
    let ts_event = parse_rfc3339_timestamp(&trade.timestamp).unwrap_or(ts_init);
    let price = instrument.try_make_price(trade.price).ok()?;
    let size = instrument.try_make_qty(trade.size as f64, None).ok()?;
    let trade_id = TradeId::from(
        format!(
            "{}:{}:{}:{}:{}",
            trade.symbol,
            trade.timestamp,
            trade.exchange.as_deref().unwrap_or(""),
            trade.price,
            trade.size,
        )
        .as_str(),
    );

    TradeTick::new_checked(
        instrument_id,
        price,
        size,
        AggressorSide::NoAggressor,
        trade_id,
        ts_event,
        ts_init,
    )
    .ok()
}

fn option_greeks(
    instrument_id: InstrumentId,
    instrument: &InstrumentAny,
    snapshot: &AlpacaOptionSnapshot,
    clock: &'static AtomicTime,
) -> Option<OptionGreeks> {
    let greeks = snapshot.greeks.as_ref()?;
    if !greeks.has_any() {
        return None;
    }

    let ts_init = clock.get_time_ns();
    let ts_event = snapshot
        .latest_quote
        .as_ref()
        .and_then(|quote| quote.timestamp.as_deref())
        .and_then(parse_rfc3339_timestamp)
        .unwrap_or(ts_init);

    Some(OptionGreeks {
        instrument_id,
        convention: GreeksConvention::BlackScholes,
        greeks: OptionGreekValues {
            delta: greeks.delta.unwrap_or_default(),
            gamma: greeks.gamma.unwrap_or_default(),
            vega: greeks.vega.unwrap_or_default(),
            theta: greeks.theta.unwrap_or_default(),
            rho: greeks.rho.unwrap_or_default(),
        },
        mark_iv: snapshot.implied_volatility,
        bid_iv: None,
        ask_iv: None,
        underlying_price: None,
        open_interest: option_open_interest(instrument),
        ts_event,
        ts_init,
    })
}

fn option_open_interest(instrument: &InstrumentAny) -> Option<f64> {
    match instrument {
        InstrumentAny::OptionContract(option) => option
            .info
            .as_ref()
            .and_then(|info| info.get_f64(ALPACA_OPEN_INTEREST_INFO_KEY)),
        _ => None,
    }
}

fn alpaca_symbol_from_instrument_id(instrument_id: InstrumentId) -> String {
    instrument_id
        .symbol
        .as_str()
        .strip_prefix("O:")
        .unwrap_or(instrument_id.symbol.as_str())
        .to_string()
}

fn instrument_symbols_for_stream(instrument_ids: &BTreeSet<InstrumentId>) -> Vec<String> {
    instrument_ids
        .iter()
        .map(|instrument_id| alpaca_symbol_from_instrument_id(*instrument_id))
        .collect()
}

fn option_market_data_subscription_counts(
    subscriptions: &OptionMarketDataSubscriptions,
) -> (usize, usize) {
    (
        subscriptions.quote_instrument_ids.len(),
        subscriptions.trade_instrument_ids.len(),
    )
}

fn parse_rfc3339_timestamp(value: &str) -> Option<UnixNanos> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| timestamp.with_timezone(&Utc).timestamp_nanos_opt())
        .and_then(|timestamp| u64::try_from(timestamp).ok())
        .map(UnixNanos::from)
}

fn send_data_event(
    sender: &tokio::sync::mpsc::UnboundedSender<DataEvent>,
    event: DataEvent,
    description: &str,
) {
    if let Err(e) = sender.send(event) {
        log::error!("Failed to send Alpaca {description}: {e}");
    }
}

fn option_contract_matches(
    contract: &AlpacaOptionContract,
    parts: &AlpacaOptionSymbolParts,
) -> bool {
    contract.symbol == parts.symbol
        || canonical_alpaca_option_symbol(&contract.symbol) == parts.canonical_symbol
}
