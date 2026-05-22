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

//! Alpaca options performance accounting from local strategy state and broker fills.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::Serialize;

use crate::{
    http::models::{AlpacaActivity, AlpacaPosition},
    runtime::StrategyStateEntry,
};

mod candidate_outcomes;

pub use candidate_outcomes::{
    collect_order_ids, earliest_entry_timestamp, entry_in_date_range, entry_performance,
    performance_record_key, summarize_performance, track_candidate_outcomes,
};

/// Standard OCC equity-option contract multiplier.
pub const OPTION_CONTRACT_MULTIPLIER: f64 = 100.0;

/// Current performance-ledger record schema version.
pub const PERFORMANCE_LEDGER_SCHEMA_VERSION: u64 = 1;

/// Default maximum number of candidates to track per candidate-ledger scan result.
pub const DEFAULT_CANDIDATE_OUTCOME_MAX_CANDIDATES: usize = 100;

/// Default maximum candidate rank to include in candidate-outcome tracking.
pub const DEFAULT_CANDIDATE_OUTCOME_MAX_RANK: u64 = 3;

/// Candidate-ledger counts used to audit opportunity history.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CandidateLedgerSummary {
    /// Whether the candidate-ledger storage is missing.
    pub missing: bool,
    /// Candidate-ledger storage location.
    pub directory: String,
    /// Number of trade dates included.
    pub files: usize,
    /// Trade dates included.
    pub dates: Vec<String>,
    /// Total records across included files.
    pub records: usize,
    /// Scanner opportunity records.
    pub candidates: usize,
    /// Scanner diagnostic records.
    pub scanner_results: usize,
    /// Decision records.
    pub decisions: usize,
    /// Submit-result records.
    pub submit_results: usize,
    /// Candidate-alert records.
    pub candidate_alerts: usize,
    /// Selected candidate-alert records.
    pub selected_candidates: usize,
    /// High-score candidate-alert records.
    pub high_score_candidates: usize,
    /// Parse errors encountered while reading ledger records.
    pub parse_errors: usize,
    /// Record counts by `type`.
    pub by_type: BTreeMap<String, usize>,
    /// Candidate counts by strategy.
    pub candidates_by_strategy: BTreeMap<String, usize>,
}

/// Candidate-outcome counts used to evaluate opportunities after observation.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CandidateOutcomeSummary {
    /// Whether the candidate-outcome storage is missing.
    pub missing: bool,
    /// Candidate-outcome storage location.
    pub directory: String,
    /// Number of trade dates included.
    pub files: usize,
    /// Trade dates included.
    pub dates: Vec<String>,
    /// Total candidate-outcome records.
    pub records: usize,
    /// Candidate-outcome records for candidates selected by the strategy.
    pub selected_records: usize,
    /// Candidate-outcome records for candidates submitted to the broker.
    pub traded_records: usize,
    /// Winning hypothetical outcomes.
    pub wins: usize,
    /// Losing hypothetical outcomes.
    pub losses: usize,
    /// Flat hypothetical outcomes.
    pub flats: usize,
    /// Hypothetical PnL in dollars.
    pub hypothetical_pnl: f64,
    /// Average winning hypothetical outcome in dollars.
    pub average_win: Option<f64>,
    /// Average losing hypothetical outcome in dollars.
    pub average_loss: Option<f64>,
    /// Largest losing hypothetical outcome in dollars.
    pub largest_loss: Option<f64>,
    /// Candidate-outcome records for candidates submitted to the broker.
    pub submitted_records: usize,
    /// Candidate-outcome records rejected by the broker.
    pub rejected_records: usize,
    /// Candidate-outcome records for selected candidates not traded at the broker.
    pub virtual_records: usize,
    /// First virtual close records for selected candidates not traded at the broker.
    pub virtual_close_records: usize,
    /// Parse errors encountered while reading outcome records.
    pub parse_errors: usize,
    /// Records containing quote warnings.
    pub records_with_warnings: usize,
    /// Selected-candidate outcome summary.
    pub selected: CandidateOutcomeBucketSummary,
    /// Submitted-candidate outcome summary.
    pub submitted: CandidateOutcomeBucketSummary,
    /// Broker-rejected candidate outcome summary.
    pub rejected: CandidateOutcomeBucketSummary,
    /// Virtual trade outcome summary for dry-run, blocked, or rejected selections.
    pub virtual_trades: CandidateOutcomeBucketSummary,
    /// First virtual close outcome summary.
    pub virtual_closes: CandidateOutcomeBucketSummary,
    /// Summaries by observation bucket.
    pub by_bucket: BTreeMap<String, CandidateOutcomeBucketSummary>,
    /// Summaries by strategy.
    pub by_strategy: BTreeMap<String, CandidateOutcomeBucketSummary>,
    /// Summaries by virtual close reason.
    pub by_virtual_close_reason: BTreeMap<String, CandidateOutcomeBucketSummary>,
}

/// Bucketed candidate-outcome performance summary.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CandidateOutcomeBucketSummary {
    /// Total candidate-outcome records.
    pub records: usize,
    /// Candidate-outcome records for selected candidates.
    pub selected_records: usize,
    /// Candidate-outcome records for submitted candidates.
    pub traded_records: usize,
    /// Candidate-outcome records for broker submission attempts.
    pub submitted_records: usize,
    /// Candidate-outcome records rejected by the broker.
    pub rejected_records: usize,
    /// Candidate-outcome records for selected candidates not traded at the broker.
    pub virtual_records: usize,
    /// First virtual close records for selected candidates not traded at the broker.
    pub virtual_close_records: usize,
    /// Winning hypothetical outcomes.
    pub wins: usize,
    /// Losing hypothetical outcomes.
    pub losses: usize,
    /// Flat hypothetical outcomes.
    pub flats: usize,
    /// Hypothetical PnL in dollars.
    pub hypothetical_pnl: f64,
    /// Average winning hypothetical outcome in dollars.
    pub average_win: Option<f64>,
    /// Average losing hypothetical outcome in dollars.
    pub average_loss: Option<f64>,
    /// Largest losing hypothetical outcome in dollars.
    pub largest_loss: Option<f64>,
}

/// Candidate-outcome tracking request.
#[derive(Clone, Debug)]
pub struct CandidateOutcomeTrackingRequest {
    /// Trade date to track. `None` uses today's date in the account entry timezone.
    pub trade_date: Option<NaiveDate>,
    /// Maximum number of unique candidates to observe.
    pub max_candidates: usize,
    /// Maximum candidate rank to include.
    pub max_rank: u64,
}

impl Default for CandidateOutcomeTrackingRequest {
    fn default() -> Self {
        Self {
            trade_date: None,
            max_candidates: DEFAULT_CANDIDATE_OUTCOME_MAX_CANDIDATES,
            max_rank: DEFAULT_CANDIDATE_OUTCOME_MAX_RANK,
        }
    }
}

/// Order identifiers associated with one strategy entry.
#[derive(Clone, Debug, Default)]
pub struct EntryOrderIds {
    /// Opening order IDs.
    pub open: BTreeSet<String>,
    /// Closing order IDs.
    pub close: BTreeSet<String>,
}

/// Fill cashflow aggregation for one side of a strategy lifecycle.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FillSummary {
    /// Number of usable fill activities.
    pub fills: usize,
    /// Net cashflow in dollars. Sells are positive; buys are negative.
    pub cashflow: Option<f64>,
    /// Total contracts filled across activities.
    pub quantity: f64,
    /// Earliest fill timestamp.
    pub first_transaction_time: Option<String>,
    /// Latest fill timestamp.
    pub last_transaction_time: Option<String>,
    /// Fill counts by symbol.
    pub symbols: BTreeMap<String, usize>,
}

/// Performance status for one strategy-state entry.
#[derive(Clone, Debug, Serialize)]
pub struct EntryPerformance {
    /// Market trade date for the entry decision.
    pub trade_date: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Strategy name.
    pub strategy: String,
    /// Entry lifecycle status.
    pub status: String,
    /// Option symbols tracked by the entry.
    pub symbols: Vec<String>,
    /// Strategy quantity.
    pub quantity: u64,
    /// Entry record timestamp.
    pub recorded_at_utc: String,
    /// Close record timestamp.
    pub closed_at_utc: Option<String>,
    /// Scanner score at entry.
    pub score: f64,
    /// Quoted entry premium per spread or option. Credits are positive; debits are negative.
    pub quoted_entry_premium: Option<f64>,
    /// Quoted entry cashflow in dollars.
    pub quoted_entry_cashflow: Option<f64>,
    /// Opening fill summary.
    pub open: FillSummary,
    /// Closing fill summary.
    pub close: FillSummary,
    /// Realized PnL in dollars, when both opening and closing fills are available.
    pub realized_pnl: Option<f64>,
    /// Broker-reported open unrealized PnL in dollars, when the entry is active.
    pub open_unrealized_pnl: Option<f64>,
    /// Close trigger reason stored in strategy state.
    pub close_reason: Option<String>,
    /// Entry parent order ID.
    pub parent_order_id: Option<String>,
    /// Close parent order ID.
    pub close_parent_order_id: Option<String>,
    /// Entry order-list/client ID.
    pub order_list_id: String,
    /// Close order-list/client ID.
    pub close_order_list_id: Option<String>,
    /// Data gaps or accounting warnings for this entry.
    pub warnings: Vec<String>,
}

/// Aggregate PnL by strategy.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StrategyPerformanceSummary {
    /// Total entries.
    pub entries: usize,
    /// Active entries.
    pub active_entries: usize,
    /// Closed entries.
    pub closed_entries: usize,
    /// Canceled entries.
    pub canceled_entries: usize,
    /// Closed entries with realized PnL reconstructed from fills.
    pub realized_entries: usize,
    /// Closed entries without enough fill data for realized PnL.
    pub missing_realized_entries: usize,
    /// Reconstructed realized PnL in dollars.
    pub realized_pnl: f64,
    /// Broker-reported open unrealized PnL in dollars.
    pub open_unrealized_pnl: f64,
    /// Realized plus open unrealized PnL in dollars.
    pub observed_total_pnl: f64,
}

/// Aggregate PnL for the report.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PerformanceSummary {
    /// Total entries.
    pub entries: usize,
    /// Active entries.
    pub active_entries: usize,
    /// Closed entries.
    pub closed_entries: usize,
    /// Canceled entries.
    pub canceled_entries: usize,
    /// Closed entries with realized PnL reconstructed from fills.
    pub realized_entries: usize,
    /// Closed entries without enough fill data for realized PnL.
    pub missing_realized_entries: usize,
    /// Reconstructed realized PnL in dollars.
    pub realized_pnl: f64,
    /// Broker-reported open unrealized PnL in dollars.
    pub open_unrealized_pnl: f64,
    /// Realized plus open unrealized PnL in dollars.
    pub observed_total_pnl: f64,
    /// Aggregate PnL by strategy.
    pub by_strategy: BTreeMap<String, StrategyPerformanceSummary>,
}

/// Full Alpaca options performance report.
#[derive(Clone, Debug, Serialize)]
pub struct PerformanceReport {
    /// Report timestamp.
    pub checked_at_utc: String,
    /// Fleet account ID, when configured.
    pub account_id: Option<String>,
    /// Strategy state path.
    pub state_path: String,
    /// Candidate-ledger opportunity summary.
    pub opportunities: CandidateLedgerSummary,
    /// Immutable realized-trade ledger summary.
    pub ledger_summary: PerformanceLedgerSummary,
    /// Candidate-outcome summary for tracked opportunities.
    pub candidate_outcomes: CandidateOutcomeSummary,
    /// Aggregate PnL summary.
    pub summary: PerformanceSummary,
    /// Per-entry accounting rows.
    pub entries: Vec<EntryPerformance>,
    /// Report-level warnings.
    pub warnings: Vec<String>,
}

/// Result from appending a performance-ledger record.
#[derive(Clone, Debug, Serialize)]
pub struct PerformanceLedgerAppend {
    /// Storage path for the ledger record.
    pub path: String,
    /// Whether a new line was appended.
    pub appended: bool,
    /// Stable key used to dedupe close records.
    pub record_key: String,
}

/// Aggregate realized-trade performance from immutable performance-ledger records.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PerformanceLedgerSummary {
    /// Whether the performance-ledger storage is missing.
    pub missing: bool,
    /// Performance-ledger storage location.
    pub directory: String,
    /// Number of ledger dates included.
    pub files: usize,
    /// Ledger dates included.
    pub dates: Vec<String>,
    /// Total realized-trade records.
    pub records: usize,
    /// Winning trades.
    pub wins: usize,
    /// Losing trades.
    pub losses: usize,
    /// Flat trades.
    pub flats: usize,
    /// Realized PnL in dollars.
    pub realized_pnl: f64,
    /// Average winning trade in dollars.
    pub average_win: Option<f64>,
    /// Average losing trade in dollars.
    pub average_loss: Option<f64>,
    /// Largest losing trade in dollars.
    pub largest_loss: Option<f64>,
    /// Parse errors encountered while reading ledger records.
    pub parse_errors: usize,
    /// Records containing warnings.
    pub records_with_warnings: usize,
    /// Summaries by strategy.
    pub by_strategy: BTreeMap<String, PerformanceLedgerBucketSummary>,
    /// Summaries by underlying.
    pub by_underlying: BTreeMap<String, PerformanceLedgerBucketSummary>,
}

/// Bucketed realized-trade performance summary.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PerformanceLedgerBucketSummary {
    /// Total realized-trade records.
    pub records: usize,
    /// Winning trades.
    pub wins: usize,
    /// Losing trades.
    pub losses: usize,
    /// Flat trades.
    pub flats: usize,
    /// Realized PnL in dollars.
    pub realized_pnl: f64,
    /// Average winning trade in dollars.
    pub average_win: Option<f64>,
    /// Average losing trade in dollars.
    pub average_loss: Option<f64>,
    /// Largest losing trade in dollars.
    pub largest_loss: Option<f64>,
}

fn apply_entry_summary(
    entry: &EntryPerformance,
    active_entries: &mut usize,
    closed_entries: &mut usize,
    canceled_entries: &mut usize,
) {
    match entry.status.as_str() {
        "active" => *active_entries += 1,
        "closed" => *closed_entries += 1,
        "canceled" => *canceled_entries += 1,
        _ => {}
    }
}

fn entry_symbols(entry: &StrategyStateEntry) -> BTreeSet<String> {
    entry
        .symbols()
        .into_iter()
        .filter(|symbol| !symbol.trim().is_empty())
        .map(ToString::to_string)
        .collect()
}

fn fill_summary(
    activities: &[AlpacaActivity],
    order_ids: &BTreeSet<String>,
    symbols: &BTreeSet<String>,
) -> FillSummary {
    let mut summary = FillSummary::default();
    for activity in activities {
        if !activity_matches(activity, order_ids, symbols) {
            continue;
        }
        let Some(cashflow) = activity_cashflow(activity) else {
            continue;
        };
        summary.fills += 1;
        summary.cashflow = Some(summary.cashflow.unwrap_or_default() + cashflow);
        summary.quantity += activity
            .qty
            .as_deref()
            .and_then(parse_f64)
            .unwrap_or_default();
        if let Some(symbol) = activity.symbol.as_ref() {
            *summary.symbols.entry(symbol.clone()).or_insert(0) += 1;
        }
        if let Some(transaction_time) = activity.transaction_time.as_ref() {
            if summary
                .first_transaction_time
                .as_ref()
                .is_none_or(|current| transaction_time < current)
            {
                summary.first_transaction_time = Some(transaction_time.clone());
            }
            if summary
                .last_transaction_time
                .as_ref()
                .is_none_or(|current| transaction_time > current)
            {
                summary.last_transaction_time = Some(transaction_time.clone());
            }
        }
    }
    summary
}

fn activity_matches(
    activity: &AlpacaActivity,
    order_ids: &BTreeSet<String>,
    symbols: &BTreeSet<String>,
) -> bool {
    if order_ids.is_empty() || !activity_is_trade_fill(activity) {
        return false;
    }
    activity
        .order_id
        .as_ref()
        .is_some_and(|order_id| order_ids.contains(order_id))
        && activity
            .symbol
            .as_ref()
            .is_some_and(|symbol| symbols.contains(symbol))
}

fn activity_is_trade_fill(activity: &AlpacaActivity) -> bool {
    let activity_type = activity
        .activity_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let subtype = activity
        .activity_subtype
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(activity_type.as_str(), "fill" | "optrd")
        || matches!(subtype.as_str(), "fill" | "optrd")
}

fn activity_cashflow(activity: &AlpacaActivity) -> Option<f64> {
    let qty = activity.qty.as_deref().and_then(parse_f64)?;
    let price = activity.price.as_deref().and_then(parse_f64)?;
    let side = activity.side.as_deref()?.to_ascii_lowercase();
    let notional = qty.abs() * price * OPTION_CONTRACT_MULTIPLIER;
    if side.starts_with("buy") || side == "b" {
        Some(-notional)
    } else if side.starts_with("sell") || side == "s" {
        Some(notional)
    } else {
        None
    }
}

fn open_unrealized_pnl(entry: &StrategyStateEntry, positions: &[AlpacaPosition]) -> Option<f64> {
    let symbols = entry_symbols(entry);
    let mut found = false;
    let mut total = 0.0;
    for position in positions {
        let Some(symbol) = position.symbol.as_ref() else {
            continue;
        };
        if !symbols.contains(symbol) {
            continue;
        }
        if let Some(unrealized_pl) = position.unrealized_pl.as_deref().and_then(parse_f64) {
            found = true;
            total += unrealized_pl;
        }
    }
    found.then_some(total)
}

fn quoted_entry_premium(entry: &StrategyStateEntry) -> Option<f64> {
    if entry.is_debit_spread() {
        entry.entry_debit().map(|debit| -debit)
    } else {
        Some(entry.credit)
    }
}

fn entry_status(entry: &StrategyStateEntry) -> &'static str {
    if entry.closed {
        "closed"
    } else if entry.canceled {
        "canceled"
    } else if entry.is_active() {
        "active"
    } else {
        "unknown"
    }
}

fn parse_f64(value: &str) -> Option<f64> {
    value.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_performance_reconstructs_closed_credit_spread_pnl() {
        let entry = state_entry();
        let order_ids = EntryOrderIds {
            open: BTreeSet::from(["open-short".to_string(), "open-long".to_string()]),
            close: BTreeSet::from(["close-short".to_string(), "close-long".to_string()]),
        };
        let activities = vec![
            activity(
                "FILL",
                "open-short",
                "SPY260515C00720000",
                "sell",
                "1",
                "0.55",
            ),
            activity(
                "FILL",
                "open-long",
                "SPY260515C00722000",
                "buy",
                "1",
                "0.15",
            ),
            activity(
                "FILL",
                "close-short",
                "SPY260515C00720000",
                "buy",
                "1",
                "1.10",
            ),
            activity(
                "FILL",
                "close-long",
                "SPY260515C00722000",
                "sell",
                "1",
                "0.20",
            ),
        ];

        let performance = entry_performance(&entry, &order_ids, &activities, &[]);

        assert_close(performance.open.cashflow.unwrap(), 40.0);
        assert_close(performance.close.cashflow.unwrap(), -90.0);
        assert_close(performance.realized_pnl.unwrap(), -50.0);
        assert!(performance.warnings.is_empty());
    }

    #[test]
    fn entry_performance_reconstructs_closed_iron_condor_pnl() {
        let entry = iron_condor_state_entry();
        let order_ids = EntryOrderIds {
            open: BTreeSet::from([
                "open-short-put".to_string(),
                "open-long-put".to_string(),
                "open-short-call".to_string(),
                "open-long-call".to_string(),
            ]),
            close: BTreeSet::from([
                "close-short-put".to_string(),
                "close-long-put".to_string(),
                "close-short-call".to_string(),
                "close-long-call".to_string(),
            ]),
        };
        let activities = vec![
            activity(
                "FILL",
                "open-short-put",
                "GLD260515P00410000",
                "sell",
                "1",
                "0.55",
            ),
            activity(
                "FILL",
                "open-long-put",
                "GLD260515P00405000",
                "buy",
                "1",
                "0.18",
            ),
            activity(
                "FILL",
                "open-short-call",
                "GLD260515C00430000",
                "sell",
                "1",
                "2.21",
            ),
            activity(
                "FILL",
                "open-long-call",
                "GLD260515C00435000",
                "buy",
                "1",
                "0.64",
            ),
            activity(
                "FILL",
                "close-short-put",
                "GLD260515P00410000",
                "buy",
                "1",
                "0.07",
            ),
            activity(
                "FILL",
                "close-long-put",
                "GLD260515P00405000",
                "sell",
                "1",
                "0.02",
            ),
            activity(
                "FILL",
                "close-short-call",
                "GLD260515C00430000",
                "buy",
                "1",
                "4.76",
            ),
            activity(
                "FILL",
                "close-long-call",
                "GLD260515C00435000",
                "sell",
                "1",
                "0.94",
            ),
        ];

        let performance = entry_performance(&entry, &order_ids, &activities, &[]);

        assert_close(performance.open.cashflow.unwrap(), 194.0);
        assert_close(performance.close.cashflow.unwrap(), -387.0);
        assert_close(performance.realized_pnl.unwrap(), -193.0);
        assert_eq!(performance.quoted_entry_premium, Some(1.94));
        assert_eq!(performance.quoted_entry_cashflow, Some(194.0));
        assert!(performance.warnings.is_empty());
    }

    fn state_entry() -> StrategyStateEntry {
        StrategyStateEntry {
            trade_date: "2026-05-07".to_string(),
            underlying: "SPY".to_string(),
            strategy: "call_credit".to_string(),
            order_list_id: "entry-list".to_string(),
            short_symbol: "SPY260515C00720000".to_string(),
            long_symbol: "SPY260515C00722000".to_string(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: 0.40,
            debit: None,
            score: 72.5,
            parent_order_id: Some("open-parent".to_string()),
            close_order_list_id: Some("close-list".to_string()),
            close_parent_order_id: Some("close-parent".to_string()),
            close_reason: Some("stop_loss".to_string()),
            close_attempts: 1,
            last_close_submitted_at_utc: Some("2026-05-08T14:01:00Z".to_string()),
            submitted: true,
            canceled: false,
            closed: true,
            recorded_at_utc: "2026-05-07T14:00:00Z".to_string(),
            closed_at_utc: Some("2026-05-08T14:02:00Z".to_string()),
        }
    }

    fn iron_condor_state_entry() -> StrategyStateEntry {
        StrategyStateEntry {
            trade_date: "2026-05-05".to_string(),
            underlying: "GLD".to_string(),
            strategy: "iron_condor".to_string(),
            order_list_id: "entry-list".to_string(),
            short_symbol: "GLD260515P00410000".to_string(),
            long_symbol: "GLD260515P00405000".to_string(),
            short_call_symbol: Some("GLD260515C00430000".to_string()),
            long_call_symbol: Some("GLD260515C00435000".to_string()),
            quantity: 1,
            credit: 1.94,
            debit: None,
            score: 70.4,
            parent_order_id: Some("open-parent".to_string()),
            close_order_list_id: Some("close-list".to_string()),
            close_parent_order_id: Some("close-parent".to_string()),
            close_reason: Some("stop_loss".to_string()),
            close_attempts: 1,
            last_close_submitted_at_utc: Some("2026-05-08T13:31:00Z".to_string()),
            submitted: true,
            canceled: false,
            closed: true,
            recorded_at_utc: "2026-05-05T15:56:44Z".to_string(),
            closed_at_utc: Some("2026-05-08T13:36:49Z".to_string()),
        }
    }

    fn activity(
        activity_type: &str,
        order_id: &str,
        symbol: &str,
        side: &str,
        qty: &str,
        price: &str,
    ) -> AlpacaActivity {
        AlpacaActivity {
            activity_type: Some(activity_type.to_string()),
            id: Some(format!("{order_id}-{symbol}")),
            cum_qty: Some(qty.to_string()),
            leaves_qty: Some("0".to_string()),
            price: Some(price.to_string()),
            qty: Some(qty.to_string()),
            side: Some(side.to_string()),
            symbol: Some(symbol.to_string()),
            transaction_time: Some("2026-05-07T14:00:00Z".to_string()),
            order_id: Some(order_id.to_string()),
            activity_subtype: None,
            date: None,
            net_amount: None,
            cusip: None,
            per_share_amount: None,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000_001,
            "actual={actual} expected={expected}"
        );
    }
}
