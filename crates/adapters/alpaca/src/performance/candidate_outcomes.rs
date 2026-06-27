//! Historical candidate-outcome tracking from candidate ledgers and option quotes.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Value, json};

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaActivity, AlpacaOptionBar, AlpacaOptionSnapshot, AlpacaOrder, AlpacaPosition,
            OptionSnapshotsRequest,
        },
    },
    options_runtime::AlpacaOptionsRuntimeConfig,
    runtime::{StrategyState, StrategyStateEntry},
    storage::{
        CandidateLedgerSummaryFilters, append_candidate_outcome, read_candidate_ledger_records,
    },
};

use super::{
    CandidateOutcomeTrackingRequest, CandidateOutcomeTrackingResult, EntryOrderIds,
    EntryPerformance, OPTION_CONTRACT_MULTIPLIER, PerformanceSummary, apply_entry_summary,
    entry_status, entry_symbols, fill_summary, historical_marks, open_unrealized_pnl,
    quoted_entry_premium,
};

pub async fn track_candidate_outcomes(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &AlpacaOptionsRuntimeConfig,
    request: &CandidateOutcomeTrackingRequest,
) -> anyhow::Result<CandidateOutcomeTrackingResult> {
    if request.historical_fill_missing {
        anyhow::ensure!(
            request.historical_fill_lookahead_minutes > 0,
            "historical_fill_lookahead_minutes must be positive"
        );
    }
    let trade_date = request.trade_date.unwrap_or_else(|| {
        Utc::now()
            .with_timezone(&config.entry_timezone)
            .date_naive()
    });
    let Some(storage) = &config.storage_repository else {
        anyhow::bail!("storage is not connected");
    };
    let records = read_candidate_ledger_records(
        storage,
        config.storage_account_id(),
        CandidateLedgerSummaryFilters {
            since: Some(trade_date),
            until: Some(trade_date),
        },
    )
    .await?;
    let selected = selected_candidate_actions(&records);
    let candidates = collect_track_candidates(
        &records,
        &selected,
        request.max_rank,
        request.max_candidates,
    );
    if candidates.is_empty() {
        return Ok(CandidateOutcomeTrackingResult::default());
    }

    let symbols = candidates
        .iter()
        .flat_map(|candidate| candidate.symbols.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut snapshot_request = OptionSnapshotsRequest::for_symbols(symbols);
    snapshot_request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&snapshot_request).await?.snapshots;
    let payload_account_id = Some(config.storage_account_id().to_string());
    let close_config = VirtualCloseConfig::from(config);
    let mut snapshot_outcomes = BTreeMap::<String, CandidateOutcomeValue>::new();
    let mut historical_fill_candidates = Vec::new();
    for candidate in &candidates {
        if let Some(outcome) = value_candidate_outcome(candidate, &snapshots, close_config) {
            snapshot_outcomes.insert(candidate.identity_key.clone(), outcome);
        } else if request.historical_fill_missing {
            historical_fill_candidates.push(candidate.clone());
        }
    }
    let mut result = CandidateOutcomeTrackingResult::default();
    let historical_bars = if historical_fill_candidates.is_empty() {
        None
    } else {
        let mut warnings = Vec::new();
        let bars = historical_marks::fetch_candidate_bars(
            client,
            &historical_fill_candidates,
            &request.historical_fill_timeframe,
            request.historical_fill_lookahead_minutes,
            &mut warnings,
        )
        .await?;
        result.warnings.extend(warnings);
        Some(bars)
    };

    for candidate in &candidates {
        let outcome = snapshot_outcomes
            .remove(&candidate.identity_key)
            .or_else(|| {
                historical_bars.as_ref().and_then(|bars| {
                    let mut warnings = Vec::new();
                    let outcome = value_candidate_outcome_from_historical_bars(
                        candidate,
                        bars,
                        request.historical_fill_lookahead_minutes,
                        close_config,
                        &mut warnings,
                    );
                    if outcome.is_none() && !warnings.is_empty() {
                        result.warnings.push(format!(
                            "candidate_outcome_missing_mark identity={} warnings={}",
                            candidate.identity_key,
                            warnings.join(",")
                        ));
                    }
                    outcome
                })
            });
        let Some(outcome) = outcome else {
            result.missing_mark_candidates += 1;
            continue;
        };
        if outcome.mark_source == "historical_bar" {
            result.historical_bar_candidates += 1;
        } else {
            result.snapshot_candidates += 1;
        }
        for bucket in candidate_observation_buckets(candidate, config, &outcome) {
            let record_key = format!(
                "{}|{}|{bucket}",
                candidate.trade_date, candidate.identity_key
            );
            let payload = json!({
                "schema_version": 3,
                "ts_utc": Utc::now().to_rfc3339(),
                "type": "candidate_outcome",
                "trade_date": &candidate.trade_date,
                "account_id": payload_account_id.clone(),
                "record_key": &record_key,
                "candidate_identity_key": &candidate.identity_key,
                "observation_bucket": bucket,
                "candidate_type": &candidate.candidate_type,
                "strategy": &candidate.strategy,
                "underlying": &candidate.underlying,
                "rank": candidate.rank,
                "score": candidate.score,
                "symbols": &candidate.symbols,
                "quantity": candidate.quantity,
                "was_selected": candidate.was_selected,
                "was_submitted": candidate.was_submitted,
                "was_traded": candidate.was_traded,
                "was_rejected": candidate.was_rejected,
                "was_dry_run": candidate.was_dry_run,
                "virtual_trade": candidate.virtual_trade,
                "selected_action": &candidate.selected_action,
                "selected_reason": &candidate.selected_reason,
                "selected_details": &candidate.selected_details,
                "accepted": candidate.accepted,
                "rejected": candidate.rejected,
                "terminal_rejection_recorded": candidate.terminal_rejection_recorded,
                "rejection_reasons": &candidate.rejection_reasons,
                "entry_net_premium": candidate.entry_net_premium,
                "close_net_premium": outcome.close_net_premium,
                "mark_source": &outcome.mark_source,
                "mark_ts_utc": &outcome.mark_ts_utc,
                "historical_bar_volume": outcome.historical_bar_volume,
                "hypothetical_pnl_per_unit": outcome.hypothetical_pnl_per_unit,
                "hypothetical_pnl": outcome.hypothetical_pnl,
                "hypothetical_pnl_fraction": outcome.hypothetical_pnl_fraction,
                "management_close_reason": &outcome.management_close_reason,
                "virtual_close_reason": &outcome.virtual_close_reason,
                "virtual_close": outcome.virtual_close_reason.is_some() && bucket == "virtual_close",
                "quote_warnings": &outcome.warnings,
                "candidate": &candidate.record,
            });
            let appended_record = append_candidate_outcome(
                storage,
                config.storage_account_id(),
                &candidate.trade_date,
                &record_key,
                &payload,
            )
            .await?;
            if appended_record {
                result.appended += 1;
            }
        }
    }
    Ok(result)
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
    let fill_quality = super::fill_quality(entry, &symbols, quoted_entry_cashflow, &open);

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
        submitted_at_utc: entry.submitted_at_utc.clone(),
        closed_at_utc: entry.closed_at_utc.clone(),
        score: entry.score,
        quoted_entry_premium,
        quoted_entry_cashflow,
        open,
        close,
        realized_pnl,
        fill_quality,
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
        summary.fill_quality.add(&entry.fill_quality);

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
        strategy_summary.fill_quality.add(&entry.fill_quality);
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

fn date_in_range(date: NaiveDate, since: Option<NaiveDate>, until: Option<NaiveDate>) -> bool {
    since.is_none_or(|since| date >= since) && until.is_none_or(|until| date <= until)
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

pub fn performance_record_key(entry: &EntryPerformance) -> String {
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
pub(super) struct TrackCandidate {
    pub(super) identity_key: String,
    pub(super) trade_date: String,
    pub(super) ts_utc: Option<DateTime<Utc>>,
    pub(super) candidate_type: String,
    pub(super) strategy: String,
    pub(super) underlying: String,
    pub(super) rank: Option<u64>,
    pub(super) score: Option<f64>,
    pub(super) symbols: Vec<String>,
    pub(super) entry_kind: CandidateEntryKind,
    pub(super) entry_net_premium: f64,
    pub(super) quantity: u64,
    pub(super) record: Value,
    pub(super) was_selected: bool,
    pub(super) was_submitted: bool,
    pub(super) was_traded: bool,
    pub(super) was_rejected: bool,
    pub(super) was_dry_run: bool,
    pub(super) virtual_trade: bool,
    pub(super) selected_action: Option<String>,
    pub(super) selected_reason: Option<String>,
    pub(super) selected_details: Vec<String>,
    pub(super) accepted: Option<u64>,
    pub(super) rejected: Option<u64>,
    pub(super) terminal_rejection_recorded: Option<bool>,
    pub(super) rejection_reasons: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateEntryKind {
    Credit,
    Debit,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CandidateSelection {
    action: Option<String>,
    reason: Option<String>,
    details: Vec<String>,
    accepted: Option<u64>,
    rejected: Option<u64>,
    terminal_rejection_recorded: Option<bool>,
    rejection_reasons: Vec<String>,
    quantity: Option<u64>,
}

impl CandidateSelection {
    fn was_selected(&self) -> bool {
        self.action.is_some() || self.accepted.is_some() || self.rejected.is_some()
    }

    fn was_submitted(&self) -> bool {
        self.action
            .as_deref()
            .is_some_and(|action| matches!(action, "submit" | "submitted" | "selected"))
            || self.accepted.is_some()
            || self.rejected.is_some()
    }

    fn was_rejected(&self) -> bool {
        self.rejected.is_some_and(|rejected| rejected > 0)
            || self.terminal_rejection_recorded == Some(true)
    }

    fn was_dry_run(&self) -> bool {
        self.action.as_deref() == Some("dry_run")
    }

    fn was_traded(&self) -> bool {
        self.accepted.is_some_and(|accepted| accepted > 0)
            || (self.was_submitted() && !self.was_rejected())
    }

    fn virtual_trade(&self) -> bool {
        self.was_selected() && !self.was_traded()
    }

    fn quantity(&self) -> u64 {
        self.quantity.unwrap_or(1).max(1)
    }
}

#[derive(Clone, Copy, Debug)]
struct VirtualCloseConfig {
    force_flatten: bool,
    profit_target_close_fraction: f64,
    stop_loss_close_multiple: f64,
    max_hold_secs: u64,
    expiration_exit_days: i64,
}

impl From<&AlpacaOptionsRuntimeConfig> for VirtualCloseConfig {
    fn from(config: &AlpacaOptionsRuntimeConfig) -> Self {
        Self {
            force_flatten: config.force_flatten,
            profit_target_close_fraction: config.profit_target_close_fraction,
            stop_loss_close_multiple: config.stop_loss_close_multiple,
            max_hold_secs: config.max_hold_secs,
            expiration_exit_days: config.expiration_exit_days,
        }
    }
}

#[derive(Clone, Debug)]
struct CandidateOutcomeValue {
    mark_source: String,
    mark_ts_utc: Option<String>,
    historical_bar_volume: Option<u64>,
    close_net_premium: f64,
    hypothetical_pnl_per_unit: f64,
    hypothetical_pnl: f64,
    hypothetical_pnl_fraction: Option<f64>,
    management_close_reason: Option<String>,
    virtual_close_reason: Option<String>,
    warnings: Vec<String>,
}

pub(super) fn selected_candidate_actions(
    records: &[Value],
) -> BTreeMap<String, CandidateSelection> {
    let mut selected = BTreeMap::<String, CandidateSelection>::new();
    for record in records
        .iter()
        .filter(|record| record_str(record, "type") == Some("candidate_alert"))
    {
        let Some(identity_key) = record_str(record, "candidate_identity_key") else {
            continue;
        };
        let selection = selected.entry(identity_key.to_string()).or_default();
        match record_str(record, "alert_type") {
            Some("selected_candidate") => {
                selection.action = Some(
                    record_str(record, "action")
                        .unwrap_or("selected")
                        .to_string(),
                );
                selection.quantity = record_u64(record, "quantity");
                selection.reason = record_str(record, "reason").map(ToString::to_string);
                selection.details = string_array(record.get("details"));
            }
            Some("candidate_submit_rejected") => {
                selection.accepted = record_u64(record, "accepted");
                selection.rejected = record_u64(record, "rejected");
                selection.terminal_rejection_recorded = record
                    .get("terminal_rejection_recorded")
                    .and_then(Value::as_bool);
                selection.rejection_reasons = record
                    .get("rejection_reasons")
                    .and_then(Value::as_array)
                    .map(|reasons| {
                        reasons
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToString::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
            }
            _ => {}
        }
    }
    selected
}

pub(super) fn collect_track_candidates(
    records: &[Value],
    selected: &BTreeMap<String, CandidateSelection>,
    max_rank: u64,
    max_candidates: usize,
) -> Vec<TrackCandidate> {
    let mut by_identity = BTreeMap::<String, TrackCandidate>::new();
    for record in records {
        if record_str(record, "type") != Some("candidate") {
            continue;
        }
        let Some(candidate) = track_candidate_from_record(record, selected) else {
            continue;
        };
        if record_u64(record, "rank").unwrap_or(1) > max_rank && !candidate.was_selected {
            continue;
        }
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
        right.was_selected.cmp(&left.was_selected).then_with(|| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    if max_candidates > 0 {
        let selected_count = candidates
            .iter()
            .filter(|candidate| candidate.was_selected)
            .count();
        candidates.truncate(max_candidates.max(selected_count));
    }
    candidates
}

fn track_candidate_from_record(
    record: &Value,
    selected: &BTreeMap<String, CandidateSelection>,
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
    let selection = selected.get(&identity_key).cloned().unwrap_or_default();
    let selected_action = selection.action.clone();
    let selected_reason = selection.reason.clone();
    let selected_details = selection.details.clone();
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
        quantity: selection.quantity(),
        record: record.clone(),
        was_selected: selection.was_selected(),
        was_submitted: selection.was_submitted(),
        was_traded: selection.was_traded(),
        was_rejected: selection.was_rejected(),
        was_dry_run: selection.was_dry_run(),
        virtual_trade: selection.virtual_trade(),
        selected_action,
        selected_reason,
        selected_details,
        accepted: selection.accepted,
        rejected: selection.rejected,
        terminal_rejection_recorded: selection.terminal_rejection_recorded,
        rejection_reasons: selection.rejection_reasons,
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

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn value_candidate_outcome(
    candidate: &TrackCandidate,
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    close_config: VirtualCloseConfig,
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
    Some(candidate_outcome_value(
        candidate,
        close_net_premium,
        close_config,
        "snapshot",
        None,
        None,
        warnings,
    ))
}

fn value_candidate_outcome_from_historical_bars(
    candidate: &TrackCandidate,
    bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    lookahead_minutes: i64,
    close_config: VirtualCloseConfig,
    warnings: &mut Vec<String>,
) -> Option<CandidateOutcomeValue> {
    let mark =
        historical_marks::historical_candidate_mark(candidate, bars, lookahead_minutes, warnings)?;
    Some(candidate_outcome_value(
        candidate,
        mark.net_premium,
        close_config,
        "historical_bar",
        mark.mark_ts_utc,
        Some(mark.total_volume),
        std::mem::take(warnings),
    ))
}

fn candidate_outcome_value(
    candidate: &TrackCandidate,
    close_net_premium: f64,
    close_config: VirtualCloseConfig,
    mark_source: &str,
    mark_ts_utc: Option<String>,
    historical_bar_volume: Option<u64>,
    warnings: Vec<String>,
) -> CandidateOutcomeValue {
    let pnl_per_contract = match candidate.entry_kind {
        CandidateEntryKind::Credit => candidate.entry_net_premium - close_net_premium,
        CandidateEntryKind::Debit => close_net_premium - candidate.entry_net_premium,
    };
    let hypothetical_pnl_per_unit = pnl_per_contract * OPTION_CONTRACT_MULTIPLIER;
    let hypothetical_pnl = hypothetical_pnl_per_unit * candidate.quantity as f64;
    let hypothetical_pnl_fraction = (candidate.entry_net_premium > 0.0)
        .then_some(pnl_per_contract / candidate.entry_net_premium);
    let management_close_reason = virtual_close_reason(candidate, close_net_premium, close_config);
    let virtual_close_reason = candidate
        .virtual_trade
        .then(|| management_close_reason.clone())
        .flatten();
    CandidateOutcomeValue {
        mark_source: mark_source.to_string(),
        mark_ts_utc,
        historical_bar_volume,
        close_net_premium,
        hypothetical_pnl_per_unit,
        hypothetical_pnl,
        hypothetical_pnl_fraction,
        management_close_reason,
        virtual_close_reason,
        warnings,
    }
}

fn virtual_close_reason(
    candidate: &TrackCandidate,
    close_net_premium: f64,
    config: VirtualCloseConfig,
) -> Option<String> {
    if config.force_flatten {
        return Some("manual_flatten".to_string());
    }
    match candidate.entry_kind {
        CandidateEntryKind::Credit => {
            if close_net_premium
                <= candidate.entry_net_premium * config.profit_target_close_fraction.max(0.0)
            {
                return Some("profit_target".to_string());
            }
            if config.stop_loss_close_multiple > 0.0
                && close_net_premium
                    >= candidate.entry_net_premium * config.stop_loss_close_multiple
            {
                return Some("stop_loss".to_string());
            }
        }
        CandidateEntryKind::Debit => {
            if close_net_premium
                >= candidate.entry_net_premium
                    * (1.0 + config.profit_target_close_fraction.max(0.0))
            {
                return Some("profit_target".to_string());
            }
            if config.stop_loss_close_multiple > 0.0
                && close_net_premium
                    <= candidate.entry_net_premium / config.stop_loss_close_multiple
            {
                return Some("stop_loss".to_string());
            }
        }
    }
    if config.max_hold_secs > 0
        && candidate.ts_utc.is_some_and(|ts| {
            Utc::now().signed_duration_since(ts).num_seconds() >= config.max_hold_secs as i64
        })
    {
        return Some("max_hold".to_string());
    }
    if config.expiration_exit_days >= 0
        && candidate_days_to_expiration(candidate)
            .is_some_and(|days| days <= config.expiration_exit_days)
    {
        return Some("expiration_risk".to_string());
    }
    None
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
    config: &AlpacaOptionsRuntimeConfig,
    outcome: &CandidateOutcomeValue,
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
    if candidate.virtual_trade && outcome.virtual_close_reason.is_some() {
        buckets.push("virtual_close");
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::http::models::AlpacaOptionQuote;

    #[test]
    fn selected_rejected_candidate_is_tracked_as_virtual_trade_beyond_rank_cap() {
        let identity = "naked_call|SLV|SLV260522C00030000";
        let records = vec![
            json!({
                "type": "candidate_alert",
                "alert_type": "selected_candidate",
                "candidate_identity_key": identity,
                "action": "submit",
                "quantity": 2,
            }),
            json!({
                "type": "candidate_alert",
                "alert_type": "candidate_submit_rejected",
                "candidate_identity_key": identity,
                "accepted": 0,
                "rejected": 1,
                "terminal_rejection_recorded": true,
                "rejection_reasons": ["account not eligible"],
            }),
            naked_candidate_record(9),
        ];

        let selections = selected_candidate_actions(&records);
        let candidates = collect_track_candidates(&records, &selections, 3, 1);

        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert!(candidate.was_selected);
        assert!(candidate.was_submitted);
        assert!(candidate.was_rejected);
        assert!(!candidate.was_traded);
        assert!(candidate.virtual_trade);
        assert_eq!(candidate.quantity, 2);
        assert_eq!(candidate.rejected, Some(1));
        assert_eq!(candidate.rejection_reasons, vec!["account not eligible"]);
    }

    #[test]
    fn virtual_credit_candidate_closes_at_profit_target() {
        let records = vec![
            json!({
                "type": "candidate_alert",
                "alert_type": "selected_candidate",
                "candidate_identity_key": "naked_call|SLV|SLV260522C00030000",
                "action": "dry_run",
                "quantity": 2,
            }),
            naked_candidate_record(1),
        ];
        let selections = selected_candidate_actions(&records);
        let candidate = track_candidate_from_record(&records[1], &selections).unwrap();
        let snapshots = BTreeMap::from([(
            "SLV260522C00030000".to_string(),
            option_snapshot(0.35, 0.40),
        )]);

        let outcome = value_candidate_outcome(
            &candidate,
            &snapshots,
            VirtualCloseConfig {
                force_flatten: false,
                profit_target_close_fraction: 0.50,
                stop_loss_close_multiple: 2.0,
                max_hold_secs: 0,
                expiration_exit_days: -1,
            },
        )
        .unwrap();

        assert_eq!(
            outcome.management_close_reason.as_deref(),
            Some("profit_target")
        );
        assert_eq!(
            outcome.virtual_close_reason.as_deref(),
            Some("profit_target")
        );
        assert!((outcome.hypothetical_pnl_per_unit - 60.0).abs() < f64::EPSILON);
        assert!((outcome.hypothetical_pnl - 120.0).abs() < f64::EPSILON);
    }

    fn naked_candidate_record(rank: u64) -> Value {
        json!({
            "type": "candidate",
            "trade_date": "2026-05-22",
            "ts_utc": "2026-05-22T13:30:00Z",
            "candidate_type": "naked_option",
            "strategy": "naked_call",
            "underlying": "SLV",
            "rank": rank,
            "score": 104.9,
            "short_symbol": "SLV260522C00030000",
            "credit": 1.00,
            "short": {
                "expiration_date": "2026-05-22"
            }
        })
    }

    fn option_snapshot(bid: f64, ask: f64) -> AlpacaOptionSnapshot {
        AlpacaOptionSnapshot {
            latest_quote: Some(AlpacaOptionQuote {
                ask_price: Some(ask),
                ask_size: None,
                bid_price: Some(bid),
                bid_size: None,
                timestamp: None,
            }),
            latest_trade: None,
            minute_bar: None,
            daily_bar: None,
            prev_daily_bar: None,
            greeks: None,
            implied_volatility: None,
        }
    }
}
