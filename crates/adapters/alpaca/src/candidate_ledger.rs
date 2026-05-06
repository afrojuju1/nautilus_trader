// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  You may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

//! Append-only candidate ledger records for Alpaca scanner evidence.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde_json::{Map, Value};

/// Current candidate-ledger record schema version.
pub const CANDIDATE_LEDGER_SCHEMA_VERSION: u64 = 1;

/// Returns the default candidate-ledger directory for a strategy state file.
#[must_use]
pub fn default_candidate_ledger_dir(state_path: &Path, account_id: Option<&str>) -> PathBuf {
    let parent = state_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let Some(account_id) = account_id else {
        return parent.join("candidate-ledger");
    };
    if parent.file_name().and_then(|name| name.to_str()) == Some(account_id) {
        return parent.join("candidate-ledger");
    }
    if parent.file_name().and_then(|name| name.to_str()) == Some("alpaca") {
        return parent.join(account_id).join("candidate-ledger");
    }
    parent
        .join("alpaca")
        .join(account_id)
        .join("candidate-ledger")
}

/// Appends one schema-versioned JSONL record to the candidate ledger.
///
/// # Errors
///
/// Returns an error if the ledger directory cannot be created, the ledger file cannot be opened, or
/// the JSON record cannot be serialized or written.
pub fn append_candidate_ledger_record(
    ledger_dir: &Path,
    trade_date: &str,
    account_id: Option<&str>,
    record_type: &str,
    payload: Value,
) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(ledger_dir)?;
    let ledger_path = ledger_dir.join(format!("{trade_date}.jsonl"));

    let mut record = Map::new();
    record.insert(
        "schema_version".to_string(),
        Value::from(CANDIDATE_LEDGER_SCHEMA_VERSION),
    );
    record.insert("ts_utc".to_string(), Value::String(Utc::now().to_rfc3339()));
    record.insert("type".to_string(), Value::String(record_type.to_string()));
    record.insert(
        "trade_date".to_string(),
        Value::String(trade_date.to_string()),
    );
    record.insert(
        "account_id".to_string(),
        account_id.map_or(Value::Null, |id| Value::String(id.to_string())),
    );
    if let Value::Object(fields) = payload {
        record.extend(fields);
    } else {
        record.insert("payload".to_string(), payload);
    }

    let line = serde_json::to_string(&Value::Object(record))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ledger_path)?;
    writeln!(file, "{line}")?;
    Ok(ledger_path)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::*;

    #[test]
    fn default_candidate_ledger_dir_uses_account_folder() {
        assert_eq!(
            default_candidate_ledger_dir(
                Path::new("/tmp/nautilus_trader/alpaca/paper-main/strategy_state.json"),
                Some("paper-main"),
            ),
            PathBuf::from("/tmp/nautilus_trader/alpaca/paper-main/candidate-ledger"),
        );
        assert_eq!(
            default_candidate_ledger_dir(
                Path::new("/tmp/nautilus_trader/alpaca_options_engine_state.json"),
                Some("paper-main"),
            ),
            PathBuf::from("/tmp/nautilus_trader/alpaca/paper-main/candidate-ledger"),
        );
    }

    #[test]
    fn append_candidate_ledger_record_writes_jsonl() {
        let dir =
            std::env::temp_dir().join(format!("nautilus-candidate-ledger-{}", unique_suffix(),));
        let path = append_candidate_ledger_record(
            &dir,
            "2026-05-05",
            Some("paper-main"),
            "scanner_result",
            json!({"underlying": "SPY", "result": "candidate"}),
        )
        .unwrap();

        let raw = fs::read_to_string(path).unwrap();
        let record = raw
            .lines()
            .next()
            .and_then(|line| serde_json::from_str::<Value>(line).ok())
            .unwrap();

        assert_eq!(record["schema_version"], CANDIDATE_LEDGER_SCHEMA_VERSION);
        assert_eq!(record["type"], "scanner_result");
        assert_eq!(record["trade_date"], "2026-05-05");
        assert_eq!(record["account_id"], "paper-main");
        assert_eq!(record["underlying"], "SPY");
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
