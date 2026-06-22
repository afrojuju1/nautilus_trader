//! Alpaca live data client.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
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
            DataResponse, ForwardPricesResponse, InstrumentResponse, InstrumentsResponse,
            RequestForwardPrices, RequestInstrument, RequestInstruments, SubscribeInstrument,
            SubscribeInstrumentStatus, SubscribeInstruments, SubscribeOptionGreeks,
            SubscribeQuotes, UnsubscribeInstrument, UnsubscribeInstrumentStatus,
            UnsubscribeInstruments, UnsubscribeOptionGreeks, UnsubscribeQuotes,
        },
    },
};
use nautilus_core::{
    AtomicMap, MUTEX_POISONED, Params, UnixNanos,
    datetime::datetime_to_unix_nanos,
    time::{AtomicTime, get_atomic_clock_realtime},
};
use nautilus_model::{
    data::{Data, ForwardPrice, QuoteTick, greeks::OptionGreekValues, option_chain::OptionGreeks},
    enums::GreeksConvention,
    identifiers::{ClientId, InstrumentId, Venue},
    instruments::{Instrument, InstrumentAny},
};
use rust_decimal::Decimal;
use tokio::task::JoinHandle;

use crate::{
    common::consts::{
        ALPACA_OPEN_INTEREST_INFO_KEY, ALPACA_OPTION_CHAIN_EXPIRATION_PARAM,
        ALPACA_OPTION_CHAIN_MAX_EXPIRATION_PARAM, ALPACA_OPTION_CHAIN_MIN_EXPIRATION_PARAM,
        ALPACA_OPTION_CHAIN_TYPE_PARAM, ALPACA_OPTION_CHAIN_UNDERLYING_PARAM, ALPACA_VENUE,
    },
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaOptionContract, AlpacaOptionQuote, AlpacaOptionSnapshot, AlpacaOptionType,
            OptionSnapshotsRequest, StockSnapshotsRequest,
        },
    },
    providers::AlpacaOptionContractProvider,
};

const DEFAULT_OPTION_SNAPSHOT_POLL_SECS: u64 = 5;

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

    fn instrument_ids(&self) -> Vec<InstrumentId> {
        self.quote_instrument_ids
            .union(&self.greeks_instrument_ids)
            .copied()
            .collect()
    }

    fn should_emit_quote(&self, instrument_id: &InstrumentId) -> bool {
        self.quote_instrument_ids.contains(instrument_id)
    }

    fn should_emit_greeks(&self, instrument_id: &InstrumentId) -> bool {
        self.greeks_instrument_ids.contains(instrument_id)
    }
}

#[derive(Clone, Debug)]
struct AlpacaOptionSymbolParts {
    symbol: String,
    canonical_symbol: String,
    underlying_symbol: String,
    expiration_date: String,
    option_type: AlpacaOptionType,
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
        let interval = self.option_snapshot_poll_interval();
        let feed = self.config.option_feed.as_str().to_string();
        let clock = self.clock;

        self.spawn_task("option_snapshot_poller", async move {
            loop {
                match poll_option_snapshots(
                    &http_client,
                    &sender,
                    &instruments,
                    &subscriptions,
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

    async fn load_option_instrument(
        provider: &AlpacaOptionContractProvider,
        instrument_id: InstrumentId,
    ) -> anyhow::Result<InstrumentAny> {
        let parts = parse_alpaca_option_symbol(instrument_id)?;
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
        log::info!("Connected: client_id={}", self.client_id);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if !self.is_connected() {
            return Ok(());
        }

        self.abort_pending_tasks();
        self.mark_option_snapshot_poller_stopped();
        self.is_connected.store(false, Ordering::Release);
        log::info!("Disconnected: client_id={}", self.client_id);
        Ok(())
    }

    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        log::debug!(
            "Subscribing to Alpaca option snapshot quotes: {}",
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
        self.subscribe_option_snapshot_quotes(instrument_id);

        Ok(())
    }

    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        log::debug!(
            "Unsubscribing from Alpaca option snapshot quotes: {}",
            cmd.instrument_id,
        );
        self.option_snapshot_subscriptions
            .lock()
            .expect(MUTEX_POISONED)
            .quote_instrument_ids
            .remove(&cmd.instrument_id);
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
    feed: &str,
    clock: &'static AtomicTime,
) -> anyhow::Result<bool> {
    let instrument_ids = {
        let subscriptions = subscriptions.lock().expect(MUTEX_POISONED);
        if subscriptions.is_empty() {
            return Ok(false);
        }
        subscriptions.instrument_ids()
    };

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
        || canonical_option_symbol(&contract.symbol) == parts.canonical_symbol
}

fn canonical_option_symbol(symbol: &str) -> String {
    symbol.strip_prefix("O:").unwrap_or(symbol).to_string()
}

fn parse_alpaca_option_symbol(
    instrument_id: InstrumentId,
) -> anyhow::Result<AlpacaOptionSymbolParts> {
    if instrument_id.venue != Venue::new(ALPACA_VENUE) {
        return Err(anyhow!(
            "expected Alpaca venue {}, got {}",
            ALPACA_VENUE,
            instrument_id.venue
        ));
    }

    let symbol = instrument_id.symbol.as_str();
    let canonical_symbol = canonical_option_symbol(symbol);
    let first_digit = canonical_symbol
        .find(|ch: char| ch.is_ascii_digit())
        .ok_or_else(|| anyhow!("invalid Alpaca option symbol `{symbol}`: missing expiration"))?;

    let (underlying_symbol, option_details) = canonical_symbol.split_at(first_digit);
    if underlying_symbol.is_empty() {
        return Err(anyhow!(
            "invalid Alpaca option symbol `{symbol}`: missing underlying"
        ));
    }

    if option_details.len() < 15 {
        return Err(anyhow!(
            "invalid Alpaca option symbol `{symbol}`: expected YYMMDD, option side, and strike"
        ));
    }

    let expiration = &option_details[..6];
    let side = option_details.as_bytes()[6] as char;
    let strike = &option_details[7..];
    if !expiration.chars().all(|ch| ch.is_ascii_digit())
        || strike.is_empty()
        || !strike.chars().all(|ch| ch.is_ascii_digit())
    {
        return Err(anyhow!(
            "invalid Alpaca option symbol `{symbol}`: malformed expiration or strike"
        ));
    }

    let option_type = match side {
        'C' => AlpacaOptionType::Call,
        'P' => AlpacaOptionType::Put,
        _ => {
            return Err(anyhow!(
                "invalid Alpaca option symbol `{symbol}`: expected C or P option side"
            ));
        }
    };

    let expiration_date = format!(
        "20{}-{}-{}",
        &expiration[..2],
        &expiration[2..4],
        &expiration[4..6]
    );
    let underlying_symbol = underlying_symbol.to_string();

    Ok(AlpacaOptionSymbolParts {
        symbol: symbol.to_string(),
        canonical_symbol,
        underlying_symbol,
        expiration_date,
        option_type,
    })
}
