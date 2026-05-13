//! Imports legacy Alpaca JSONL ledgers into Postgres storage.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use chrono::{NaiveDate, Utc};
use nautilus_alpaca::{options_runtime::OptionsEngineConfig, storage::StorageRepository};
use serde_json::Value;
use sqlx::types::Json;

#[derive(Clone, Debug, Default)]
struct MigrationSummary {
    candidate_files: usize,
    candidate_records: usize,
    candidate_inserted: u64,
    candidate_skipped: u64,
    performance_files: usize,
    performance_records: usize,
    performance_inserted: u64,
    performance_skipped: u64,
    outcome_files: usize,
    outcome_records: usize,
    outcome_inserted: u64,
    outcome_skipped: u64,
    parse_errors: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = OptionsEngineConfig::from_runtime_env_with_storage().await?;
    let Some(storage) = config.storage_repository.as_ref() else {
        anyhow::bail!("ALPACA_STORAGE_DATABASE_URL is required for alpaca-migrate-jsonl-storage");
    };
    let root = migration_root();
    let accounts = account_dirs(&root)?;
    if accounts.is_empty() {
        println!("jsonl_storage_migration root={} accounts=0", root.display());
        return Ok(());
    }

    let mut total = MigrationSummary::default();
    for account_dir in accounts {
        let Some(account_id) = account_dir.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let summary = migrate_account(storage, account_id, &account_dir).await?;
        total.add(&summary);
        print_summary(account_id, &account_dir, &summary);
    }
    print_summary("total", &root, &total);
    Ok(())
}

async fn migrate_account(
    storage: &StorageRepository,
    account_id: &str,
    account_dir: &Path,
) -> anyhow::Result<MigrationSummary> {
    let mut summary = MigrationSummary::default();
    migrate_candidate_ledger(
        storage,
        account_id,
        &account_dir.join("candidate-ledger"),
        &mut summary,
    )
    .await?;
    migrate_performance_ledger(
        storage,
        account_id,
        &account_dir.join("performance-ledger"),
        &mut summary,
    )
    .await?;
    migrate_candidate_outcomes(
        storage,
        account_id,
        &account_dir.join("candidate-outcomes"),
        &mut summary,
    )
    .await?;
    Ok(summary)
}

async fn migrate_candidate_ledger(
    storage: &StorageRepository,
    account_id: &str,
    directory: &Path,
    summary: &mut MigrationSummary,
) -> anyhow::Result<()> {
    for file in jsonl_files(directory)? {
        summary.candidate_files += 1;
        let file_date = ledger_file_date(&file);
        for (line_number, parsed) in read_jsonl_file(&file)?.into_iter().enumerate() {
            let mut record = match parsed {
                Ok(record) => record,
                Err(_) => {
                    summary.parse_errors += 1;
                    continue;
                }
            };
            summary.candidate_records += 1;
            normalize_account_id(&mut record, account_id);
            let Some(trade_date) = record_date(&record, "trade_date", file_date) else {
                summary.parse_errors += 1;
                continue;
            };
            let ts_utc = record
                .get("ts_utc")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| Utc::now().to_rfc3339());
            let record_type = record
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let alert_type = record
                .get("alert_type")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let severity = record
                .get("severity")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let alert_key = record
                .get("alert_key")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let migration_key = migration_key("candidate_ledger", &file, line_number, &record);
            insert_string_field(&mut record, "migration_key", migration_key.clone());

            let query = format!(
                "INSERT INTO \"{}\".candidate_ledger (account_id, trade_date, ts_utc, record_type, alert_type, severity, alert_key, migration_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5, $6, $7, $8, $9)\n         ON CONFLICT DO NOTHING",
                storage.schema()
            );
            let result = sqlx::query(&query)
                .bind(account_id)
                .bind(trade_date.to_string())
                .bind(ts_utc)
                .bind(record_type)
                .bind(alert_type)
                .bind(severity)
                .bind(alert_key)
                .bind(migration_key)
                .bind(Json(record))
                .execute(storage.pool())
                .await?;
            count_insert(summary, LedgerKind::Candidate, result.rows_affected());
        }
    }
    Ok(())
}

async fn migrate_performance_ledger(
    storage: &StorageRepository,
    account_id: &str,
    directory: &Path,
    summary: &mut MigrationSummary,
) -> anyhow::Result<()> {
    for file in jsonl_files(directory)? {
        summary.performance_files += 1;
        let file_date = ledger_file_date(&file);
        for (line_number, parsed) in read_jsonl_file(&file)?.into_iter().enumerate() {
            let mut record = match parsed {
                Ok(record) => record,
                Err(_) => {
                    summary.parse_errors += 1;
                    continue;
                }
            };
            summary.performance_records += 1;
            normalize_account_id(&mut record, account_id);
            let Some(ledger_date) = record_date(&record, "ledger_date", file_date) else {
                summary.parse_errors += 1;
                continue;
            };
            let ts_utc = record
                .get("ts_utc")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| Utc::now().to_rfc3339());
            let record_key = record
                .get("record_key")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    migration_key("performance_ledger", &file, line_number, &record)
                });
            insert_string_field(&mut record, "record_key", record_key.clone());

            let query = format!(
                "INSERT INTO \"{}\".performance_ledger (account_id, ledger_date, ts_utc, record_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5)\n         ON CONFLICT (account_id, record_key) DO NOTHING",
                storage.schema()
            );
            let result = sqlx::query(&query)
                .bind(account_id)
                .bind(ledger_date.to_string())
                .bind(ts_utc)
                .bind(record_key)
                .bind(Json(record))
                .execute(storage.pool())
                .await?;
            count_insert(summary, LedgerKind::Performance, result.rows_affected());
        }
    }
    Ok(())
}

async fn migrate_candidate_outcomes(
    storage: &StorageRepository,
    account_id: &str,
    directory: &Path,
    summary: &mut MigrationSummary,
) -> anyhow::Result<()> {
    for file in jsonl_files(directory)? {
        summary.outcome_files += 1;
        let file_date = ledger_file_date(&file);
        for (line_number, parsed) in read_jsonl_file(&file)?.into_iter().enumerate() {
            let mut record = match parsed {
                Ok(record) => record,
                Err(_) => {
                    summary.parse_errors += 1;
                    continue;
                }
            };
            summary.outcome_records += 1;
            normalize_account_id(&mut record, account_id);
            let Some(trade_date) = record_date(&record, "trade_date", file_date) else {
                summary.parse_errors += 1;
                continue;
            };
            let ts_utc = record
                .get("ts_utc")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| Utc::now().to_rfc3339());
            let record_key = record
                .get("record_key")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| migration_key("candidate_outcome", &file, line_number, &record));
            insert_string_field(&mut record, "record_key", record_key.clone());

            let query = format!(
                "INSERT INTO \"{}\".candidate_outcome (account_id, trade_date, ts_utc, record_key, payload)\n         VALUES ($1, $2::date, $3::timestamptz, $4, $5)\n         ON CONFLICT (account_id, record_key) DO NOTHING",
                storage.schema()
            );
            let result = sqlx::query(&query)
                .bind(account_id)
                .bind(trade_date.to_string())
                .bind(ts_utc)
                .bind(record_key)
                .bind(Json(record))
                .execute(storage.pool())
                .await?;
            count_insert(summary, LedgerKind::Outcome, result.rows_affected());
        }
    }
    Ok(())
}

fn account_dirs(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut accounts = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        if path.join("candidate-ledger").is_dir()
            || path.join("performance-ledger").is_dir()
            || path.join("candidate-outcomes").is_dir()
        {
            accounts.push(path);
        }
    }
    accounts.sort();
    Ok(accounts)
}

fn jsonl_files(directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn read_jsonl_file(path: &Path) -> anyhow::Result<Vec<Result<Value, serde_json::Error>>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect())
}

fn ledger_file_date(path: &Path) -> Option<NaiveDate> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

fn record_date(record: &Value, key: &str, fallback: Option<NaiveDate>) -> Option<NaiveDate> {
    record
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .or(fallback)
}

fn normalize_account_id(record: &mut Value, account_id: &str) {
    insert_string_field(record, "account_id", account_id.to_string());
}

fn insert_string_field(record: &mut Value, key: &str, value: String) {
    if let Value::Object(fields) = record {
        fields.insert(key.to_string(), Value::String(value));
    }
}

fn migration_key(kind: &str, path: &Path, line_number: usize, record: &Value) -> String {
    let stable_payload = serde_json::to_string(record).unwrap_or_default();
    format!(
        "jsonl:{kind}:{}:{}:{:016x}",
        path.display(),
        line_number + 1,
        fnv1a64(stable_payload.as_bytes())
    )
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn migration_root() -> PathBuf {
    env::var("ALPACA_JSONL_MIGRATION_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| state_home().join("nautilus_trader").join("alpaca"))
}

fn state_home() -> PathBuf {
    env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".local/state"))
}

fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Clone, Copy, Debug)]
enum LedgerKind {
    Candidate,
    Performance,
    Outcome,
}

fn count_insert(summary: &mut MigrationSummary, kind: LedgerKind, rows_affected: u64) {
    let inserted = rows_affected > 0;
    match kind {
        LedgerKind::Candidate => {
            if inserted {
                summary.candidate_inserted += 1;
            } else {
                summary.candidate_skipped += 1;
            }
        }
        LedgerKind::Performance => {
            if inserted {
                summary.performance_inserted += 1;
            } else {
                summary.performance_skipped += 1;
            }
        }
        LedgerKind::Outcome => {
            if inserted {
                summary.outcome_inserted += 1;
            } else {
                summary.outcome_skipped += 1;
            }
        }
    }
}

impl MigrationSummary {
    fn add(&mut self, value: &Self) {
        self.candidate_files += value.candidate_files;
        self.candidate_records += value.candidate_records;
        self.candidate_inserted += value.candidate_inserted;
        self.candidate_skipped += value.candidate_skipped;
        self.performance_files += value.performance_files;
        self.performance_records += value.performance_records;
        self.performance_inserted += value.performance_inserted;
        self.performance_skipped += value.performance_skipped;
        self.outcome_files += value.outcome_files;
        self.outcome_records += value.outcome_records;
        self.outcome_inserted += value.outcome_inserted;
        self.outcome_skipped += value.outcome_skipped;
        self.parse_errors += value.parse_errors;
    }
}

fn print_summary(account_id: &str, path: &Path, summary: &MigrationSummary) {
    println!(
        "jsonl_storage_migration account={} path={} candidate_files={} candidate_records={} candidate_inserted={} candidate_skipped={} performance_files={} performance_records={} performance_inserted={} performance_skipped={} outcome_files={} outcome_records={} outcome_inserted={} outcome_skipped={} parse_errors={}",
        account_id,
        path.display(),
        summary.candidate_files,
        summary.candidate_records,
        summary.candidate_inserted,
        summary.candidate_skipped,
        summary.performance_files,
        summary.performance_records,
        summary.performance_inserted,
        summary.performance_skipped,
        summary.outcome_files,
        summary.outcome_records,
        summary.outcome_inserted,
        summary.outcome_skipped,
        summary.parse_errors,
    );
}
