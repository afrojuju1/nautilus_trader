//! Performance and candidate-outcome ledger persistence in Postgres.

use chrono::NaiveDate;
use serde_json::Value;
use sqlx::{AssertSqlSafe, Row as _, types::Json};

use super::OperationalRepository;

#[derive(Debug, Default)]
pub struct PerformanceLedgerSummaryFilters {
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
}

pub type CandidateOutcomeSummaryFilters = PerformanceLedgerSummaryFilters;

pub async fn append_performance_ledger_payload(
    storage: &OperationalRepository,
    account_id: &str,
    ledger_date: &str,
    record_key: &str,
    payload: Value,
) -> anyhow::Result<bool> {
    use chrono::Utc;

    let date = chrono::NaiveDate::parse_from_str(ledger_date, "%Y-%m-%d")?;
    let query = format!(
        "INSERT INTO \"{}\".performance_ledger (account_id, ledger_date, ts_utc, record_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5)\n         ON CONFLICT (account_id, record_key) DO UPDATE\n         SET payload = EXCLUDED.payload, ts_utc = EXCLUDED.ts_utc",
        storage.schema()
    );

    let result = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(date.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(record_key)
        .bind(Json(payload))
        .execute(storage.pool())
        .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn read_performance_ledger_records(
    storage: &OperationalRepository,
    account_id: &str,
    filters: PerformanceLedgerSummaryFilters,
) -> anyhow::Result<Vec<Value>> {
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

    Ok(rows
        .into_iter()
        .map(|row| {
            let payload: Json<Value> = row.try_get("payload")?;
            Ok(payload.0)
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?)
}

pub async fn append_candidate_outcome(
    storage: &OperationalRepository,
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

pub async fn read_candidate_outcome_records(
    storage: &OperationalRepository,
    account_id: &str,
    filters: CandidateOutcomeSummaryFilters,
) -> anyhow::Result<Vec<Value>> {
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

    Ok(rows
        .into_iter()
        .map(|row| {
            let payload: Json<Value> = row.try_get("payload")?;
            Ok(payload.0)
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?)
}
