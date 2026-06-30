//! Strategy state persistence in Postgres.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sqlx::{AssertSqlSafe, Row as _, types::Json};
use uuid::Uuid;

use super::OperationalRepository;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StrategyStateMetadata {
    pub version: i64,
    pub writer_id: Option<String>,
    pub run_id: Option<String>,
    pub last_event_id: Option<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct StrategyStateStorageSummary {
    pub rows: i64,
    pub active_rows: i64,
    pub broker_leg_evidence: i64,
}

#[derive(Debug, Clone)]
pub struct StrategyStateMutation {
    pub event_id: Uuid,
    pub event_type: String,
    pub strategy: Option<String>,
    pub underlying: Option<String>,
    pub trade_date: Option<NaiveDate>,
    pub order_list_id: Option<String>,
    pub client_order_id: Option<String>,
    pub venue_order_id: Option<String>,
    pub ts_event: Option<DateTime<Utc>>,
    pub writer_id: Option<String>,
    pub run_id: Option<Uuid>,
    pub expected_version: Option<i64>,
    pub payload: Value,
}

impl StrategyStateMutation {
    #[must_use]
    pub fn new(event_id: Uuid, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id,
            event_type: event_type.into(),
            strategy: None,
            underlying: None,
            trade_date: None,
            order_list_id: None,
            client_order_id: None,
            venue_order_id: None,
            ts_event: None,
            writer_id: None,
            run_id: None,
            expected_version: None,
            payload,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct StrategyStateWriteResult {
    pub status: StrategyStateWriteStatus,
    pub state_version: i64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StrategyStateWriteStatus {
    Applied,
    DuplicateEvent,
}

pub async fn load_strategy_state<T>(
    storage: &OperationalRepository,
    account_id: &str,
) -> anyhow::Result<T>
where
    T: Default + DeserializeOwned,
{
    Ok(load_strategy_state_record(storage, account_id)
        .await?
        .unwrap_or_default())
}

pub async fn load_strategy_state_record<T>(
    storage: &OperationalRepository,
    account_id: &str,
) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let payloads = load_strategy_state_payloads(storage, account_id).await?;
    if payloads.is_empty() && !strategy_state_account_exists(storage, account_id).await? {
        return Ok(None);
    }

    Ok(Some(serde_json::from_value(serde_json::json!({
        "entries": payloads,
    }))?))
}

async fn strategy_state_account_exists(
    storage: &OperationalRepository,
    account_id: &str,
) -> anyhow::Result<bool> {
    let query = format!(
        "SELECT EXISTS(SELECT 1 FROM \"{}\".strategy_state_account WHERE account_id = $1)",
        storage.schema()
    );

    match sqlx::query_scalar::<_, bool>(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_one(storage.pool())
        .await
    {
        Ok(exists) => Ok(exists),
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("42P01") => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub async fn load_strategy_state_metadata(
    storage: &OperationalRepository,
    account_id: &str,
) -> anyhow::Result<Option<StrategyStateMetadata>> {
    let query = format!(
        "SELECT version, writer_id, run_id::text AS run_id, last_event_id::text AS last_event_id \
         FROM \"{}\".strategy_state_account WHERE account_id = $1",
        storage.schema()
    );

    let Some(row) = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_optional(storage.pool())
        .await?
    else {
        return Ok(None);
    };

    Ok(Some(StrategyStateMetadata {
        version: row.try_get("version")?,
        writer_id: row.try_get("writer_id")?,
        run_id: row.try_get("run_id")?,
        last_event_id: row.try_get("last_event_id")?,
    }))
}

pub async fn save_strategy_state(
    storage: &OperationalRepository,
    account_id: &str,
    state: &(impl Serialize + ?Sized),
) -> anyhow::Result<()> {
    replace_strategy_state_rows(storage, account_id, state).await?;
    Ok(())
}

pub async fn load_strategy_state_payloads(
    storage: &OperationalRepository,
    account_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let query = format!(
        "SELECT payload FROM \"{}\".strategy_state \
         WHERE account_id = $1 \
         ORDER BY COALESCE(recorded_at, submitted_at, updated_at), strategy_id, entry_id",
        storage.schema()
    );

    match sqlx::query_scalar::<_, Json<Value>>(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_all(storage.pool())
        .await
    {
        Ok(rows) => Ok(rows.into_iter().map(|row| row.0).collect()),
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("42P01") => {
            Ok(Vec::new())
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn load_strategy_state_storage_summary(
    storage: &OperationalRepository,
    account_id: &str,
) -> anyhow::Result<StrategyStateStorageSummary> {
    let query = format!(
        "SELECT \
             COUNT(*)::BIGINT AS rows, \
             COUNT(*) FILTER (WHERE status = 'active')::BIGINT AS active_rows, \
             (SELECT COUNT(*)::BIGINT \
                FROM \"{}\".strategy_broker_leg_evidence evidence \
               WHERE evidence.account_id = $1) AS broker_leg_evidence \
         FROM \"{}\".strategy_state state \
         WHERE state.account_id = $1",
        storage.schema(),
        storage.schema()
    );

    match sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_one(storage.pool())
        .await
    {
        Ok(row) => Ok(StrategyStateStorageSummary {
            rows: row.try_get("rows")?,
            active_rows: row.try_get("active_rows")?,
            broker_leg_evidence: row.try_get("broker_leg_evidence")?,
        }),
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("42P01") => {
            Ok(StrategyStateStorageSummary::default())
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn persist_strategy_state_mutation(
    storage: &OperationalRepository,
    account_id: &str,
    mutation: &StrategyStateMutation,
    state: &(impl Serialize + ?Sized),
) -> anyhow::Result<StrategyStateWriteResult> {
    let state_payload = serde_json::to_value(state)?;
    let mut transaction = storage.pool().begin().await?;
    let event_inserted =
        insert_strategy_state_event(storage, &mut transaction, account_id, mutation).await?;

    if !event_inserted {
        let state_version = load_state_version(storage, &mut transaction, account_id).await?;
        transaction.commit().await?;
        return Ok(StrategyStateWriteResult {
            status: StrategyStateWriteStatus::DuplicateEvent,
            state_version,
        });
    }

    ensure_account_state_row(storage, &mut transaction, account_id).await?;
    let current_version = lock_state_version(storage, &mut transaction, account_id).await?;
    if let Some(expected_version) = mutation.expected_version
        && current_version != expected_version
    {
        anyhow::bail!(
            "strategy state version conflict for account {account_id}: expected {expected_version}, current {current_version}"
        );
    }
    let state_version = current_version.saturating_add(1);
    replace_strategy_state_rows_in_transaction(
        storage,
        &mut transaction,
        account_id,
        &state_payload,
    )
    .await?;
    update_strategy_state_account_metadata(
        storage,
        &mut transaction,
        account_id,
        mutation,
        state_version,
    )
    .await?;
    transaction.commit().await?;

    Ok(StrategyStateWriteResult {
        status: StrategyStateWriteStatus::Applied,
        state_version,
    })
}

async fn replace_strategy_state_rows(
    storage: &OperationalRepository,
    account_id: &str,
    state: &(impl Serialize + ?Sized),
) -> anyhow::Result<()> {
    let state_payload = serde_json::to_value(state)?;
    let mut transaction = storage.pool().begin().await?;
    ensure_account_state_row(storage, &mut transaction, account_id).await?;
    replace_strategy_state_rows_in_transaction(
        storage,
        &mut transaction,
        account_id,
        &state_payload,
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

async fn replace_strategy_state_rows_in_transaction(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
    state_payload: &Value,
) -> anyhow::Result<()> {
    let delete_query = format!(
        "DELETE FROM \"{}\".strategy_state WHERE account_id = $1",
        storage.schema()
    );
    sqlx::query(AssertSqlSafe(delete_query))
        .bind(account_id)
        .execute(&mut **transaction)
        .await?;

    let Some(entries) = state_payload.get("entries").and_then(Value::as_array) else {
        return Ok(());
    };

    for entry in entries {
        let Some(state_row) = StrategyStateRow::from_entry(account_id, entry) else {
            continue;
        };
        upsert_strategy_state_row(storage, transaction, &state_row, entry).await?;
        replace_strategy_state_evidence(storage, transaction, &state_row, entry).await?;
    }

    Ok(())
}

struct StrategyStateRow {
    account_id: String,
    strategy_id: String,
    entry_id: String,
    trade_date: Option<String>,
    underlying: Option<String>,
    status: String,
    spread_instrument_id: Option<String>,
    spread_raw_symbol: Option<String>,
    order_list_id: Option<String>,
    close_order_list_id: Option<String>,
    broker_parent_order_id: Option<String>,
    broker_close_parent_order_id: Option<String>,
    quantity: Option<i64>,
    submitted_at: Option<String>,
    recorded_at: Option<String>,
    closed_at: Option<String>,
}

impl StrategyStateRow {
    fn from_entry(account_id: &str, entry: &Value) -> Option<Self> {
        let strategy_id = json_string(entry, "strategy").unwrap_or_else(|| "unknown".to_string());
        let spread_instrument_id = json_string(entry, "spread_instrument_id");
        let order_list_id = json_string(entry, "order_list_id");
        let entry_id = order_list_id
            .clone()
            .or_else(|| spread_instrument_id.clone())?;
        let status = if json_bool(entry, "closed") {
            "closed"
        } else if json_bool(entry, "canceled") {
            "canceled"
        } else if json_bool(entry, "submitted") {
            "active"
        } else {
            "pending"
        }
        .to_string();

        Some(Self {
            account_id: account_id.to_string(),
            strategy_id,
            entry_id,
            trade_date: json_date(entry, "trade_date"),
            underlying: json_string(entry, "underlying"),
            status,
            spread_instrument_id,
            spread_raw_symbol: json_string(entry, "spread_raw_symbol"),
            order_list_id,
            close_order_list_id: json_string(entry, "close_order_list_id"),
            broker_parent_order_id: json_string(entry, "parent_order_id"),
            broker_close_parent_order_id: json_string(entry, "close_parent_order_id"),
            quantity: json_i64(entry, "quantity"),
            submitted_at: json_timestamp(entry, "submitted_at_utc"),
            recorded_at: json_timestamp(entry, "recorded_at_utc"),
            closed_at: json_timestamp(entry, "closed_at_utc"),
        })
    }
}

async fn upsert_strategy_state_row(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    state_row: &StrategyStateRow,
    payload: &Value,
) -> anyhow::Result<()> {
    let query = format!(
        "INSERT INTO \"{}\".strategy_state (\
             account_id, strategy_id, entry_id, trade_date, underlying, status, \
             spread_instrument_id, spread_raw_symbol, order_list_id, close_order_list_id, \
             broker_parent_order_id, broker_close_parent_order_id, quantity, submitted_at, \
             recorded_at, closed_at, payload, updated_at\
         ) VALUES (\
             $1, $2, $3, $4::date, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
             $14::timestamptz, $15::timestamptz, $16::timestamptz, $17, NOW()\
         ) \
         ON CONFLICT (account_id, strategy_id, entry_id) \
         DO UPDATE SET \
             trade_date = EXCLUDED.trade_date, \
             underlying = EXCLUDED.underlying, \
             status = EXCLUDED.status, \
             spread_instrument_id = EXCLUDED.spread_instrument_id, \
             spread_raw_symbol = EXCLUDED.spread_raw_symbol, \
             order_list_id = EXCLUDED.order_list_id, \
             close_order_list_id = EXCLUDED.close_order_list_id, \
             broker_parent_order_id = EXCLUDED.broker_parent_order_id, \
             broker_close_parent_order_id = EXCLUDED.broker_close_parent_order_id, \
             quantity = EXCLUDED.quantity, \
             submitted_at = EXCLUDED.submitted_at, \
             recorded_at = EXCLUDED.recorded_at, \
             closed_at = EXCLUDED.closed_at, \
             payload = EXCLUDED.payload, \
             updated_at = NOW()",
        storage.schema()
    );

    sqlx::query(AssertSqlSafe(query))
        .bind(&state_row.account_id)
        .bind(&state_row.strategy_id)
        .bind(&state_row.entry_id)
        .bind(state_row.trade_date.as_deref())
        .bind(state_row.underlying.as_deref())
        .bind(&state_row.status)
        .bind(state_row.spread_instrument_id.as_deref())
        .bind(state_row.spread_raw_symbol.as_deref())
        .bind(state_row.order_list_id.as_deref())
        .bind(state_row.close_order_list_id.as_deref())
        .bind(state_row.broker_parent_order_id.as_deref())
        .bind(state_row.broker_close_parent_order_id.as_deref())
        .bind(state_row.quantity)
        .bind(state_row.submitted_at.as_deref())
        .bind(state_row.recorded_at.as_deref())
        .bind(state_row.closed_at.as_deref())
        .bind(Json(payload.clone()))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn replace_strategy_state_evidence(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    state_row: &StrategyStateRow,
    entry: &Value,
) -> anyhow::Result<()> {
    let delete_query = format!(
        "DELETE FROM \"{}\".strategy_broker_leg_evidence \
         WHERE account_id = $1 AND strategy_id = $2 AND entry_id = $3",
        storage.schema()
    );
    sqlx::query(AssertSqlSafe(delete_query))
        .bind(&state_row.account_id)
        .bind(&state_row.strategy_id)
        .bind(&state_row.entry_id)
        .execute(&mut **transaction)
        .await?;

    for evidence in strategy_state_evidence(state_row, entry) {
        insert_strategy_broker_evidence(storage, transaction, &evidence).await?;
    }
    Ok(())
}

struct StrategyBrokerEvidence {
    account_id: String,
    strategy_id: String,
    entry_id: String,
    evidence_id: String,
    evidence_type: String,
    symbol: Option<String>,
    instrument_id: Option<String>,
    ratio: Option<i64>,
    broker_order_id: Option<String>,
    client_order_id: Option<String>,
    order_list_id: Option<String>,
    payload: Value,
}

fn strategy_state_evidence(
    state_row: &StrategyStateRow,
    entry: &Value,
) -> Vec<StrategyBrokerEvidence> {
    let mut evidence = Vec::new();

    if let Some(legs) = entry.get("spread_legs").and_then(Value::as_array) {
        for (index, leg) in legs.iter().enumerate() {
            let symbol = json_string(leg, "symbol");
            let instrument_id = json_string(leg, "instrument_id");
            let evidence_key = symbol
                .clone()
                .or_else(|| instrument_id.clone())
                .unwrap_or_else(|| index.to_string());
            evidence.push(StrategyBrokerEvidence {
                account_id: state_row.account_id.clone(),
                strategy_id: state_row.strategy_id.clone(),
                entry_id: state_row.entry_id.clone(),
                evidence_id: format!("spread_leg:{evidence_key}"),
                evidence_type: "spread_leg".to_string(),
                symbol,
                instrument_id,
                ratio: json_i64(leg, "ratio"),
                broker_order_id: None,
                client_order_id: None,
                order_list_id: state_row.order_list_id.clone(),
                payload: serde_json::json!({
                    "source": "strategy_state.spread_legs",
                    "leg": leg,
                }),
            });
        }
    }

    for key in [
        "short_symbol",
        "long_symbol",
        "short_call_symbol",
        "long_call_symbol",
    ] {
        if let Some(symbol) = json_string(entry, key) {
            evidence.push(StrategyBrokerEvidence {
                account_id: state_row.account_id.clone(),
                strategy_id: state_row.strategy_id.clone(),
                entry_id: state_row.entry_id.clone(),
                evidence_id: format!("strategy_leg:{key}:{symbol}"),
                evidence_type: "strategy_leg".to_string(),
                symbol: Some(symbol),
                instrument_id: None,
                ratio: None,
                broker_order_id: None,
                client_order_id: None,
                order_list_id: state_row.order_list_id.clone(),
                payload: serde_json::json!({
                    "source": key,
                }),
            });
        }
    }

    if let Some(parent_order_id) = &state_row.broker_parent_order_id {
        evidence.push(StrategyBrokerEvidence {
            account_id: state_row.account_id.clone(),
            strategy_id: state_row.strategy_id.clone(),
            entry_id: state_row.entry_id.clone(),
            evidence_id: format!("entry_parent_order:{parent_order_id}"),
            evidence_type: "entry_parent_order".to_string(),
            symbol: None,
            instrument_id: None,
            ratio: None,
            broker_order_id: Some(parent_order_id.clone()),
            client_order_id: state_row.order_list_id.clone(),
            order_list_id: state_row.order_list_id.clone(),
            payload: serde_json::json!({
                "source": "strategy_state.parent_order_id",
                "broker_order_id": parent_order_id,
                "order_list_id": state_row.order_list_id,
            }),
        });
    }

    if let Some(close_parent_order_id) = &state_row.broker_close_parent_order_id {
        evidence.push(StrategyBrokerEvidence {
            account_id: state_row.account_id.clone(),
            strategy_id: state_row.strategy_id.clone(),
            entry_id: state_row.entry_id.clone(),
            evidence_id: format!("close_parent_order:{close_parent_order_id}"),
            evidence_type: "close_parent_order".to_string(),
            symbol: None,
            instrument_id: None,
            ratio: None,
            broker_order_id: Some(close_parent_order_id.clone()),
            client_order_id: state_row.close_order_list_id.clone(),
            order_list_id: state_row.close_order_list_id.clone(),
            payload: serde_json::json!({
                "source": "strategy_state.close_parent_order_id",
                "broker_order_id": close_parent_order_id,
                "close_order_list_id": state_row.close_order_list_id,
            }),
        });
    }

    evidence
}

async fn insert_strategy_broker_evidence(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    evidence: &StrategyBrokerEvidence,
) -> anyhow::Result<()> {
    let query = format!(
        "INSERT INTO \"{}\".strategy_broker_leg_evidence (\
             account_id, strategy_id, entry_id, evidence_id, evidence_type, symbol, \
             instrument_id, ratio, broker_order_id, client_order_id, order_list_id, payload, updated_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW()\
         ) \
         ON CONFLICT (account_id, strategy_id, entry_id, evidence_id) \
         DO UPDATE SET \
             evidence_type = EXCLUDED.evidence_type, \
             symbol = EXCLUDED.symbol, \
             instrument_id = EXCLUDED.instrument_id, \
             ratio = EXCLUDED.ratio, \
             broker_order_id = EXCLUDED.broker_order_id, \
             client_order_id = EXCLUDED.client_order_id, \
             order_list_id = EXCLUDED.order_list_id, \
             payload = EXCLUDED.payload, \
             updated_at = NOW()",
        storage.schema()
    );

    sqlx::query(AssertSqlSafe(query))
        .bind(&evidence.account_id)
        .bind(&evidence.strategy_id)
        .bind(&evidence.entry_id)
        .bind(&evidence.evidence_id)
        .bind(&evidence.evidence_type)
        .bind(evidence.symbol.as_deref())
        .bind(evidence.instrument_id.as_deref())
        .bind(evidence.ratio)
        .bind(evidence.broker_order_id.as_deref())
        .bind(evidence.client_order_id.as_deref())
        .bind(evidence.order_list_id.as_deref())
        .bind(Json(evidence.payload.clone()))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

fn json_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn json_bool(payload: &Value, key: &str) -> bool {
    payload
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or_default()
}

fn json_i64(payload: &Value, key: &str) -> Option<i64> {
    payload.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
            .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
    })
}

fn json_date(payload: &Value, key: &str) -> Option<String> {
    json_string(payload, key).and_then(|value| {
        NaiveDate::parse_from_str(&value, "%Y-%m-%d")
            .ok()
            .map(|date| date.to_string())
    })
}

fn json_timestamp(payload: &Value, key: &str) -> Option<String> {
    json_string(payload, key).and_then(|value| {
        DateTime::parse_from_rfc3339(&value)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc).to_rfc3339())
    })
}

async fn insert_strategy_state_event(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
    mutation: &StrategyStateMutation,
) -> anyhow::Result<bool> {
    let query = format!(
        "INSERT INTO \"{}\".strategy_state_events \
            (account_id, event_id, event_type, strategy, underlying, trade_date, \
             order_list_id, client_order_id, venue_order_id, ts_event, payload) \
         VALUES ($1, $2::uuid, $3, $4, $5, $6::date, $7, $8, $9, $10::timestamptz, $11) \
         ON CONFLICT (account_id, event_id) DO NOTHING \
         RETURNING id",
        storage.schema()
    );
    let event_id = mutation.event_id.to_string();
    let trade_date = mutation.trade_date.map(|value| value.to_string());
    let ts_event = mutation.ts_event.map(|value| value.to_rfc3339());
    let row = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(event_id)
        .bind(&mutation.event_type)
        .bind(mutation.strategy.as_deref())
        .bind(mutation.underlying.as_deref())
        .bind(trade_date)
        .bind(mutation.order_list_id.as_deref())
        .bind(mutation.client_order_id.as_deref())
        .bind(mutation.venue_order_id.as_deref())
        .bind(ts_event)
        .bind(Json(mutation.payload.clone()))
        .fetch_optional(&mut **transaction)
        .await?;
    Ok(row.is_some())
}

async fn load_state_version(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
) -> anyhow::Result<i64> {
    let query = format!(
        "SELECT version FROM \"{}\".strategy_state_account WHERE account_id = $1",
        storage.schema()
    );
    Ok(sqlx::query_scalar::<_, i64>(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_optional(&mut **transaction)
        .await?
        .unwrap_or_default())
}

async fn ensure_account_state_row(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
) -> anyhow::Result<()> {
    let query = format!(
        "INSERT INTO \"{}\".strategy_state_account (account_id, version, updated_at) \
         VALUES ($1, 0, NOW()) \
         ON CONFLICT (account_id) DO NOTHING",
        storage.schema()
    );
    sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn lock_state_version(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
) -> anyhow::Result<i64> {
    let query = format!(
        "SELECT version FROM \"{}\".strategy_state_account WHERE account_id = $1 FOR UPDATE",
        storage.schema()
    );
    let Some(version) = sqlx::query_scalar::<_, i64>(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_optional(&mut **transaction)
        .await?
    else {
        anyhow::bail!("strategy state account row was not initialized for account {account_id}");
    };
    Ok(version)
}

async fn update_strategy_state_account_metadata(
    storage: &OperationalRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
    mutation: &StrategyStateMutation,
    state_version: i64,
) -> anyhow::Result<()> {
    let query = format!(
        "UPDATE \"{}\".strategy_state_account \
         SET version = $2, writer_id = $3, run_id = $4::uuid, \
             last_event_id = $5::uuid, updated_at = NOW() \
         WHERE account_id = $1",
        storage.schema()
    );
    let run_id = mutation.run_id.map(|value| value.to_string());
    let event_id = mutation.event_id.to_string();
    let result = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(state_version)
        .bind(mutation.writer_id.as_deref())
        .bind(run_id)
        .bind(event_id)
        .execute(&mut **transaction)
        .await?;
    if result.rows_affected() != 1 {
        anyhow::bail!("strategy state account update affected no rows for account {account_id}");
    }
    Ok(())
}
