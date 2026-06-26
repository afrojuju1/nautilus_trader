//! Live market-data dual-write worker.

use std::{
    env, fs,
    path::PathBuf,
    sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, anyhow};
use indexmap::IndexMap;
use nautilus_model::{data::QuoteTick, identifiers::InstrumentId};
use uuid::Uuid;

use crate::{
    backend::catalog::ParquetDataCatalog,
    warehouse::clickhouse::{
        ClickHouseConnectOptions, ClickHouseQuoteTickRow, write_quote_tick_rows,
    },
};

pub const MARKET_DATA_DUAL_WRITE_ENV: &str = "NAUTILUS_MARKET_DATA_DUAL_WRITE";
pub const MARKET_DATA_CATALOG_URI_ENV: &str = "NAUTILUS_MARKET_DATA_CATALOG_URI";
pub const MARKET_DATA_WAREHOUSE_SOURCE_ENV: &str = "NAUTILUS_MARKET_DATA_WAREHOUSE_SOURCE";
pub const MARKET_DATA_WAREHOUSE_BATCH_SIZE_ENV: &str = "NAUTILUS_MARKET_DATA_WAREHOUSE_BATCH_SIZE";
pub const MARKET_DATA_WAREHOUSE_FLUSH_MS_ENV: &str = "NAUTILUS_MARKET_DATA_WAREHOUSE_FLUSH_MS";
pub const MARKET_DATA_WAREHOUSE_QUEUE_CAPACITY_ENV: &str =
    "NAUTILUS_MARKET_DATA_WAREHOUSE_QUEUE_CAPACITY";

const DEFAULT_SOURCE: &str = "live";
const DEFAULT_BATCH_SIZE: usize = 500;
const DEFAULT_FLUSH_MS: u64 = 1_000;
const DEFAULT_QUEUE_CAPACITY: usize = 10_000;

/// Configuration for live market-data writes to catalog and ClickHouse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketDataDualWriteConfig {
    pub catalog_uri: String,
    pub source: String,
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub queue_capacity: usize,
    pub ingest_run_id: Uuid,
    pub clickhouse: ClickHouseConnectOptions,
}

impl MarketDataDualWriteConfig {
    /// Returns dual-write config when `NAUTILUS_MARKET_DATA_DUAL_WRITE` is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when dual-write is enabled but required env vars are invalid.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        if !env_flag_enabled(MARKET_DATA_DUAL_WRITE_ENV)? {
            return Ok(None);
        }

        let catalog_uri = catalog_uri_from_env()?;
        let source = env::var(MARKET_DATA_WAREHOUSE_SOURCE_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_SOURCE.to_string());
        let batch_size =
            positive_usize_env(MARKET_DATA_WAREHOUSE_BATCH_SIZE_ENV, DEFAULT_BATCH_SIZE)?;
        let flush_ms = positive_u64_env(MARKET_DATA_WAREHOUSE_FLUSH_MS_ENV, DEFAULT_FLUSH_MS)?;
        let queue_capacity = positive_usize_env(
            MARKET_DATA_WAREHOUSE_QUEUE_CAPACITY_ENV,
            DEFAULT_QUEUE_CAPACITY,
        )?;

        Ok(Some(Self {
            catalog_uri,
            source,
            batch_size,
            flush_interval: Duration::from_millis(flush_ms),
            queue_capacity,
            ingest_run_id: Uuid::new_v4(),
            clickhouse: ClickHouseConnectOptions::from_env(),
        }))
    }
}

/// Non-blocking live QuoteTick writer for Parquet catalog and ClickHouse.
pub struct LiveMarketDataDualWriter {
    sender: Option<SyncSender<QuoteTick>>,
    handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for LiveMarketDataDualWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(LiveMarketDataDualWriter))
            .field("running", &self.handle.is_some())
            .finish_non_exhaustive()
    }
}

impl LiveMarketDataDualWriter {
    /// Builds a writer from environment variables when dual-write is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when enabled config is invalid or the worker cannot start.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let Some(config) = MarketDataDualWriteConfig::from_env()? else {
            return Ok(None);
        };

        Self::start(config).map(Some)
    }

    /// Starts a live dual-write worker.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker cannot initialize catalog or ClickHouse access.
    pub fn start(config: MarketDataDualWriteConfig) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
        let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
        let worker_config = config.clone();

        let handle = thread::Builder::new()
            .name("market-data-dual-writer".to_string())
            .spawn(move || {
                let startup = Worker::new(worker_config);
                let mut worker = match startup {
                    Ok(worker) => {
                        let _ = startup_sender.send(Ok(()));
                        worker
                    }
                    Err(error) => {
                        let _ = startup_sender.send(Err(error.to_string()));
                        return;
                    }
                };

                worker.run(receiver);
            })
            .context("failed to spawn market-data dual-write worker")?;

        match startup_receiver
            .recv()
            .context("market-data dual-write worker exited before startup")?
        {
            Ok(()) => {
                log::info!(
                    "Started market-data dual-write worker: source={} catalog_uri={} ingest_run_id={} clickhouse={}",
                    config.source,
                    config.catalog_uri,
                    config.ingest_run_id,
                    config.clickhouse.connection_string_masked(),
                );
                Ok(Self {
                    sender: Some(sender),
                    handle: Some(handle),
                })
            }
            Err(error) => {
                let _ = handle.join();
                Err(anyhow!(
                    "failed to start market-data dual-write worker: {error}"
                ))
            }
        }
    }

    /// Enqueues a QuoteTick for background catalog and ClickHouse writes.
    pub fn write_quote(&self, quote: QuoteTick) {
        let Some(sender) = &self.sender else {
            return;
        };

        match sender.try_send(quote) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                log::warn!("Market-data dual-write queue full; dropping QuoteTick");
            }
            Err(TrySendError::Disconnected(_)) => {
                log::warn!("Market-data dual-write worker stopped; dropping QuoteTick");
            }
        }
    }
}

impl Drop for LiveMarketDataDualWriter {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(handle) = self.handle.take()
            && let Err(error) = handle.join()
        {
            log::warn!("Market-data dual-write worker join failed: {error:?}");
        }
    }
}

struct Worker {
    config: MarketDataDualWriteConfig,
    catalog: ParquetDataCatalog,
    runtime: tokio::runtime::Runtime,
}

impl Worker {
    fn new(config: MarketDataDualWriteConfig) -> anyhow::Result<Self> {
        ensure_local_catalog_dir(&config.catalog_uri)?;
        let catalog = ParquetDataCatalog::from_uri(&config.catalog_uri, None, None, None, None)
            .with_context(|| format!("failed to open catalog {}", config.catalog_uri))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to build ClickHouse writer runtime")?;
        runtime
            .block_on(crate::warehouse::clickhouse::check_health(
                &config.clickhouse,
            ))
            .context("failed ClickHouse health check for live dual-write")?;

        Ok(Self {
            config,
            catalog,
            runtime,
        })
    }

    fn run(&mut self, receiver: mpsc::Receiver<QuoteTick>) {
        let mut batch = Vec::with_capacity(self.config.batch_size);

        loop {
            match receiver.recv_timeout(self.config.flush_interval) {
                Ok(quote) => {
                    batch.push(quote);
                    if batch.len() >= self.config.batch_size {
                        self.flush(&mut batch);
                    }
                }
                Err(RecvTimeoutError::Timeout) => self.flush(&mut batch),
                Err(RecvTimeoutError::Disconnected) => {
                    self.flush(&mut batch);
                    break;
                }
            }
        }
    }

    fn flush(&mut self, batch: &mut Vec<QuoteTick>) {
        if batch.is_empty() {
            return;
        }

        let quotes = std::mem::take(batch);
        for (instrument_id, instrument_quotes) in quote_batches_by_instrument(&quotes) {
            if let Err(error) =
                self.catalog
                    .write_to_parquet(&instrument_quotes, None, None, Some(true))
            {
                log::warn!(
                    "Market-data catalog write failed: rows={} instrument_id={} catalog_uri={} error={error:?}",
                    instrument_quotes.len(),
                    instrument_id,
                    self.config.catalog_uri,
                );
            }
        }

        let client = self.config.clickhouse.client();
        let rows = quotes
            .iter()
            .map(|quote| {
                ClickHouseQuoteTickRow::from_quote_tick(
                    quote,
                    &self.config.source,
                    self.config.ingest_run_id,
                )
            })
            .collect::<Vec<_>>();
        if let Err(error) = self
            .runtime
            .block_on(write_quote_tick_rows(&client, rows.as_slice()))
        {
            log::warn!(
                "Market-data ClickHouse write failed: rows={} source={} ingest_run_id={} error={error:?}",
                rows.len(),
                self.config.source,
                self.config.ingest_run_id,
            );
        }
    }
}

fn quote_batches_by_instrument(quotes: &[QuoteTick]) -> IndexMap<InstrumentId, Vec<QuoteTick>> {
    let mut batches = IndexMap::new();
    for quote in quotes {
        batches
            .entry(quote.instrument_id)
            .or_insert_with(Vec::new)
            .push(*quote);
    }
    batches
}

fn ensure_local_catalog_dir(catalog_uri: &str) -> anyhow::Result<()> {
    let path = if catalog_uri.contains("://") {
        let url = url::Url::parse(catalog_uri)
            .with_context(|| format!("invalid catalog URI {catalog_uri}"))?;
        if url.scheme() != "file" {
            return Ok(());
        }
        url.to_file_path()
            .map_err(|()| anyhow!("invalid file catalog URI {catalog_uri}"))?
    } else {
        PathBuf::from(catalog_uri)
    };

    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create catalog directory {}", path.display()))
}

fn env_flag_enabled(name: &str) -> anyhow::Result<bool> {
    let Ok(value) = env::var(name) else {
        return Ok(false);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        other => Err(anyhow!(
            "invalid {name} value '{other}', expected true/false"
        )),
    }
}

fn catalog_uri_from_env() -> anyhow::Result<String> {
    if let Ok(value) = env::var(MARKET_DATA_CATALOG_URI_ENV) {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
    }

    if let Ok(value) = env::var("NAUTILUS_CATALOG_PATH") {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
    }

    if let Ok(value) = env::var("NAUTILUS_PATH") {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(PathBuf::from(value)
                .join("catalog")
                .to_string_lossy()
                .to_string());
        }
    }

    Err(anyhow!(
        "{MARKET_DATA_CATALOG_URI_ENV}, NAUTILUS_CATALOG_PATH, or NAUTILUS_PATH is required when {MARKET_DATA_DUAL_WRITE_ENV}=true"
    ))
}

fn positive_usize_env(name: &str, default: usize) -> anyhow::Result<usize> {
    let Ok(value) = env::var(name) else {
        return Ok(default);
    };
    let parsed = value
        .trim()
        .parse::<usize>()
        .with_context(|| format!("invalid {name} value '{value}'"))?;
    if parsed == 0 {
        return Err(anyhow!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn positive_u64_env(name: &str, default: u64) -> anyhow::Result<u64> {
    let Ok(value) = env::var(name) else {
        return Ok(default);
    };
    let parsed = value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("invalid {name} value '{value}'"))?;
    if parsed == 0 {
        return Err(anyhow!("{name} must be greater than zero"));
    }
    Ok(parsed)
}
