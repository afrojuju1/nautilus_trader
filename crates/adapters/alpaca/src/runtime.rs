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

//! Runtime state and operator-event helpers shared by Alpaca account-engine binaries.

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::strategy::{CreditSpreadKind, SpreadCandidate};

/// Persisted state for the Alpaca index credit runner.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StrategyState {
    /// Known entries for the current strategy runtime.
    #[serde(default)]
    pub entries: Vec<StrategyStateEntry>,
}

impl StrategyState {
    /// Returns `true` when a live entry already exists for a trade date and underlying.
    #[must_use]
    pub fn has_submitted_underlying(&self, trade_date: &str, underlying: &str) -> bool {
        self.entries.iter().any(|entry| {
            entry.submitted
                && !entry.closed
                && !entry.canceled
                && entry.trade_date == trade_date
                && entry.underlying == underlying
        })
    }

    /// Appends one submitted spread entry to the state.
    pub fn record_submission(
        &mut self,
        trade_date: String,
        underlying: String,
        kind: CreditSpreadKind,
        quantity: u64,
        order_list_id: String,
        candidate: &SpreadCandidate,
        parent_order_id: Option<String>,
    ) {
        self.entries.push(StrategyStateEntry {
            trade_date,
            underlying,
            strategy: credit_spread_strategy_name(kind).to_string(),
            order_list_id,
            short_symbol: candidate.short.symbol.clone(),
            long_symbol: candidate.long.symbol.clone(),
            quantity,
            credit: candidate.credit,
            score: candidate.score,
            parent_order_id,
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: Utc::now().to_rfc3339(),
            closed_at_utc: None,
        });
    }
}

/// Persisted state for one broker-native spread entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyStateEntry {
    /// Market trade date for the entry decision.
    pub trade_date: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Strategy name.
    #[serde(default = "default_strategy_name")]
    pub strategy: String,
    /// Nautilus order-list ID used as Alpaca parent client order ID.
    pub order_list_id: String,
    /// Short option symbol.
    pub short_symbol: String,
    /// Long option symbol.
    pub long_symbol: String,
    /// Spread quantity.
    #[serde(default = "default_quantity")]
    pub quantity: u64,
    /// Entry credit.
    pub credit: f64,
    /// Scanner score at entry.
    pub score: f64,
    /// Alpaca parent order ID for the entry.
    pub parent_order_id: Option<String>,
    /// Close order-list ID, if a close has been submitted.
    #[serde(default)]
    pub close_order_list_id: Option<String>,
    /// Alpaca parent order ID for the close.
    #[serde(default)]
    pub close_parent_order_id: Option<String>,
    /// Close trigger reason.
    #[serde(default)]
    pub close_reason: Option<String>,
    /// Whether entry submission was accepted/recorded.
    pub submitted: bool,
    /// Whether the entry was canceled or terminal without an open position.
    #[serde(default)]
    pub canceled: bool,
    /// Whether the position was closed.
    #[serde(default)]
    pub closed: bool,
    /// Entry record timestamp.
    pub recorded_at_utc: String,
    /// Close record timestamp.
    #[serde(default)]
    pub closed_at_utc: Option<String>,
}

impl StrategyStateEntry {
    /// Returns `true` when the entry still represents active broker exposure.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.submitted && !self.canceled && !self.closed
    }
}

/// Returns the strategy name for a credit-spread kind.
#[must_use]
pub fn credit_spread_strategy_name(kind: CreditSpreadKind) -> &'static str {
    match kind {
        CreditSpreadKind::Put => "index_put_credit_entry",
        CreditSpreadKind::Call => "index_call_credit_entry",
    }
}

/// Loads strategy state from a JSON file, or returns default state if the file is missing.
///
/// # Errors
///
/// Returns an error if the file cannot be read or decoded.
pub fn load_strategy_state(path: &Path) -> anyhow::Result<StrategyState> {
    if !path.exists() {
        return Ok(StrategyState::default());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

/// Saves strategy state through a same-directory temp file followed by atomic rename.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, the temp file cannot be written, or
/// the temp file cannot be renamed into place.
pub fn save_strategy_state_atomic(path: &Path, state: &StrategyState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let tmp_path = temp_state_path(path);
    fs::write(&tmp_path, serde_json::to_string_pretty(state)?)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

/// Emits one structured operator event to stdout.
pub fn emit_operator_event(event_type: &str, payload: Value) {
    let mut event = Map::new();
    event.insert("ts_utc".to_string(), Value::String(Utc::now().to_rfc3339()));
    event.insert("type".to_string(), Value::String(event_type.to_string()));
    if let Value::Object(fields) = payload {
        event.extend(fields);
    }
    if let Ok(line) = serde_json::to_string(&Value::Object(event)) {
        println!("operator_event={line}");
    }
}

/// Reads structured operator events from a log file.
#[must_use]
pub fn read_operator_events(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .map(|contents| {
            contents
                .lines()
                .filter_map(|line| line.strip_prefix("operator_event="))
                .filter_map(|value| serde_json::from_str(value).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn temp_state_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("strategy_state.json");
    let tmp_name = format!(".{file_name}.tmp-{}", std::process::id());
    path.with_file_name(tmp_name)
}

fn default_strategy_name() -> String {
    credit_spread_strategy_name(CreditSpreadKind::Put).to_string()
}

fn default_quantity() -> u64 {
    1
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn strategy_state_atomic_save_round_trips() {
        let mut state = StrategyState::default();
        state.entries.push(StrategyStateEntry {
            trade_date: "2026-05-02".to_string(),
            underlying: "SPY".to_string(),
            strategy: credit_spread_strategy_name(CreditSpreadKind::Put).to_string(),
            order_list_id: "order-list-1".to_string(),
            short_symbol: "SPY260512P00708000".to_string(),
            long_symbol: "SPY260512P00705000".to_string(),
            quantity: 1,
            credit: 0.46,
            score: 61.9,
            parent_order_id: Some("parent-1".to_string()),
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: "2026-05-02T19:30:29Z".to_string(),
            closed_at_utc: None,
        });

        let path = std::env::temp_dir()
            .join(format!("nautilus-alpaca-runtime-{}", unique_suffix()))
            .join("state.json");
        save_strategy_state_atomic(&path, &state).unwrap();
        let loaded = load_strategy_state(&path).unwrap();

        assert_eq!(loaded.entries.len(), 1);
        assert!(loaded.entries[0].is_active());
        assert!(loaded.has_submitted_underlying("2026-05-02", "SPY"));
        assert!(!temp_state_path(&path).exists());
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
