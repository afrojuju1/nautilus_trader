//! Backtest market-data cache persistence in Postgres.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::storage::StorageRepository;

/// Reads one cached backtest market-data payload.
///
/// # Errors
///
/// Returns an error when Postgres read or JSON decoding fails.
pub async fn read_backtest_market_cache<T>(
    storage: &StorageRepository,
    account_id: &str,
    cache_kind: &str,
    cache_key: &str,
) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let sql = format!(
        "SELECT payload FROM \"{}\".backtest_market_cache \
         WHERE account_id = $1 AND cache_kind = $2 AND cache_key = $3",
        storage.schema(),
    );
    let payload = sqlx::query_scalar::<_, Value>(&sql)
        .bind(account_id)
        .bind(cache_kind)
        .bind(cache_key)
        .fetch_optional(storage.pool())
        .await?;
    payload.map(serde_json::from_value).transpose().map_err(Into::into)
}

/// Writes one cached backtest market-data payload.
///
/// # Errors
///
/// Returns an error when JSON encoding or Postgres write fails.
pub async fn write_backtest_market_cache<T>(
    storage: &StorageRepository,
    account_id: &str,
    cache_kind: &str,
    cache_key: &str,
    payload: &T,
) -> anyhow::Result<()>
where
    T: Serialize,
{
    let payload = serde_json::to_value(payload)?;
    let sql = format!(
        "INSERT INTO \"{}\".backtest_market_cache \
            (account_id, cache_kind, cache_key, payload, updated_at) \
         VALUES ($1, $2, $3, $4, NOW()) \
         ON CONFLICT (account_id, cache_kind, cache_key) DO UPDATE \
         SET payload = EXCLUDED.payload, updated_at = NOW()",
        storage.schema(),
    );
    sqlx::query(&sql)
        .bind(account_id)
        .bind(cache_kind)
        .bind(cache_key)
        .bind(payload)
        .execute(storage.pool())
        .await?;
    Ok(())
}
