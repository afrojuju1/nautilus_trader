use std::fmt::{Display, Formatter, Result as FmtResult};

use crate::sql::pg::{
    PostgresMigrationStatus, postgres_migration_status, run_schema_migrations,
    validate_postgres_identifier,
};
use sqlx::{PgPool, postgres::PgPoolOptions};

use super::OPERATIONAL_SCHEMA_DEFAULT;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/operational");

#[derive(Debug, Clone)]
pub struct OperationalRepository {
    pool: PgPool,
    schema: String,
}

pub type OperationalMigrationStatus = PostgresMigrationStatus;

#[derive(Debug, thiserror::Error)]
#[error("invalid operational schema: {schema}")]
pub struct OperationalInitError {
    pub schema: String,
}

impl OperationalRepository {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        Self::connect_with_schema(database_url, OPERATIONAL_SCHEMA_DEFAULT).await
    }

    pub async fn connect_with_schema(database_url: &str, schema: &str) -> anyhow::Result<Self> {
        let schema = if schema.trim().is_empty() {
            OPERATIONAL_SCHEMA_DEFAULT.to_string()
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

    /// Connects to an operational schema without applying migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if schema validation or database connection fails.
    pub async fn connect_read_only_with_schema(
        database_url: &str,
        schema: &str,
    ) -> anyhow::Result<Self> {
        let schema = if schema.trim().is_empty() {
            OPERATIONAL_SCHEMA_DEFAULT.to_string()
        } else {
            validate_schema(schema)?;
            schema.to_string()
        };

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool, schema })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub async fn migration_status(&self) -> anyhow::Result<OperationalMigrationStatus> {
        postgres_migration_status(&self.pool, &self.schema).await
    }

    async fn apply_migrations(&self) -> anyhow::Result<OperationalMigrationStatus> {
        run_schema_migrations(&self.pool, &self.schema, &MIGRATOR).await
    }
}

fn validate_schema(schema: &str) -> anyhow::Result<()> {
    validate_postgres_identifier(schema, "operational schema").map_err(|_| {
        OperationalInitError {
            schema: schema.to_string(),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_valid_identifier(value: &str) -> bool {
        validate_postgres_identifier(value, "operational schema").is_ok()
    }

    #[test]
    fn rejects_invalid_identifier_chars() {
        assert!(is_valid_identifier("trading_ops"));
        assert!(!is_valid_identifier("trading-ops"));
        assert!(!is_valid_identifier("trading.ops"));
        assert!(!is_valid_identifier("trading ops"));
    }
}

impl Display for OperationalRepository {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}", self.schema)
    }
}
