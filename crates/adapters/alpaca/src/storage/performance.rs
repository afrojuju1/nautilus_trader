//! Performance and candidate-outcome persistence in Postgres.

use chrono::NaiveDate;
use serde_json::Value;
use sqlx::{AssertSqlSafe, Row as _, types::Json};

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

    let result = sqlx::query(AssertSqlSafe(query))
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
    let rows = sqlx::query(AssertSqlSafe(query))
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
    let preserve_existing =
        record.get("observation_bucket").and_then(Value::as_str) == Some("virtual_close");
    let conflict_clause = if preserve_existing {
        "ON CONFLICT (account_id, record_key) DO NOTHING"
    } else {
        "ON CONFLICT (account_id, record_key) DO UPDATE\n         SET payload = EXCLUDED.payload, ts_utc = EXCLUDED.ts_utc"
    };
    let query = format!(
        "INSERT INTO \"{}\".candidate_outcome (account_id, trade_date, ts_utc, record_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5)\n         {}",
        storage.schema(),
        conflict_clause,
    );
    let result = sqlx::query(AssertSqlSafe(query))
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
    let rows = sqlx::query(AssertSqlSafe(query))
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
    let mut records_by_key = std::collections::BTreeMap::<String, Value>::new();
    for row in rows {
        let payload: sqlx::types::Json<Value> = row.try_get("payload")?;
        let record = payload.0;
        let key = candidate_outcome_semantic_key(&record)
            .or_else(|| {
                record
                    .get("record_key")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| format!("unknown|{}", records_by_key.len()));
        records_by_key.insert(key, record);
    }

    let mut aggregate = OutcomeStats::default();
    let mut selected_aggregate = OutcomeStats::default();
    let mut submitted_aggregate = OutcomeStats::default();
    let mut rejected_aggregate = OutcomeStats::default();
    let mut virtual_aggregate = OutcomeStats::default();
    let mut virtual_close_aggregate = OutcomeStats::default();
    let mut dates = std::collections::BTreeSet::<String>::new();
    let mut bucket_stats = std::collections::BTreeMap::<String, OutcomeStats>::new();
    let mut strategy_stats = std::collections::BTreeMap::<String, OutcomeStats>::new();
    let mut close_reason_stats = std::collections::BTreeMap::<String, OutcomeStats>::new();

    for record in records_by_key.into_values() {
        let Some(hypothetical_pnl) = record.get("hypothetical_pnl").and_then(Value::as_f64) else {
            summary.parse_errors += 1;
            continue;
        };
        let flags = outcome_flags(&record);
        aggregate.add(hypothetical_pnl, flags);
        if flags.was_selected {
            selected_aggregate.add(hypothetical_pnl, flags);
        }
        if flags.was_submitted {
            submitted_aggregate.add(hypothetical_pnl, flags);
        }
        if flags.was_rejected {
            rejected_aggregate.add(hypothetical_pnl, flags);
        }
        if flags.virtual_trade {
            virtual_aggregate.add(hypothetical_pnl, flags);
        }
        if flags.virtual_close {
            virtual_close_aggregate.add(hypothetical_pnl, flags);
            if let Some(reason) = record.get("virtual_close_reason").and_then(Value::as_str) {
                close_reason_stats
                    .entry(reason.to_string())
                    .or_default()
                    .add(hypothetical_pnl, flags);
            }
        }
        if let Some(trade_date) = record.get("trade_date").and_then(Value::as_str) {
            dates.insert(trade_date.to_string());
        }
        if let Some(bucket) = record.get("observation_bucket").and_then(Value::as_str) {
            bucket_stats
                .entry(bucket.to_string())
                .or_default()
                .add(hypothetical_pnl, flags);
        }
        if let Some(strategy) = record.get("strategy").and_then(Value::as_str) {
            strategy_stats
                .entry(strategy.to_string())
                .or_default()
                .add(hypothetical_pnl, flags);
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
    summary.submitted_records = aggregate.submitted_records;
    summary.rejected_records = aggregate.rejected_records;
    summary.virtual_records = aggregate.virtual_records;
    summary.virtual_close_records = aggregate.virtual_close_records;
    summary.selected = selected_aggregate.into();
    summary.submitted = submitted_aggregate.into();
    summary.rejected = rejected_aggregate.into();
    summary.virtual_trades = virtual_aggregate.into();
    summary.virtual_closes = virtual_close_aggregate.into();
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
    summary.by_virtual_close_reason = close_reason_stats
        .into_iter()
        .map(|(reason, stats)| (reason, stats.into()))
        .collect();

    Ok(summary)
}

fn candidate_outcome_semantic_key(record: &Value) -> Option<String> {
    Some(format!(
        "{}|{}|{}",
        record.get("trade_date")?.as_str()?,
        record.get("candidate_identity_key")?.as_str()?,
        record.get("observation_bucket")?.as_str()?,
    ))
}

#[derive(Clone, Copy, Default)]
struct OutcomeFlags {
    was_selected: bool,
    was_submitted: bool,
    was_traded: bool,
    was_rejected: bool,
    virtual_trade: bool,
    virtual_close: bool,
}

fn outcome_flags(record: &Value) -> OutcomeFlags {
    let was_selected = record
        .get("was_selected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let selected_action = record.get("selected_action").and_then(Value::as_str);
    let was_submitted = record
        .get("was_submitted")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            selected_action
                .is_some_and(|action| matches!(action, "submit" | "submitted" | "selected"))
        });
    let was_rejected = record
        .get("was_rejected")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            record
                .get("rejected")
                .and_then(Value::as_u64)
                .is_some_and(|rejected| rejected > 0)
                || record
                    .get("terminal_rejection_recorded")
                    .and_then(Value::as_bool)
                    == Some(true)
        });
    let was_traded = record
        .get("was_traded")
        .and_then(Value::as_bool)
        .unwrap_or(was_submitted && !was_rejected);
    let virtual_trade = record
        .get("virtual_trade")
        .and_then(Value::as_bool)
        .unwrap_or(was_selected && !was_traded);
    let virtual_close = record
        .get("virtual_close")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            record
                .get("virtual_close_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.is_empty())
        });
    OutcomeFlags {
        was_selected,
        was_submitted,
        was_traded,
        was_rejected,
        virtual_trade,
        virtual_close,
    }
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
    submitted_records: usize,
    traded_records: usize,
    rejected_records: usize,
    virtual_records: usize,
    virtual_close_records: usize,
    wins: usize,
    losses: usize,
    flats: usize,
    hypothetical_pnl: f64,
    win_sum: f64,
    loss_sum: f64,
    largest_loss: Option<f64>,
}

impl OutcomeStats {
    fn add(&mut self, hypothetical_pnl: f64, flags: OutcomeFlags) {
        self.records += 1;
        self.hypothetical_pnl += hypothetical_pnl;
        if flags.was_selected {
            self.selected_records += 1;
        }
        if flags.was_submitted {
            self.submitted_records += 1;
        }
        if flags.was_traded {
            self.traded_records += 1;
        }
        if flags.was_rejected {
            self.rejected_records += 1;
        }
        if flags.virtual_trade {
            self.virtual_records += 1;
        }
        if flags.virtual_close {
            self.virtual_close_records += 1;
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
            submitted_records: value.submitted_records,
            traded_records: value.traded_records,
            rejected_records: value.rejected_records,
            virtual_records: value.virtual_records,
            virtual_close_records: value.virtual_close_records,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn outcome_flags_classify_rejected_virtual_close() {
        let record = json!({
            "was_selected": true,
            "was_submitted": true,
            "was_traded": false,
            "was_rejected": true,
            "virtual_trade": true,
            "virtual_close": true,
            "virtual_close_reason": "profit_target",
        });

        let flags = outcome_flags(&record);

        assert!(flags.was_selected);
        assert!(flags.was_submitted);
        assert!(!flags.was_traded);
        assert!(flags.was_rejected);
        assert!(flags.virtual_trade);
        assert!(flags.virtual_close);
    }

    #[test]
    fn outcome_stats_count_selected_and_virtual_records() {
        let mut stats = OutcomeStats::default();
        stats.add(
            12.0,
            OutcomeFlags {
                was_selected: true,
                was_submitted: false,
                was_traded: false,
                was_rejected: false,
                virtual_trade: true,
                virtual_close: true,
            },
        );

        let summary: crate::performance::CandidateOutcomeBucketSummary = stats.into();

        assert_eq!(summary.records, 1);
        assert_eq!(summary.selected_records, 1);
        assert_eq!(summary.submitted_records, 0);
        assert_eq!(summary.traded_records, 0);
        assert_eq!(summary.virtual_records, 1);
        assert_eq!(summary.virtual_close_records, 1);
        assert_eq!(summary.wins, 1);
        assert_eq!(summary.hypothetical_pnl, 12.0);
    }
}
