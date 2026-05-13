//! Performance and candidate-outcome persistence in Postgres.

use chrono::NaiveDate;
use serde_json::Value;
use sqlx::{Row as _, types::Json};

use crate::{
    options_runtime::OptionsEngineConfig,
    performance::{
        CandidateOutcomeSummary, EntryPerformance, PerformanceLedgerAppend,
        PerformanceLedgerSummary,
    },
    storage::StorageRepository,
};

#[derive(Debug, Default)]
pub struct PerformanceLedgerSummaryFilters {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
}

pub type CandidateOutcomeSummaryFilters = PerformanceLedgerSummaryFilters;

pub async fn append_performance_ledger_record(
    storage: &StorageRepository,
    account_id: &str,
    ledger_date: &str,
    entry: &EntryPerformance,
    existing_record_key: Option<&str>,
) -> anyhow::Result<PerformanceLedgerAppend> {
    use chrono::Utc;

    let date = chrono::NaiveDate::parse_from_str(ledger_date, "%Y-%m-%d")?;
    let record_key = if let Some(existing_record_key) = existing_record_key {
        existing_record_key.to_string()
    } else {
        crate::performance::performance_record_key(entry)
    };
    let mut record = serde_json::Map::new();
    let performance = serde_json::to_value(entry)?;
    record.insert(
        "schema_version".to_string(),
        serde_json::Value::from(crate::performance::PERFORMANCE_LEDGER_SCHEMA_VERSION),
    );
    record.insert(
        "ts_utc".to_string(),
        serde_json::Value::String(Utc::now().to_rfc3339()),
    );
    record.insert(
        "type".to_string(),
        serde_json::Value::String("realized_trade".to_string()),
    );
    record.insert(
        "ledger_date".to_string(),
        serde_json::Value::String(ledger_date.to_string()),
    );
    if let serde_json::Value::Object(fields) = performance {
        record.extend(fields);
    }
    record.insert(
        "account_id".to_string(),
        serde_json::Value::String(account_id.to_string()),
    );
    record.insert(
        "record_key".to_string(),
        serde_json::Value::String(record_key.clone()),
    );

    let query = format!(
        "INSERT INTO \"{}\".performance_ledger (account_id, ledger_date, ts_utc, record_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5)\n         ON CONFLICT (account_id, record_key) DO UPDATE\n         SET payload = EXCLUDED.payload, ts_utc = EXCLUDED.ts_utc",
        storage.schema()
    );

    let result = sqlx::query(&query)
        .bind(account_id)
        .bind(date.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(&record_key)
        .bind(Json(Value::Object(record)))
        .execute(storage.pool())
        .await?;

    Ok(PerformanceLedgerAppend {
        path: format!(
            "postgres://{schema}.performance_ledger#{account}/{record_key}",
            schema = storage.schema(),
            account = account_id,
            record_key = record_key
        ),
        appended: result.rows_affected() > 0,
        record_key,
    })
}

pub async fn summarize_performance_ledger(
    storage: &StorageRepository,
    account_id: &str,
    filters: PerformanceLedgerSummaryFilters,
) -> anyhow::Result<PerformanceLedgerSummary> {
    let query = format!(
        "SELECT payload FROM \"{}\".performance_ledger WHERE account_id = $1 AND ($2::date IS NULL OR ledger_date >= $2::date) AND ($3::date IS NULL OR ledger_date <= $3::date) AND (payload->>'type') = 'realized_trade' ORDER BY ts_utc ASC",
        storage.schema()
    );
    let rows = sqlx::query(&query)
        .bind(account_id)
        .bind(filters.since.map(|d| d.to_string()))
        .bind(filters.until.map(|d| d.to_string()))
        .fetch_all(storage.pool())
        .await?;

    let mut summary = PerformanceLedgerSummary {
        directory: format!(
            "postgres://{schema}.performance_ledger/{account}",
            schema = storage.schema(),
            account = account_id
        ),
        ..Default::default()
    };

    let mut win_sum = 0.0;
    let mut loss_sum = 0.0;
    let mut bucket_stats = BucketStats::default();
    let mut strategy_stats = std::collections::BTreeMap::<String, BucketStats>::new();
    let mut underlying_stats = std::collections::BTreeMap::<String, BucketStats>::new();
    let mut dates = std::collections::BTreeSet::<String>::new();

    for row in rows {
        let payload: Json<Value> = row.try_get("payload")?;
        let record = payload.0;
        let Some(realized_pnl) = record.get("realized_pnl").and_then(Value::as_f64) else {
            summary.parse_errors += 1;
            continue;
        };

        summary.records += 1;
        summary.realized_pnl += realized_pnl;
        if let Some(ledger_date) = record.get("ledger_date").and_then(Value::as_str) {
            dates.insert(ledger_date.to_string());
        }
        if record
            .get("warnings")
            .and_then(Value::as_array)
            .is_some_and(|warnings| !warnings.is_empty())
        {
            summary.records_with_warnings += 1;
        }

        bucket_stats.add(realized_pnl);
        if realized_pnl > 0.0 {
            summary.wins += 1;
            win_sum += realized_pnl;
        } else if realized_pnl < 0.0 {
            summary.losses += 1;
            loss_sum += realized_pnl;
            summary.largest_loss = Some(
                summary
                    .largest_loss
                    .map_or(realized_pnl, |current| current.min(realized_pnl)),
            );
        } else {
            summary.flats += 1;
        }

        if let Some(strategy) = record.get("strategy").and_then(Value::as_str) {
            strategy_stats
                .entry(strategy.to_string())
                .or_default()
                .add(realized_pnl);
        }
        if let Some(underlying) = record.get("underlying").and_then(Value::as_str) {
            underlying_stats
                .entry(underlying.to_string())
                .or_default()
                .add(realized_pnl);
        }
    }

    summary.missing = false;
    summary.files = 1;
    summary.dates = dates.into_iter().collect();
    summary.average_win = (summary.wins > 0).then_some(win_sum / summary.wins as f64);
    summary.average_loss = (summary.losses > 0).then_some(loss_sum / summary.losses as f64);
    summary.by_strategy = strategy_stats
        .into_iter()
        .map(|(strategy, stats)| (strategy, stats.into_summary()))
        .collect();
    summary.by_underlying = underlying_stats
        .into_iter()
        .map(|(underlying, stats)| (underlying, stats.into_summary()))
        .collect();
    let aggregate = bucket_stats.into_summary();
    if summary.records > 0 {
        summary.largest_loss = aggregate.largest_loss;
    }

    Ok(summary)
}

pub async fn append_candidate_outcome(
    storage: &StorageRepository,
    account_id: &str,
    trade_date: &str,
    record_key: &str,
    payload: &Value,
) -> anyhow::Result<bool> {
    use chrono::Utc;

    let date = chrono::NaiveDate::parse_from_str(trade_date, "%Y-%m-%d")?;
    let record = match payload {
        Value::Object(fields) => Value::Object(fields.clone()),
        _ => Value::Object(
            [("payload".to_string(), payload.clone())]
                .into_iter()
                .collect(),
        ),
    };
    let query = format!(
        "INSERT INTO \"{}\".candidate_outcome (account_id, trade_date, ts_utc, record_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5)\n         ON CONFLICT (account_id, record_key) DO UPDATE\n         SET payload = EXCLUDED.payload, ts_utc = EXCLUDED.ts_utc",
        storage.schema()
    );
    let result = sqlx::query(&query)
        .bind(account_id)
        .bind(date.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(record_key)
        .bind(Json(record))
        .execute(storage.pool())
        .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn summarize_candidate_outcomes(
    storage: &StorageRepository,
    account_id: &str,
    filters: CandidateOutcomeSummaryFilters,
) -> anyhow::Result<CandidateOutcomeSummary> {
    let query = format!(
        "SELECT payload FROM \"{}\".candidate_outcome WHERE account_id = $1 AND ($2::date IS NULL OR trade_date >= $2::date) AND ($3::date IS NULL OR trade_date <= $3::date) ORDER BY ts_utc ASC",
        storage.schema()
    );
    let rows = sqlx::query(&query)
        .bind(account_id)
        .bind(filters.since.map(|d| d.to_string()))
        .bind(filters.until.map(|d| d.to_string()))
        .fetch_all(storage.pool())
        .await?;

    let mut summary = CandidateOutcomeSummary {
        directory: format!(
            "postgres://{schema}.candidate_outcome/{account}",
            schema = storage.schema(),
            account = account_id,
        ),
        ..Default::default()
    };
    let mut aggregate = OutcomeStats::default();
    let mut dates = std::collections::BTreeSet::<String>::new();
    let mut bucket_stats = std::collections::BTreeMap::<String, OutcomeStats>::new();
    let mut strategy_stats = std::collections::BTreeMap::<String, OutcomeStats>::new();

    for row in rows {
        let payload: sqlx::types::Json<Value> = row.try_get("payload")?;
        let record = payload.0;
        let Some(hypothetical_pnl) = record.get("hypothetical_pnl").and_then(Value::as_f64) else {
            summary.parse_errors += 1;
            continue;
        };

        let was_selected = record
            .get("was_selected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let was_traded = record
            .get("was_traded")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        aggregate.add(hypothetical_pnl, was_selected, was_traded);
        if let Some(trade_date) = record.get("trade_date").and_then(Value::as_str) {
            dates.insert(trade_date.to_string());
        }
        if let Some(bucket) = record.get("observation_bucket").and_then(Value::as_str) {
            bucket_stats.entry(bucket.to_string()).or_default().add(
                hypothetical_pnl,
                was_selected,
                was_traded,
            );
        }
        if let Some(strategy) = record.get("strategy").and_then(Value::as_str) {
            strategy_stats.entry(strategy.to_string()).or_default().add(
                hypothetical_pnl,
                was_selected,
                was_traded,
            );
        }

        if record
            .get("quote_warnings")
            .and_then(Value::as_array)
            .is_some_and(|warnings| !warnings.is_empty())
        {
            summary.records_with_warnings += 1;
        }
    }

    summary.records = aggregate.records;
    summary.selected_records = aggregate.selected_records;
    summary.traded_records = aggregate.traded_records;
    summary.wins = aggregate.wins;
    summary.losses = aggregate.losses;
    summary.flats = aggregate.flats;
    summary.hypothetical_pnl = aggregate.hypothetical_pnl;
    summary.average_win = aggregate.average_win();
    summary.average_loss = aggregate.average_loss();
    summary.largest_loss = aggregate.largest_loss;
    summary.missing = false;
    summary.files = 1;
    summary.dates = dates.into_iter().collect();
    summary.by_bucket = bucket_stats
        .into_iter()
        .map(|(bucket, stats)| (bucket, stats.into()))
        .collect();
    summary.by_strategy = strategy_stats
        .into_iter()
        .map(|(strategy, stats)| (strategy, stats.into()))
        .collect();

    Ok(summary)
}

#[derive(Clone, Default)]
struct BucketStats {
    records: usize,
    wins: usize,
    losses: usize,
    flats: usize,
    realized_pnl: f64,
    win_sum: f64,
    loss_sum: f64,
    largest_loss: Option<f64>,
}

impl BucketStats {
    fn add(&mut self, value: f64) {
        self.records += 1;
        self.realized_pnl += value;
        if value > 0.0 {
            self.wins += 1;
            self.win_sum += value;
        } else if value < 0.0 {
            self.losses += 1;
            self.loss_sum += value;
            self.largest_loss = Some(
                self.largest_loss
                    .map_or(value, |current| current.min(value)),
            );
        } else {
            self.flats += 1;
        }
    }

    fn into_summary(self) -> crate::performance::PerformanceLedgerBucketSummary {
        crate::performance::PerformanceLedgerBucketSummary {
            records: self.records,
            wins: self.wins,
            losses: self.losses,
            flats: self.flats,
            realized_pnl: self.realized_pnl,
            average_win: (self.wins > 0).then_some(self.win_sum / self.wins as f64),
            average_loss: (self.losses > 0).then_some(self.loss_sum / self.losses as f64),
            largest_loss: self.largest_loss,
        }
    }
}

#[derive(Clone, Default)]
struct OutcomeStats {
    records: usize,
    selected_records: usize,
    traded_records: usize,
    wins: usize,
    losses: usize,
    flats: usize,
    hypothetical_pnl: f64,
    win_sum: f64,
    loss_sum: f64,
    largest_loss: Option<f64>,
}

impl OutcomeStats {
    fn add(&mut self, hypothetical_pnl: f64, was_selected: bool, was_traded: bool) {
        self.records += 1;
        self.hypothetical_pnl += hypothetical_pnl;
        if was_selected {
            self.selected_records += 1;
        }
        if was_traded {
            self.traded_records += 1;
        }
        if hypothetical_pnl > 0.0 {
            self.wins += 1;
            self.win_sum += hypothetical_pnl;
        } else if hypothetical_pnl < 0.0 {
            self.losses += 1;
            self.loss_sum += hypothetical_pnl;
            self.largest_loss = Some(
                self.largest_loss
                    .map_or(hypothetical_pnl, |current| current.min(hypothetical_pnl)),
            );
        } else {
            self.flats += 1;
        }
    }

    fn average_win(&self) -> Option<f64> {
        (self.wins > 0).then_some(self.win_sum / self.wins as f64)
    }

    fn average_loss(&self) -> Option<f64> {
        (self.losses > 0).then_some(self.loss_sum / self.losses as f64)
    }
}

impl From<OutcomeStats> for crate::performance::CandidateOutcomeBucketSummary {
    fn from(value: OutcomeStats) -> Self {
        Self {
            records: value.records,
            selected_records: value.selected_records,
            traded_records: value.traded_records,
            wins: value.wins,
            losses: value.losses,
            flats: value.flats,
            hypothetical_pnl: value.hypothetical_pnl,
            average_win: value.average_win(),
            average_loss: value.average_loss(),
            largest_loss: value.largest_loss,
        }
    }
}

pub async fn summarize_performance_ledger_records(
    config: &OptionsEngineConfig,
    since: Option<chrono::NaiveDate>,
    until: Option<chrono::NaiveDate>,
) -> anyhow::Result<PerformanceLedgerSummary> {
    let Some(storage) = &config.storage_repository else {
        anyhow::bail!("storage is not connected");
    };
    let account_id = config.storage_account_id();
    let filters = PerformanceLedgerSummaryFilters { since, until };
    summarize_performance_ledger(storage, account_id, filters).await
}

pub async fn summarize_candidate_outcomes_records(
    config: &OptionsEngineConfig,
    since: Option<chrono::NaiveDate>,
    until: Option<chrono::NaiveDate>,
) -> anyhow::Result<CandidateOutcomeSummary> {
    let Some(storage) = &config.storage_repository else {
        anyhow::bail!("storage is not connected");
    };
    let account_id = config.storage_account_id();
    let filters = CandidateOutcomeSummaryFilters { since, until };
    summarize_candidate_outcomes(storage, account_id, filters).await
}
