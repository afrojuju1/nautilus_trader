use std::{collections::BTreeMap, env};

use anyhow::{Context, anyhow, bail};
use chrono::DateTime;
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaOptionBar, OptionBarsRequest},
    },
    runtime_env::load_options_env_file,
};
use serde::Serialize;

#[derive(Clone, Debug)]
struct Args {
    symbols: Vec<String>,
    short_symbol: Option<String>,
    long_symbol: Option<String>,
    start: String,
    end: String,
    timeframe: String,
    feed: Option<String>,
    quantity: u64,
    json_output: bool,
}

#[derive(Debug, Serialize)]
struct BacktestReport {
    start: String,
    end: String,
    timeframe: String,
    feed: String,
    mark_source: &'static str,
    symbols: Vec<SymbolBarSummary>,
    position: Option<PositionBacktest>,
}

#[derive(Debug, Serialize)]
struct SymbolBarSummary {
    symbol: String,
    bars: usize,
    first_timestamp: String,
    last_timestamp: String,
    first_open: f64,
    first_close: f64,
    last_close: f64,
    high: f64,
    low: f64,
    volume: f64,
}

#[derive(Debug, Serialize)]
struct PositionBacktest {
    short_symbol: Option<String>,
    long_symbol: Option<String>,
    quantity: u64,
    entry_timestamp: String,
    exit_timestamp: String,
    entry_net_cashflow: f64,
    exit_net_cashflow: f64,
    pnl: f64,
    pnl_per_contract: f64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_options_env_file()?;

    let args = parse_args()?;
    validate_rfc3339("start", &args.start)?;
    validate_rfc3339("end", &args.end)?;

    let mut config = AlpacaDataClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let feed = args
        .feed
        .clone()
        .unwrap_or_else(|| config.option_feed.as_str().to_string());

    let client = AlpacaHttpClient::from_data_config(&config)?;

    let symbols = requested_symbols(&args);
    if symbols.is_empty() {
        bail!("provide --symbols, --short, or --long");
    }

    let mut request =
        OptionBarsRequest::for_symbols(symbols.clone(), args.timeframe.clone(), args.start.clone());
    request.end = Some(args.end.clone());
    request.feed = Some(feed.clone());

    let response = client.option_bars(&request).await?;

    let summaries = symbols
        .iter()
        .filter_map(|symbol| {
            response
                .bars
                .get(symbol)
                .and_then(|bars| summarize_symbol(symbol, bars))
        })
        .collect::<Vec<_>>();

    let position = simulate_position(&args, &response.bars)?;

    let report = BacktestReport {
        start: args.start,
        end: args.end,
        timeframe: args.timeframe,
        feed,
        mark_source: "option_bar_close",
        symbols: summaries,
        position,
    };

    if args.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
    let mut symbols = Vec::new();
    let mut short_symbol = None;
    let mut long_symbol = None;
    let mut start = None;
    let mut end = None;
    let mut timeframe = "1Min".to_string();
    let mut feed = None;
    let mut quantity = 1;
    let mut json_output = false;

    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--symbols" => {
                symbols.extend(split_symbols(next_value(&mut iter, "--symbols")?));
            }
            "--short" => {
                short_symbol = Some(next_value(&mut iter, "--short")?);
            }
            "--long" => {
                long_symbol = Some(next_value(&mut iter, "--long")?);
            }
            "--start" => {
                start = Some(next_value(&mut iter, "--start")?);
            }
            "--end" => {
                end = Some(next_value(&mut iter, "--end")?);
            }
            "--timeframe" => {
                timeframe = next_value(&mut iter, "--timeframe")?;
            }
            "--feed" => {
                feed = Some(next_value(&mut iter, "--feed")?);
            }
            "--quantity" | "--qty" => {
                quantity = next_value(&mut iter, "--quantity")?
                    .parse::<u64>()
                    .context("--quantity must be a positive integer")?;
                if quantity == 0 {
                    bail!("--quantity must be greater than zero");
                }
            }
            "--json" => {
                json_output = true;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            unknown => {
                bail!("unknown argument {unknown}");
            }
        }
    }

    Ok(Args {
        symbols,
        short_symbol,
        long_symbol,
        start: start.ok_or_else(|| anyhow!("missing --start"))?,
        end: end.ok_or_else(|| anyhow!("missing --end"))?,
        timeframe,
        feed,
        quantity,
        json_output,
    })
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    iter.next()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{flag} requires a value"))
}

fn split_symbols(value: String) -> impl Iterator<Item = String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
}

fn requested_symbols(args: &Args) -> Vec<String> {
    let mut symbols = args.symbols.clone();
    if let Some(symbol) = args.short_symbol.as_ref() {
        symbols.push(symbol.clone());
    }
    if let Some(symbol) = args.long_symbol.as_ref() {
        symbols.push(symbol.clone());
    }
    symbols.sort();
    symbols.dedup();
    symbols
}

fn validate_rfc3339(label: &str, value: &str) -> anyhow::Result<()> {
    DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("--{label} must be RFC3339, for example 2024-03-01T14:30:00Z"))?;
    Ok(())
}

fn summarize_symbol(symbol: &str, bars: &[AlpacaOptionBar]) -> Option<SymbolBarSummary> {
    let sorted = sorted_bars(bars);
    let first = sorted.first()?;
    let last = sorted.last()?;

    let high = sorted
        .iter()
        .filter_map(|bar| bar.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low = sorted
        .iter()
        .filter_map(|bar| bar.low)
        .fold(f64::INFINITY, f64::min);
    let volume = sorted.iter().filter_map(|bar| bar.volume).sum::<u64>() as f64;

    Some(SymbolBarSummary {
        symbol: symbol.to_string(),
        bars: sorted.len(),
        first_timestamp: bar_timestamp(first)?.to_string(),
        last_timestamp: bar_timestamp(last)?.to_string(),
        first_open: first.open.unwrap_or_default(),
        first_close: first.close.unwrap_or_default(),
        last_close: last.close.unwrap_or_default(),
        high: finite_or_default(high),
        low: finite_or_default(low),
        volume,
    })
}

fn simulate_position(
    args: &Args,
    bars_by_symbol: &BTreeMap<String, Vec<AlpacaOptionBar>>,
) -> anyhow::Result<Option<PositionBacktest>> {
    match (args.short_symbol.as_ref(), args.long_symbol.as_ref()) {
        (None, None) => Ok(None),
        (Some(short_symbol), Some(long_symbol)) => {
            simulate_two_leg(args, bars_by_symbol, short_symbol, long_symbol).map(Some)
        }
        (Some(short_symbol), None) => {
            let bars = sorted_symbol_bars(bars_by_symbol, short_symbol)?;
            let entry = bars
                .first()
                .ok_or_else(|| anyhow!("missing bars for {short_symbol}"))?;
            let exit = bars
                .last()
                .ok_or_else(|| anyhow!("missing bars for {short_symbol}"))?;
            let entry_net_cashflow = bar_close(short_symbol, entry)?;
            let exit_net_cashflow = -bar_close(short_symbol, exit)?;
            Ok(Some(position_backtest(
                Some(short_symbol),
                None,
                args.quantity,
                bar_timestamp(entry)
                    .ok_or_else(|| anyhow!("missing entry timestamp for {short_symbol}"))?,
                bar_timestamp(exit).ok_or_else(|| anyhow!("missing exit timestamp for {short_symbol}"))?,
                entry_net_cashflow,
                exit_net_cashflow,
            )))
        }
        (None, Some(long_symbol)) => {
            let bars = sorted_symbol_bars(bars_by_symbol, long_symbol)?;
            let entry = bars
                .first()
                .ok_or_else(|| anyhow!("missing bars for {long_symbol}"))?;
            let exit = bars
                .last()
                .ok_or_else(|| anyhow!("missing bars for {long_symbol}"))?;
            let entry_net_cashflow = -bar_close(long_symbol, entry)?;
            let exit_net_cashflow = bar_close(long_symbol, exit)?;
            Ok(Some(position_backtest(
                None,
                Some(long_symbol),
                args.quantity,
                bar_timestamp(entry)
                    .ok_or_else(|| anyhow!("missing entry timestamp for {long_symbol}"))?,
                bar_timestamp(exit).ok_or_else(|| anyhow!("missing exit timestamp for {long_symbol}"))?,
                entry_net_cashflow,
                exit_net_cashflow,
            )))
        }
    }
}

fn simulate_two_leg(
    args: &Args,
    bars_by_symbol: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    short_symbol: &str,
    long_symbol: &str,
) -> anyhow::Result<PositionBacktest> {
    let short_bars = sorted_symbol_bars(bars_by_symbol, short_symbol)?;
    let long_bars = sorted_symbol_bars(bars_by_symbol, long_symbol)?;
    let (entry_time, entry_short, entry_long) = first_common_bar(&short_bars, &long_bars)
        .ok_or_else(|| anyhow!("no common entry bar for {short_symbol} and {long_symbol}"))?;
    let (exit_time, exit_short, exit_long) = last_common_bar(&short_bars, &long_bars)
        .ok_or_else(|| anyhow!("no common exit bar for {short_symbol} and {long_symbol}"))?;

    let entry_net_cashflow = bar_close(short_symbol, entry_short)? - bar_close(long_symbol, entry_long)?;
    let exit_net_cashflow = bar_close(long_symbol, exit_long)? - bar_close(short_symbol, exit_short)?;

    Ok(position_backtest(
        Some(short_symbol),
        Some(long_symbol),
        args.quantity,
        entry_time,
        exit_time,
        entry_net_cashflow,
        exit_net_cashflow,
    ))
}

fn sorted_symbol_bars<'a>(
    bars_by_symbol: &'a BTreeMap<String, Vec<AlpacaOptionBar>>,
    symbol: &str,
) -> anyhow::Result<Vec<&'a AlpacaOptionBar>> {
    let bars = bars_by_symbol
        .get(symbol)
        .ok_or_else(|| anyhow!("missing bars for {symbol}"))?;
    Ok(sorted_bars(bars))
}

fn sorted_bars(bars: &[AlpacaOptionBar]) -> Vec<&AlpacaOptionBar> {
    let mut sorted = bars.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    sorted
}

fn bar_timestamp(bar: &AlpacaOptionBar) -> Option<&str> {
    bar.timestamp.as_deref().filter(|value| !value.is_empty())
}

fn bar_close(symbol: &str, bar: &AlpacaOptionBar) -> anyhow::Result<f64> {
    bar.close
        .filter(|value| *value > 0.0)
        .ok_or_else(|| anyhow!("missing close price for {symbol}"))
}

fn finite_or_default(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn first_common_bar<'a>(
    short_bars: &'a [&'a AlpacaOptionBar],
    long_bars: &'a [&'a AlpacaOptionBar],
) -> Option<(&'a str, &'a AlpacaOptionBar, &'a AlpacaOptionBar)> {
    let long_by_time = long_bars
        .iter()
        .filter_map(|bar| bar_timestamp(bar).map(|timestamp| (timestamp, *bar)))
        .collect::<BTreeMap<_, _>>();

    short_bars.iter().find_map(|short_bar| {
        let timestamp = bar_timestamp(short_bar)?;
        long_by_time
            .get(timestamp)
            .map(|long_bar| (timestamp, *short_bar, *long_bar))
    })
}

fn last_common_bar<'a>(
    short_bars: &'a [&'a AlpacaOptionBar],
    long_bars: &'a [&'a AlpacaOptionBar],
) -> Option<(&'a str, &'a AlpacaOptionBar, &'a AlpacaOptionBar)> {
    let long_by_time = long_bars
        .iter()
        .filter_map(|bar| bar_timestamp(bar).map(|timestamp| (timestamp, *bar)))
        .collect::<BTreeMap<_, _>>();

    short_bars.iter().rev().find_map(|short_bar| {
        let timestamp = bar_timestamp(short_bar)?;
        long_by_time
            .get(timestamp)
            .map(|long_bar| (timestamp, *short_bar, *long_bar))
    })
}

fn position_backtest(
    short_symbol: Option<&str>,
    long_symbol: Option<&str>,
    quantity: u64,
    entry_timestamp: &str,
    exit_timestamp: &str,
    entry_net_cashflow: f64,
    exit_net_cashflow: f64,
) -> PositionBacktest {
    let pnl_per_contract = entry_net_cashflow + exit_net_cashflow;
    let pnl = pnl_per_contract * 100.0 * quantity as f64;

    PositionBacktest {
        short_symbol: short_symbol.map(str::to_string),
        long_symbol: long_symbol.map(str::to_string),
        quantity,
        entry_timestamp: entry_timestamp.to_string(),
        exit_timestamp: exit_timestamp.to_string(),
        entry_net_cashflow,
        exit_net_cashflow,
        pnl,
        pnl_per_contract,
    }
}

fn print_report(report: &BacktestReport) {
    println!(
        "Alpaca options backtest {} -> {} timeframe={} feed={} mark_source={}",
        report.start, report.end, report.timeframe, report.feed, report.mark_source
    );

    for symbol in &report.symbols {
        println!(
            "{} bars={} first={} last={} first_close={:.4} last_close={:.4} high={:.4} low={:.4} volume={:.0}",
            symbol.symbol,
            symbol.bars,
            symbol.first_timestamp,
            symbol.last_timestamp,
            symbol.first_close,
            symbol.last_close,
            symbol.high,
            symbol.low,
            symbol.volume
        );
    }

    if let Some(position) = report.position.as_ref() {
        println!(
            "position short={} long={} qty={} entry={} exit={} entry_cashflow={:.4} exit_cashflow={:.4} pnl_per_contract={:.4} pnl={:.2}",
            position.short_symbol.as_deref().unwrap_or("-"),
            position.long_symbol.as_deref().unwrap_or("-"),
            position.quantity,
            position.entry_timestamp,
            position.exit_timestamp,
            position.entry_net_cashflow,
            position.exit_net_cashflow,
            position.pnl_per_contract,
            position.pnl
        );
    }
}

fn print_usage() {
    println!(
        "Usage: alpaca-options-backtest --start 2024-03-01T14:30:00Z --end 2024-03-01T20:00:00Z [--symbols SYM1,SYM2] [--short SYM] [--long SYM] [--timeframe 1Min] [--feed indicative] [--quantity 1] [--json]"
    );
}
