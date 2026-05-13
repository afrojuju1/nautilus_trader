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

#[cfg(feature = "live")]
use crate::storage::StorageRepository;

use crate::strategy::{
    CreditSpreadKind, DebitSpreadCandidate, DebitSpreadKind, IronCondorCandidate,
    NakedOptionCandidate, NakedOptionKind, SpreadCandidate,
};

const CANCELED_DEBIT_REPLACEMENT_EXEMPTIONS_PER_DAY: usize = 1;

/// Persisted state for the Alpaca index credit runner.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StrategyState {
    /// Known entries for the current strategy runtime.
    #[serde(default)]
    pub entries: Vec<StrategyStateEntry>,
}

/// Draft for one newly submitted strategy-state entry.
#[derive(Clone, Debug)]
pub struct StrategyStateEntryDraft {
    /// Market trade date for the entry.
    pub trade_date: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Strategy name.
    pub strategy: String,
    /// Entry order-list/client ID.
    pub order_list_id: String,
    /// Primary short option symbol.
    pub short_symbol: String,
    /// Primary long option symbol, if any.
    pub long_symbol: String,
    /// Short call symbol for four-leg entries.
    pub short_call_symbol: Option<String>,
    /// Long call symbol for four-leg entries.
    pub long_call_symbol: Option<String>,
    /// Strategy quantity.
    pub quantity: u64,
    /// Entry credit. Debits are stored as a negative credit for legacy state readers.
    pub credit: f64,
    /// Entry debit for long-premium strategies.
    pub debit: Option<f64>,
    /// Scanner score at entry.
    pub score: f64,
    /// Broker parent order ID.
    pub parent_order_id: Option<String>,
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

    /// Returns `true` when any submitted entry exists for a trade date and underlying.
    ///
    /// This intentionally includes entries later marked closed or canceled so the entry scanner
    /// cannot churn back into the same underlying after an intraday stop-loss or terminal order.
    #[must_use]
    pub fn has_submitted_underlying_today(&self, trade_date: &str, underlying: &str) -> bool {
        self.entries.iter().any(|entry| {
            entry.submitted && entry.trade_date == trade_date && entry.underlying == underlying
        })
    }

    /// Returns the same-day submit count used by risk gates.
    ///
    /// One canceled long-premium debit entry is exempt so a no-fill directional order can be
    /// replaced once after broker reconciliation confirms it no longer represents exposure.
    #[must_use]
    pub fn risk_counted_daily_submits(&self, trade_date: &str) -> usize {
        let submitted = self
            .entries
            .iter()
            .filter(|entry| entry.submitted && entry.trade_date == trade_date)
            .count();
        let replacement_exemptions = self
            .entries
            .iter()
            .filter(|entry| {
                entry.submitted
                    && entry.trade_date == trade_date
                    && entry.is_canceled_debit_replacement_candidate()
            })
            .count()
            .min(CANCELED_DEBIT_REPLACEMENT_EXEMPTIONS_PER_DAY);

        submitted.saturating_sub(replacement_exemptions)
    }

    /// Returns `true` when a same-day underlying entry should block another entry by risk policy.
    #[must_use]
    pub fn has_risk_counted_submitted_underlying_today(
        &self,
        trade_date: &str,
        underlying: &str,
    ) -> bool {
        let submitted = self
            .entries
            .iter()
            .filter(|entry| {
                entry.submitted && entry.trade_date == trade_date && entry.underlying == underlying
            })
            .count();
        let replacement_exemptions = self
            .entries
            .iter()
            .filter(|entry| {
                entry.submitted
                    && entry.trade_date == trade_date
                    && entry.underlying == underlying
                    && entry.is_canceled_debit_replacement_candidate()
            })
            .count()
            .min(CANCELED_DEBIT_REPLACEMENT_EXEMPTIONS_PER_DAY);

        submitted.saturating_sub(replacement_exemptions) > 0
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
        self.record_entry_submission(StrategyStateEntryDraft {
            trade_date,
            underlying,
            strategy: credit_spread_strategy_name(kind).to_string(),
            order_list_id,
            short_symbol: candidate.short.symbol.clone(),
            long_symbol: candidate.long.symbol.clone(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: candidate.credit,
            debit: None,
            score: candidate.score,
            parent_order_id,
        });
    }

    /// Appends one submitted iron-condor entry to the state.
    pub fn record_iron_condor_submission(
        &mut self,
        trade_date: String,
        underlying: String,
        quantity: u64,
        order_list_id: String,
        candidate: &IronCondorCandidate,
        parent_order_id: Option<String>,
    ) {
        self.record_entry_submission(StrategyStateEntryDraft {
            trade_date,
            underlying,
            strategy: "iron_condor".to_string(),
            order_list_id,
            short_symbol: candidate.put.short.symbol.clone(),
            long_symbol: candidate.put.long.symbol.clone(),
            short_call_symbol: Some(candidate.call.short.symbol.clone()),
            long_call_symbol: Some(candidate.call.long.symbol.clone()),
            quantity,
            credit: candidate.credit,
            debit: None,
            score: candidate.score,
            parent_order_id,
        });
    }

    /// Appends one submitted long-premium debit spread entry to the state.
    pub fn record_debit_submission(
        &mut self,
        trade_date: String,
        underlying: String,
        kind: DebitSpreadKind,
        quantity: u64,
        order_list_id: String,
        candidate: &DebitSpreadCandidate,
        parent_order_id: Option<String>,
    ) {
        self.record_entry_submission(StrategyStateEntryDraft {
            trade_date,
            underlying,
            strategy: debit_spread_strategy_name(kind).to_string(),
            order_list_id,
            short_symbol: candidate.short.symbol.clone(),
            long_symbol: candidate.long.symbol.clone(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: -candidate.debit,
            debit: Some(candidate.debit),
            score: candidate.score,
            parent_order_id,
        });
    }

    /// Appends one submitted naked short option entry to the state.
    pub fn record_naked_option_submission(
        &mut self,
        trade_date: String,
        underlying: String,
        kind: NakedOptionKind,
        quantity: u64,
        order_list_id: String,
        candidate: &NakedOptionCandidate,
        parent_order_id: Option<String>,
    ) {
        self.record_entry_submission(StrategyStateEntryDraft {
            trade_date,
            underlying,
            strategy: naked_option_strategy_name(kind).to_string(),
            order_list_id,
            short_symbol: candidate.short.symbol.clone(),
            long_symbol: String::new(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: candidate.credit,
            debit: None,
            score: candidate.score,
            parent_order_id,
        });
    }

    /// Appends one submitted entry draft to the state.
    pub fn record_entry_submission(&mut self, draft: StrategyStateEntryDraft) {
        self.entries.push(StrategyStateEntry {
            trade_date: draft.trade_date,
            underlying: draft.underlying,
            strategy: draft.strategy,
            order_list_id: draft.order_list_id,
            short_symbol: draft.short_symbol,
            long_symbol: draft.long_symbol,
            short_call_symbol: draft.short_call_symbol,
            long_call_symbol: draft.long_call_symbol,
            quantity: draft.quantity,
            credit: draft.credit,
            debit: draft.debit,
            score: draft.score,
            parent_order_id: draft.parent_order_id,
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            close_attempts: 0,
            last_close_submitted_at_utc: None,
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
    /// Short call option symbol for four-leg iron condors.
    #[serde(default)]
    pub short_call_symbol: Option<String>,
    /// Long call option symbol for four-leg iron condors.
    #[serde(default)]
    pub long_call_symbol: Option<String>,
    /// Spread quantity.
    #[serde(default = "default_quantity")]
    pub quantity: u64,
    /// Entry credit.
    pub credit: f64,
    /// Entry debit for long-premium spreads.
    #[serde(default)]
    pub debit: Option<f64>,
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
    /// Number of accepted close submissions for this entry.
    #[serde(default)]
    pub close_attempts: u32,
    /// Last close submission timestamp.
    #[serde(default)]
    pub last_close_submitted_at_utc: Option<String>,
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

    /// Returns `true` when this entry stores four iron-condor legs.
    #[must_use]
    pub fn is_iron_condor(&self) -> bool {
        self.short_call_symbol.is_some() && self.long_call_symbol.is_some()
    }

    /// Returns `true` when this entry stores a long-premium debit spread.
    #[must_use]
    pub fn is_debit_spread(&self) -> bool {
        self.debit.is_some() || is_debit_strategy_name(&self.strategy)
    }

    /// Returns the entry debit for long-premium spreads.
    #[must_use]
    pub fn entry_debit(&self) -> Option<f64> {
        self.debit.or_else(|| {
            (is_debit_strategy_name(&self.strategy) && self.credit < 0.0).then_some(-self.credit)
        })
    }

    /// Returns `true` when this entry can be exempted from one same-day replacement limit.
    #[must_use]
    pub fn is_canceled_debit_replacement_candidate(&self) -> bool {
        self.canceled && !self.closed && self.is_debit_spread()
    }

    /// Returns `true` when this entry stores one naked short option.
    #[must_use]
    pub fn is_naked_option(&self) -> bool {
        self.strategy.contains("_naked_") || self.long_symbol.is_empty()
    }

    /// Returns all option symbols tracked by this entry.
    #[must_use]
    pub fn symbols(&self) -> Vec<&str> {
        let mut symbols = vec![self.short_symbol.as_str()];
        if !self.long_symbol.is_empty() {
            symbols.push(self.long_symbol.as_str());
        }
        if let Some(symbol) = self.short_call_symbol.as_deref() {
            symbols.push(symbol);
        }
        if let Some(symbol) = self.long_call_symbol.as_deref() {
            symbols.push(symbol);
        }
        symbols
    }

    /// Records a submitted close order for this entry.
    pub fn record_close_submission(
        &mut self,
        close_order_list_id: String,
        close_parent_order_id: Option<String>,
        close_reason: String,
    ) {
        self.close_order_list_id = Some(close_order_list_id);
        self.close_parent_order_id = close_parent_order_id;
        self.close_reason = Some(close_reason);
        self.close_attempts = self.close_attempts.saturating_add(1);
        self.last_close_submitted_at_utc = Some(Utc::now().to_rfc3339());
    }

    /// Clears a submitted close order so management can submit a replacement.
    pub fn clear_close_submission(&mut self) {
        self.close_order_list_id = None;
        self.close_parent_order_id = None;
        self.close_reason = None;
    }

    /// Marks the entry closed.
    pub fn mark_closed(&mut self, close_parent_order_id: Option<String>) {
        self.closed = true;
        self.close_parent_order_id = close_parent_order_id;
        self.closed_at_utc = Some(Utc::now().to_rfc3339());
    }

    /// Marks the entry canceled or terminal without open exposure.
    pub fn mark_canceled(&mut self) {
        self.canceled = true;
    }
}

/// Returns the strategy name for a credit-spread kind.
#[must_use]
pub fn credit_spread_strategy_name(kind: CreditSpreadKind) -> &'static str {
    match kind {
        CreditSpreadKind::Put => "put_credit",
        CreditSpreadKind::Call => "call_credit",
    }
}

/// Returns the strategy name for a debit-spread kind.
#[must_use]
pub fn debit_spread_strategy_name(kind: DebitSpreadKind) -> &'static str {
    match kind {
        DebitSpreadKind::Call => "call_debit",
        DebitSpreadKind::Put => "put_debit",
    }
}

/// Returns the strategy name for a naked short option kind.
#[must_use]
pub fn naked_option_strategy_name(kind: NakedOptionKind) -> &'static str {
    match kind {
        NakedOptionKind::Call => "naked_call",
        NakedOptionKind::Put => "naked_put",
        NakedOptionKind::CallOneToThreeDte => "naked_call_1_3dte",
        NakedOptionKind::PutOneToThreeDte => "naked_put_1_3dte",
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

#[cfg(feature = "live")]
pub async fn load_strategy_state_with_storage(
    _path: &Path,
    storage: &StorageRepository,
    account_id: &str,
) -> anyhow::Result<StrategyState> {
    crate::storage::load_strategy_state(storage, account_id).await
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

#[cfg(feature = "live")]
pub async fn save_strategy_state_with_storage(
    _path: &Path,
    storage: &StorageRepository,
    state: &StrategyState,
    account_id: &str,
) -> anyhow::Result<()> {
    crate::storage::save_strategy_state(storage, account_id, state).await
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

fn is_debit_strategy_name(strategy: &str) -> bool {
    matches!(strategy, "call_debit" | "put_debit")
        || strategy.contains("_debit_")
        || strategy.ends_with("_debit")
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
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: 0.46,
            debit: None,
            score: 61.9,
            parent_order_id: Some("parent-1".to_string()),
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            close_attempts: 0,
            last_close_submitted_at_utc: None,
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

    #[test]
    fn iron_condor_state_lifecycle_tracks_four_legs_and_close() {
        let mut state = StrategyState::default();
        let candidate = iron_condor_candidate();
        state.record_iron_condor_submission(
            "2026-05-04".to_string(),
            "SPY".to_string(),
            1,
            "open-list-1".to_string(),
            &candidate,
            Some("open-parent-1".to_string()),
        );

        assert!(state.has_submitted_underlying("2026-05-04", "SPY"));
        let entry = state.entries.first_mut().unwrap();
        assert!(entry.is_active());
        assert!(entry.is_iron_condor());
        assert_eq!(
            entry.symbols(),
            vec![
                "SPY260508P00710000",
                "SPY260508P00708000",
                "SPY260508C00729000",
                "SPY260508C00731000",
            ],
        );

        entry.record_close_submission(
            "close-list-1".to_string(),
            Some("close-parent-1".to_string()),
            "profit_target".to_string(),
        );
        assert!(entry.is_active());
        assert_eq!(entry.close_order_list_id.as_deref(), Some("close-list-1"));
        assert_eq!(entry.close_reason.as_deref(), Some("profit_target"));
        assert_eq!(entry.close_attempts, 1);
        assert!(entry.last_close_submitted_at_utc.is_some());

        entry.mark_closed(Some("close-parent-filled".to_string()));
        assert!(!entry.is_active());
        assert!(entry.closed);
        assert_eq!(
            entry.close_parent_order_id.as_deref(),
            Some("close-parent-filled"),
        );
        assert!(entry.closed_at_utc.is_some());
        assert!(!state.has_submitted_underlying("2026-05-04", "SPY"));
        assert!(state.has_submitted_underlying_today("2026-05-04", "SPY"));
        assert!(!state.has_submitted_underlying_today("2026-05-04", "QQQ"));
        assert!(!state.has_submitted_underlying_today("2026-05-05", "SPY"));
    }

    #[test]
    fn naked_option_state_tracks_single_short_leg() {
        let mut state = StrategyState::default();
        let candidate = crate::strategy::NakedOptionCandidate {
            short: scored_contract("SPY260508P00710000", 710.0),
            credit: 0.72,
            capital_requirement_model:
                crate::strategy::OptionCapitalRequirementModel::CashSecuredPut,
            estimated_buying_power_requirement: 71_000.0,
            buying_power_usage_pct: Some(0.071),
            return_on_buying_power: 0.001014,
            score: 75.0,
        };
        state.record_naked_option_submission(
            "2026-05-04".to_string(),
            "SPY".to_string(),
            NakedOptionKind::Put,
            1,
            "open-list-1".to_string(),
            &candidate,
            Some("open-parent-1".to_string()),
        );

        let entry = state.entries.first().unwrap();
        assert!(entry.is_active());
        assert!(entry.is_naked_option());
        assert_eq!(
            entry.strategy,
            naked_option_strategy_name(NakedOptionKind::Put)
        );
        assert_eq!(entry.symbols(), vec!["SPY260508P00710000"]);
        assert_eq!(entry.credit, 0.72);
        assert!(entry.long_symbol.is_empty());
    }

    #[test]
    fn risk_daily_submits_exempts_one_canceled_debit_replacement() {
        let mut state = StrategyState::default();
        let mut canceled_debit = debit_state_entry("open-list-1");
        canceled_debit.mark_canceled();
        state.entries.push(canceled_debit);

        assert_eq!(state.risk_counted_daily_submits("2026-05-04"), 0);
        assert!(!state.has_risk_counted_submitted_underlying_today("2026-05-04", "SLV"));
        assert!(state.has_submitted_underlying_today("2026-05-04", "SLV"));

        let mut second_canceled_debit = debit_state_entry("open-list-2");
        second_canceled_debit.mark_canceled();
        state.entries.push(second_canceled_debit);

        assert_eq!(state.risk_counted_daily_submits("2026-05-04"), 1);
        assert!(state.has_risk_counted_submitted_underlying_today("2026-05-04", "SLV"));
    }

    #[test]
    fn debit_strategy_name_recovers_legacy_negative_credit_entry_debit() {
        let mut entry = debit_state_entry("open-list-1");
        entry.debit = None;

        assert!(entry.is_debit_spread());
        assert_eq!(entry.entry_debit(), Some(0.88));
    }

    #[test]
    fn risk_daily_submits_counts_closed_credit_entries() {
        let mut state = StrategyState::default();
        let mut credit = StrategyStateEntry {
            trade_date: "2026-05-04".to_string(),
            underlying: "SPY".to_string(),
            strategy: credit_spread_strategy_name(CreditSpreadKind::Put).to_string(),
            order_list_id: "open-list-1".to_string(),
            short_symbol: "SPY260512P00708000".to_string(),
            long_symbol: "SPY260512P00705000".to_string(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: 0.50,
            debit: None,
            score: 60.0,
            parent_order_id: Some("open-parent-1".to_string()),
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            close_attempts: 0,
            last_close_submitted_at_utc: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: "2026-05-04T14:00:00Z".to_string(),
            closed_at_utc: None,
        };
        credit.mark_closed(Some("close-parent-1".to_string()));
        state.entries.push(credit);

        assert_eq!(state.risk_counted_daily_submits("2026-05-04"), 1);
        assert!(state.has_risk_counted_submitted_underlying_today("2026-05-04", "SPY"));
    }

    fn iron_condor_candidate() -> IronCondorCandidate {
        IronCondorCandidate {
            put: SpreadCandidate {
                short: scored_contract("SPY260508P00710000", 710.0),
                long: scored_contract("SPY260508P00708000", 708.0),
                width: 2.0,
                credit: 0.42,
                max_loss: 1.58,
                return_on_risk: 0.265,
                score: 70.0,
            },
            call: SpreadCandidate {
                short: scored_contract("SPY260508C00729000", 729.0),
                long: scored_contract("SPY260508C00731000", 731.0),
                width: 2.0,
                credit: 0.39,
                max_loss: 1.61,
                return_on_risk: 0.242,
                score: 68.0,
            },
            credit: 0.81,
            max_loss: 1.19,
            return_on_risk: 0.681,
            score: 94.1,
        }
    }

    fn scored_contract(symbol: &str, strike: f64) -> crate::strategy::ScoredContract {
        crate::strategy::ScoredContract {
            symbol: symbol.to_string(),
            expiration_date: "2026-05-08".to_string(),
            dte: 4,
            strike,
            bid: 1.0,
            ask: 1.1,
            delta_abs: 0.22,
            spread_pct: 0.05,
            bid_size: 10,
            ask_size: 10,
            volume: 100,
            open_interest: 1_000,
            implied_volatility: Some(0.2),
            metrics: None,
        }
    }

    fn debit_state_entry(order_list_id: &str) -> StrategyStateEntry {
        StrategyStateEntry {
            trade_date: "2026-05-04".to_string(),
            underlying: "SLV".to_string(),
            strategy: debit_spread_strategy_name(DebitSpreadKind::Call).to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: "SLV260529C00075000".to_string(),
            long_symbol: "SLV260529C00073000".to_string(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: -0.88,
            debit: Some(0.88),
            score: 74.5,
            parent_order_id: Some("open-parent-1".to_string()),
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            close_attempts: 0,
            last_close_submitted_at_utc: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: "2026-05-04T14:00:00Z".to_string(),
            closed_at_utc: None,
        }
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
