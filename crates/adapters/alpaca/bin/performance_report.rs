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

//! Alpaca options performance report from strategy state, candidate ledgers, and broker fills.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{
            AlpacaOptionSnapshot, AlpacaOrder, ListActivitiesRequest, OptionSnapshotsRequest,
        },
    },
    options_runtime::OptionsEngineConfig,
    performance::{
        EntryOrderIds, EntryPerformance, PerformanceReport, append_performance_ledger_record,
        collect_order_ids, default_performance_ledger_dir, earliest_entry_timestamp,
        entry_in_date_range, entry_performance, summarize_candidate_ledger,
        summarize_candidate_outcomes, summarize_performance, summarize_performance_ledger,
    },
    runtime::{StrategyStateEntry, load_strategy_state},
};
use serde_json::{Value, json};

const DEFAULT_TRACK_MAX_CANDIDATES: usize = 100;
const DEFAULT_TRACK_MAX_RANK: u64 = 3;

#[derive(Debug, Default)]
struct Args {
    json_output: bool,
    append_ledger: bool,
    send_discord: bool,
    alerts_env_file: Option<PathBuf>,
    track_candidates: bool,
    track_date: Option<NaiveDate>,
    track_max_candidates: usize,
    track_max_rank: u64,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let config = OptionsEngineConfig::from_runtime_env()?;
    let state = load_strategy_state(&config.state_path)?;
    let entries = state
        .entries
        .iter()
        .filter(|entry| entry_in_date_range(entry, args.since, args.until))
        .cloned()
        .collect::<Vec<_>>();

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    let client = AlpacaHttpClient::from_data_config(&data_config)?;

    let activities = if let Some(after) = activity_after_timestamp(&entries, args.since) {
        let mut request = ListActivitiesRequest::option_reconciliation();
        request.direction = Some("asc".to_string());
        request.after = Some(after);
        request.page_size = 100;
        client.account_activities_all(&request).await?
    } else {
        Vec::new()
    };
    let positions = client.positions().await?;

    let mut report_entries = Vec::new();
    let mut warnings = Vec::new();
    for entry in &entries {
        match resolve_entry_order_ids(&client, entry).await {
            Ok(order_ids) => {
                report_entries.push(entry_performance(
                    entry,
                    &order_ids,
                    &activities,
                    &positions,
                ));
            }
            Err(error) => {
                warnings.push(format!(
                    "order_lookup_failed underlying={} strategy={} order_list_id={} error={error}",
                    entry.underlying, entry.strategy, entry.order_list_id,
                ));
                report_entries.push(entry_performance(
                    entry,
                    &EntryOrderIds::default(),
                    &activities,
                    &positions,
                ));
            }
        }
    }

    if args.append_ledger {
        append_closed_entries_to_performance_ledger(&config, &report_entries, &mut warnings)?;
    }
    if args.track_candidates {
        let tracked = track_candidate_outcomes(&client, &data_config, &config, &args).await?;
        warnings.push(format!("candidate_outcomes_appended={tracked}"));
    }

    let opportunities =
        summarize_candidate_ledger(&config.candidate_ledger_dir, args.since, args.until)?;
    let ledger_summary =
        summarize_performance_ledger(&performance_ledger_dir(&config), args.since, args.until)?;
    let candidate_outcomes =
        summarize_candidate_outcomes(&candidate_outcome_dir(&config), args.since, args.until)?;
    let summary = summarize_performance(&report_entries);
    let report = PerformanceReport {
        checked_at_utc: Utc::now().to_rfc3339(),
        account_id: config.fleet_account_id.clone(),
        state_path: config.state_path.display().to_string(),
        candidate_ledger_dir: config.candidate_ledger_dir.display().to_string(),
        opportunities,
        ledger_summary,
        candidate_outcomes,
        summary,
        entries: report_entries,
        warnings,
    };

    if args.send_discord {
        send_discord_digest(&report, args.alerts_env_file.as_deref()).await?;
    }

    if args.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        track_max_candidates: DEFAULT_TRACK_MAX_CANDIDATES,
        track_max_rank: DEFAULT_TRACK_MAX_RANK,
        ..Args::default()
    };
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => args.json_output = true,
            "--append-ledger" => args.append_ledger = true,
            "--send-discord" => args.send_discord = true,
            "--alerts-env-file" => {
                args.alerts_env_file = Some(PathBuf::from(parse_string_arg(
                    "--alerts-env-file",
                    iter.next(),
                )?));
            }
            "--track-candidates" => args.track_candidates = true,
            "--track-date" => {
                args.track_date = Some(parse_date_arg("--track-date", iter.next())?);
            }
            "--track-max-candidates" => {
                args.track_max_candidates =
                    parse_string_arg("--track-max-candidates", iter.next())?.parse()?;
            }
            "--track-max-rank" => {
                args.track_max_rank = parse_string_arg("--track-max-rank", iter.next())?.parse()?;
            }
            "--since" => {
                args.since = Some(parse_date_arg("--since", iter.next())?);
            }
            "--until" => {
                args.until = Some(parse_date_arg("--until", iter.next())?);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            value => anyhow::bail!("unexpected argument: {value}"),
        }
    }
    if let (Some(since), Some(until)) = (args.since, args.until)
        && since > until
    {
        anyhow::bail!("--since must be before or equal to --until");
    }
    Ok(args)
}

fn parse_date_arg(name: &str, value: Option<String>) -> anyhow::Result<NaiveDate> {
    let value = parse_string_arg(name, value)?;
    Ok(NaiveDate::parse_from_str(&value, "%Y-%m-%d")?)
}

fn parse_string_arg(name: &str, value: Option<String>) -> anyhow::Result<String> {
    let Some(value) = value else {
        anyhow::bail!("{name} requires a value");
    };
    Ok(value)
}

fn print_usage() {
    eprintln!(
        "usage: alpaca-performance-report [--json] [--append-ledger] [--send-discord] [--track-candidates] [--since YYYY-MM-DD] [--until YYYY-MM-DD]"
    );
}

fn activity_after_timestamp(
    entries: &[StrategyStateEntry],
    since: Option<NaiveDate>,
) -> Option<String> {
    if let Some(since) = since {
        return since
            .and_hms_opt(0, 0, 0)
            .map(|timestamp| timestamp.and_utc().to_rfc3339());
    }
    let state = nautilus_alpaca::runtime::StrategyState {
        entries: entries.to_vec(),
    };
    let earliest = earliest_entry_timestamp(&state)?;
    DateTime::parse_from_rfc3339(&earliest)
        .ok()
        .map(|timestamp| {
            timestamp
                .with_timezone(&Utc)
                .checked_sub_signed(Duration::days(1))
                .unwrap_or_else(Utc::now)
                .to_rfc3339()
        })
}

async fn resolve_entry_order_ids(
    client: &AlpacaHttpClient,
    entry: &StrategyStateEntry,
) -> anyhow::Result<EntryOrderIds> {
    let mut order_ids = EntryOrderIds::default();
    if let Some(order) = lookup_order(
        client,
        entry.parent_order_id.as_deref(),
        Some(entry.order_list_id.as_str()),
    )
    .await?
    {
        collect_order_ids(&order, &mut order_ids.open);
    }
    if let Some(order) = lookup_order(
        client,
        entry.close_parent_order_id.as_deref(),
        entry.close_order_list_id.as_deref(),
    )
    .await?
    {
        collect_order_ids(&order, &mut order_ids.close);
    }
    add_state_order_id(entry.parent_order_id.as_deref(), &mut order_ids.open);
    add_state_order_id(entry.close_parent_order_id.as_deref(), &mut order_ids.close);
    Ok(order_ids)
}

async fn lookup_order(
    client: &AlpacaHttpClient,
    order_id: Option<&str>,
    client_order_id: Option<&str>,
) -> anyhow::Result<Option<AlpacaOrder>> {
    if let Some(order_id) = order_id.filter(|value| !value.trim().is_empty()) {
        match client.order_by_id(order_id, true).await {
            Ok(order) => return Ok(Some(order)),
            Err(Error::HttpStatus { status, .. }) if status == 404 => {}
            Err(error) => return Err(error.into()),
        }
    }
    if let Some(client_order_id) = client_order_id.filter(|value| !value.trim().is_empty()) {
        match client.order_by_client_order_id(client_order_id, true).await {
            Ok(order) => return Ok(Some(order)),
            Err(Error::HttpStatus { status, .. }) if status == 404 => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn add_state_order_id(order_id: Option<&str>, order_ids: &mut BTreeSet<String>) {
    if let Some(order_id) = order_id.filter(|value| !value.trim().is_empty()) {
        order_ids.insert(order_id.to_string());
    }
}

fn append_closed_entries_to_performance_ledger(
    config: &OptionsEngineConfig,
    entries: &[EntryPerformance],
    warnings: &mut Vec<String>,
) -> anyhow::Result<()> {
    let ledger_dir = performance_ledger_dir(config);
    for entry in entries.iter().filter(|entry| entry.status == "closed") {
        let ledger_date = performance_ledger_date(entry, config);
        let append = append_performance_ledger_record(
            &ledger_dir,
            &ledger_date,
            config.fleet_account_id.as_deref(),
            entry,
        )?;
        if append.appended {
            warnings.push(format!(
                "performance_ledger_appended path={} record_key={}",
                append.path, append.record_key
            ));
        }
    }
    Ok(())
}

fn performance_ledger_dir(config: &OptionsEngineConfig) -> PathBuf {
    env::var("ALPACA_PERFORMANCE_LEDGER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            default_performance_ledger_dir(&config.state_path, config.fleet_account_id.as_deref())
        })
}

fn performance_ledger_date(entry: &EntryPerformance, config: &OptionsEngineConfig) -> String {
    entry
        .closed_at_utc
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|timestamp| {
            timestamp
                .with_timezone(&config.entry_timezone)
                .date_naive()
                .to_string()
        })
        .unwrap_or_else(|| {
            Utc::now()
                .with_timezone(&config.entry_timezone)
                .date_naive()
                .to_string()
        })
}

async fn track_candidate_outcomes(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &OptionsEngineConfig,
    args: &Args,
) -> anyhow::Result<usize> {
    let trade_date = args.track_date.unwrap_or_else(|| {
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
        args.track_max_rank,
        args.track_max_candidates,
    );
    if candidates.is_empty() {
        return Ok(0);
    }

    let symbols = candidates
        .iter()
        .flat_map(|candidate| candidate.symbols.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut request = OptionSnapshotsRequest::for_symbols(symbols);
    request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&request).await?.snapshots;
    let outcome_dir = candidate_outcome_dir(config);
    let mut appended = 0;
    for candidate in candidates.iter_mut() {
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
            if append_deduped_jsonl(&outcome_dir, &candidate.trade_date, &record_key, payload)? {
                appended += 1;
            }
        }
    }
    Ok(appended)
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
    let hypothetical_pnl = pnl_per_contract * 100.0;
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

fn candidate_outcome_dir(config: &OptionsEngineConfig) -> PathBuf {
    config
        .candidate_ledger_dir
        .parent()
        .map(|path| path.join("candidate-outcomes"))
        .unwrap_or_else(|| PathBuf::from("candidate-outcomes"))
}

fn append_deduped_jsonl(
    directory: &Path,
    date: &str,
    record_key: &str,
    payload: Value,
) -> anyhow::Result<bool> {
    fs::create_dir_all(directory)?;
    let path = directory.join(format!("{date}.jsonl"));
    if path.exists() {
        for line in fs::read_to_string(&path)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if record.get("record_key").and_then(Value::as_str) == Some(record_key) {
                return Ok(false);
            }
        }
    }
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
    Ok(true)
}

fn read_jsonl_records(path: &Path) -> anyhow::Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
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

async fn send_discord_digest(
    report: &PerformanceReport,
    alerts_env_file: Option<&Path>,
) -> anyhow::Result<()> {
    load_alerts_env(alerts_env_file)?;
    let webhook_url = env::var("DISCORD_WEBHOOK_URL")
        .or_else(|_| env::var("NAUTILUS_ALPACA_DISCORD_WEBHOOK_URL"))
        .map_err(|_| anyhow::anyhow!("missing DISCORD_WEBHOOK_URL for --send-discord"))?;
    reqwest::Client::new()
        .post(webhook_url)
        .json(&json!({ "content": format_discord_digest(report) }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

fn load_alerts_env(path: Option<&Path>) -> anyhow::Result<()> {
    let path = path
        .map(PathBuf::from)
        .or_else(|| env::var_os("NAUTILUS_ALPACA_ALERTS_ENV_FILE").map(PathBuf::from))
        .unwrap_or_else(default_alerts_env_path);
    if path.exists() {
        dotenvy::from_path_override(&path)?;
    }
    Ok(())
}

fn default_alerts_env_path() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("nautilus-trader")
        .join("alpaca")
        .join("alerts.env")
}

fn format_discord_digest(report: &PerformanceReport) -> String {
    let account = report.account_id.as_deref().unwrap_or("unknown");
    let ledger = &report.ledger_summary;
    format!(
        "**Alpaca performance digest** `{account}`\n\
opportunities={} selected={} submit_results={}\n\
closed={} wins={} losses={} realized={} avg_win={} avg_loss={} largest_loss={}\n\
candidate_outcomes={} hypothetical={}\n\
open_unrealized={} observed_total={}",
        report.opportunities.candidates,
        report.opportunities.selected_candidates,
        report.opportunities.submit_results,
        ledger.records,
        ledger.wins,
        ledger.losses,
        format_money(ledger.realized_pnl),
        format_optional_money(ledger.average_win),
        format_optional_money(ledger.average_loss),
        format_optional_money(ledger.largest_loss),
        report.candidate_outcomes.records,
        format_money(report.candidate_outcomes.hypothetical_pnl),
        format_money(report.summary.open_unrealized_pnl),
        format_money(report.summary.open_unrealized_pnl + ledger.realized_pnl),
    )
}

fn print_human_report(report: &PerformanceReport) {
    println!(
        "performance account={} checked_at={} state_path={}",
        report.account_id.as_deref().unwrap_or("unknown"),
        report.checked_at_utc,
        report.state_path,
    );
    println!(
        "opportunities files={} records={} candidates={} selected={} high_score={} submit_results={} parse_errors={}",
        report.opportunities.files,
        report.opportunities.records,
        report.opportunities.candidates,
        report.opportunities.selected_candidates,
        report.opportunities.high_score_candidates,
        report.opportunities.submit_results,
        report.opportunities.parse_errors,
    );
    println!(
        "performance_ledger files={} records={} wins={} losses={} flats={} realized={} avg_win={} avg_loss={} largest_loss={} warnings={}",
        report.ledger_summary.files,
        report.ledger_summary.records,
        report.ledger_summary.wins,
        report.ledger_summary.losses,
        report.ledger_summary.flats,
        format_money(report.ledger_summary.realized_pnl),
        format_optional_money(report.ledger_summary.average_win),
        format_optional_money(report.ledger_summary.average_loss),
        format_optional_money(report.ledger_summary.largest_loss),
        report.ledger_summary.records_with_warnings,
    );
    println!(
        "candidate_outcomes files={} records={} selected={} traded={} wins={} losses={} flats={} hypothetical={} avg_win={} avg_loss={} largest_loss={} warnings={} parse_errors={}",
        report.candidate_outcomes.files,
        report.candidate_outcomes.records,
        report.candidate_outcomes.selected_records,
        report.candidate_outcomes.traded_records,
        report.candidate_outcomes.wins,
        report.candidate_outcomes.losses,
        report.candidate_outcomes.flats,
        format_money(report.candidate_outcomes.hypothetical_pnl),
        format_optional_money(report.candidate_outcomes.average_win),
        format_optional_money(report.candidate_outcomes.average_loss),
        format_optional_money(report.candidate_outcomes.largest_loss),
        report.candidate_outcomes.records_with_warnings,
        report.candidate_outcomes.parse_errors,
    );
    if !report.candidate_outcomes.by_bucket.is_empty() {
        println!("candidate_outcomes_by_bucket:");
        for (bucket, summary) in &report.candidate_outcomes.by_bucket {
            println!(
                "  bucket={} records={} selected={} traded={} wins={} losses={} flats={} hypothetical={}",
                bucket,
                summary.records,
                summary.selected_records,
                summary.traded_records,
                summary.wins,
                summary.losses,
                summary.flats,
                format_money(summary.hypothetical_pnl),
            );
        }
    }
    println!(
        "entries total={} active={} closed={} canceled={} realized={} missing_realized={}",
        report.summary.entries,
        report.summary.active_entries,
        report.summary.closed_entries,
        report.summary.canceled_entries,
        report.summary.realized_entries,
        report.summary.missing_realized_entries,
    );
    println!(
        "pnl realized={} open_unrealized={} observed_total={}",
        format_money(report.summary.realized_pnl),
        format_money(report.summary.open_unrealized_pnl),
        format_money(report.summary.observed_total_pnl),
    );
    if !report.summary.by_strategy.is_empty() {
        println!("by_strategy:");
        for (strategy, summary) in &report.summary.by_strategy {
            println!(
                "  strategy={} entries={} active={} closed={} canceled={} realized={} missing_realized={} realized_pnl={} open_unrealized={} observed_total={}",
                strategy,
                summary.entries,
                summary.active_entries,
                summary.closed_entries,
                summary.canceled_entries,
                summary.realized_entries,
                summary.missing_realized_entries,
                format_money(summary.realized_pnl),
                format_money(summary.open_unrealized_pnl),
                format_money(summary.observed_total_pnl),
            );
        }
    }
    if !report.entries.is_empty() {
        println!("entries:");
        for entry in &report.entries {
            print_entry(entry);
        }
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn print_entry(entry: &EntryPerformance) {
    println!(
        "  {} {} {} status={} score={:.1} open={} close={} realized={} open_unrealized={} reason={} warnings={}",
        entry.trade_date,
        entry.underlying,
        entry.strategy,
        entry.status,
        entry.score,
        format_optional_money(entry.open.cashflow),
        format_optional_money(entry.close.cashflow),
        format_optional_money(entry.realized_pnl),
        format_optional_money(entry.open_unrealized_pnl),
        entry.close_reason.as_deref().unwrap_or("none"),
        if entry.warnings.is_empty() {
            "none".to_string()
        } else {
            entry.warnings.join(",")
        },
    );
}

fn format_optional_money(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_string(), format_money)
}

fn format_money(value: f64) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let amount = value.abs();
    format!("{sign}${amount:.2}")
}
