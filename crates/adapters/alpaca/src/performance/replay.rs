//! Historical option candidate replay against Alpaca option bars.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::{
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaOptionBar, OptionBarsRequest},
    },
    options_runtime::AlpacaOptionsRuntimeConfig,
    storage::{CandidateLedgerSummaryFilters, read_candidate_ledger_records},
};

use super::{
    OPTION_CONTRACT_MULTIPLIER,
    candidate_outcomes::{
        CandidateEntryKind, TrackCandidate, collect_track_candidates, selected_candidate_actions,
    },
};

/// Historical replay request over candidate-ledger records.
#[derive(Clone, Debug)]
pub struct HistoricalReplayRequest {
    /// First trade date to include. Defaults to today's account-local date.
    pub since: Option<NaiveDate>,
    /// Last trade date to include. Defaults to `since`.
    pub until: Option<NaiveDate>,
    /// Maximum number of unique candidates to replay. Zero means unlimited.
    pub max_candidates: usize,
    /// Maximum candidate rank to include, unless a candidate was selected.
    pub max_rank: u64,
    /// Minutes after candidate timestamp used for the historical mark.
    pub lookahead_minutes: i64,
    /// Alpaca historical option bar timeframe.
    pub timeframe: String,
    /// Whether to include per-candidate records in the report payload.
    pub include_records: bool,
}

impl Default for HistoricalReplayRequest {
    fn default() -> Self {
        Self {
            since: None,
            until: None,
            max_candidates: 100,
            max_rank: 3,
            lookahead_minutes: 390,
            timeframe: "1Min".to_string(),
            include_records: false,
        }
    }
}

/// Historical replay report.
#[derive(Clone, Debug, Default, Serialize)]
pub struct HistoricalReplayReport {
    /// Report generation timestamp.
    pub checked_at_utc: String,
    /// Storage account identifier used for candidate-ledger reads.
    pub account_id: Option<String>,
    /// First trade date included.
    pub since: String,
    /// Last trade date included.
    pub until: String,
    /// Alpaca historical option bar timeframe.
    pub timeframe: String,
    /// Minutes after candidate timestamp used for the historical mark.
    pub lookahead_minutes: i64,
    /// Maximum candidate rank included, unless selected.
    pub max_rank: u64,
    /// Maximum unique candidates requested. Zero means unlimited.
    pub max_candidates: usize,
    /// Candidate-ledger records read.
    pub ledger_records: usize,
    /// Unique candidates selected for replay.
    pub candidate_records: usize,
    /// Replay records with a complete historical mark.
    pub evaluated_records: usize,
    /// Replay records missing one or more leg marks.
    pub missing_records: usize,
    /// Aggregate replay summary.
    pub summary: HistoricalReplayBucketSummary,
    /// Replay summary by strategy.
    pub by_strategy: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Replay summary by underlying.
    pub by_underlying: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Replay summary by DTE bucket.
    pub by_dte_bucket: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Replay summary by scanner score bucket.
    pub by_score_bucket: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Replay summary by scanner-time short-leg delta bucket.
    pub by_delta_bucket: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Replay summary by scanner-time spread-width bucket.
    pub by_spread_width_bucket: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Replay summary by historical option-bar volume bucket.
    pub by_liquidity_bucket: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Replay summary by persisted decision reason.
    pub by_decision_reason: BTreeMap<String, HistoricalReplayBucketSummary>,
    /// Report-level warnings.
    pub warnings: Vec<String>,
    /// Optional per-candidate replay records.
    pub records: Vec<HistoricalReplayRecord>,
}

/// Replay aggregate bucket.
#[derive(Clone, Debug, Default, Serialize)]
pub struct HistoricalReplayBucketSummary {
    /// Candidate records in this bucket.
    pub records: usize,
    /// Candidate records selected by the live strategy.
    pub selected_records: usize,
    /// Candidate records submitted to the broker.
    pub submitted_records: usize,
    /// Candidate records rejected by the broker.
    pub rejected_records: usize,
    /// Selected candidates that did not trade at the broker.
    pub virtual_records: usize,
    /// Records with a complete historical mark.
    pub evaluated_records: usize,
    /// Records missing one or more leg marks.
    pub missing_records: usize,
    /// Winning replay outcomes.
    pub wins: usize,
    /// Losing replay outcomes.
    pub losses: usize,
    /// Flat replay outcomes.
    pub flats: usize,
    /// Replay PnL in dollars.
    pub hypothetical_pnl: f64,
    /// Average winning replay outcome in dollars.
    pub average_win: Option<f64>,
    /// Average losing replay outcome in dollars.
    pub average_loss: Option<f64>,
    /// Largest losing replay outcome in dollars.
    pub largest_loss: Option<f64>,
    /// Sum of historical option-bar volumes used for complete marks.
    pub total_volume: u64,
    /// Average option-bar volume used for complete marks.
    pub average_volume: Option<f64>,
}

/// One candidate replay record.
#[derive(Clone, Debug, Serialize)]
pub struct HistoricalReplayRecord {
    /// Candidate trade date.
    pub trade_date: String,
    /// Candidate identity key.
    pub candidate_identity_key: String,
    /// Candidate type from the ledger.
    pub candidate_type: String,
    /// Strategy family.
    pub strategy: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Candidate rank from the ledger.
    pub rank: Option<u64>,
    /// Candidate score from the ledger.
    pub score: Option<f64>,
    /// Option symbols used for the replay mark.
    pub symbols: Vec<String>,
    /// Strategy quantity.
    pub quantity: u64,
    /// Whether this candidate was selected live.
    pub was_selected: bool,
    /// Whether this candidate was submitted live.
    pub was_submitted: bool,
    /// Whether this candidate was rejected live.
    pub was_rejected: bool,
    /// Whether this selected candidate was not traded live.
    pub virtual_trade: bool,
    /// Selected-candidate action from the decision ledger.
    pub selected_action: Option<String>,
    /// Stable selected-candidate reason from the decision ledger.
    pub selected_reason: Option<String>,
    /// Detailed selected-candidate diagnostics from the decision ledger.
    pub selected_details: Vec<String>,
    /// Broker rejection reasons recorded for the selected candidate.
    pub rejection_reasons: Vec<String>,
    /// Stable replay decision reason used for explanation bucketing.
    pub decision_reason: String,
    /// Quoted entry premium from the candidate ledger.
    pub entry_net_premium: f64,
    /// Historical replay close mark.
    pub close_net_premium: Option<f64>,
    /// Replay PnL per one spread or option contract package.
    pub hypothetical_pnl_per_unit: Option<f64>,
    /// Replay PnL for configured quantity.
    pub hypothetical_pnl: Option<f64>,
    /// Replay PnL as a fraction of entry premium.
    pub hypothetical_pnl_fraction: Option<f64>,
    /// Days from trade date to option expiration.
    pub dte: Option<i64>,
    /// DTE bucket.
    pub dte_bucket: String,
    /// Score bucket.
    pub score_bucket: String,
    /// Scanner-time short-leg absolute delta.
    pub delta_abs: Option<f64>,
    /// Delta bucket.
    pub delta_bucket: String,
    /// Scanner-time spread width.
    pub spread_width: Option<f64>,
    /// Spread-width bucket.
    pub spread_width_bucket: String,
    /// Liquidity bucket.
    pub liquidity_bucket: String,
    /// Latest bar timestamp used by the replay mark.
    pub mark_ts_utc: Option<String>,
    /// Total option-bar volume used by the replay mark.
    pub total_volume: u64,
    /// Record-level warnings.
    pub warnings: Vec<String>,
}

/// Replays historical candidate-ledger records against Alpaca historical option bars.
///
/// # Errors
///
/// Returns an error if storage is not connected, the request is invalid, candidate-ledger reads
/// fail, or Alpaca historical option-bar requests fail.
pub async fn replay_historical_candidates(
    client: &AlpacaHttpClient,
    config: &AlpacaOptionsRuntimeConfig,
    request: &HistoricalReplayRequest,
) -> anyhow::Result<HistoricalReplayReport> {
    anyhow::ensure!(
        request.lookahead_minutes > 0,
        "lookahead_minutes must be positive"
    );
    let Some(storage) = &config.storage_repository else {
        anyhow::bail!("storage is not connected");
    };
    let default_date = Utc::now()
        .with_timezone(&config.entry_timezone)
        .date_naive();
    let since = request.since.unwrap_or(default_date);
    let until = request.until.unwrap_or(since);
    anyhow::ensure!(since <= until, "since must be before or equal to until");

    let ledger_records = read_candidate_ledger_records(
        storage,
        config.storage_account_id(),
        CandidateLedgerSummaryFilters {
            since: Some(since),
            until: Some(until),
        },
    )
    .await?;
    let selected = selected_candidate_actions(&ledger_records);
    let candidates = collect_track_candidates(
        &ledger_records,
        &selected,
        request.max_rank,
        request.max_candidates,
    );
    let mut warnings = Vec::new();
    let bars = fetch_candidate_bars(client, request, &candidates, &mut warnings).await?;

    let mut aggregate = ReplayStats::default();
    let mut by_strategy = BTreeMap::<String, ReplayStats>::new();
    let mut by_underlying = BTreeMap::<String, ReplayStats>::new();
    let mut by_dte_bucket = BTreeMap::<String, ReplayStats>::new();
    let mut by_score_bucket = BTreeMap::<String, ReplayStats>::new();
    let mut by_delta_bucket = BTreeMap::<String, ReplayStats>::new();
    let mut by_spread_width_bucket = BTreeMap::<String, ReplayStats>::new();
    let mut by_liquidity_bucket = BTreeMap::<String, ReplayStats>::new();
    let mut by_decision_reason = BTreeMap::<String, ReplayStats>::new();
    let mut records = Vec::new();

    for candidate in &candidates {
        let record = replay_candidate(candidate, &bars, request.lookahead_minutes);
        aggregate.add(&record);
        by_strategy
            .entry(record.strategy.clone())
            .or_default()
            .add(&record);
        by_underlying
            .entry(record.underlying.clone())
            .or_default()
            .add(&record);
        by_dte_bucket
            .entry(record.dte_bucket.clone())
            .or_default()
            .add(&record);
        by_score_bucket
            .entry(record.score_bucket.clone())
            .or_default()
            .add(&record);
        by_delta_bucket
            .entry(record.delta_bucket.clone())
            .or_default()
            .add(&record);
        by_spread_width_bucket
            .entry(record.spread_width_bucket.clone())
            .or_default()
            .add(&record);
        by_liquidity_bucket
            .entry(record.liquidity_bucket.clone())
            .or_default()
            .add(&record);
        by_decision_reason
            .entry(record.decision_reason.clone())
            .or_default()
            .add(&record);
        if request.include_records {
            records.push(record);
        }
    }

    let summary = aggregate.into_summary();
    Ok(HistoricalReplayReport {
        checked_at_utc: Utc::now().to_rfc3339(),
        account_id: Some(config.storage_account_id().to_string()),
        since: since.to_string(),
        until: until.to_string(),
        timeframe: request.timeframe.clone(),
        lookahead_minutes: request.lookahead_minutes,
        max_rank: request.max_rank,
        max_candidates: request.max_candidates,
        ledger_records: ledger_records.len(),
        candidate_records: candidates.len(),
        evaluated_records: summary.evaluated_records,
        missing_records: summary.missing_records,
        summary,
        by_strategy: into_summary_map(by_strategy),
        by_underlying: into_summary_map(by_underlying),
        by_dte_bucket: into_summary_map(by_dte_bucket),
        by_score_bucket: into_summary_map(by_score_bucket),
        by_delta_bucket: into_summary_map(by_delta_bucket),
        by_spread_width_bucket: into_summary_map(by_spread_width_bucket),
        by_liquidity_bucket: into_summary_map(by_liquidity_bucket),
        by_decision_reason: into_summary_map(by_decision_reason),
        warnings,
        records,
    })
}

async fn fetch_candidate_bars(
    client: &AlpacaHttpClient,
    request: &HistoricalReplayRequest,
    candidates: &[TrackCandidate],
    warnings: &mut Vec<String>,
) -> anyhow::Result<BTreeMap<String, Vec<AlpacaOptionBar>>> {
    let symbols = candidates
        .iter()
        .flat_map(|candidate| candidate.symbols.iter().cloned())
        .collect::<BTreeSet<_>>();
    if symbols.is_empty() {
        return Ok(BTreeMap::new());
    }

    let Some(start) = candidates
        .iter()
        .filter_map(|candidate| candidate.ts_utc)
        .min()
    else {
        warnings.push("candidate_records_missing_timestamps".to_string());
        return Ok(BTreeMap::new());
    };
    let Some(end) = candidates
        .iter()
        .filter_map(|candidate| {
            candidate.ts_utc.and_then(|ts| {
                ts.checked_add_signed(Duration::minutes(request.lookahead_minutes + 5))
            })
        })
        .max()
    else {
        warnings.push("candidate_records_missing_replay_end".to_string());
        return Ok(BTreeMap::new());
    };

    let mut bars_request =
        OptionBarsRequest::for_symbols(symbols, request.timeframe.clone(), start.to_rfc3339());
    bars_request.end = Some(end.to_rfc3339());
    Ok(client.option_bars(&bars_request).await?.bars)
}

fn replay_candidate(
    candidate: &TrackCandidate,
    bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    lookahead_minutes: i64,
) -> HistoricalReplayRecord {
    let mut warnings = Vec::new();
    let dte = candidate_dte(candidate);
    let (close_net_premium, mark_ts_utc, total_volume) =
        historical_close_mark(candidate, bars, lookahead_minutes, &mut warnings)
            .map(|mark| (Some(mark.net_premium), mark.mark_ts_utc, mark.total_volume))
            .unwrap_or((None, None, 0));
    let (hypothetical_pnl_per_unit, hypothetical_pnl, hypothetical_pnl_fraction) =
        close_net_premium.map_or((None, None, None), |close_net_premium| {
            let pnl_per_contract = match candidate.entry_kind {
                CandidateEntryKind::Credit => candidate.entry_net_premium - close_net_premium,
                CandidateEntryKind::Debit => close_net_premium - candidate.entry_net_premium,
            };
            let per_unit = pnl_per_contract * OPTION_CONTRACT_MULTIPLIER;
            let pnl = per_unit * candidate.quantity as f64;
            let fraction = (candidate.entry_net_premium > 0.0)
                .then_some(pnl_per_contract / candidate.entry_net_premium);
            (Some(per_unit), Some(pnl), fraction)
        });
    let score_bucket = score_bucket(candidate.score).to_string();
    let delta_abs = candidate_delta_abs(candidate);
    let delta_bucket = delta_bucket(delta_abs).to_string();
    let spread_width = candidate_spread_width(candidate);
    let spread_width_bucket = spread_width_bucket(spread_width).to_string();
    let dte_bucket = dte_bucket(dte).to_string();
    let liquidity_bucket = liquidity_bucket(total_volume, close_net_premium.is_some()).to_string();
    let decision_reason = replay_decision_reason(candidate);

    HistoricalReplayRecord {
        trade_date: candidate.trade_date.clone(),
        candidate_identity_key: candidate.identity_key.clone(),
        candidate_type: candidate.candidate_type.clone(),
        strategy: candidate.strategy.clone(),
        underlying: candidate.underlying.clone(),
        rank: candidate.rank,
        score: candidate.score,
        symbols: candidate.symbols.clone(),
        quantity: candidate.quantity,
        was_selected: candidate.was_selected,
        was_submitted: candidate.was_submitted,
        was_rejected: candidate.was_rejected,
        virtual_trade: candidate.virtual_trade,
        selected_action: candidate.selected_action.clone(),
        selected_reason: candidate.selected_reason.clone(),
        selected_details: candidate.selected_details.clone(),
        rejection_reasons: candidate.rejection_reasons.clone(),
        decision_reason,
        entry_net_premium: candidate.entry_net_premium,
        close_net_premium,
        hypothetical_pnl_per_unit,
        hypothetical_pnl,
        hypothetical_pnl_fraction,
        dte,
        dte_bucket,
        score_bucket,
        delta_abs,
        delta_bucket,
        spread_width,
        spread_width_bucket,
        liquidity_bucket,
        mark_ts_utc,
        total_volume,
        warnings,
    }
}

#[derive(Clone, Debug)]
struct HistoricalCloseMark {
    net_premium: f64,
    mark_ts_utc: Option<String>,
    total_volume: u64,
}

fn historical_close_mark(
    candidate: &TrackCandidate,
    bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    lookahead_minutes: i64,
    warnings: &mut Vec<String>,
) -> Option<HistoricalCloseMark> {
    let Some(start) = candidate.ts_utc else {
        warnings.push("missing_candidate_ts".to_string());
        return None;
    };
    let Some(target) = start.checked_add_signed(Duration::minutes(lookahead_minutes)) else {
        warnings.push("invalid_replay_target_ts".to_string());
        return None;
    };
    let mut leg_marks = Vec::new();
    for symbol in &candidate.symbols {
        leg_marks.push(bar_mark(symbol, bars, start, target, warnings)?);
    }

    let net_premium = if candidate.candidate_type == "iron_condor" {
        leg_marks.first()?.close - leg_marks.get(1)?.close + leg_marks.get(2)?.close
            - leg_marks.get(3)?.close
    } else if candidate.entry_kind == CandidateEntryKind::Debit {
        leg_marks.first()?.close - leg_marks.get(1)?.close
    } else if leg_marks.len() == 1 {
        leg_marks.first()?.close
    } else {
        leg_marks.first()?.close - leg_marks.get(1)?.close
    };
    let mark_ts_utc = leg_marks
        .iter()
        .filter_map(|mark| mark.ts_utc)
        .max()
        .map(|timestamp| timestamp.to_rfc3339());
    let total_volume = leg_marks.iter().map(|mark| mark.volume).sum();

    Some(HistoricalCloseMark {
        net_premium,
        mark_ts_utc,
        total_volume,
    })
}

#[derive(Clone, Copy, Debug)]
struct BarMark {
    close: f64,
    ts_utc: Option<DateTime<Utc>>,
    volume: u64,
}

fn bar_mark(
    symbol: &str,
    bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    start: DateTime<Utc>,
    target: DateTime<Utc>,
    warnings: &mut Vec<String>,
) -> Option<BarMark> {
    let mark = bars_for_symbol(bars, symbol).and_then(|symbol_bars| {
        symbol_bars
            .iter()
            .filter_map(|bar| {
                let ts_utc = bar_timestamp(bar)?;
                (ts_utc >= start && ts_utc <= target).then_some(BarMark {
                    close: bar.close?,
                    ts_utc: Some(ts_utc),
                    volume: bar.volume.unwrap_or(0),
                })
            })
            .last()
    });
    if mark.is_none() {
        warnings.push(format!("missing_historical_bar:{symbol}"));
    }
    mark
}

fn bars_for_symbol<'a>(
    bars: &'a BTreeMap<String, Vec<AlpacaOptionBar>>,
    symbol: &str,
) -> Option<&'a Vec<AlpacaOptionBar>> {
    let canonical = symbol.strip_prefix("O:").unwrap_or(symbol);
    bars.get(symbol)
        .or_else(|| bars.get(canonical))
        .or_else(|| {
            bars.iter()
                .find(|(key, _)| {
                    key.eq_ignore_ascii_case(symbol) || key.eq_ignore_ascii_case(canonical)
                })
                .map(|(_, bars)| bars)
        })
}

fn bar_timestamp(bar: &AlpacaOptionBar) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(bar.timestamp.as_deref()?)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn candidate_dte(candidate: &TrackCandidate) -> Option<i64> {
    let trade_date = NaiveDate::parse_from_str(&candidate.trade_date, "%Y-%m-%d").ok()?;
    let expiration = candidate_expiration(candidate)?;
    Some((expiration - trade_date).num_days())
}

fn candidate_expiration(candidate: &TrackCandidate) -> Option<NaiveDate> {
    nested_str(&candidate.record, &["short", "expiration_date"])
        .or_else(|| nested_str(&candidate.record, &["put", "short", "expiration_date"]))
        .or_else(|| nested_str(&candidate.record, &["call", "short", "expiration_date"]))
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

fn nested_str<'a>(record: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut value = record;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_str()
}

fn candidate_delta_abs(candidate: &TrackCandidate) -> Option<f64> {
    if candidate.candidate_type == "iron_condor" {
        return [
            nested_f64(&candidate.record, &["put", "short", "delta_abs"]),
            nested_f64(&candidate.record, &["call", "short", "delta_abs"]),
        ]
        .into_iter()
        .flatten()
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    }
    nested_f64(&candidate.record, &["short", "delta_abs"])
}

fn candidate_spread_width(candidate: &TrackCandidate) -> Option<f64> {
    record_f64(&candidate.record, "width").or_else(|| {
        if candidate.candidate_type == "iron_condor" {
            [
                nested_f64(&candidate.record, &["put", "width"]),
                nested_f64(&candidate.record, &["call", "width"]),
            ]
            .into_iter()
            .flatten()
            .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
        } else {
            None
        }
    })
}

fn replay_decision_reason(candidate: &TrackCandidate) -> String {
    if candidate.was_rejected {
        return candidate
            .rejection_reasons
            .first()
            .cloned()
            .unwrap_or_else(|| "broker_rejected".to_string());
    }
    if let Some(reason) = candidate
        .selected_reason
        .as_ref()
        .filter(|reason| !reason.is_empty())
    {
        return reason.clone();
    }
    if let Some(action) = candidate
        .selected_action
        .as_ref()
        .filter(|action| !action.is_empty())
    {
        return action.clone();
    }
    if candidate.was_selected {
        "selected".to_string()
    } else {
        "not_selected".to_string()
    }
}

fn record_f64(record: &Value, key: &str) -> Option<f64> {
    record.get(key)?.as_f64()
}

fn nested_f64(record: &Value, path: &[&str]) -> Option<f64> {
    let mut value = record;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_f64()
}

fn score_bucket(score: Option<f64>) -> &'static str {
    match score {
        Some(score) if score >= 100.0 => "score_ge_100",
        Some(score) if score >= 75.0 => "score_75_100",
        Some(score) if score >= 50.0 => "score_50_75",
        Some(_) => "score_lt_50",
        None => "score_unknown",
    }
}

fn delta_bucket(delta_abs: Option<f64>) -> &'static str {
    match delta_abs {
        Some(delta) if delta < 0.20 => "delta_lt_20",
        Some(delta) if delta < 0.30 => "delta_20_30",
        Some(delta) if delta < 0.50 => "delta_30_50",
        Some(_) => "delta_ge_50",
        None => "delta_unknown",
    }
}

fn spread_width_bucket(width: Option<f64>) -> &'static str {
    match width {
        Some(width) if width <= 1.0 => "width_le_1",
        Some(width) if width <= 5.0 => "width_1_5",
        Some(width) if width <= 10.0 => "width_5_10",
        Some(_) => "width_gt_10",
        None => "width_unknown",
    }
}

fn dte_bucket(dte: Option<i64>) -> &'static str {
    match dte {
        Some(days) if days <= 0 => "dte_0",
        Some(days) if days <= 7 => "dte_1_7",
        Some(days) if days <= 14 => "dte_8_14",
        Some(days) if days <= 30 => "dte_15_30",
        Some(_) => "dte_gt_30",
        None => "dte_unknown",
    }
}

fn liquidity_bucket(total_volume: u64, evaluated: bool) -> &'static str {
    if !evaluated {
        "liquidity_missing"
    } else if total_volume == 0 {
        "volume_0"
    } else if total_volume < 10 {
        "volume_1_9"
    } else if total_volume < 100 {
        "volume_10_99"
    } else {
        "volume_ge_100"
    }
}

#[derive(Clone, Debug, Default)]
struct ReplayStats {
    records: usize,
    selected_records: usize,
    submitted_records: usize,
    rejected_records: usize,
    virtual_records: usize,
    evaluated_records: usize,
    missing_records: usize,
    wins: usize,
    losses: usize,
    flats: usize,
    hypothetical_pnl: f64,
    win_sum: f64,
    loss_sum: f64,
    largest_loss: Option<f64>,
    total_volume: u64,
}

impl ReplayStats {
    fn add(&mut self, record: &HistoricalReplayRecord) {
        self.records += 1;
        if record.was_selected {
            self.selected_records += 1;
        }
        if record.was_submitted {
            self.submitted_records += 1;
        }
        if record.was_rejected {
            self.rejected_records += 1;
        }
        if record.virtual_trade {
            self.virtual_records += 1;
        }
        if let Some(pnl) = record.hypothetical_pnl {
            self.evaluated_records += 1;
            self.hypothetical_pnl += pnl;
            self.total_volume += record.total_volume;
            if pnl > 0.0 {
                self.wins += 1;
                self.win_sum += pnl;
            } else if pnl < 0.0 {
                self.losses += 1;
                self.loss_sum += pnl;
                self.largest_loss = Some(self.largest_loss.map_or(pnl, |current| current.min(pnl)));
            } else {
                self.flats += 1;
            }
        } else {
            self.missing_records += 1;
        }
    }

    fn into_summary(self) -> HistoricalReplayBucketSummary {
        HistoricalReplayBucketSummary {
            records: self.records,
            selected_records: self.selected_records,
            submitted_records: self.submitted_records,
            rejected_records: self.rejected_records,
            virtual_records: self.virtual_records,
            evaluated_records: self.evaluated_records,
            missing_records: self.missing_records,
            wins: self.wins,
            losses: self.losses,
            flats: self.flats,
            hypothetical_pnl: self.hypothetical_pnl,
            average_win: (self.wins > 0).then_some(self.win_sum / self.wins as f64),
            average_loss: (self.losses > 0).then_some(self.loss_sum / self.losses as f64),
            largest_loss: self.largest_loss,
            total_volume: self.total_volume,
            average_volume: (self.evaluated_records > 0)
                .then_some(self.total_volume as f64 / self.evaluated_records as f64),
        }
    }
}

fn into_summary_map(
    stats: BTreeMap<String, ReplayStats>,
) -> BTreeMap<String, HistoricalReplayBucketSummary> {
    stats
        .into_iter()
        .map(|(key, stats)| (key, stats.into_summary()))
        .collect()
}
