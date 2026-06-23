//! ClickHouse-backed market-data warehouse utilities.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::Path,
    time::Instant,
};

use anyhow::{Context, anyhow};
use clickhouse::{Client, Row};
use serde::Deserialize;

pub const DEFAULT_CLICKHOUSE_URL: &str = "http://localhost:8123";
pub const DEFAULT_CLICKHOUSE_USERNAME: &str = "default";
pub const DEFAULT_CLICKHOUSE_DATABASE: &str = "default";
pub const DEFAULT_MIGRATIONS_DIR: &str = "schema/sql/clickhouse";

const WAREHOUSE_DATABASE: &str = "warehouse";
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ClickHouseConnectOptions {
    pub url: String,
    pub username: String,
    pub password: String,
    pub database: String,
}

impl ClickHouseConnectOptions {
    #[must_use]
    pub const fn new(url: String, username: String, password: String, database: String) -> Self {
        Self {
            url,
            username,
            password,
            database,
        }
    }

    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with_overrides(None, None, None, None)
    }

    #[must_use]
    pub fn from_env_with_overrides(
        url: Option<String>,
        username: Option<String>,
        password: Option<String>,
        database: Option<String>,
    ) -> Self {
        let url = url
            .or_else(|| env::var("CLICKHOUSE_URL").ok())
            .unwrap_or_else(|| DEFAULT_CLICKHOUSE_URL.to_string());
        let username = username
            .or_else(|| env::var("CLICKHOUSE_USER").ok())
            .unwrap_or_else(|| DEFAULT_CLICKHOUSE_USERNAME.to_string());
        let password = password
            .or_else(|| env::var("CLICKHOUSE_PASSWORD").ok())
            .unwrap_or_default();
        let database = database
            .or_else(|| env::var("CLICKHOUSE_DATABASE").ok())
            .or_else(|| env::var("CLICKHOUSE_DB").ok())
            .unwrap_or_else(|| DEFAULT_CLICKHOUSE_DATABASE.to_string());
        Self::new(url, username, password, database)
    }

    #[must_use]
    pub fn client(&self) -> Client {
        Client::default()
            .with_url(&self.url)
            .with_user(&self.username)
            .with_password(&self.password)
            .with_database(&self.database)
    }

    #[must_use]
    pub fn connection_string_masked(&self) -> String {
        format!(
            "{url}?database={database}&user={username}&password=***",
            url = self.url,
            database = self.database,
            username = self.username,
        )
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ClickHouseMigrationReport {
    pub applied: Vec<ClickHouseMigrationOutcome>,
    pub skipped: Vec<ClickHouseMigrationOutcome>,
}

impl ClickHouseMigrationReport {
    #[must_use]
    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }

    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ClickHouseMigrationOutcome {
    pub version: u32,
    pub description: String,
    pub checksum: String,
    pub statements: usize,
}

#[derive(Debug, Clone)]
struct MigrationFile {
    version: u32,
    description: String,
    checksum: String,
    statements: Vec<String>,
}

#[derive(Debug, Clone, Row, Deserialize)]
struct AppliedMigrationRow {
    version: u32,
    checksum: String,
    success: u8,
}

/// Runs all pending ClickHouse warehouse migrations from the default migrations directory.
///
/// # Errors
///
/// Returns an error if the ClickHouse connection, migration discovery, checksum validation, DDL
/// execution, or migration metadata update fails.
pub async fn run_default_migrations(
    options: &ClickHouseConnectOptions,
) -> anyhow::Result<ClickHouseMigrationReport> {
    run_migrations(options, Path::new(DEFAULT_MIGRATIONS_DIR)).await
}

/// Runs all pending ClickHouse warehouse migrations from `migrations_dir`.
///
/// # Errors
///
/// Returns an error if the ClickHouse connection, migration discovery, checksum validation, DDL
/// execution, or migration metadata update fails.
pub async fn run_migrations(
    options: &ClickHouseConnectOptions,
    migrations_dir: &Path,
) -> anyhow::Result<ClickHouseMigrationReport> {
    let client = options.client();
    ensure_migration_metadata(&client).await?;

    let applied = load_applied_migrations(&client).await?;
    let migrations = load_migration_files(migrations_dir)?;
    apply_migration_files(&client, &applied, migrations).await
}

async fn ensure_migration_metadata(client: &Client) -> anyhow::Result<()> {
    client
        .query(&format!(
            "CREATE DATABASE IF NOT EXISTS {WAREHOUSE_DATABASE}"
        ))
        .execute()
        .await?;
    client
        .query(
            r#"
CREATE TABLE IF NOT EXISTS warehouse.schema_migrations
(
    version UInt32,
    description String,
    checksum String,
    applied_at DateTime64(9, 'UTC') DEFAULT now64(9),
    execution_ms UInt64,
    success UInt8
)
ENGINE = MergeTree
ORDER BY (version, applied_at)
"#,
        )
        .execute()
        .await?;
    Ok(())
}

async fn load_applied_migrations(
    client: &Client,
) -> anyhow::Result<BTreeMap<u32, AppliedMigrationRow>> {
    let rows = client
        .query(
            r#"
SELECT
    version,
    argMax(checksum, applied_at) AS checksum,
    argMax(success, applied_at) AS success
FROM warehouse.schema_migrations
GROUP BY version
ORDER BY version
"#,
        )
        .fetch_all::<AppliedMigrationRow>()
        .await?;

    Ok(rows.into_iter().map(|row| (row.version, row)).collect())
}

async fn apply_migration_files(
    client: &Client,
    applied: &BTreeMap<u32, AppliedMigrationRow>,
    migrations: Vec<MigrationFile>,
) -> anyhow::Result<ClickHouseMigrationReport> {
    let mut report = ClickHouseMigrationReport {
        applied: Vec::new(),
        skipped: Vec::new(),
    };

    for migration in migrations {
        if let Some(row) = applied.get(&migration.version) {
            if row.checksum != migration.checksum {
                return Err(anyhow!(
                    "ClickHouse migration {} checksum drift: applied={} current={}",
                    migration.version,
                    row.checksum,
                    migration.checksum
                ));
            }
            if row.success != 1 {
                return Err(anyhow!(
                    "ClickHouse migration {} has a failed metadata record",
                    migration.version
                ));
            }
            report.skipped.push(migration.outcome());
            continue;
        }

        apply_one_migration(client, &migration).await?;
        report.applied.push(migration.outcome());
    }

    Ok(report)
}

async fn apply_one_migration(client: &Client, migration: &MigrationFile) -> anyhow::Result<()> {
    let start = Instant::now();
    let execution_result = execute_migration_statements(client, migration).await;
    let success = execution_result.is_ok();
    let execution_ms = elapsed_millis_u64(start);

    if let Err(error) = record_migration(client, migration, execution_ms, success).await {
        if let Err(execution_error) = execution_result {
            return Err(anyhow!(
                "ClickHouse migration {} failed: {execution_error}; additionally failed to record migration metadata: {error}",
                migration.version
            ));
        }
        return Err(error);
    }

    execution_result
}

async fn execute_migration_statements(
    client: &Client,
    migration: &MigrationFile,
) -> anyhow::Result<()> {
    for statement in &migration.statements {
        client
            .query(statement)
            .with_setting("wait_end_of_query", "1")
            .execute()
            .await
            .with_context(|| {
                format!(
                    "failed to execute ClickHouse migration {} statement",
                    migration.version
                )
            })?;
    }
    Ok(())
}

async fn record_migration(
    client: &Client,
    migration: &MigrationFile,
    execution_ms: u64,
    success: bool,
) -> anyhow::Result<()> {
    client
        .query(
            r#"
INSERT INTO warehouse.schema_migrations
    (version, description, checksum, execution_ms, success)
VALUES (?, ?, ?, ?, ?)
"#,
        )
        .bind(migration.version)
        .bind(migration.description.as_str())
        .bind(migration.checksum.as_str())
        .bind(execution_ms)
        .bind(u8::from(success))
        .execute()
        .await?;
    Ok(())
}

fn load_migration_files(migrations_dir: &Path) -> anyhow::Result<Vec<MigrationFile>> {
    let entries = fs::read_dir(migrations_dir).with_context(|| {
        format!(
            "failed to read ClickHouse migrations directory {}",
            migrations_dir.display()
        )
    })?;
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();

    let mut versions = BTreeSet::new();
    let mut migrations = Vec::new();
    for path in paths {
        if path.extension().and_then(|value| value.to_str()) != Some("sql") {
            continue;
        }
        let (version, description) = parse_migration_filename(&path)?;
        if !versions.insert(version) {
            return Err(anyhow!("duplicate ClickHouse migration version {version}"));
        }
        let sql = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read ClickHouse migration file {}",
                path.display()
            )
        })?;
        let statements = split_sql_statements(&sql);
        if statements.is_empty() {
            return Err(anyhow!(
                "ClickHouse migration {} has no SQL statements",
                path.display()
            ));
        }
        migrations.push(MigrationFile {
            version,
            description,
            checksum: blake3::hash(sql.as_bytes()).to_hex().to_string(),
            statements,
        });
    }

    Ok(migrations)
}

fn parse_migration_filename(path: &Path) -> anyhow::Result<(u32, String)> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("invalid ClickHouse migration filename {}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("invalid ClickHouse migration filename {file_name}"))?;
    let (version, description) = stem.split_once('_').ok_or_else(|| {
        anyhow!("migration filename must be VERSION_description.sql: {file_name}")
    })?;
    let version = version
        .parse::<u32>()
        .with_context(|| format!("invalid migration version in {file_name}"))?;
    if description.trim().is_empty() {
        return Err(anyhow!(
            "migration description must not be empty: {file_name}"
        ));
    }
    Ok((version, description.to_string()))
}

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_string = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_string = !in_string;
                current.push(c);
            }
            '-' if !in_string && chars.peek() == Some(&'-') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        current.push('\n');
                        break;
                    }
                }
            }
            ';' if !in_string => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    statements.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }

    statements
}

fn elapsed_millis_u64(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl MigrationFile {
    fn outcome(&self) -> ClickHouseMigrationOutcome {
        ClickHouseMigrationOutcome {
            version: self.version,
            description: self.description.clone(),
            checksum: self.checksum.clone(),
            statements: self.statements.len(),
        }
    }
}
