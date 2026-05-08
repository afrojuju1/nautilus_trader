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
    collections::BTreeSet,
    env,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{AlpacaOrder, ListActivitiesRequest},
    },
    options_runtime::OptionsEngineConfig,
    performance::{
        CandidateOutcomeTrackingRequest, DEFAULT_CANDIDATE_OUTCOME_MAX_CANDIDATES,
        DEFAULT_CANDIDATE_OUTCOME_MAX_RANK, EntryOrderIds, EntryPerformance, PerformanceReport,
        append_performance_ledger_record, collect_order_ids, default_candidate_outcome_dir,
        default_performance_ledger_dir, earliest_entry_timestamp, entry_in_date_range,
        entry_performance, summarize_candidate_ledger, summarize_candidate_outcomes,
        summarize_performance, summarize_performance_ledger, track_candidate_outcomes,
    },
    runtime::{StrategyStateEntry, load_strategy_state},
};
use serde_json::json;

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
        let tracked = track_candidate_outcomes(
            &client,
            &data_config,
            &config,
            &CandidateOutcomeTrackingRequest {
                trade_date: args.track_date,
                max_candidates: args.track_max_candidates,
                max_rank: args.track_max_rank,
            },
        )
        .await?;
        warnings.push(format!("candidate_outcomes_appended={tracked}"));
    }

    let opportunities =
        summarize_candidate_ledger(&config.candidate_ledger_dir, args.since, args.until)?;
    let ledger_summary =
        summarize_performance_ledger(&performance_ledger_dir(&config), args.since, args.until)?;
    let candidate_outcomes = summarize_candidate_outcomes(
        &default_candidate_outcome_dir(&config),
        args.since,
        args.until,
    )?;
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
        track_max_candidates: DEFAULT_CANDIDATE_OUTCOME_MAX_CANDIDATES,
        track_max_rank: DEFAULT_CANDIDATE_OUTCOME_MAX_RANK,
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
