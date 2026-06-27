//! Candidate-ledger persistence in Postgres.

use chrono::{NaiveDate, Utc};
use serde_json::Value;
use sqlx::{AssertSqlSafe, Row, types::Json};

use crate::{
    options_runtime::AlpacaOptionsRuntimeConfig, performance::CandidateLedgerSummary,
    storage::StorageRepository,
};

pub const CANDIDATE_LEDGER_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Default)]
pub struct CandidateLedgerSummaryFilters {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
}

pub async fn append_candidate_ledger_record(
    storage: &StorageRepository,
    account_id: &str,
    trade_date: &str,
    record_type: &str,
    payload: Value,
) -> anyhow::Result<()> {
    let date = chrono::NaiveDate::parse_from_str(trade_date, "%Y-%m-%d")?;
    let mut record = serde_json::Map::new();
    record.insert(
        "schema_version".to_string(),
        Value::from(CANDIDATE_LEDGER_SCHEMA_VERSION),
    );
    record.insert("ts_utc".to_string(), Value::String(Utc::now().to_rfc3339()));
    record.insert("type".to_string(), Value::String(record_type.to_string()));
    record.insert(
        "trade_date".to_string(),
        Value::String(trade_date.to_string()),
    );
    record.insert(
        "account_id".to_string(),
        Value::String(account_id.to_string()),
    );
    if let Value::Object(fields) = payload {
        record.extend(fields);
    } else {
        record.insert("payload".to_string(), payload);
    }

    let alert_type = record.get("alert_type").and_then(Value::as_str);
    let severity = record.get("severity").and_then(Value::as_str);
    let alert_key = record.get("alert_key").and_then(Value::as_str);

    let mut final_record = serde_json::Map::new();
    final_record.insert(
        "account_id".to_string(),
        Value::String(account_id.to_string()),
    );
    for (key, value) in record.iter() {
        final_record.insert(key.clone(), value.clone());
    }
    let query = format!(
        "INSERT INTO \"{}\".candidate_ledger (account_id, trade_date, ts_utc, record_type, alert_type, severity, alert_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5, $6, $7, $8)",
        storage.schema()
    );

    sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(date.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(record_type)
        .bind(alert_type)
        .bind(severity)
        .bind(alert_key)
        .bind(Json(Value::Object(final_record)))
        .execute(storage.pool())
        .await?;

    Ok(())
}

pub async fn read_candidate_ledger_records(
    storage: &StorageRepository,
    account_id: &str,
    filters: CandidateLedgerSummaryFilters,
) -> anyhow::Result<Vec<Value>> {
    let query = format!(
        "SELECT payload FROM \"{}\".candidate_ledger WHERE account_id = $1 AND ($2::date IS NULL OR trade_date >= $2::date) AND ($3::date IS NULL OR trade_date <= $3::date) ORDER BY ts_utc ASC",
        storage.schema()
    );
    let rows = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(filters.since.map(|d| d.to_string()))
        .bind(filters.until.map(|d| d.to_string()))
        .fetch_all(storage.pool())
        .await?;

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let payload: Json<Value> = row.get("payload");
        records.push(payload.0);
    }
    Ok(records)
}

pub async fn summarize_candidate_ledger(
    storage: &StorageRepository,
    account_id: &str,
    filters: CandidateLedgerSummaryFilters,
) -> anyhow::Result<CandidateLedgerSummary> {
    let records = read_candidate_ledger_records(storage, account_id, filters).await?;

    let mut summary = CandidateLedgerSummary::default();
    summary.directory = format!(
        "postgres://{account}/candidate_ledger",
        account = account_id
    );
    summary.files = 0;
    summary.dates = records
        .iter()
        .filter_map(|record| {
            record
                .get("trade_date")
                .and_then(Value::as_str)
                .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
                .map(|value| value.to_string())
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    summary.files = summary.dates.len();

    for record in records {
        summary.records += 1;
        let record_type = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        *summary.by_type.entry(record_type.clone()).or_insert(0) += 1;

        match record_type.as_str() {
            "candidate" => {
                summary.candidates += 1;
                if let Some(strategy) = record.get("strategy").and_then(Value::as_str) {
                    *summary
                        .candidates_by_strategy
                        .entry(strategy.to_string())
                        .or_insert(0) += 1;
                }
            }
            "scanner_result" => summary.scanner_results += 1,
            "decision" => summary.decisions += 1,
            "submit_result" => summary.submit_results += 1,
            "candidate_alert" => {
                summary.candidate_alerts += 1;
                match record.get("alert_type").and_then(Value::as_str) {
                    Some("selected_candidate") => summary.selected_candidates += 1,
                    Some("high_score_candidate") => summary.high_score_candidates += 1,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(summary)
}

/// Async wrapper that reads from Postgres storage.
pub async fn summarize_candidate_ledger_records(
    config: &AlpacaOptionsRuntimeConfig,
    since: Option<chrono::NaiveDate>,
    until: Option<chrono::NaiveDate>,
) -> anyhow::Result<CandidateLedgerSummary> {
    let Some(storage) = config.storage_repository.as_ref() else {
        anyhow::bail!("storage is not connected");
    };
    let account_id = config.storage_account_id();
    let filters = CandidateLedgerSummaryFilters { since, until };
    summarize_candidate_ledger(storage, account_id, filters).await
}
