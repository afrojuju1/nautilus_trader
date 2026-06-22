//! Alpaca live data client.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, anyhow};
use nautilus_common::{
    clients::DataClient,
    live::{runner::get_data_event_sender, runtime::get_runtime},
    messages::{
        DataEvent,
        data::{
            DataResponse, InstrumentResponse, InstrumentsResponse, RequestInstrument,
            RequestInstruments, SubscribeInstrument, SubscribeInstruments, UnsubscribeInstrument,
            UnsubscribeInstruments,
        },
    },
};
use nautilus_core::{
    AtomicMap, MUTEX_POISONED,
    datetime::datetime_to_unix_nanos,
    time::{AtomicTime, get_atomic_clock_realtime},
};
use nautilus_model::{
    identifiers::{ClientId, InstrumentId, Venue},
    instruments::{Instrument, InstrumentAny},
};
use tokio::task::JoinHandle;

use crate::{
    common::consts::ALPACA_VENUE,
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaOptionContract, AlpacaOptionType},
    },
    providers::AlpacaOptionContractProvider,
};

#[derive(Clone, Debug)]
struct AlpacaOptionSymbolParts {
    symbol: String,
    canonical_symbol: String,
    underlying_symbol: String,
    expiration_date: String,
    option_type: AlpacaOptionType,
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
        self.is_connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        log::debug!("Resetting Alpaca data client {}", self.client_id);
        self.abort_pending_tasks();
        self.instruments.rcu(|instruments| instruments.clear());
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
        log::info!("Connected: client_id={}", self.client_id);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if !self.is_connected() {
            return Ok(());
        }

        self.abort_pending_tasks();
        self.is_connected.store(false, Ordering::Release);
        log::info!("Disconnected: client_id={}", self.client_id);
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

    fn request_instruments(&self, request: RequestInstruments) -> anyhow::Result<()> {
        let request_id = request.request_id;
        let client_id = request.client_id.unwrap_or(self.client_id);
        let venue = self.venue();
        let start_nanos = datetime_to_unix_nanos(request.start);
        let end_nanos = datetime_to_unix_nanos(request.end);
        let params = request.params;
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
