//! Strategy state persistence in Postgres.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sqlx::{AssertSqlSafe, types::Json};
use uuid::Uuid;

use crate::{runtime::StrategyState, storage::StorageRepository};

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
    pub snapshot_version: i64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StrategyStateWriteStatus {
    Applied,
    DuplicateEvent,
}

pub async fn load_strategy_state(
    storage: &StorageRepository,
    account_id: &str,
) -> anyhow::Result<StrategyState> {
    Ok(load_strategy_state_record(storage, account_id)
        .await?
        .unwrap_or_default())
}

pub async fn load_strategy_state_record(
    storage: &StorageRepository,
    account_id: &str,
) -> anyhow::Result<Option<StrategyState>> {
    let query = format!(
        "SELECT state FROM \"{}\".strategy_state WHERE account_id = $1",
        storage.schema()
    );

    let row = sqlx::query_scalar::<_, Json<Value>>(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_optional(storage.pool())
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(serde_json::from_value(row.0)?))
}

pub async fn save_strategy_state(
    storage: &StorageRepository,
    account_id: &str,
    state: &StrategyState,
) -> anyhow::Result<()> {
    let payload = serde_json::to_value(state)?;
    let query = format!(
        "INSERT INTO \"{}\".strategy_state (account_id, state, updated_at)\n         VALUES ($1, $2, NOW())\n         ON CONFLICT (account_id)\n         DO UPDATE SET state = EXCLUDED.state, updated_at = NOW()",
        storage.schema()
    );

    sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(Json(payload))
        .execute(storage.pool())
        .await?;
    Ok(())
}

pub async fn persist_strategy_state_mutation(
    storage: &StorageRepository,
    account_id: &str,
    mutation: &StrategyStateMutation,
    state: &StrategyState,
) -> anyhow::Result<StrategyStateWriteResult> {
    let snapshot = serde_json::to_value(state)?;
    let mut transaction = storage.pool().begin().await?;
    let event_inserted =
        insert_strategy_state_event(storage, &mut transaction, account_id, mutation).await?;

    if !event_inserted {
        let snapshot_version = load_snapshot_version(storage, &mut transaction, account_id).await?;
        transaction.commit().await?;
        return Ok(StrategyStateWriteResult {
            status: StrategyStateWriteStatus::DuplicateEvent,
            snapshot_version,
        });
    }

    ensure_snapshot_row(storage, &mut transaction, account_id, &snapshot).await?;
    let current_version = lock_snapshot_version(storage, &mut transaction, account_id).await?;
    if let Some(expected_version) = mutation.expected_version
        && current_version != expected_version
    {
        anyhow::bail!(
            "strategy state version conflict for account {account_id}: expected {expected_version}, current {current_version}"
        );
    }
    let snapshot_version = current_version.saturating_add(1);
    update_strategy_state_snapshot(
        storage,
        &mut transaction,
        account_id,
        mutation,
        snapshot,
        snapshot_version,
    )
    .await?;
    transaction.commit().await?;

    Ok(StrategyStateWriteResult {
        status: StrategyStateWriteStatus::Applied,
        snapshot_version,
    })
}

async fn insert_strategy_state_event(
    storage: &StorageRepository,
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

async fn load_snapshot_version(
    storage: &StorageRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
) -> anyhow::Result<i64> {
    let query = format!(
        "SELECT version FROM \"{}\".strategy_state WHERE account_id = $1",
        storage.schema()
    );
    Ok(sqlx::query_scalar::<_, i64>(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_optional(&mut **transaction)
        .await?
        .unwrap_or_default())
}

async fn ensure_snapshot_row(
    storage: &StorageRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
    snapshot: &Value,
) -> anyhow::Result<()> {
    let query = format!(
        "INSERT INTO \"{}\".strategy_state (account_id, state, version, updated_at) \
         VALUES ($1, $2, 0, NOW()) \
         ON CONFLICT (account_id) DO NOTHING",
        storage.schema()
    );
    sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(Json(snapshot.clone()))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn lock_snapshot_version(
    storage: &StorageRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
) -> anyhow::Result<i64> {
    let query = format!(
        "SELECT version FROM \"{}\".strategy_state WHERE account_id = $1 FOR UPDATE",
        storage.schema()
    );
    let Some(version) = sqlx::query_scalar::<_, i64>(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_optional(&mut **transaction)
        .await?
    else {
        anyhow::bail!("strategy state snapshot row was not initialized for account {account_id}");
    };
    Ok(version)
}

async fn update_strategy_state_snapshot(
    storage: &StorageRepository,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: &str,
    mutation: &StrategyStateMutation,
    snapshot: Value,
    snapshot_version: i64,
) -> anyhow::Result<()> {
    let query = format!(
        "UPDATE \"{}\".strategy_state \
         SET state = $2, version = $3, writer_id = $4, run_id = $5::uuid, \
             last_event_id = $6::uuid, updated_at = NOW() \
         WHERE account_id = $1",
        storage.schema()
    );
    let run_id = mutation.run_id.map(|value| value.to_string());
    let event_id = mutation.event_id.to_string();
    let result = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(Json(snapshot))
        .bind(snapshot_version)
        .bind(mutation.writer_id.as_deref())
        .bind(run_id)
        .bind(event_id)
        .execute(&mut **transaction)
        .await?;
    if result.rows_affected() != 1 {
        anyhow::bail!("strategy state snapshot update affected no rows for account {account_id}");
    }
    Ok(())
}
