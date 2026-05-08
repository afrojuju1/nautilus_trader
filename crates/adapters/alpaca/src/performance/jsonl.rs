//! JSONL append and scan helpers for Alpaca performance ledgers.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use chrono::NaiveDate;
use serde_json::Value;

#[derive(Clone, Debug)]
pub(super) struct JsonlAppend {
    pub(super) path: String,
    pub(super) appended: bool,
    pub(super) record_key: String,
}

#[derive(Clone, Debug, Default)]
pub(super) struct JsonlScanSummary {
    pub(super) missing: bool,
    pub(super) files: usize,
    pub(super) dates: Vec<String>,
}

pub(super) fn append_deduped_jsonl_record(
    directory: &Path,
    date: &str,
    record_key: &str,
    payload: Value,
) -> anyhow::Result<JsonlAppend> {
    fs::create_dir_all(directory)?;
    let path = directory.join(format!("{date}.jsonl"));
    if jsonl_file_has_record_key(&path, record_key)? {
        return Ok(JsonlAppend {
            path: path.display().to_string(),
            appended: false,
            record_key: record_key.to_string(),
        });
    }

    let line = serde_json::to_string(&payload)?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;

    Ok(JsonlAppend {
        path: path.display().to_string(),
        appended: true,
        record_key: record_key.to_string(),
    })
}

pub(super) fn scan_jsonl_records<F>(
    directory: &Path,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
    mut visit: F,
) -> anyhow::Result<JsonlScanSummary>
where
    F: FnMut(NaiveDate, Result<Value, serde_json::Error>),
{
    let mut summary = JsonlScanSummary::default();
    if !directory.exists() {
        summary.missing = true;
        return Ok(summary);
    }

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(date) = ledger_file_date(&path) else {
            continue;
        };
        if !date_in_range(date, since, until) {
            continue;
        }

        summary.files += 1;
        summary.dates.push(date.to_string());
        for line in fs::read_to_string(&path)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            visit(date, serde_json::from_str::<Value>(line));
        }
    }
    summary.dates.sort();
    Ok(summary)
}

pub(super) fn read_jsonl_records(path: &Path) -> anyhow::Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

pub(super) fn date_in_range(
    date: NaiveDate,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
) -> bool {
    since.is_none_or(|since| date >= since) && until.is_none_or(|until| date <= until)
}

fn jsonl_file_has_record_key(path: &Path, record_key: &str) -> anyhow::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    for line in fs::read_to_string(path)?.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("record_key").and_then(Value::as_str) == Some(record_key) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ledger_file_date(path: &Path) -> Option<NaiveDate> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}
