//! Candidate-ledger persistence in Postgres.

use chrono::{NaiveDate, Utc};
use serde_json::Value;
use sqlx::{AssertSqlSafe, Row, types::Json};

use super::OperationalRepository;

pub const CANDIDATE_LEDGER_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Default)]
pub struct CandidateLedgerSummaryFilters {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
}

pub async fn append_candidate_ledger_record(
    storage: &OperationalRepository,
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
    storage: &OperationalRepository,
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
