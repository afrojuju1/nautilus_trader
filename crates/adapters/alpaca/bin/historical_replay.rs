//! Historical Alpaca option candidate replay report.

use std::env;

use chrono::NaiveDate;
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::client::AlpacaHttpClient,
    options_runtime::AlpacaOptionsRuntimeConfig,
    performance::{
        HistoricalReplayBucketSummary, HistoricalReplayRecord, HistoricalReplayReport,
        HistoricalReplayRequest, replay_historical_candidates,
    },
};

#[derive(Debug)]
struct Args {
    json_output: bool,
    include_records: bool,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
    max_candidates: usize,
    max_rank: u64,
    lookahead_minutes: i64,
    timeframe: String,
}

impl Default for Args {
    fn default() -> Self {
        let defaults = HistoricalReplayRequest::default();
        Self {
            json_output: false,
            include_records: defaults.include_records,
            since: defaults.since,
            until: defaults.until,
            max_candidates: defaults.max_candidates,
            max_rank: defaults.max_rank,
            lookahead_minutes: defaults.lookahead_minutes,
            timeframe: defaults.timeframe,
        }
    }
}

pub(crate) async fn run() -> anyhow::Result<()> {
    let args = parse_args()?;
    let config = AlpacaOptionsRuntimeConfig::from_runtime_env_with_storage().await?;
    if config.storage_repository.is_none() {
        anyhow::bail!("ALPACA_STORAGE_DATABASE_URL is required for alpaca-ops replay");
    }

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    let client = AlpacaHttpClient::from_data_config(&data_config)?;

    let report = replay_historical_candidates(
        &client,
        &config,
        &HistoricalReplayRequest {
            since: args.since,
            until: args.until,
            max_candidates: args.max_candidates,
            max_rank: args.max_rank,
            lookahead_minutes: args.lookahead_minutes,
            timeframe: args.timeframe,
            include_records: args.include_records,
        },
    )
    .await?;

    if args.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args::default();
    let mut iter = crate::ops_args().into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => args.json_output = true,
            "--include-records" => args.include_records = true,
            "--since" => args.since = Some(parse_date_arg("--since", iter.next())?),
            "--until" => args.until = Some(parse_date_arg("--until", iter.next())?),
            "--max-candidates" => {
                args.max_candidates = parse_string_arg("--max-candidates", iter.next())?.parse()?;
            }
            "--max-rank" => {
                args.max_rank = parse_string_arg("--max-rank", iter.next())?.parse()?;
            }
            "--lookahead-minutes" => {
                args.lookahead_minutes =
                    parse_string_arg("--lookahead-minutes", iter.next())?.parse()?;
            }
            "--timeframe" => {
                args.timeframe = parse_string_arg("--timeframe", iter.next())?;
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
    if args.lookahead_minutes <= 0 {
        anyhow::bail!("--lookahead-minutes must be positive");
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
        "usage: alpaca-ops replay [--json] [--include-records] [--since YYYY-MM-DD] [--until YYYY-MM-DD] [--max-candidates N] [--max-rank N] [--lookahead-minutes N] [--timeframe 1Min]"
    );
}

fn print_human_report(report: &HistoricalReplayReport) {
    println!(
        "historical_replay account={} checked_at={} since={} until={} timeframe={} lookahead_minutes={} max_rank={} max_candidates={}",
        report.account_id.as_deref().unwrap_or("unknown"),
        report.checked_at_utc,
        report.since,
        report.until,
        report.timeframe,
        report.lookahead_minutes,
        report.max_rank,
        report.max_candidates,
    );
    println!(
        "ledger records={} candidates={} evaluated={} missing={} warnings={}",
        report.ledger_records,
        report.candidate_records,
        report.evaluated_records,
        report.missing_records,
        report.warnings.len(),
    );
    print_bucket("summary", "", &report.summary);
    print_bucket_map("by_strategy", &report.by_strategy);
    print_bucket_map("by_underlying", &report.by_underlying);
    print_bucket_map("by_dte_bucket", &report.by_dte_bucket);
    print_bucket_map("by_score_bucket", &report.by_score_bucket);
    print_bucket_map("by_delta_bucket", &report.by_delta_bucket);
    print_bucket_map("by_spread_width_bucket", &report.by_spread_width_bucket);
    print_bucket_map("by_liquidity_bucket", &report.by_liquidity_bucket);
    print_bucket_map("by_decision_reason", &report.by_decision_reason);
    if !report.records.is_empty() {
        println!("records:");
        for record in &report.records {
            print_record(record);
        }
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn print_bucket_map(
    name: &str,
    buckets: &std::collections::BTreeMap<String, HistoricalReplayBucketSummary>,
) {
    if buckets.is_empty() {
        return;
    }
    println!("{name}:");
    for (bucket, summary) in buckets {
        print_bucket("  bucket", bucket, summary);
    }
}

fn print_bucket(prefix: &str, name: &str, summary: &HistoricalReplayBucketSummary) {
    let name = if name.is_empty() {
        String::new()
    } else {
        format!(" name={name}")
    };
    println!(
        "{prefix}:{name} records={} selected={} submitted={} rejected={} virtual={} evaluated={} missing={} wins={} losses={} flats={} hypothetical={} avg_win={} avg_loss={} largest_loss={} total_volume={} avg_volume={}",
        summary.records,
        summary.selected_records,
        summary.submitted_records,
        summary.rejected_records,
        summary.virtual_records,
        summary.evaluated_records,
        summary.missing_records,
        summary.wins,
        summary.losses,
        summary.flats,
        format_money(summary.hypothetical_pnl),
        format_optional_money(summary.average_win),
        format_optional_money(summary.average_loss),
        format_optional_money(summary.largest_loss),
        summary.total_volume,
        summary
            .average_volume
            .map_or_else(|| "n/a".to_string(), |value| format!("{value:.1}")),
    );
}

fn print_record(record: &HistoricalReplayRecord) {
    println!(
        "  {} {} {} rank={} score={} decision_reason={} action={} selected_reason={} dte={} delta={} width={} score_bucket={} liquidity={} close={} pnl={} volume={} warnings={}",
        record.trade_date,
        record.underlying,
        record.strategy,
        record
            .rank
            .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
        record
            .score
            .map_or_else(|| "n/a".to_string(), |value| format!("{value:.1}")),
        &record.decision_reason,
        record.selected_action.as_deref().unwrap_or("n/a"),
        record.selected_reason.as_deref().unwrap_or("n/a"),
        record
            .dte
            .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
        record
            .delta_abs
            .map_or_else(|| "n/a".to_string(), |value| format!("{value:.2}")),
        record
            .spread_width
            .map_or_else(|| "n/a".to_string(), |value| format!("{value:.2}")),
        record.score_bucket,
        record.liquidity_bucket,
        format_optional_money(record.close_net_premium),
        format_optional_money(record.hypothetical_pnl),
        record.total_volume,
        if record.warnings.is_empty() {
            "none".to_string()
        } else {
            record.warnings.join(",")
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
