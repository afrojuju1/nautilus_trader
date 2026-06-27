//! Shared historical option-bar marks for candidate outcome and replay analysis.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};

use crate::http::{
    client::AlpacaHttpClient,
    models::{AlpacaOptionBar, OptionBarsRequest},
};

use super::candidate_outcomes::{CandidateEntryKind, TrackCandidate};

#[derive(Clone, Debug)]
pub(super) struct HistoricalCandidateMark {
    pub(super) net_premium: f64,
    pub(super) mark_ts_utc: Option<String>,
    pub(super) total_volume: u64,
}

pub(super) async fn fetch_candidate_bars(
    client: &AlpacaHttpClient,
    candidates: &[TrackCandidate],
    timeframe: &str,
    lookahead_minutes: i64,
    feed: Option<&str>,
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
            candidate
                .ts_utc
                .and_then(|ts| ts.checked_add_signed(Duration::minutes(lookahead_minutes + 5)))
        })
        .max()
    else {
        warnings.push("candidate_records_missing_historical_mark_end".to_string());
        return Ok(BTreeMap::new());
    };

    let mut bars_request =
        OptionBarsRequest::for_symbols(symbols, timeframe.to_string(), start.to_rfc3339());
    bars_request.end = Some(end.to_rfc3339());
    bars_request.feed = feed.map(ToString::to_string);
    Ok(client.option_bars(&bars_request).await?.bars)
}

pub(super) fn historical_candidate_mark(
    candidate: &TrackCandidate,
    bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    lookahead_minutes: i64,
    warnings: &mut Vec<String>,
) -> Option<HistoricalCandidateMark> {
    let Some(start) = candidate.ts_utc else {
        warnings.push("missing_candidate_ts".to_string());
        return None;
    };
    let Some(target) = start.checked_add_signed(Duration::minutes(lookahead_minutes)) else {
        warnings.push("invalid_historical_mark_target_ts".to_string());
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

    Some(HistoricalCandidateMark {
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
