use std::fmt::{Display, Formatter, Result as FmtResult};

use nautilus_infrastructure::sql::pg::{
    PostgresMigrationStatus, postgres_migration_status, run_schema_migrations,
    validate_postgres_identifier,
};
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::storage::STORAGE_SCHEMA_DEFAULT;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Clone)]
pub struct StorageRepository {
    pool: PgPool,
    schema: String,
}

pub type StorageMigrationStatus = PostgresMigrationStatus;

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
        repository.apply_migrations().await?;
        Ok(repository)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub async fn migration_status(&self) -> anyhow::Result<StorageMigrationStatus> {
        postgres_migration_status(&self.pool, &self.schema).await
    }

    async fn apply_migrations(&self) -> anyhow::Result<StorageMigrationStatus> {
        run_schema_migrations(&self.pool, &self.schema, &MIGRATOR).await
    }
}

fn validate_schema(schema: &str) -> anyhow::Result<()> {
    validate_postgres_identifier(schema, "storage schema").map_err(|_| StorageInitError {
        schema: schema.to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_valid_identifier(value: &str) -> bool {
        validate_postgres_identifier(value, "storage schema").is_ok()
    }

    #[test]
    fn rejects_invalid_identifier_chars() {
        assert!(is_valid_identifier("alpaca_trader"));
        assert!(!is_valid_identifier("alpaca-trader"));
        assert!(!is_valid_identifier("alpaca.trader"));
        assert!(!is_valid_identifier("alpaca trader"));
    }
}

impl Display for StorageRepository {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}", self.schema)
    }
}
