use std::fmt::{Display, Formatter, Result as FmtResult};

use sqlx::{AssertSqlSafe, PgPool, postgres::PgPoolOptions};

use crate::storage::STORAGE_SCHEMA_DEFAULT;

#[derive(Debug, Clone)]
pub struct StorageRepository {
    pool: PgPool,
    schema: String,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid storage schema: {schema}")]
pub struct StorageInitError {
    pub schema: String,
}

impl StorageRepository {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        Self::connect_with_schema(database_url, STORAGE_SCHEMA_DEFAULT).await
    }

    pub async fn connect_with_schema(database_url: &str, schema: &str) -> anyhow::Result<Self> {
        let schema = if schema.trim().is_empty() {
            STORAGE_SCHEMA_DEFAULT.to_string()
        } else {
            validate_schema(schema)?;
            schema.to_string()
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        let repository = Self { pool, schema };
        repository.init_schema().await?;
        Ok(repository)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    async fn init_schema(&self) -> anyhow::Result<()> {
        let table_schema = &self.schema;
        let schema_sql = format!(
            r#"
CREATE SCHEMA IF NOT EXISTS "{table_schema}";

CREATE TABLE IF NOT EXISTS "{table_schema}".strategy_state (
    account_id TEXT PRIMARY KEY,
    state JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS "{table_schema}".candidate_ledger (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    trade_date DATE NOT NULL,
    ts_utc TIMESTAMPTZ NOT NULL,
    record_type TEXT NOT NULL,
    alert_type TEXT,
    severity TEXT,
    alert_key TEXT,
    migration_key TEXT,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE "{table_schema}".candidate_ledger
    ADD COLUMN IF NOT EXISTS migration_key TEXT;

CREATE INDEX IF NOT EXISTS "ix_candidate_ledger_account_date"
    ON "{table_schema}".candidate_ledger (account_id, trade_date, ts_utc);

CREATE UNIQUE INDEX IF NOT EXISTS "ux_candidate_ledger_account_migration_key"
    ON "{table_schema}".candidate_ledger (account_id, migration_key)
    WHERE migration_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS "{table_schema}".performance_ledger (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    ledger_date DATE NOT NULL,
    ts_utc TIMESTAMPTZ NOT NULL,
    record_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_performance_ledger_record_key UNIQUE (account_id, record_key)
);

CREATE INDEX IF NOT EXISTS "ix_performance_ledger_account_date"
    ON "{table_schema}".performance_ledger (account_id, ledger_date, ts_utc);

CREATE TABLE IF NOT EXISTS "{table_schema}".candidate_outcome (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    trade_date DATE NOT NULL,
    ts_utc TIMESTAMPTZ NOT NULL,
    record_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_candidate_outcome_record_key UNIQUE (account_id, record_key)
);

CREATE INDEX IF NOT EXISTS "ix_candidate_outcome_account_date"
    ON "{table_schema}".candidate_outcome (account_id, trade_date, ts_utc);

CREATE TABLE IF NOT EXISTS "{table_schema}".backtest_market_cache (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    cache_kind TEXT NOT NULL,
    cache_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_backtest_market_cache_key UNIQUE (account_id, cache_kind, cache_key)
);

CREATE INDEX IF NOT EXISTS "ix_backtest_market_cache_account_kind"
    ON "{table_schema}".backtest_market_cache (account_id, cache_kind);
"#
        );
        for statement in schema_sql
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            sqlx::query(AssertSqlSafe(statement))
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}

fn validate_schema(schema: &str) -> anyhow::Result<()> {
    if !is_valid_identifier(schema) {
        return Err(StorageInitError {
            schema: schema.to_string(),
        }
        .into());
    }
    Ok(())
}

fn is_valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|value| {
            value.is_ascii_alphanumeric() || value == '_' || value == '.' || value == '-'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_identifier_chars() {
        assert!(is_valid_identifier("alpaca_trader"));
        assert!(!is_valid_identifier("alpaca trader"));
    }
}

impl Display for StorageRepository {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}", self.schema)
    }
}
