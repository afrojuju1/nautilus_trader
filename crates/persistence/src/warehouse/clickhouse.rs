//! ClickHouse-backed market-data warehouse utilities.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::Path,
    str::FromStr,
    time::Instant,
};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use clickhouse::{Client, Row};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::QuoteTick,
    identifiers::InstrumentId,
    types::{Price, Quantity, price::PriceRaw, quantity::QuantityRaw},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backend::catalog::ParquetDataCatalog;

pub const DEFAULT_CLICKHOUSE_URL: &str = "http://localhost:8123";
pub const DEFAULT_CLICKHOUSE_USERNAME: &str = "default";
pub const DEFAULT_CLICKHOUSE_DATABASE: &str = MARKET_DATA_DATABASE;
pub const DEFAULT_MIGRATIONS_DIR: &str = "schema/sql/clickhouse";
pub const MARKET_DATA_READ_SOURCE_ENV: &str = "NAUTILUS_MARKET_DATA_READ_SOURCE";

const CLICKHOUSE_BOOTSTRAP_DATABASE: &str = "default";
const MARKET_DATA_DATABASE: &str = "market";
const QUOTE_TICKS_TABLE: &str = "quote_ticks";
const QUOTE_TICKS_QUALIFIED_TABLE: &str = "market.quote_ticks";
const WAREHOUSE_METADATA_DATABASE: &str = "warehouse";
const SCHEMA_MIGRATIONS_QUALIFIED_TABLE: &str = "warehouse.schema_migrations";

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum MarketDataReadSource {
    #[default]
    Catalog,
    ClickHouse,
}

impl MarketDataReadSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::ClickHouse => "clickhouse",
        }
    }

    /// Returns the configured market-data read source, defaulting to `catalog`.
    ///
    /// # Errors
    ///
    /// Returns an error if `NAUTILUS_MARKET_DATA_READ_SOURCE` contains an unknown value.
    pub fn from_env() -> anyhow::Result<Self> {
        match env::var(MARKET_DATA_READ_SOURCE_ENV) {
            Ok(value) => value.parse(),
            Err(env::VarError::NotPresent) => Ok(Self::default()),
            Err(err) => Err(anyhow!(
                "failed to read {MARKET_DATA_READ_SOURCE_ENV}: {err}"
            )),
        }
    }
}

impl FromStr for MarketDataReadSource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "catalog" => Ok(Self::Catalog),
            "clickhouse" => Ok(Self::ClickHouse),
            other => Err(anyhow!(
                "invalid market data read source '{other}', expected 'catalog' or 'clickhouse'"
            )),
        }
    }
}

impl std::fmt::Display for MarketDataReadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ClickHouseConnectOptions {
    pub url: String,
    pub username: String,
    pub password: String,
    pub database: String,
}

impl ClickHouseConnectOptions {
    #[must_use]
    pub const fn new(url: String, username: String, password: String, database: String) -> Self {
        Self {
            url,
            username,
            password,
            database,
        }
    }

    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with_overrides(None, None, None, None)
    }

    #[must_use]
    pub fn from_env_with_overrides(
        url: Option<String>,
        username: Option<String>,
        password: Option<String>,
        database: Option<String>,
    ) -> Self {
        let url = url
            .or_else(|| env::var("CLICKHOUSE_URL").ok())
            .unwrap_or_else(|| DEFAULT_CLICKHOUSE_URL.to_string());
        let username = username
            .or_else(|| env::var("CLICKHOUSE_USER").ok())
            .unwrap_or_else(|| DEFAULT_CLICKHOUSE_USERNAME.to_string());
        let password = password
            .or_else(|| env::var("CLICKHOUSE_PASSWORD").ok())
            .unwrap_or_default();
        let database = database
            .or_else(|| env::var("CLICKHOUSE_DATABASE").ok())
            .or_else(|| env::var("CLICKHOUSE_DB").ok())
            .unwrap_or_else(|| DEFAULT_CLICKHOUSE_DATABASE.to_string());
        Self::new(url, username, password, database)
    }

    #[must_use]
    pub fn client(&self) -> Client {
        self.client_for_database(&self.database)
    }

    #[must_use]
    pub fn client_for_database(&self, database: &str) -> Client {
        Client::default()
            .with_url(&self.url)
            .with_user(&self.username)
            .with_password(&self.password)
            .with_database(database)
    }

    #[must_use]
    pub fn connection_string_masked(&self) -> String {
        format!(
            "{url}?database={database}&user={username}&password=***",
            url = self.url,
            database = self.database,
            username = self.username,
        )
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ClickHouseMigrationReport {
    pub applied: Vec<ClickHouseMigrationOutcome>,
    pub skipped: Vec<ClickHouseMigrationOutcome>,
}

impl ClickHouseMigrationReport {
    #[must_use]
    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }

    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ClickHouseMigrationOutcome {
    pub version: u32,
    pub description: String,
    pub checksum: String,
    pub statements: usize,
}

#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct ClickHouseQuoteTickRow {
    pub ts_event: u64,
    pub ts_init: u64,
    pub instrument_id: String,
    pub venue: String,
    pub source: String,
    pub bid_price_raw: i128,
    pub ask_price_raw: i128,
    pub bid_size_raw: u128,
    pub ask_size_raw: u128,
    pub price_precision: u8,
    pub size_precision: u8,
    #[serde(with = "clickhouse::serde::uuid")]
    pub ingest_run_id: Uuid,
}

impl ClickHouseQuoteTickRow {
    #[must_use]
    pub fn from_quote_tick(quote: &QuoteTick, source: &str, ingest_run_id: Uuid) -> Self {
        Self {
            ts_event: quote.ts_event.as_u64(),
            ts_init: quote.ts_init.as_u64(),
            instrument_id: quote.instrument_id.to_string(),
            venue: quote.instrument_id.venue.to_string(),
            source: source.to_string(),
            bid_price_raw: quote.bid_price.raw.into(),
            ask_price_raw: quote.ask_price.raw.into(),
            bid_size_raw: quote.bid_size.raw.into(),
            ask_size_raw: quote.ask_size.raw.into(),
            price_precision: quote.bid_price.precision,
            size_precision: quote.bid_size.precision,
            ingest_run_id,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QuoteTickSmokeReport {
    pub ingest_run_id: Uuid,
    pub written: usize,
    pub count: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QuoteTickCatalogBackfill {
    pub catalog_uri: String,
    pub instrument_ids: Vec<String>,
    pub start: Option<UnixNanos>,
    pub end: Option<UnixNanos>,
    pub source: String,
    pub batch_size: usize,
}

impl QuoteTickCatalogBackfill {
    /// Creates a catalog-backed quote tick backfill request from Unix nanosecond bounds.
    ///
    /// # Errors
    ///
    /// Returns an error if the source label is empty, the batch size is zero, or the time bounds are
    /// reversed.
    pub fn from_unix_nanos(
        catalog_uri: String,
        instrument_ids: Vec<String>,
        start_ns: Option<u64>,
        end_ns: Option<u64>,
        source: String,
        batch_size: usize,
    ) -> anyhow::Result<Self> {
        if source.trim().is_empty() {
            return Err(anyhow!("backfill source label cannot be empty"));
        }
        if batch_size == 0 {
            return Err(anyhow!("backfill batch size must be greater than zero"));
        }
        if let (Some(start), Some(end)) = (start_ns, end_ns) {
            if start > end {
                return Err(anyhow!(
                    "backfill start_ns ({start}) must be less than or equal to end_ns ({end})"
                ));
            }
        }

        Ok(Self {
            catalog_uri,
            instrument_ids,
            start: start_ns.map(UnixNanos::from),
            end: end_ns.map(UnixNanos::from),
            source,
            batch_size,
        })
    }

    fn catalog_identifiers(&self) -> Option<Vec<String>> {
        if self.instrument_ids.is_empty() {
            None
        } else {
            Some(self.instrument_ids.clone())
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QuoteTickBackfillReport {
    pub catalog_uri: String,
    pub instrument_ids: Vec<String>,
    pub source: String,
    pub ingest_run_id: Uuid,
    pub catalog_rows: usize,
    pub written_rows: usize,
    pub warehouse_rows: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QuoteTickReadRequest {
    pub catalog_uri: String,
    pub instrument_ids: Vec<String>,
    pub start: Option<UnixNanos>,
    pub end: Option<UnixNanos>,
    pub source: String,
    pub read_source: MarketDataReadSource,
}

impl QuoteTickReadRequest {
    /// Creates a quote tick read request from Unix nanosecond bounds.
    ///
    /// # Errors
    ///
    /// Returns an error if the ClickHouse source label is empty or the time bounds are reversed.
    /// ClickHouse-backed reads additionally require both bounds when executed.
    pub fn from_unix_nanos(
        catalog_uri: String,
        instrument_ids: Vec<String>,
        start_ns: Option<u64>,
        end_ns: Option<u64>,
        source: String,
        read_source: MarketDataReadSource,
    ) -> anyhow::Result<Self> {
        if source.trim().is_empty() {
            return Err(anyhow!("quote tick read source label cannot be empty"));
        }
        if let (Some(start), Some(end)) = (start_ns, end_ns) {
            if start > end {
                return Err(anyhow!(
                    "quote tick read start_ns ({start}) must be less than or equal to end_ns ({end})"
                ));
            }
        }

        Ok(Self {
            catalog_uri,
            instrument_ids,
            start: start_ns.map(UnixNanos::from),
            end: end_ns.map(UnixNanos::from),
            source,
            read_source,
        })
    }

    fn catalog_identifiers(&self) -> Option<Vec<String>> {
        if self.instrument_ids.is_empty() {
            None
        } else {
            Some(self.instrument_ids.clone())
        }
    }

    fn ensure_bounded_clickhouse_read(&self) -> anyhow::Result<()> {
        ensure_bounded_quote_tick_range(self.start, self.end)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QuoteTickValidationSummary {
    pub rows: usize,
    pub first_ts_init: Option<u64>,
    pub last_ts_init: Option<u64>,
    pub checksum: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct QuoteTickValidationReport {
    pub catalog_uri: String,
    pub instrument_ids: Vec<String>,
    pub source: String,
    pub read_source: MarketDataReadSource,
    pub catalog: QuoteTickValidationSummary,
    pub clickhouse: QuoteTickValidationSummary,
    pub selected: QuoteTickValidationSummary,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct QuoteTickValidationRow {
    instrument_id: String,
    ts_event: u64,
    ts_init: u64,
    bid_price_raw: i128,
    ask_price_raw: i128,
    bid_size_raw: u128,
    ask_size_raw: u128,
    price_precision: u8,
    size_precision: u8,
}

#[derive(Debug, Clone)]
struct MigrationFile {
    version: u32,
    description: String,
    checksum: String,
    statements: Vec<String>,
}

#[derive(Debug, Clone, Row, Deserialize)]
struct AppliedMigrationRow {
    version: u32,
    checksum: String,
    success: u8,
}

#[derive(Debug, Clone, Row, Deserialize)]
struct CountRow {
    count: u64,
}

/// Checks that the ClickHouse warehouse endpoint accepts queries.
///
/// # Errors
///
/// Returns an error if the ClickHouse query fails.
pub async fn check_health(options: &ClickHouseConnectOptions) -> anyhow::Result<()> {
    options.client().query("SELECT 1").execute().await?;
    Ok(())
}

/// Writes `QuoteTick` rows to the canonical market-data quote table.
///
/// # Errors
///
/// Returns an error if ClickHouse rejects the insert.
pub async fn write_quote_tick_rows(
    client: &Client,
    rows: &[ClickHouseQuoteTickRow],
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let market_client = client.clone().with_database(MARKET_DATA_DATABASE);
    let mut insert = market_client
        .insert::<ClickHouseQuoteTickRow>(QUOTE_TICKS_TABLE)
        .await?;
    for row in rows {
        insert.write(row).await?;
    }
    insert.end().await?;
    Ok(())
}

/// Counts quote rows for an ingest run.
///
/// # Errors
///
/// Returns an error if ClickHouse rejects the count query.
pub async fn count_quote_ticks_for_run(
    client: &Client,
    ingest_run_id: Uuid,
) -> anyhow::Result<u64> {
    let row = client
        .query(&format!(
            "SELECT count() AS count FROM {QUOTE_TICKS_QUALIFIED_TABLE} WHERE ingest_run_id = toUUID(?)"
        ))
        .bind(ingest_run_id.to_string())
        .fetch_one::<CountRow>()
        .await?;
    Ok(row.count)
}

/// Reads `QuoteTick` rows from ClickHouse for a source and optional range.
///
/// # Errors
///
/// Returns an error if ClickHouse rejects the query.
pub async fn read_quote_tick_rows(
    client: &Client,
    source: &str,
    instrument_ids: &[String],
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
) -> anyhow::Result<Vec<ClickHouseQuoteTickRow>> {
    ensure_bounded_quote_tick_range(start, end)?;

    let start_event_date = start.map(unix_nanos_utc_date).transpose()?;
    let end_event_date = end.map(unix_nanos_utc_date).transpose()?;

    let mut sql = format!("SELECT ?fields FROM {QUOTE_TICKS_QUALIFIED_TABLE} WHERE source = ?");
    if start.is_some() {
        sql.push_str(" AND event_date >= toDate(?) AND ts_init >= ?");
    }
    if end.is_some() {
        sql.push_str(" AND event_date <= toDate(?) AND ts_init <= ?");
    }
    if !instrument_ids.is_empty() {
        sql.push_str(" AND instrument_id IN ?");
    }
    sql.push_str(" ORDER BY instrument_id, ts_init, ts_event, ingest_run_id");

    let mut query = client.query(&sql).bind(source);
    if let (Some(event_date), Some(start)) = (start_event_date, start) {
        query = query.bind(event_date);
        query = query.bind(start.as_u64());
    }
    if let (Some(event_date), Some(end)) = (end_event_date, end) {
        query = query.bind(event_date);
        query = query.bind(end.as_u64());
    }
    if !instrument_ids.is_empty() {
        query = query.bind(instrument_ids.to_vec());
    }

    Ok(query.fetch_all::<ClickHouseQuoteTickRow>().await?)
}

fn ensure_bounded_quote_tick_range(
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
) -> anyhow::Result<()> {
    if start.is_none() || end.is_none() {
        return Err(anyhow!(
            "ClickHouse QuoteTick reads require both start_ns and end_ns bounds to avoid unbounded warehouse scans"
        ));
    }
    Ok(())
}

fn unix_nanos_utc_date(value: UnixNanos) -> anyhow::Result<String> {
    let value = value.as_u64();
    let secs = value / 1_000_000_000;
    let nanos = (value % 1_000_000_000) as u32;
    let secs = i64::try_from(secs)
        .map_err(|err| anyhow!("Unix nanosecond timestamp is outside i64 range: {err}"))?;
    let datetime = DateTime::<Utc>::from_timestamp(secs, nanos)
        .ok_or_else(|| anyhow!("Unix nanosecond timestamp is outside UTC datetime range"))?;
    Ok(datetime.date_naive().format("%Y-%m-%d").to_string())
}

/// Reads `QuoteTick` data from the configured market-data source.
///
/// # Errors
///
/// Returns an error if the selected source cannot be read or ClickHouse rows cannot be converted
/// back into Nautilus model values.
pub async fn read_quote_ticks(
    options: &ClickHouseConnectOptions,
    request: &QuoteTickReadRequest,
) -> anyhow::Result<Vec<QuoteTick>> {
    match request.read_source {
        MarketDataReadSource::Catalog => {
            read_catalog_quote_ticks(
                &request.catalog_uri,
                request.catalog_identifiers(),
                request.start,
                request.end,
            )
            .await
        }
        MarketDataReadSource::ClickHouse => {
            request.ensure_bounded_clickhouse_read()?;
            let client = options.client();
            let rows = read_quote_tick_rows(
                &client,
                &request.source,
                &request.instrument_ids,
                request.start,
                request.end,
            )
            .await?;
            rows.iter().map(quote_tick_from_clickhouse_row).collect()
        }
    }
}

/// Writes a tiny synthetic `QuoteTick` batch and reads it back by ingest run.
///
/// # Errors
///
/// Returns an error if ClickHouse health, insert, or readback fails.
pub async fn run_quote_tick_smoke(
    options: &ClickHouseConnectOptions,
    source: &str,
) -> anyhow::Result<QuoteTickSmokeReport> {
    check_health(options).await?;
    let client = options.client();
    let ingest_run_id = Uuid::new_v4();
    let quote = synthetic_quote_tick();
    let row = ClickHouseQuoteTickRow::from_quote_tick(&quote, source, ingest_run_id);
    write_quote_tick_rows(&client, &[row]).await?;
    let count = count_quote_ticks_for_run(&client, ingest_run_id).await?;
    Ok(QuoteTickSmokeReport {
        ingest_run_id,
        written: 1,
        count,
    })
}

async fn read_catalog_quote_ticks(
    catalog_uri: &str,
    instrument_ids: Option<Vec<String>>,
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
) -> anyhow::Result<Vec<QuoteTick>> {
    let catalog_uri = catalog_uri.to_string();
    tokio::task::spawn_blocking(move || {
        let mut catalog = ParquetDataCatalog::from_uri(&catalog_uri, None, None, None, None)
            .with_context(|| format!("failed to open catalog {catalog_uri}"))?;
        catalog
            .quote_ticks(instrument_ids, start, end)
            .with_context(|| format!("failed to query QuoteTick data from {catalog_uri}"))
    })
    .await
    .context("catalog QuoteTick read task failed")?
}

/// Backfills quote ticks from a Nautilus `ParquetDataCatalog` into ClickHouse.
///
/// # Errors
///
/// Returns an error if catalog reading, ClickHouse writing, or count validation fails.
pub async fn backfill_quote_ticks_from_catalog(
    options: &ClickHouseConnectOptions,
    request: &QuoteTickCatalogBackfill,
) -> anyhow::Result<QuoteTickBackfillReport> {
    check_health(options).await?;

    let quotes = read_catalog_quote_ticks(
        &request.catalog_uri,
        request.catalog_identifiers(),
        request.start,
        request.end,
    )
    .await?;
    if quotes.is_empty() {
        return Err(anyhow!(
            "catalog backfill found no QuoteTick rows for catalog_uri={} instruments={} start={:?} end={:?}",
            request.catalog_uri,
            if request.instrument_ids.is_empty() {
                "*".to_string()
            } else {
                request.instrument_ids.join(",")
            },
            request.start,
            request.end
        ));
    }

    let client = options.client();
    let ingest_run_id = Uuid::new_v4();
    let mut written_rows = 0usize;

    for chunk in quotes.chunks(request.batch_size) {
        let rows = chunk
            .iter()
            .map(|quote| {
                ClickHouseQuoteTickRow::from_quote_tick(quote, &request.source, ingest_run_id)
            })
            .collect::<Vec<_>>();
        write_quote_tick_rows(&client, &rows).await?;
        written_rows += rows.len();
    }

    let warehouse_rows = count_quote_ticks_for_run(&client, ingest_run_id).await?;
    let expected_rows = u64::try_from(quotes.len()).context("catalog row count overflowed u64")?;
    if warehouse_rows != expected_rows {
        return Err(anyhow!(
            "ClickHouse backfill count mismatch for ingest_run_id={ingest_run_id}: catalog_rows={} warehouse_rows={warehouse_rows}",
            quotes.len()
        ));
    }

    Ok(QuoteTickBackfillReport {
        catalog_uri: request.catalog_uri.clone(),
        instrument_ids: request.instrument_ids.clone(),
        source: request.source.clone(),
        ingest_run_id,
        catalog_rows: quotes.len(),
        written_rows,
        warehouse_rows,
    })
}

/// Validates catalog quote ticks against ClickHouse rows for the same source and range.
///
/// # Errors
///
/// Returns an error if either side cannot be read, the catalog range is empty, or validation
/// summaries do not match.
pub async fn validate_quote_ticks(
    options: &ClickHouseConnectOptions,
    request: &QuoteTickReadRequest,
) -> anyhow::Result<QuoteTickValidationReport> {
    request.ensure_bounded_clickhouse_read()?;
    check_health(options).await?;

    let catalog_quotes = read_catalog_quote_ticks(
        &request.catalog_uri,
        request.catalog_identifiers(),
        request.start,
        request.end,
    )
    .await?;
    if catalog_quotes.is_empty() {
        return Err(anyhow!(
            "catalog validation found no QuoteTick rows for catalog_uri={} instruments={} start={:?} end={:?}",
            request.catalog_uri,
            if request.instrument_ids.is_empty() {
                "*".to_string()
            } else {
                request.instrument_ids.join(",")
            },
            request.start,
            request.end
        ));
    }

    let client = options.client();
    let clickhouse_rows = read_quote_tick_rows(
        &client,
        &request.source,
        &request.instrument_ids,
        request.start,
        request.end,
    )
    .await?;
    let selected_quotes = read_quote_ticks(options, request).await?;

    let report = QuoteTickValidationReport {
        catalog_uri: request.catalog_uri.clone(),
        instrument_ids: request.instrument_ids.clone(),
        source: request.source.clone(),
        read_source: request.read_source,
        catalog: quote_tick_validation_summary(
            catalog_quotes
                .iter()
                .map(QuoteTickValidationRow::from)
                .collect(),
        ),
        clickhouse: quote_tick_validation_summary(
            clickhouse_rows
                .iter()
                .map(QuoteTickValidationRow::from)
                .collect(),
        ),
        selected: quote_tick_validation_summary(
            selected_quotes
                .iter()
                .map(QuoteTickValidationRow::from)
                .collect(),
        ),
    };

    if report.catalog != report.clickhouse {
        return Err(anyhow!(
            "QuoteTick validation mismatch for source={} read_source={}: catalog_rows={} clickhouse_rows={} catalog_first_ts_init={:?} clickhouse_first_ts_init={:?} catalog_last_ts_init={:?} clickhouse_last_ts_init={:?} catalog_checksum={} clickhouse_checksum={}",
            report.source,
            report.read_source,
            report.catalog.rows,
            report.clickhouse.rows,
            report.catalog.first_ts_init,
            report.clickhouse.first_ts_init,
            report.catalog.last_ts_init,
            report.clickhouse.last_ts_init,
            report.catalog.checksum,
            report.clickhouse.checksum
        ));
    }
    let expected_selected = match report.read_source {
        MarketDataReadSource::Catalog => &report.catalog,
        MarketDataReadSource::ClickHouse => &report.clickhouse,
    };
    if &report.selected != expected_selected {
        return Err(anyhow!(
            "QuoteTick selected read mismatch for read_source={}: selected_rows={} expected_rows={} selected_checksum={} expected_checksum={}",
            report.read_source,
            report.selected.rows,
            expected_selected.rows,
            report.selected.checksum,
            expected_selected.checksum
        ));
    }

    Ok(report)
}

/// Runs all pending ClickHouse warehouse migrations from the default migrations directory.
///
/// # Errors
///
/// Returns an error if the ClickHouse connection, migration discovery, checksum validation, DDL
/// execution, or migration metadata update fails.
pub async fn run_default_migrations(
    options: &ClickHouseConnectOptions,
) -> anyhow::Result<ClickHouseMigrationReport> {
    run_migrations(options, Path::new(DEFAULT_MIGRATIONS_DIR)).await
}

/// Runs all pending ClickHouse warehouse migrations from `migrations_dir`.
///
/// # Errors
///
/// Returns an error if the ClickHouse connection, migration discovery, checksum validation, DDL
/// execution, or migration metadata update fails.
pub async fn run_migrations(
    options: &ClickHouseConnectOptions,
    migrations_dir: &Path,
) -> anyhow::Result<ClickHouseMigrationReport> {
    let bootstrap_client = options.client_for_database(CLICKHOUSE_BOOTSTRAP_DATABASE);
    ensure_database(&bootstrap_client, MARKET_DATA_DATABASE).await?;
    ensure_database(&bootstrap_client, WAREHOUSE_METADATA_DATABASE).await?;
    ensure_migration_metadata(&bootstrap_client).await?;

    let client = options.client();
    let applied = load_applied_migrations(&bootstrap_client).await?;
    let migrations = load_migration_files(migrations_dir)?;
    apply_migration_files(&client, &applied, migrations).await
}

async fn ensure_database(client: &Client, database: &str) -> anyhow::Result<()> {
    client
        .query(&format!("CREATE DATABASE IF NOT EXISTS {database}"))
        .execute()
        .await?;
    Ok(())
}

async fn ensure_migration_metadata(client: &Client) -> anyhow::Result<()> {
    client
        .query(&format!(
            r#"
CREATE TABLE IF NOT EXISTS {SCHEMA_MIGRATIONS_QUALIFIED_TABLE}
(
    version UInt32,
    description String,
    checksum String,
    applied_at DateTime64(9, 'UTC') DEFAULT now64(9),
    execution_ms UInt64,
    success UInt8
)
ENGINE = MergeTree
ORDER BY (version, applied_at)
"#,
        ))
        .execute()
        .await?;
    Ok(())
}

async fn load_applied_migrations(
    client: &Client,
) -> anyhow::Result<BTreeMap<u32, AppliedMigrationRow>> {
    let rows = client
        .query(&format!(
            r#"
SELECT
    version,
    argMax(checksum, applied_at) AS checksum,
    argMax(success, applied_at) AS success
FROM {SCHEMA_MIGRATIONS_QUALIFIED_TABLE}
GROUP BY version
ORDER BY version
"#,
        ))
        .fetch_all::<AppliedMigrationRow>()
        .await?;

    Ok(rows.into_iter().map(|row| (row.version, row)).collect())
}

async fn apply_migration_files(
    client: &Client,
    applied: &BTreeMap<u32, AppliedMigrationRow>,
    migrations: Vec<MigrationFile>,
) -> anyhow::Result<ClickHouseMigrationReport> {
    let mut report = ClickHouseMigrationReport {
        applied: Vec::new(),
        skipped: Vec::new(),
    };

    for migration in migrations {
        if let Some(row) = applied.get(&migration.version) {
            if row.checksum != migration.checksum {
                return Err(anyhow!(
                    "ClickHouse migration {} checksum drift: applied={} current={}",
                    migration.version,
                    row.checksum,
                    migration.checksum
                ));
            }
            if row.success != 1 {
                return Err(anyhow!(
                    "ClickHouse migration {} has a failed metadata record",
                    migration.version
                ));
            }
            report.skipped.push(migration.outcome());
            continue;
        }

        apply_one_migration(client, &migration).await?;
        report.applied.push(migration.outcome());
    }

    Ok(report)
}

async fn apply_one_migration(client: &Client, migration: &MigrationFile) -> anyhow::Result<()> {
    let start = Instant::now();
    let execution_result = execute_migration_statements(client, migration).await;
    let success = execution_result.is_ok();
    let execution_ms = elapsed_millis_u64(start);

    if let Err(error) = record_migration(client, migration, execution_ms, success).await {
        if let Err(execution_error) = execution_result {
            return Err(anyhow!(
                "ClickHouse migration {} failed: {execution_error}; additionally failed to record migration metadata: {error}",
                migration.version
            ));
        }
        return Err(error);
    }

    execution_result
}

async fn execute_migration_statements(
    client: &Client,
    migration: &MigrationFile,
) -> anyhow::Result<()> {
    for statement in &migration.statements {
        client
            .query(statement)
            .with_setting("wait_end_of_query", "1")
            .execute()
            .await
            .with_context(|| {
                format!(
                    "failed to execute ClickHouse migration {} statement",
                    migration.version
                )
            })?;
    }
    Ok(())
}

async fn record_migration(
    client: &Client,
    migration: &MigrationFile,
    execution_ms: u64,
    success: bool,
) -> anyhow::Result<()> {
    client
        .query(&format!(
            r#"
INSERT INTO {SCHEMA_MIGRATIONS_QUALIFIED_TABLE}
    (version, description, checksum, execution_ms, success)
VALUES (?, ?, ?, ?, ?)
"#,
        ))
        .bind(migration.version)
        .bind(migration.description.as_str())
        .bind(migration.checksum.as_str())
        .bind(execution_ms)
        .bind(u8::from(success))
        .execute()
        .await?;
    Ok(())
}

fn load_migration_files(migrations_dir: &Path) -> anyhow::Result<Vec<MigrationFile>> {
    let entries = fs::read_dir(migrations_dir).with_context(|| {
        format!(
            "failed to read ClickHouse migrations directory {}",
            migrations_dir.display()
        )
    })?;
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();

    let mut versions = BTreeSet::new();
    let mut migrations = Vec::new();
    for path in paths {
        if path.extension().and_then(|value| value.to_str()) != Some("sql") {
            continue;
        }
        let (version, description) = parse_migration_filename(&path)?;
        if !versions.insert(version) {
            return Err(anyhow!("duplicate ClickHouse migration version {version}"));
        }
        let sql = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read ClickHouse migration file {}",
                path.display()
            )
        })?;
        let statements = split_sql_statements(&sql);
        if statements.is_empty() {
            return Err(anyhow!(
                "ClickHouse migration {} has no SQL statements",
                path.display()
            ));
        }
        migrations.push(MigrationFile {
            version,
            description,
            checksum: blake3::hash(sql.as_bytes()).to_hex().to_string(),
            statements,
        });
    }

    Ok(migrations)
}

fn parse_migration_filename(path: &Path) -> anyhow::Result<(u32, String)> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("invalid ClickHouse migration filename {}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("invalid ClickHouse migration filename {file_name}"))?;
    let (version, description) = stem.split_once('_').ok_or_else(|| {
        anyhow!("migration filename must be VERSION_description.sql: {file_name}")
    })?;
    let version = version
        .parse::<u32>()
        .with_context(|| format!("invalid migration version in {file_name}"))?;
    if description.trim().is_empty() {
        return Err(anyhow!(
            "migration description must not be empty: {file_name}"
        ));
    }
    Ok((version, description.to_string()))
}

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_string = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_string = !in_string;
                current.push(c);
            }
            '-' if !in_string && chars.peek() == Some(&'-') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        current.push('\n');
                        break;
                    }
                }
            }
            ';' if !in_string => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    statements.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }

    statements
}

fn elapsed_millis_u64(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn quote_tick_validation_summary(
    mut rows: Vec<QuoteTickValidationRow>,
) -> QuoteTickValidationSummary {
    let first_ts_init = rows.iter().map(|row| row.ts_init).min();
    let last_ts_init = rows.iter().map(|row| row.ts_init).max();
    rows.sort();

    let mut hasher = blake3::Hasher::new();
    for row in &rows {
        hasher.update(row.instrument_id.as_bytes());
        hasher.update(&[0]);
        hasher.update(&row.ts_event.to_le_bytes());
        hasher.update(&row.ts_init.to_le_bytes());
        hasher.update(&row.bid_price_raw.to_le_bytes());
        hasher.update(&row.ask_price_raw.to_le_bytes());
        hasher.update(&row.bid_size_raw.to_le_bytes());
        hasher.update(&row.ask_size_raw.to_le_bytes());
        hasher.update(&[row.price_precision, row.size_precision]);
    }

    QuoteTickValidationSummary {
        rows: rows.len(),
        first_ts_init,
        last_ts_init,
        checksum: hasher.finalize().to_hex().to_string(),
    }
}

fn quote_tick_from_clickhouse_row(row: &ClickHouseQuoteTickRow) -> anyhow::Result<QuoteTick> {
    let bid_price_raw = PriceRaw::try_from(row.bid_price_raw)
        .with_context(|| format!("bid price raw overflowed PriceRaw: {}", row.bid_price_raw))?;
    let ask_price_raw = PriceRaw::try_from(row.ask_price_raw)
        .with_context(|| format!("ask price raw overflowed PriceRaw: {}", row.ask_price_raw))?;
    let bid_size_raw = QuantityRaw::try_from(row.bid_size_raw)
        .with_context(|| format!("bid size raw overflowed QuantityRaw: {}", row.bid_size_raw))?;
    let ask_size_raw = QuantityRaw::try_from(row.ask_size_raw)
        .with_context(|| format!("ask size raw overflowed QuantityRaw: {}", row.ask_size_raw))?;

    Ok(QuoteTick::new(
        InstrumentId::from(row.instrument_id.as_str()),
        Price::from_raw(bid_price_raw, row.price_precision),
        Price::from_raw(ask_price_raw, row.price_precision),
        Quantity::from_raw(bid_size_raw, row.size_precision),
        Quantity::from_raw(ask_size_raw, row.size_precision),
        UnixNanos::from(row.ts_event),
        UnixNanos::from(row.ts_init),
    ))
}

fn synthetic_quote_tick() -> QuoteTick {
    QuoteTick::new(
        InstrumentId::from("AUDUSD.SIM"),
        Price::from("1.00000"),
        Price::from("1.00010"),
        Quantity::from("100000"),
        Quantity::from("100000"),
        UnixNanos::from(1_700_000_000_000_000_000),
        UnixNanos::from(1_700_000_000_000_000_001),
    )
}

impl MigrationFile {
    fn outcome(&self) -> ClickHouseMigrationOutcome {
        ClickHouseMigrationOutcome {
            version: self.version,
            description: self.description.clone(),
            checksum: self.checksum.clone(),
            statements: self.statements.len(),
        }
    }
}

impl From<&QuoteTick> for QuoteTickValidationRow {
    fn from(quote: &QuoteTick) -> Self {
        Self {
            instrument_id: quote.instrument_id.to_string(),
            ts_event: quote.ts_event.as_u64(),
            ts_init: quote.ts_init.as_u64(),
            bid_price_raw: quote.bid_price.raw.into(),
            ask_price_raw: quote.ask_price.raw.into(),
            bid_size_raw: quote.bid_size.raw.into(),
            ask_size_raw: quote.ask_size.raw.into(),
            price_precision: quote.bid_price.precision,
            size_precision: quote.bid_size.precision,
        }
    }
}

impl From<&ClickHouseQuoteTickRow> for QuoteTickValidationRow {
    fn from(row: &ClickHouseQuoteTickRow) -> Self {
        Self {
            instrument_id: row.instrument_id.clone(),
            ts_event: row.ts_event,
            ts_init: row.ts_init,
            bid_price_raw: row.bid_price_raw,
            ask_price_raw: row.ask_price_raw,
            bid_size_raw: row.bid_size_raw,
            ask_size_raw: row.ask_size_raw,
            price_precision: row.price_precision,
            size_precision: row.size_precision,
        }
    }
}
