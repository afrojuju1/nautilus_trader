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

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{
    http::models::{AlpacaActivity, AlpacaOrder, AlpacaPosition},
    runtime::{StrategyState, StrategyStateEntry},
};

/// Standard OCC equity-option contract multiplier.
pub const OPTION_CONTRACT_MULTIPLIER: f64 = 100.0;

/// Current performance-ledger record schema version.
pub const PERFORMANCE_LEDGER_SCHEMA_VERSION: u64 = 1;

/// Candidate-ledger counts used to audit opportunity history.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CandidateLedgerSummary {
    /// Whether the candidate-ledger directory is missing.
    pub missing: bool,
    /// Candidate-ledger directory.
    pub directory: String,
    /// Number of JSONL files included.
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
    /// JSON parse errors encountered while reading ledger lines.
    pub parse_errors: usize,
    /// Record counts by `type`.
    pub by_type: BTreeMap<String, usize>,
    /// Candidate counts by strategy.
    pub candidates_by_strategy: BTreeMap<String, usize>,
}

/// Candidate-outcome counts used to evaluate opportunities after observation.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CandidateOutcomeSummary {
    /// Whether the candidate-outcome directory is missing.
    pub missing: bool,
    /// Candidate-outcome directory.
    pub directory: String,
    /// Number of JSONL files included.
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
    /// JSON parse errors encountered while reading outcome lines.
    pub parse_errors: usize,
    /// Records containing quote warnings.
    pub records_with_warnings: usize,
    /// Summaries by observation bucket.
    pub by_bucket: BTreeMap<String, CandidateOutcomeBucketSummary>,
    /// Summaries by strategy.
    pub by_strategy: BTreeMap<String, CandidateOutcomeBucketSummary>,
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
    /// Candidate-ledger directory.
    pub candidate_ledger_dir: String,
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
    /// File path for the ledger date.
    pub path: String,
    /// Whether a new line was appended.
    pub appended: bool,
    /// Stable key used to dedupe close records.
    pub record_key: String,
}

/// Aggregate realized-trade performance from immutable performance-ledger files.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PerformanceLedgerSummary {
    /// Whether the performance-ledger directory is missing.
    pub missing: bool,
    /// Performance-ledger directory.
    pub directory: String,
    /// Number of JSONL files included.
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
    /// JSON parse errors encountered while reading ledger lines.
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

/// Returns the default performance-ledger directory for a strategy state file.
#[must_use]
pub fn default_performance_ledger_dir(state_path: &Path, account_id: Option<&str>) -> PathBuf {
    let parent = state_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let Some(account_id) = account_id else {
        return parent.join("performance-ledger");
    };
    if parent.file_name().and_then(|name| name.to_str()) == Some(account_id) {
        return parent.join("performance-ledger");
    }
    if parent.file_name().and_then(|name| name.to_str()) == Some("alpaca") {
        return parent.join(account_id).join("performance-ledger");
    }
    parent
        .join("alpaca")
        .join(account_id)
        .join("performance-ledger")
}

/// Appends one realized-trade performance record to an append-only JSONL ledger.
///
/// If the same record key already exists in the target daily file, no duplicate line is written.
///
/// # Errors
///
/// Returns an error if the ledger directory cannot be created, an existing file cannot be read,
/// or the record cannot be serialized or written.
pub fn append_performance_ledger_record(
    ledger_dir: &Path,
    ledger_date: &str,
    account_id: Option<&str>,
    entry: &EntryPerformance,
) -> anyhow::Result<PerformanceLedgerAppend> {
    fs::create_dir_all(ledger_dir)?;
    let ledger_path = ledger_dir.join(format!("{ledger_date}.jsonl"));
    let record_key = performance_record_key(entry);

    if performance_ledger_has_key(&ledger_path, &record_key)? {
        return Ok(PerformanceLedgerAppend {
            path: ledger_path.display().to_string(),
            appended: false,
            record_key,
        });
    }

    let mut record = Map::new();
    record.insert(
        "schema_version".to_string(),
        Value::from(PERFORMANCE_LEDGER_SCHEMA_VERSION),
    );
    record.insert("ts_utc".to_string(), Value::String(Utc::now().to_rfc3339()));
    record.insert(
        "type".to_string(),
        Value::String("realized_trade".to_string()),
    );
    record.insert(
        "ledger_date".to_string(),
        Value::String(ledger_date.to_string()),
    );
    record.insert(
        "account_id".to_string(),
        account_id.map_or(Value::Null, |id| Value::String(id.to_string())),
    );
    record.insert("record_key".to_string(), Value::String(record_key.clone()));
    if let Value::Object(fields) = serde_json::to_value(entry)? {
        record.extend(fields);
    }

    let line = serde_json::to_string(&Value::Object(record))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ledger_path)?;
    writeln!(file, "{line}")?;

    Ok(PerformanceLedgerAppend {
        path: ledger_path.display().to_string(),
        appended: true,
        record_key,
    })
}

/// Summarizes immutable performance-ledger JSONL files for an optional date range.
///
/// # Errors
///
/// Returns an error if a ledger directory entry or file cannot be read.
pub fn summarize_performance_ledger(
    directory: &Path,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
) -> anyhow::Result<PerformanceLedgerSummary> {
    let mut summary = PerformanceLedgerSummary {
        directory: directory.display().to_string(),
        ..PerformanceLedgerSummary::default()
    };
    if !directory.exists() {
        summary.missing = true;
        return Ok(summary);
    }

    let mut win_sum = 0.0;
    let mut loss_sum = 0.0;
    let mut bucket_stats = BucketStats::default();
    let mut strategy_stats = BTreeMap::<String, BucketStats>::new();
    let mut underlying_stats = BTreeMap::<String, BucketStats>::new();

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
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                summary.parse_errors += 1;
                continue;
            };
            if record.get("type").and_then(Value::as_str) != Some("realized_trade") {
                continue;
            }
            let Some(realized_pnl) = record.get("realized_pnl").and_then(Value::as_f64) else {
                summary.parse_errors += 1;
                continue;
            };
            summary.records += 1;
            summary.realized_pnl += realized_pnl;
            if record
                .get("warnings")
                .and_then(Value::as_array)
                .is_some_and(|warnings| !warnings.is_empty())
            {
                summary.records_with_warnings += 1;
            }
            bucket_stats.add(realized_pnl);
            if realized_pnl > 0.0 {
                summary.wins += 1;
                win_sum += realized_pnl;
            } else if realized_pnl < 0.0 {
                summary.losses += 1;
                loss_sum += realized_pnl;
                summary.largest_loss = Some(
                    summary
                        .largest_loss
                        .map_or(realized_pnl, |current| current.min(realized_pnl)),
                );
            } else {
                summary.flats += 1;
            }
            if let Some(strategy) = record.get("strategy").and_then(Value::as_str) {
                strategy_stats
                    .entry(strategy.to_string())
                    .or_default()
                    .add(realized_pnl);
            }
            if let Some(underlying) = record.get("underlying").and_then(Value::as_str) {
                underlying_stats
                    .entry(underlying.to_string())
                    .or_default()
                    .add(realized_pnl);
            }
        }
    }

    summary.dates.sort();
    summary.average_win = (summary.wins > 0).then_some(win_sum / summary.wins as f64);
    summary.average_loss = (summary.losses > 0).then_some(loss_sum / summary.losses as f64);
    summary.by_strategy = strategy_stats
        .into_iter()
        .map(|(key, stats)| (key, stats.into_summary()))
        .collect();
    summary.by_underlying = underlying_stats
        .into_iter()
        .map(|(key, stats)| (key, stats.into_summary()))
        .collect();
    let all = bucket_stats.into_summary();
    if summary.records > 0 {
        summary.largest_loss = all.largest_loss;
    }
    Ok(summary)
}

/// Summarizes candidate-outcome JSONL files for an optional trade-date range.
///
/// # Errors
///
/// Returns an error if an outcome directory entry or file cannot be read.
pub fn summarize_candidate_outcomes(
    directory: &Path,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
) -> anyhow::Result<CandidateOutcomeSummary> {
    let mut summary = CandidateOutcomeSummary {
        directory: directory.display().to_string(),
        ..CandidateOutcomeSummary::default()
    };
    if !directory.exists() {
        summary.missing = true;
        return Ok(summary);
    }

    let mut stats = OutcomeStats::default();
    let mut bucket_stats = BTreeMap::<String, OutcomeStats>::new();
    let mut strategy_stats = BTreeMap::<String, OutcomeStats>::new();

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
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                summary.parse_errors += 1;
                continue;
            };
            if record.get("type").and_then(Value::as_str) != Some("candidate_outcome") {
                continue;
            }
            let Some(hypothetical_pnl) = record.get("hypothetical_pnl").and_then(Value::as_f64)
            else {
                summary.parse_errors += 1;
                continue;
            };

            let was_selected = record
                .get("was_selected")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let was_traded = record
                .get("was_traded")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            stats.add(hypothetical_pnl, was_selected, was_traded);

            if record
                .get("quote_warnings")
                .and_then(Value::as_array)
                .is_some_and(|warnings| !warnings.is_empty())
            {
                summary.records_with_warnings += 1;
            }
            let bucket = record
                .get("observation_bucket")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            bucket_stats.entry(bucket.to_string()).or_default().add(
                hypothetical_pnl,
                was_selected,
                was_traded,
            );
            let strategy = record
                .get("strategy")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            strategy_stats.entry(strategy.to_string()).or_default().add(
                hypothetical_pnl,
                was_selected,
                was_traded,
            );
        }
    }

    summary.dates.sort();
    let aggregate = stats.into_summary();
    summary.records = aggregate.records;
    summary.selected_records = aggregate.selected_records;
    summary.traded_records = aggregate.traded_records;
    summary.wins = aggregate.wins;
    summary.losses = aggregate.losses;
    summary.flats = aggregate.flats;
    summary.hypothetical_pnl = aggregate.hypothetical_pnl;
    summary.average_win = aggregate.average_win;
    summary.average_loss = aggregate.average_loss;
    summary.largest_loss = aggregate.largest_loss;
    summary.by_bucket = bucket_stats
        .into_iter()
        .map(|(key, stats)| (key, stats.into_summary()))
        .collect();
    summary.by_strategy = strategy_stats
        .into_iter()
        .map(|(key, stats)| (key, stats.into_summary()))
        .collect();
    Ok(summary)
}

/// Summarizes candidate-ledger JSONL files for an optional trade-date range.
///
/// # Errors
///
/// Returns an error if a ledger directory entry or file cannot be read.
pub fn summarize_candidate_ledger(
    directory: &Path,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
) -> anyhow::Result<CandidateLedgerSummary> {
    let mut summary = CandidateLedgerSummary {
        directory: directory.display().to_string(),
        ..CandidateLedgerSummary::default()
    };
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
            summary.records += 1;
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                summary.parse_errors += 1;
                continue;
            };
            let record_type = record
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            *summary.by_type.entry(record_type.clone()).or_insert(0) += 1;
            match record_type.as_str() {
                "candidate" => {
                    summary.candidates += 1;
                    if let Some(strategy) = record.get("strategy").and_then(Value::as_str) {
                        *summary
                            .candidates_by_strategy
                            .entry(strategy.to_string())
                            .or_insert(0) += 1;
                    }
                }
                "scanner_result" => summary.scanner_results += 1,
                "decision" => summary.decisions += 1,
                "submit_result" => summary.submit_results += 1,
                "candidate_alert" => {
                    summary.candidate_alerts += 1;
                    match record.get("alert_type").and_then(Value::as_str) {
                        Some("selected_candidate") => summary.selected_candidates += 1,
                        Some("high_score_candidate") => summary.high_score_candidates += 1,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    summary.dates.sort();
    Ok(summary)
}

/// Builds one performance row from a state entry, matched fill activities, and open positions.
#[must_use]
pub fn entry_performance(
    entry: &StrategyStateEntry,
    order_ids: &EntryOrderIds,
    activities: &[AlpacaActivity],
    positions: &[AlpacaPosition],
) -> EntryPerformance {
    let symbols = entry_symbols(entry);
    let open = fill_summary(activities, &order_ids.open, &symbols);
    let close = fill_summary(activities, &order_ids.close, &symbols);
    let realized_pnl = if entry.closed {
        match (open.cashflow, close.cashflow) {
            (Some(open_cashflow), Some(close_cashflow)) => Some(open_cashflow + close_cashflow),
            _ => None,
        }
    } else {
        None
    };
    let open_unrealized_pnl = entry
        .is_active()
        .then(|| open_unrealized_pnl(entry, positions))
        .flatten();
    let quoted_entry_premium = quoted_entry_premium(entry);
    let quoted_entry_cashflow = quoted_entry_premium
        .map(|premium| premium * entry.quantity as f64 * OPTION_CONTRACT_MULTIPLIER);

    let mut warnings = Vec::new();
    if entry.submitted && order_ids.open.is_empty() {
        warnings.push("missing_open_order_id".to_string());
    }
    if (entry.closed || entry.is_active()) && open.fills == 0 {
        warnings.push("missing_open_fills".to_string());
    }
    if entry.closed && order_ids.close.is_empty() {
        warnings.push("missing_close_order_id".to_string());
    }
    if entry.closed && close.fills == 0 {
        warnings.push("missing_close_fills".to_string());
    }
    if entry.closed && realized_pnl.is_none() {
        warnings.push("missing_realized_pnl".to_string());
    }
    if entry.canceled && open.fills > 0 {
        warnings.push("canceled_entry_has_fills".to_string());
    }

    EntryPerformance {
        trade_date: entry.trade_date.clone(),
        underlying: entry.underlying.clone(),
        strategy: entry.strategy.clone(),
        status: entry_status(entry).to_string(),
        symbols: symbols.into_iter().collect(),
        quantity: entry.quantity,
        recorded_at_utc: entry.recorded_at_utc.clone(),
        closed_at_utc: entry.closed_at_utc.clone(),
        score: entry.score,
        quoted_entry_premium,
        quoted_entry_cashflow,
        open,
        close,
        realized_pnl,
        open_unrealized_pnl,
        close_reason: entry.close_reason.clone(),
        parent_order_id: entry.parent_order_id.clone(),
        close_parent_order_id: entry.close_parent_order_id.clone(),
        order_list_id: entry.order_list_id.clone(),
        close_order_list_id: entry.close_order_list_id.clone(),
        warnings,
    }
}

/// Builds aggregate report summary rows from per-entry accounting.
#[must_use]
pub fn summarize_performance(entries: &[EntryPerformance]) -> PerformanceSummary {
    let mut summary = PerformanceSummary::default();
    for entry in entries {
        summary.entries += 1;
        apply_entry_summary(
            entry,
            &mut summary.active_entries,
            &mut summary.closed_entries,
            &mut summary.canceled_entries,
        );
        if let Some(realized_pnl) = entry.realized_pnl {
            summary.realized_entries += 1;
            summary.realized_pnl += realized_pnl;
        } else if entry.status == "closed" {
            summary.missing_realized_entries += 1;
        }
        if let Some(open_unrealized_pnl) = entry.open_unrealized_pnl {
            summary.open_unrealized_pnl += open_unrealized_pnl;
        }

        let strategy_summary = summary
            .by_strategy
            .entry(entry.strategy.clone())
            .or_default();
        strategy_summary.entries += 1;
        apply_entry_summary(
            entry,
            &mut strategy_summary.active_entries,
            &mut strategy_summary.closed_entries,
            &mut strategy_summary.canceled_entries,
        );
        if let Some(realized_pnl) = entry.realized_pnl {
            strategy_summary.realized_entries += 1;
            strategy_summary.realized_pnl += realized_pnl;
        } else if entry.status == "closed" {
            strategy_summary.missing_realized_entries += 1;
        }
        if let Some(open_unrealized_pnl) = entry.open_unrealized_pnl {
            strategy_summary.open_unrealized_pnl += open_unrealized_pnl;
        }
        strategy_summary.observed_total_pnl =
            strategy_summary.realized_pnl + strategy_summary.open_unrealized_pnl;
    }
    summary.observed_total_pnl = summary.realized_pnl + summary.open_unrealized_pnl;
    summary
}

/// Returns the earliest entry timestamp in strategy state.
#[must_use]
pub fn earliest_entry_timestamp(state: &StrategyState) -> Option<String> {
    state
        .entries
        .iter()
        .map(|entry| entry.recorded_at_utc.as_str())
        .filter(|value| !value.trim().is_empty())
        .min()
        .map(ToString::to_string)
}

/// Returns `true` when the entry trade date is in the optional range.
#[must_use]
pub fn entry_in_date_range(
    entry: &StrategyStateEntry,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
) -> bool {
    NaiveDate::parse_from_str(&entry.trade_date, "%Y-%m-%d")
        .map(|date| date_in_range(date, since, until))
        .unwrap_or(true)
}

/// Collects parent and nested leg order IDs into `ids`.
pub fn collect_order_ids(order: &AlpacaOrder, ids: &mut BTreeSet<String>) {
    if let Some(id) = order.id.as_ref().filter(|value| !value.trim().is_empty()) {
        ids.insert(id.clone());
    }
    if let Some(legs) = &order.legs {
        for leg in legs {
            collect_order_ids(leg, ids);
        }
    }
}

fn performance_record_key(entry: &EntryPerformance) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        entry.trade_date,
        entry.underlying,
        entry.strategy,
        entry.order_list_id,
        entry.close_order_list_id.as_deref().unwrap_or("none"),
    )
}

fn performance_ledger_has_key(path: &Path, record_key: &str) -> anyhow::Result<bool> {
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

#[derive(Clone, Copy, Debug, Default)]
struct BucketStats {
    records: usize,
    wins: usize,
    losses: usize,
    flats: usize,
    realized_pnl: f64,
    win_sum: f64,
    loss_sum: f64,
    largest_loss: Option<f64>,
}

impl BucketStats {
    fn add(&mut self, realized_pnl: f64) {
        self.records += 1;
        self.realized_pnl += realized_pnl;
        if realized_pnl > 0.0 {
            self.wins += 1;
            self.win_sum += realized_pnl;
        } else if realized_pnl < 0.0 {
            self.losses += 1;
            self.loss_sum += realized_pnl;
            self.largest_loss = Some(
                self.largest_loss
                    .map_or(realized_pnl, |current| current.min(realized_pnl)),
            );
        } else {
            self.flats += 1;
        }
    }

    fn into_summary(self) -> PerformanceLedgerBucketSummary {
        PerformanceLedgerBucketSummary {
            records: self.records,
            wins: self.wins,
            losses: self.losses,
            flats: self.flats,
            realized_pnl: self.realized_pnl,
            average_win: (self.wins > 0).then_some(self.win_sum / self.wins as f64),
            average_loss: (self.losses > 0).then_some(self.loss_sum / self.losses as f64),
            largest_loss: self.largest_loss,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct OutcomeStats {
    records: usize,
    selected_records: usize,
    traded_records: usize,
    wins: usize,
    losses: usize,
    flats: usize,
    hypothetical_pnl: f64,
    win_sum: f64,
    loss_sum: f64,
    largest_loss: Option<f64>,
}

impl OutcomeStats {
    fn add(&mut self, hypothetical_pnl: f64, was_selected: bool, was_traded: bool) {
        self.records += 1;
        self.hypothetical_pnl += hypothetical_pnl;
        if was_selected {
            self.selected_records += 1;
        }
        if was_traded {
            self.traded_records += 1;
        }
        if hypothetical_pnl > 0.0 {
            self.wins += 1;
            self.win_sum += hypothetical_pnl;
        } else if hypothetical_pnl < 0.0 {
            self.losses += 1;
            self.loss_sum += hypothetical_pnl;
            self.largest_loss = Some(
                self.largest_loss
                    .map_or(hypothetical_pnl, |current| current.min(hypothetical_pnl)),
            );
        } else {
            self.flats += 1;
        }
    }

    fn into_summary(self) -> CandidateOutcomeBucketSummary {
        CandidateOutcomeBucketSummary {
            records: self.records,
            selected_records: self.selected_records,
            traded_records: self.traded_records,
            wins: self.wins,
            losses: self.losses,
            flats: self.flats,
            hypothetical_pnl: self.hypothetical_pnl,
            average_win: (self.wins > 0).then_some(self.win_sum / self.wins as f64),
            average_loss: (self.losses > 0).then_some(self.loss_sum / self.losses as f64),
            largest_loss: self.largest_loss,
        }
    }
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

fn ledger_file_date(path: &Path) -> Option<NaiveDate> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

fn date_in_range(date: NaiveDate, since: Option<NaiveDate>, until: Option<NaiveDate>) -> bool {
    since.is_none_or(|since| date >= since) && until.is_none_or(|until| date <= until)
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
        entry.debit.map(|debit| -debit)
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
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

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
    fn summarize_candidate_ledger_counts_opportunities() {
        let dir =
            std::env::temp_dir().join(format!("nautilus-alpaca-performance-{}", unique_suffix()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("2026-05-07.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"candidate","strategy":"iron_condor"}),
                json!({"type":"candidate_alert","alert_type":"selected_candidate"}),
                json!({"type":"submit_result","accepted":1})
            ),
        )
        .unwrap();

        let summary = summarize_candidate_ledger(&dir, None, None).unwrap();

        assert_eq!(summary.files, 1);
        assert_eq!(summary.records, 3);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.selected_candidates, 1);
        assert_eq!(summary.submit_results, 1);
        assert_eq!(summary.candidates_by_strategy["iron_condor"], 1);
    }

    #[test]
    fn summarize_candidate_outcomes_counts_hypothetical_pnl() {
        let dir = std::env::temp_dir().join(format!(
            "nautilus-alpaca-candidate-outcomes-{}",
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("2026-05-07.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                json!({
                    "type": "candidate_outcome",
                    "observation_bucket": "plus_1h",
                    "strategy": "iron_condor",
                    "was_selected": true,
                    "was_traded": true,
                    "hypothetical_pnl": 12.0,
                    "quote_warnings": [],
                }),
                json!({
                    "type": "candidate_outcome",
                    "observation_bucket": "plus_1h",
                    "strategy": "iron_condor",
                    "was_selected": false,
                    "was_traded": false,
                    "hypothetical_pnl": -8.0,
                    "quote_warnings": ["wide_quote"],
                }),
                json!({
                    "type": "candidate_outcome",
                    "observation_bucket": "next_day",
                    "strategy": "put_credit",
                    "was_selected": false,
                    "was_traded": false,
                    "hypothetical_pnl": 0.0,
                    "quote_warnings": [],
                })
            ),
        )
        .unwrap();

        let summary = summarize_candidate_outcomes(&dir, None, None).unwrap();

        assert_eq!(summary.files, 1);
        assert_eq!(summary.records, 3);
        assert_eq!(summary.selected_records, 1);
        assert_eq!(summary.traded_records, 1);
        assert_eq!(summary.wins, 1);
        assert_eq!(summary.losses, 1);
        assert_eq!(summary.flats, 1);
        assert_close(summary.hypothetical_pnl, 4.0);
        assert_eq!(summary.records_with_warnings, 1);
        assert_eq!(summary.by_bucket["plus_1h"].records, 2);
        assert_eq!(summary.by_strategy["iron_condor"].losses, 1);
    }

    #[test]
    fn append_performance_ledger_record_dedupes_by_close_key() {
        let dir = std::env::temp_dir().join(format!(
            "nautilus-alpaca-performance-ledger-{}",
            unique_suffix()
        ));
        let entry = entry_performance(&state_entry(), &EntryOrderIds::default(), &[], &[]);

        let first =
            append_performance_ledger_record(&dir, "2026-05-08", Some("paper-main"), &entry)
                .unwrap();
        let second =
            append_performance_ledger_record(&dir, "2026-05-08", Some("paper-main"), &entry)
                .unwrap();

        assert!(first.appended);
        assert!(!second.appended);
        assert_eq!(first.record_key, second.record_key);
        let raw = fs::read_to_string(first.path).unwrap();
        assert_eq!(raw.lines().count(), 1);
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

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000_001,
            "actual={actual} expected={expected}"
        );
    }
}
