//! Historical candidate-outcome tracking from candidate ledgers and option quotes.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Value, json};

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaActivity, AlpacaOptionSnapshot, AlpacaOrder, AlpacaPosition,
            OptionSnapshotsRequest,
        },
    },
    options_runtime::OptionsEngineConfig,
    runtime::{StrategyState, StrategyStateEntry},
};

use super::{
    CandidateLedgerSummary, CandidateOutcomeTrackingRequest, EntryOrderIds, EntryPerformance,
    OPTION_CONTRACT_MULTIPLIER, PerformanceSummary, apply_entry_summary,
    default_candidate_outcome_dir, entry_status, entry_symbols, fill_summary,
    jsonl::{append_deduped_jsonl_record, date_in_range, read_jsonl_records, scan_jsonl_records},
    open_unrealized_pnl, quoted_entry_premium,
};

pub async fn track_candidate_outcomes(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &OptionsEngineConfig,
    request: &CandidateOutcomeTrackingRequest,
) -> anyhow::Result<usize> {
    let trade_date = request.trade_date.unwrap_or_else(|| {
        Utc::now()
            .with_timezone(&config.entry_timezone)
            .date_naive()
    });
    let ledger_path = config
        .candidate_ledger_dir
        .join(format!("{trade_date}.jsonl"));
    let records = read_jsonl_records(&ledger_path)?;
    let selected = selected_candidate_actions(&records);
    let mut candidates = collect_track_candidates(
        &records,
        &selected,
        request.max_rank,
        request.max_candidates,
    );
    if candidates.is_empty() {
        return Ok(0);
    }

    let symbols = candidates
        .iter()
        .flat_map(|candidate| candidate.symbols.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut snapshot_request = OptionSnapshotsRequest::for_symbols(symbols);
    snapshot_request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&snapshot_request).await?.snapshots;
    let outcome_dir = default_candidate_outcome_dir(config);
    let mut appended = 0;
    for candidate in &mut candidates {
        let Some(outcome) = value_candidate_outcome(candidate, &snapshots) else {
            continue;
        };
        for bucket in candidate_observation_buckets(candidate, config) {
            let record_key = format!("{}|{bucket}", candidate.identity_key);
            let payload = json!({
                "schema_version": 1,
                "ts_utc": Utc::now().to_rfc3339(),
                "type": "candidate_outcome",
                "trade_date": candidate.trade_date,
                "account_id": config.fleet_account_id,
                "record_key": record_key,
                "candidate_identity_key": candidate.identity_key,
                "observation_bucket": bucket,
                "candidate_type": candidate.candidate_type,
                "strategy": candidate.strategy,
                "underlying": candidate.underlying,
                "rank": candidate.rank,
                "score": candidate.score,
                "symbols": candidate.symbols,
                "was_selected": candidate.was_selected,
                "was_traded": candidate.was_traded,
                "selected_action": candidate.selected_action,
                "entry_net_premium": candidate.entry_net_premium,
                "close_net_premium": outcome.close_net_premium,
                "hypothetical_pnl": outcome.hypothetical_pnl,
                "hypothetical_pnl_fraction": outcome.hypothetical_pnl_fraction,
                "quote_warnings": outcome.warnings,
                "candidate": candidate.record,
            });
            if append_deduped_jsonl_record(
                &outcome_dir,
                &candidate.trade_date,
                &record_key,
                payload,
            )?
            .appended
            {
                appended += 1;
            }
        }
    }
    Ok(appended)
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
    let scan = scan_jsonl_records(directory, since, until, |_, parsed| {
        summary.records += 1;
        let Ok(record) = parsed else {
            summary.parse_errors += 1;
            return;
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
    })?;

    summary.missing = scan.missing;
    summary.files = scan.files;
    summary.dates = scan.dates;
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

pub(super) fn performance_record_key(entry: &EntryPerformance) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        entry.trade_date,
        entry.underlying,
        entry.strategy,
        entry.order_list_id,
        entry.close_order_list_id.as_deref().unwrap_or("none"),
    )
}

#[derive(Clone, Debug)]
struct TrackCandidate {
    identity_key: String,
    trade_date: String,
    ts_utc: Option<DateTime<Utc>>,
    candidate_type: String,
    strategy: String,
    underlying: String,
    rank: Option<u64>,
    score: Option<f64>,
    symbols: Vec<String>,
    entry_kind: CandidateEntryKind,
    entry_net_premium: f64,
    record: Value,
    was_selected: bool,
    was_traded: bool,
    selected_action: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateEntryKind {
    Credit,
    Debit,
}

#[derive(Clone, Debug)]
struct CandidateOutcomeValue {
    close_net_premium: f64,
    hypothetical_pnl: f64,
    hypothetical_pnl_fraction: Option<f64>,
    warnings: Vec<String>,
}

fn selected_candidate_actions(records: &[Value]) -> BTreeMap<String, String> {
    records
        .iter()
        .filter(|record| record_str(record, "type") == Some("candidate_alert"))
        .filter(|record| record_str(record, "alert_type") == Some("selected_candidate"))
        .filter_map(|record| {
            Some((
                record_str(record, "candidate_identity_key")?.to_string(),
                record_str(record, "action")
                    .unwrap_or("selected")
                    .to_string(),
            ))
        })
        .collect()
}

fn collect_track_candidates(
    records: &[Value],
    selected: &BTreeMap<String, String>,
    max_rank: u64,
    max_candidates: usize,
) -> Vec<TrackCandidate> {
    let mut by_identity = BTreeMap::<String, TrackCandidate>::new();
    for record in records {
        if record_str(record, "type") != Some("candidate") {
            continue;
        }
        if record_u64(record, "rank").unwrap_or(1) > max_rank {
            continue;
        }
        let Some(candidate) = track_candidate_from_record(record, selected) else {
            continue;
        };
        by_identity
            .entry(candidate.identity_key.clone())
            .and_modify(|current| {
                if candidate.score.unwrap_or_default() > current.score.unwrap_or_default() {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }

    let mut candidates = by_identity.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(max_candidates);
    candidates
}

fn track_candidate_from_record(
    record: &Value,
    selected: &BTreeMap<String, String>,
) -> Option<TrackCandidate> {
    let strategy = record_str(record, "strategy")?.to_string();
    let underlying = record_str(record, "underlying")?.to_string();
    let candidate_type = record_str(record, "candidate_type")
        .unwrap_or("unknown")
        .to_string();
    let symbols = candidate_symbols(record);
    if symbols.is_empty() {
        return None;
    }
    let identity_key = candidate_identity_key(&strategy, &underlying, &symbols);
    let selected_action = selected.get(&identity_key).cloned();
    let entry_kind = if candidate_type == "debit_spread" || strategy.contains("debit") {
        CandidateEntryKind::Debit
    } else {
        CandidateEntryKind::Credit
    };
    let entry_net_premium = match entry_kind {
        CandidateEntryKind::Credit => record_f64(record, "credit")?,
        CandidateEntryKind::Debit => record_f64(record, "debit")?,
    };
    Some(TrackCandidate {
        identity_key,
        trade_date: record_str(record, "trade_date")?.to_string(),
        ts_utc: record_ts(record),
        candidate_type,
        strategy,
        underlying,
        rank: record_u64(record, "rank"),
        score: record_f64(record, "score"),
        symbols,
        entry_kind,
        entry_net_premium,
        record: record.clone(),
        was_selected: selected_action.is_some(),
        was_traded: selected_action
            .as_deref()
            .is_some_and(|action| action == "selected" || action == "submitted"),
        selected_action,
    })
}

fn candidate_symbols(record: &Value) -> Vec<String> {
    if let (Some(short_put), Some(long_put), Some(short_call), Some(long_call)) = (
        record_str(record, "short_put_symbol"),
        record_str(record, "long_put_symbol"),
        record_str(record, "short_call_symbol"),
        record_str(record, "long_call_symbol"),
    ) {
        return [short_put, long_put, short_call, long_call]
            .into_iter()
            .map(ToString::to_string)
            .collect();
    }
    if let Some(short) = record_str(record, "short_symbol") {
        if let Some(long) = record_str(record, "long_symbol")
            && !long.is_empty()
        {
            if record_str(record, "candidate_type") == Some("debit_spread")
                || record_str(record, "strategy").is_some_and(|strategy| strategy.contains("debit"))
            {
                return vec![long.to_string(), short.to_string()];
            }
            return vec![short.to_string(), long.to_string()];
        }
        return vec![short.to_string()];
    }
    Vec::new()
}

fn candidate_identity_key(strategy: &str, underlying: &str, symbols: &[String]) -> String {
    format!("{}|{}|{}", strategy, underlying, symbols.join("|"))
}

fn value_candidate_outcome(
    candidate: &TrackCandidate,
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
) -> Option<CandidateOutcomeValue> {
    let mut warnings = Vec::new();
    let close_net_premium = if candidate.candidate_type == "iron_condor" {
        let short_put = record_str(&candidate.record, "short_put_symbol")?;
        let long_put = record_str(&candidate.record, "long_put_symbol")?;
        let short_call = record_str(&candidate.record, "short_call_symbol")?;
        let long_call = record_str(&candidate.record, "long_call_symbol")?;
        quote_ask(snapshots, short_put, &mut warnings)?
            - quote_bid(snapshots, long_put, &mut warnings)?
            + quote_ask(snapshots, short_call, &mut warnings)?
            - quote_bid(snapshots, long_call, &mut warnings)?
    } else if candidate.entry_kind == CandidateEntryKind::Debit {
        let long = candidate.symbols.first()?;
        let short = candidate.symbols.get(1)?;
        quote_bid(snapshots, long, &mut warnings)? - quote_ask(snapshots, short, &mut warnings)?
    } else if candidate.symbols.len() == 1 {
        quote_ask(snapshots, &candidate.symbols[0], &mut warnings)?
    } else {
        let short = candidate.symbols.first()?;
        let long = candidate.symbols.get(1)?;
        quote_ask(snapshots, short, &mut warnings)? - quote_bid(snapshots, long, &mut warnings)?
    };
    let pnl_per_contract = match candidate.entry_kind {
        CandidateEntryKind::Credit => candidate.entry_net_premium - close_net_premium,
        CandidateEntryKind::Debit => close_net_premium - candidate.entry_net_premium,
    };
    let hypothetical_pnl = pnl_per_contract * OPTION_CONTRACT_MULTIPLIER;
    let hypothetical_pnl_fraction = (candidate.entry_net_premium > 0.0)
        .then_some(pnl_per_contract / candidate.entry_net_premium);
    Some(CandidateOutcomeValue {
        close_net_premium,
        hypothetical_pnl,
        hypothetical_pnl_fraction,
        warnings,
    })
}

fn quote_bid(
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    symbol: &str,
    warnings: &mut Vec<String>,
) -> Option<f64> {
    let bid = snapshots
        .get(symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref())
        .and_then(|quote| quote.bid_price)
        .filter(|value| *value > 0.0);
    if bid.is_none() {
        warnings.push(format!("missing_bid:{symbol}"));
    }
    bid
}

fn quote_ask(
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    symbol: &str,
    warnings: &mut Vec<String>,
) -> Option<f64> {
    let ask = snapshots
        .get(symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref())
        .and_then(|quote| quote.ask_price)
        .filter(|value| *value > 0.0);
    if ask.is_none() {
        warnings.push(format!("missing_ask:{symbol}"));
    }
    ask
}

fn candidate_observation_buckets(
    candidate: &TrackCandidate,
    config: &OptionsEngineConfig,
) -> Vec<&'static str> {
    let mut buckets = Vec::new();
    let now_utc = Utc::now();
    if candidate
        .ts_utc
        .is_some_and(|ts| now_utc.signed_duration_since(ts) >= Duration::hours(1))
    {
        buckets.push("plus_1h");
    }
    let now_local = now_utc.with_timezone(&config.entry_timezone);
    let Ok(trade_date) = NaiveDate::parse_from_str(&candidate.trade_date, "%Y-%m-%d") else {
        return buckets;
    };
    if now_local.date_naive() == trade_date && now_local.time() >= config.close_end {
        buckets.push("same_day_close");
    }
    if now_local.date_naive() > trade_date {
        buckets.push("next_day");
    }
    if config.expiration_exit_days >= 0
        && candidate_days_to_expiration(candidate)
            .is_some_and(|days| days <= config.expiration_exit_days)
    {
        buckets.push("expiration_risk");
    }
    buckets.sort();
    buckets.dedup();
    buckets
}

fn candidate_days_to_expiration(candidate: &TrackCandidate) -> Option<i64> {
    let expiration = nested_str(&candidate.record, &["short", "expiration_date"])
        .or_else(|| nested_str(&candidate.record, &["put", "short", "expiration_date"]))
        .or_else(|| nested_str(&candidate.record, &["call", "short", "expiration_date"]))?;
    let expiration = NaiveDate::parse_from_str(expiration, "%Y-%m-%d").ok()?;
    let today = Utc::now().date_naive();
    Some((expiration - today).num_days())
}

fn record_str<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    record.get(key)?.as_str()
}

fn nested_str<'a>(record: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut value = record;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_str()
}

fn record_f64(record: &Value, key: &str) -> Option<f64> {
    record.get(key)?.as_f64()
}

fn record_u64(record: &Value, key: &str) -> Option<u64> {
    record.get(key)?.as_u64()
}

fn record_ts(record: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(record_str(record, "ts_utc")?)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}
