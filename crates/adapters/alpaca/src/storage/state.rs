//! Strategy state persistence in Postgres.

use serde_json::Value;
use sqlx::types::Json;

use crate::{runtime::StrategyState, storage::StorageRepository};

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

    let row = sqlx::query_scalar::<_, Json<Value>>(&query)
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

    sqlx::query(&query)
        .bind(account_id)
        .bind(Json(payload))
        .execute(storage.pool())
        .await?;
    Ok(())
}
