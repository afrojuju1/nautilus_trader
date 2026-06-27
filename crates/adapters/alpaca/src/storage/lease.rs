//! Runtime lease persistence in Postgres.

use std::time::Duration;

use chrono::Utc;
use sqlx::{AssertSqlSafe, Row as _};
use uuid::Uuid;

use crate::storage::StorageRepository;

#[derive(Debug, Clone)]
pub struct RuntimeLeaseRequest {
    pub holder_id: String,
    pub run_id: Uuid,
    pub service_name: Option<String>,
    pub mode: String,
    pub ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct RuntimeLeaseStatus {
    pub acquired: bool,
    pub holder_id: String,
    pub run_id: String,
    pub expires_at: String,
}

pub async fn acquire_runtime_lease(
    storage: &StorageRepository,
    account_id: &str,
    request: &RuntimeLeaseRequest,
) -> anyhow::Result<RuntimeLeaseStatus> {
    let expires_at = lease_expiry(request.ttl)?;
    let query = format!(
        "INSERT INTO \"{}\".runtime_lease \
            (account_id, holder_id, run_id, service_name, mode, acquired_at, heartbeat_at, expires_at) \
         VALUES ($1, $2, $3::uuid, $4, $5, NOW(), NOW(), $6::timestamptz) \
         ON CONFLICT (account_id) DO UPDATE \
         SET holder_id = EXCLUDED.holder_id, run_id = EXCLUDED.run_id, \
             service_name = EXCLUDED.service_name, mode = EXCLUDED.mode, \
             heartbeat_at = NOW(), expires_at = EXCLUDED.expires_at \
         WHERE \"{}\".runtime_lease.expires_at <= NOW() \
            OR \"{}\".runtime_lease.holder_id = EXCLUDED.holder_id \
            OR \"{}\".runtime_lease.run_id = EXCLUDED.run_id \
         RETURNING holder_id, run_id::text AS run_id, expires_at::text AS expires_at",
        storage.schema(),
        storage.schema(),
        storage.schema(),
        storage.schema()
    );
    let row = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(&request.holder_id)
        .bind(request.run_id.to_string())
        .bind(request.service_name.as_deref())
        .bind(&request.mode)
        .bind(&expires_at)
        .fetch_optional(storage.pool())
        .await?;

    if let Some(row) = row {
        return Ok(RuntimeLeaseStatus {
            acquired: true,
            holder_id: row.try_get("holder_id")?,
            run_id: row.try_get("run_id")?,
            expires_at: row.try_get("expires_at")?,
        });
    }

    let mut status = load_runtime_lease(storage, account_id).await?;
    status.acquired = false;
    Ok(status)
}

pub async fn heartbeat_runtime_lease(
    storage: &StorageRepository,
    account_id: &str,
    run_id: Uuid,
    ttl: Duration,
) -> anyhow::Result<bool> {
    let expires_at = lease_expiry(ttl)?;
    let query = format!(
        "UPDATE \"{}\".runtime_lease \
         SET heartbeat_at = NOW(), expires_at = $3::timestamptz \
         WHERE account_id = $1 AND run_id = $2::uuid",
        storage.schema()
    );
    let result = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(run_id.to_string())
        .bind(expires_at)
        .execute(storage.pool())
        .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn release_runtime_lease(
    storage: &StorageRepository,
    account_id: &str,
    run_id: Uuid,
) -> anyhow::Result<bool> {
    let query = format!(
        "DELETE FROM \"{}\".runtime_lease WHERE account_id = $1 AND run_id = $2::uuid",
        storage.schema()
    );
    let result = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .bind(run_id.to_string())
        .execute(storage.pool())
        .await?;
    Ok(result.rows_affected() == 1)
}

async fn load_runtime_lease(
    storage: &StorageRepository,
    account_id: &str,
) -> anyhow::Result<RuntimeLeaseStatus> {
    let query = format!(
        "SELECT holder_id, run_id::text AS run_id, expires_at::text AS expires_at \
         FROM \"{}\".runtime_lease WHERE account_id = $1",
        storage.schema()
    );
    let Some(row) = sqlx::query(AssertSqlSafe(query))
        .bind(account_id)
        .fetch_optional(storage.pool())
        .await?
    else {
        anyhow::bail!("runtime lease row was not returned for account {account_id}");
    };

    Ok(RuntimeLeaseStatus {
        acquired: false,
        holder_id: row.try_get("holder_id")?,
        run_id: row.try_get("run_id")?,
        expires_at: row.try_get("expires_at")?,
    })
}

fn lease_expiry(ttl: Duration) -> anyhow::Result<String> {
    let ttl = chrono::Duration::from_std(ttl)?;
    Ok((Utc::now() + ttl).to_rfc3339())
}
