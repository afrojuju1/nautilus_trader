use std::{collections::BTreeMap, env};

use anyhow::{Context, anyhow, bail};
use chrono::{Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaOptionBar, AlpacaOptionContract, AlpacaOptionGreeks, AlpacaOptionQuote,
            AlpacaOptionSnapshot, AlpacaStockBar, ListOptionContractsRequest, OptionBarsRequest,
            StockBarsRequest,
        },
    },
    options_runtime::{
        OptionsEngineConfig, SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry,
        SelectedNakedOptionEntry, SelectedOptionsEntry,
    },
    runtime_env::load_options_env_file,
    strategy::{
        CreditSpreadKind, DebitSpreadKind, NakedOptionCapitalContext, NakedOptionKind,
        scan_credit_spread_snapshot_at, scan_debit_spread_snapshot_at,
        scan_iron_condor_snapshots_at, scan_naked_option_snapshot_at,
    },
};
use serde::Serialize;

const DAYS_PER_YEAR: f64 = 365.25;
const SCANNER_RISK_FREE_RATE: f64 = 0.0425;
const OPTION_CONTRACT_MULTIPLIER: f64 = 100.0;

#[derive(Clone, Debug)]
struct Args {
    start: NaiveDate,
    end: NaiveDate,
    entry_time: Option<NaiveTime>,
    exit_time: Option<NaiveTime>,
    underlyings: Option<Vec<String>>,
    strategies: Option<Vec<String>>,
    timeframe: String,
    option_feed: Option<String>,
    stock_feed: Option<String>,
    assumed_iv: f64,
    synthetic_spread_pct: f64,
    quantity: Option<u64>,
    json_output: bool,
}

#[derive(Debug, Serialize)]
struct BacktestReport {
    start: String,
    end: String,
    entry_time: String,
    exit_time: String,
    timeframe: String,
    option_feed: String,
    stock_feed: String,
    mark_source: &'static str,
    assumed_iv: f64,
    synthetic_spread_pct: f64,
    underlyings: Vec<String>,
    strategies: Vec<String>,
    summary: BacktestSummary,
    days: Vec<BacktestDay>,
}

#[derive(Default, Debug, Serialize)]
struct BacktestSummary {
    scan_days: usize,
    evaluated_underlying_days: usize,
    selected_trades: usize,
    closed_trades: usize,
    winning_trades: usize,
    total_pnl: f64,
    average_pnl: f64,
    win_rate: f64,
    by_strategy: BTreeMap<String, StrategySummary>,
}

#[derive(Default, Debug, Serialize)]
struct StrategySummary {
    selected_trades: usize,
    closed_trades: usize,
    winning_trades: usize,
    total_pnl: f64,
    average_pnl: f64,
    win_rate: f64,
}

#[derive(Debug, Serialize)]
struct BacktestDay {
    trade_date: String,
    underlying: String,
    contracts: usize,
    snapshots: usize,
    underlying_price: Option<f64>,
    diagnostics: Vec<ScannerDiagnostic>,
    selected: Option<TradeBacktest>,
}

#[derive(Debug, Serialize)]
struct ScannerDiagnostic {
    strategy: String,
    contracts: usize,
    snapshots: usize,
    scoreable: usize,
    candidates: usize,
    rejections: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct TradeBacktest {
    strategy: String,
    underlying: String,
    trade_date: String,
    entry_timestamp: String,
    exit_timestamp: String,
    quantity: u64,
    score: f64,
    premium_kind: String,
    entry_premium: f64,
    entry_net_cashflow: f64,
    exit_net_cashflow: Option<f64>,
    pnl: Option<f64>,
    legs: Vec<BacktestLeg>,
    exit_status: String,
}

#[derive(Clone, Debug, Serialize)]
struct BacktestLeg {
    symbol: String,
    side: LegSide,
    entry_close: Option<f64>,
    exit_close: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum LegSide {
    Long,
    Short,
}

impl LegSide {
    const fn entry_sign(self) -> f64 {
        match self {
            Self::Long => -1.0,
            Self::Short => 1.0,
        }
    }

    const fn exit_sign(self) -> f64 {
        match self {
            Self::Long => 1.0,
            Self::Short => -1.0,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_options_env_file()?;

    let args = parse_args()?;
    if args.end < args.start {
        bail!("--end must be on or after --start");
    }

    let mut config = OptionsEngineConfig::from_runtime_env()?;
    apply_backtest_overrides(&mut config, &args)?;

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let option_feed = args
        .option_feed
        .clone()
        .unwrap_or_else(|| data_config.option_feed.as_str().to_string());
    let stock_feed = args
        .stock_feed
        .clone()
        .unwrap_or_else(|| data_config.stock_feed.as_str().to_string());

    let entry_time = args.entry_time.unwrap_or(config.entry_start);
    let exit_time = args.exit_time.unwrap_or(config.close_end);
    let client = AlpacaHttpClient::from_data_config(&data_config)?;

    let mut days = Vec::new();
    for trade_date in trading_dates(args.start, args.end) {
        for underlying in config.underlyings.clone() {
            let result = backtest_underlying_day(
                &client,
                &config,
                &underlying,
                trade_date,
                entry_time,
                exit_time,
                &args.timeframe,
                &option_feed,
                &stock_feed,
                args.assumed_iv,
                args.synthetic_spread_pct,
            )
            .await?;
            days.push(result);
        }
    }

    let summary = summarize(&days);
    let report = BacktestReport {
        start: args.start.to_string(),
        end: args.end.to_string(),
        entry_time: entry_time.format("%H:%M:%S").to_string(),
        exit_time: exit_time.format("%H:%M:%S").to_string(),
        timeframe: args.timeframe,
        option_feed,
        stock_feed,
        mark_source: "historical_bar_close_with_synthetic_quote",
        assumed_iv: args.assumed_iv,
        synthetic_spread_pct: args.synthetic_spread_pct,
        underlyings: config.underlyings,
        strategies: enabled_strategy_names(&config),
        summary,
        days,
    };

    if args.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    Ok(())
}

async fn backtest_underlying_day(
    client: &AlpacaHttpClient,
    config: &OptionsEngineConfig,
    underlying: &str,
    trade_date: NaiveDate,
    entry_time: NaiveTime,
    exit_time: NaiveTime,
    timeframe: &str,
    option_feed: &str,
    stock_feed: &str,
    assumed_iv: f64,
    synthetic_spread_pct: f64,
) -> anyhow::Result<BacktestDay> {
    let entry_timestamp = timestamp_for(config, trade_date, entry_time)?;
    let exit_timestamp = timestamp_for(config, trade_date, exit_time)?;
    let entry_end = timestamp_plus_minutes(config, trade_date, entry_time, 1)?;
    let exit_end = timestamp_plus_minutes(config, trade_date, exit_time, 1)?;
    let (min_dte, max_dte) = scanner_dte_window(config);

    let contracts = load_contracts(client, underlying, trade_date, min_dte, max_dte).await?;
    let symbols = contracts
        .iter()
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();

    let underlying_price = load_underlying_price_at(
        client,
        underlying,
        timeframe,
        stock_feed,
        &entry_timestamp,
        &entry_end,
    )
    .await?;

    let option_bars = if symbols.is_empty() {
        BTreeMap::new()
    } else {
        load_option_bars(
            client,
            symbols.clone(),
            timeframe,
            option_feed,
            &entry_timestamp,
            &entry_end,
        )
        .await?
    };
    let snapshots = build_snapshot_map(
        &contracts,
        &option_bars,
        trade_date,
        underlying_price,
        assumed_iv,
        synthetic_spread_pct,
    );

    let mut diagnostics = Vec::new();
    let selected = select_historical_entry(
        config,
        underlying,
        trade_date,
        &contracts,
        &snapshots,
        underlying_price,
        &mut diagnostics,
    );

    let selected = match selected {
        Some(entry) => Some(
            simulate_selected_entry(
                client,
                entry,
                config.quantity,
                trade_date,
                &entry_timestamp,
                &exit_timestamp,
                &exit_end,
                timeframe,
                option_feed,
                &option_bars,
            )
            .await?,
        ),
        None => None,
    };

    Ok(BacktestDay {
        trade_date: trade_date.to_string(),
        underlying: underlying.to_string(),
        contracts: contracts.len(),
        snapshots: snapshots.len(),
        underlying_price,
        diagnostics,
        selected,
    })
}

fn select_historical_entry(
    config: &OptionsEngineConfig,
    underlying: &str,
    trade_date: NaiveDate,
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    underlying_price: Option<f64>,
    diagnostics: &mut Vec<ScannerDiagnostic>,
) -> Option<SelectedOptionsEntry> {
    let put_contracts = contracts_for_type(contracts, "put");
    let call_contracts = contracts_for_type(contracts, "call");
    let mut selected: Option<SelectedOptionsEntry> = None;

    for kind in &config.spread_kinds {
        let strategy_contracts = match kind {
            CreditSpreadKind::Put => &put_contracts,
            CreditSpreadKind::Call => &call_contracts,
        };
        let result = scan_credit_spread_snapshot_at(
            underlying.to_string(),
            strategy_contracts,
            snapshots,
            &config.scanner,
            *kind,
            trade_date,
        );
        diagnostics.push(ScannerDiagnostic {
            strategy: credit_strategy_name(*kind).to_string(),
            contracts: result.contract_count,
            snapshots: result.snapshot_count,
            scoreable: result.scoreable_count,
            candidates: result.candidates.len(),
            rejections: result.rejection_counts.clone(),
        });
        if let Some(best) = result.candidates.first()
            && selected.as_ref().is_none_or(|current| best.score > current.score())
        {
            selected = Some(SelectedOptionsEntry::Credit(SelectedEntry {
                underlying: underlying.to_string(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }
    }

    if config.iron_condor_enabled {
        let result = scan_iron_condor_snapshots_at(
            underlying.to_string(),
            &put_contracts,
            snapshots,
            &call_contracts,
            snapshots,
            &config.iron_condor_scanner,
            trade_date,
        );
        diagnostics.push(ScannerDiagnostic {
            strategy: "iron_condor".to_string(),
            contracts: result.contract_count,
            snapshots: result.snapshot_count,
            scoreable: result.scoreable_count,
            candidates: result.candidates.len(),
            rejections: result.rejection_counts.clone(),
        });
        if let Some(best) = result.candidates.first()
            && selected.as_ref().is_none_or(|current| best.score > current.score())
        {
            selected = Some(SelectedOptionsEntry::IronCondor(SelectedIronCondorEntry {
                underlying: underlying.to_string(),
                candidate: best.clone(),
            }));
        }
    }

    for kind in &config.debit_kinds {
        let strategy_contracts = match kind {
            DebitSpreadKind::Put => &put_contracts,
            DebitSpreadKind::Call => &call_contracts,
        };
        let result = scan_debit_spread_snapshot_at(
            underlying.to_string(),
            strategy_contracts,
            snapshots,
            &config.debit_scanner,
            *kind,
            trade_date,
        );
        diagnostics.push(ScannerDiagnostic {
            strategy: debit_strategy_name(*kind).to_string(),
            contracts: result.contract_count,
            snapshots: result.snapshot_count,
            scoreable: result.scoreable_count,
            candidates: result.candidates.len(),
            rejections: result.rejection_counts.clone(),
        });
        if let Some(best) = result.candidates.first()
            && selected.as_ref().is_none_or(|current| best.score > current.score())
        {
            selected = Some(SelectedOptionsEntry::Debit(SelectedDebitEntry {
                underlying: underlying.to_string(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }
    }

    if let Some(price) = underlying_price {
        for kind in &config.naked_kinds {
            let strategy_contracts = if kind.is_put() { &put_contracts } else { &call_contracts };
            let capital = NakedOptionCapitalContext {
                options_buying_power: None,
                quantity: config.quantity,
            };
            let result = scan_naked_option_snapshot_at(
                underlying.to_string(),
                strategy_contracts,
                snapshots,
                config.naked_scanner_for(*kind),
                *kind,
                price,
                Some(capital),
                trade_date,
            );
            diagnostics.push(ScannerDiagnostic {
                strategy: naked_strategy_name(*kind).to_string(),
                contracts: result.contract_count,
                snapshots: result.snapshot_count,
                scoreable: result.scoreable_count,
                candidates: result.candidates.len(),
                rejections: result.rejection_counts.clone(),
            });
            if let Some(best) = result.candidates.first()
                && selected.as_ref().is_none_or(|current| best.score > current.score())
            {
                selected = Some(SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
                    underlying: underlying.to_string(),
                    kind: *kind,
                    candidate: best.clone(),
                }));
            }
        }
    }

    selected
}

async fn simulate_selected_entry(
    client: &AlpacaHttpClient,
    entry: SelectedOptionsEntry,
    quantity: u64,
    trade_date: NaiveDate,
    entry_timestamp: &str,
    exit_timestamp: &str,
    exit_end: &str,
    timeframe: &str,
    option_feed: &str,
    entry_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
) -> anyhow::Result<TradeBacktest> {
    let descriptor = entry.descriptor();
    let mut legs = selected_legs(&entry);
    for leg in &mut legs {
        leg.entry_close = entry_bars
            .get(&leg.symbol)
            .and_then(|bars| first_bar_close(bars));
    }

    let symbols = legs.iter().map(|leg| leg.symbol.clone()).collect::<Vec<_>>();
    let exit_bars = load_option_bars(
        client,
        symbols,
        timeframe,
        option_feed,
        exit_timestamp,
        exit_end,
    )
    .await?;

    let mut exit_net_cashflow = 0.0;
    let mut missing_exit = false;
    for leg in &mut legs {
        leg.exit_close = exit_bars
            .get(&leg.symbol)
            .and_then(|bars| first_bar_close(bars));
        if let Some(exit_close) = leg.exit_close {
            exit_net_cashflow += leg.side.exit_sign() * exit_close;
        } else {
            missing_exit = true;
        }
    }

    let entry_net_cashflow = match descriptor.premium_kind.as_str() {
        "credit" => descriptor.premium,
        "debit" => -descriptor.premium,
        _ => legs
            .iter()
            .filter_map(|leg| leg.entry_close.map(|price| leg.side.entry_sign() * price))
            .sum(),
    };
    let pnl_per_contract = entry_net_cashflow + exit_net_cashflow;
    let pnl = (!missing_exit).then_some(pnl_per_contract * OPTION_CONTRACT_MULTIPLIER * quantity as f64);

    Ok(TradeBacktest {
        strategy: descriptor.strategy.to_string(),
        underlying: descriptor.underlying,
        trade_date: trade_date.to_string(),
        entry_timestamp: entry_timestamp.to_string(),
        exit_timestamp: exit_timestamp.to_string(),
        quantity,
        score: descriptor.score,
        premium_kind: descriptor.premium_kind.as_str().to_string(),
        entry_premium: descriptor.premium,
        entry_net_cashflow,
        exit_net_cashflow: (!missing_exit).then_some(exit_net_cashflow),
        pnl,
        legs,
        exit_status: if missing_exit { "missing_exit_bar" } else { "closed" }.to_string(),
    })
}

async fn load_contracts(
    client: &AlpacaHttpClient,
    underlying: &str,
    scan_date: NaiveDate,
    min_dte: i64,
    max_dte: i64,
) -> anyhow::Result<Vec<AlpacaOptionContract>> {
    let min_expiration = (scan_date + Duration::days(min_dte)).to_string();
    let max_expiration = (scan_date + Duration::days(max_dte)).to_string();
    let mut contracts = Vec::new();

    for status in ["active", "inactive"] {
        let mut request = ListOptionContractsRequest::active([underlying.to_string()]);
        request.status = status.to_string();
        request.expiration_date_gte = Some(min_expiration.clone());
        request.expiration_date_lte = Some(max_expiration.clone());
        let mut response = client
            .list_option_contracts(&request)
            .await
            .with_context(|| format!("failed to load {status} contracts for {underlying}"))?;
        contracts.append(&mut response.option_contracts);
    }

    contracts.sort_by(|left, right| left.symbol.cmp(&right.symbol));
    contracts.dedup_by(|left, right| left.symbol == right.symbol);
    Ok(contracts)
}

async fn load_option_bars(
    client: &AlpacaHttpClient,
    symbols: Vec<String>,
    timeframe: &str,
    feed: &str,
    start: &str,
    end: &str,
) -> anyhow::Result<BTreeMap<String, Vec<AlpacaOptionBar>>> {
    if symbols.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut request = OptionBarsRequest::for_symbols(symbols, timeframe.to_string(), start.to_string());
    request.end = Some(end.to_string());
    request.feed = Some(feed.to_string());
    Ok(client.option_bars(&request).await?.bars)
}

async fn load_underlying_price_at(
    client: &AlpacaHttpClient,
    symbol: &str,
    timeframe: &str,
    feed: &str,
    start: &str,
    end: &str,
) -> anyhow::Result<Option<f64>> {
    let mut request = StockBarsRequest::for_symbols([symbol.to_string()], timeframe.to_string(), start.to_string());
    request.end = Some(end.to_string());
    request.feed = Some(feed.to_string());
    let response = client.stock_bars(&request).await?;
    Ok(response
        .bars
        .get(symbol)
        .and_then(|bars| first_stock_bar_close(bars)))
}

fn build_snapshot_map(
    contracts: &[AlpacaOptionContract],
    bars_by_symbol: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    scan_date: NaiveDate,
    underlying_price: Option<f64>,
    assumed_iv: f64,
    synthetic_spread_pct: f64,
) -> BTreeMap<String, AlpacaOptionSnapshot> {
    let mut snapshots = BTreeMap::new();
    for contract in contracts {
        let Some(bar) = bars_by_symbol
            .get(&contract.symbol)
            .and_then(|bars| first_option_bar(bars))
        else {
            continue;
        };
        let Some(close) = bar.close.filter(|value| *value > 0.0) else {
            continue;
        };
        let half_spread = (synthetic_spread_pct.max(0.0) / 2.0).min(0.49);
        let bid = (close * (1.0 - half_spread)).max(0.01);
        let ask = (close * (1.0 + half_spread)).max(bid);
        let delta = underlying_price.and_then(|price| estimated_delta(contract, scan_date, price, assumed_iv));
        snapshots.insert(
            contract.symbol.clone(),
            AlpacaOptionSnapshot {
                latest_quote: Some(AlpacaOptionQuote {
                    ask_price: Some(ask),
                    ask_size: Some(10),
                    bid_price: Some(bid),
                    bid_size: Some(10),
                    timestamp: bar.timestamp.clone(),
                }),
                latest_trade: None,
                minute_bar: Some(bar.clone()),
                daily_bar: Some(bar.clone()),
                prev_daily_bar: None,
                greeks: Some(AlpacaOptionGreeks {
                    delta,
                    gamma: None,
                    rho: None,
                    theta: None,
                    vega: None,
                }),
                implied_volatility: Some(assumed_iv),
            },
        );
    }
    snapshots
}

fn estimated_delta(
    contract: &AlpacaOptionContract,
    scan_date: NaiveDate,
    underlying_price: f64,
    assumed_iv: f64,
) -> Option<f64> {
    let strike = contract.strike_price.parse::<f64>().ok()?;
    let expiration = NaiveDate::parse_from_str(&contract.expiration_date, "%Y-%m-%d").ok()?;
    let dte = expiration.signed_duration_since(scan_date).num_days().max(1) as f64;
    let years = dte / DAYS_PER_YEAR;
    if underlying_price <= 0.0 || strike <= 0.0 || assumed_iv <= 0.0 || years <= 0.0 {
        return None;
    }
    let d1 = ((underlying_price / strike).ln()
        + (SCANNER_RISK_FREE_RATE + 0.5 * assumed_iv * assumed_iv) * years)
        / (assumed_iv * years.sqrt());
    let call_delta = normal_cdf(d1);
    if contract.option_type.eq_ignore_ascii_case("call") {
        Some(call_delta)
    } else {
        Some(call_delta - 1.0)
    }
}

fn normal_cdf(value: f64) -> f64 {
    1.0 / (1.0 + (-1.702 * value).exp())
}

fn selected_legs(entry: &SelectedOptionsEntry) -> Vec<BacktestLeg> {
    match entry {
        SelectedOptionsEntry::Credit(entry) => vec![
            BacktestLeg {
                symbol: entry.candidate.short.symbol.clone(),
                side: LegSide::Short,
                entry_close: None,
                exit_close: None,
            },
            BacktestLeg {
                symbol: entry.candidate.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
            },
        ],
        SelectedOptionsEntry::IronCondor(entry) => vec![
            BacktestLeg {
                symbol: entry.candidate.put.short.symbol.clone(),
                side: LegSide::Short,
                entry_close: None,
                exit_close: None,
            },
            BacktestLeg {
                symbol: entry.candidate.put.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
            },
            BacktestLeg {
                symbol: entry.candidate.call.short.symbol.clone(),
                side: LegSide::Short,
                entry_close: None,
                exit_close: None,
            },
            BacktestLeg {
                symbol: entry.candidate.call.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
            },
        ],
        SelectedOptionsEntry::Debit(entry) => vec![
            BacktestLeg {
                symbol: entry.candidate.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
            },
            BacktestLeg {
                symbol: entry.candidate.short.symbol.clone(),
                side: LegSide::Short,
                entry_close: None,
                exit_close: None,
            },
        ],
        SelectedOptionsEntry::NakedOption(entry) => vec![BacktestLeg {
            symbol: entry.candidate.short.symbol.clone(),
            side: LegSide::Short,
            entry_close: None,
            exit_close: None,
        }],
    }
}

fn contracts_for_type(contracts: &[AlpacaOptionContract], option_type: &str) -> Vec<AlpacaOptionContract> {
    contracts
        .iter()
        .filter(|contract| contract.option_type.eq_ignore_ascii_case(option_type))
        .cloned()
        .collect()
}

fn first_option_bar(bars: &[AlpacaOptionBar]) -> Option<&AlpacaOptionBar> {
    bars.iter()
        .filter(|bar| bar.close.is_some_and(|value| value > 0.0))
        .min_by(|left, right| left.timestamp.cmp(&right.timestamp))
}

fn first_bar_close(bars: &[AlpacaOptionBar]) -> Option<f64> {
    first_option_bar(bars).and_then(|bar| bar.close)
}

fn first_stock_bar_close(bars: &[AlpacaStockBar]) -> Option<f64> {
    bars.iter()
        .filter_map(|bar| bar.close.filter(|value| *value > 0.0).map(|close| (bar.timestamp.clone(), close)))
        .min_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, close)| close)
}

fn scanner_dte_window(config: &OptionsEngineConfig) -> (i64, i64) {
    let mut min_dte = Vec::new();
    let mut max_dte = Vec::new();
    if !config.spread_kinds.is_empty() {
        min_dte.push(config.scanner.min_dte);
        max_dte.push(config.scanner.max_dte);
    }
    if config.iron_condor_enabled {
        min_dte.push(config.iron_condor_scanner.credit.min_dte);
        max_dte.push(config.iron_condor_scanner.credit.max_dte);
    }
    if !config.debit_kinds.is_empty() {
        min_dte.push(config.debit_scanner.min_dte);
        max_dte.push(config.debit_scanner.max_dte);
    }
    for kind in &config.naked_kinds {
        let scanner = config.naked_scanner_for(*kind);
        min_dte.push(scanner.min_dte);
        max_dte.push(scanner.max_dte);
    }
    (
        min_dte.into_iter().min().unwrap_or(0),
        max_dte.into_iter().max().unwrap_or(45),
    )
}

fn timestamp_for(config: &OptionsEngineConfig, date: NaiveDate, time: NaiveTime) -> anyhow::Result<String> {
    let local = date.and_time(time);
    let timestamp = config
        .entry_timezone
        .from_local_datetime(&local)
        .single()
        .ok_or_else(|| anyhow!("ambiguous or invalid local timestamp {local}"))?;
    Ok(timestamp.with_timezone(&Utc).to_rfc3339())
}

fn timestamp_plus_minutes(
    config: &OptionsEngineConfig,
    date: NaiveDate,
    time: NaiveTime,
    minutes: i64,
) -> anyhow::Result<String> {
    let local = date.and_time(time) + Duration::minutes(minutes);
    let timestamp = config
        .entry_timezone
        .from_local_datetime(&local)
        .single()
        .ok_or_else(|| anyhow!("ambiguous or invalid local timestamp {local}"))?;
    Ok(timestamp.with_timezone(&Utc).to_rfc3339())
}

fn trading_dates(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let mut dates = Vec::new();
    let mut date = start;
    while date <= end {
        if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
            dates.push(date);
        }
        date += Duration::days(1);
    }
    dates
}

fn summarize(days: &[BacktestDay]) -> BacktestSummary {
    let mut summary = BacktestSummary {
        scan_days: days
            .iter()
            .map(|day| day.trade_date.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        evaluated_underlying_days: days.len(),
        ..BacktestSummary::default()
    };

    for day in days {
        let Some(trade) = day.selected.as_ref() else {
            continue;
        };
        summary.selected_trades += 1;
        let strategy = summary.by_strategy.entry(trade.strategy.clone()).or_default();
        strategy.selected_trades += 1;
        if let Some(pnl) = trade.pnl {
            summary.closed_trades += 1;
            strategy.closed_trades += 1;
            summary.total_pnl += pnl;
            strategy.total_pnl += pnl;
            if pnl > 0.0 {
                summary.winning_trades += 1;
                strategy.winning_trades += 1;
            }
        }
    }

    if summary.closed_trades > 0 {
        summary.average_pnl = summary.total_pnl / summary.closed_trades as f64;
        summary.win_rate = summary.winning_trades as f64 / summary.closed_trades as f64;
    }
    for strategy in summary.by_strategy.values_mut() {
        if strategy.closed_trades > 0 {
            strategy.average_pnl = strategy.total_pnl / strategy.closed_trades as f64;
            strategy.win_rate = strategy.winning_trades as f64 / strategy.closed_trades as f64;
        }
    }
    summary
}

fn apply_backtest_overrides(config: &mut OptionsEngineConfig, args: &Args) -> anyhow::Result<()> {
    if let Some(underlyings) = args.underlyings.as_ref() {
        config.underlyings = underlyings.clone();
    }
    if config.underlyings.is_empty() {
        bail!("no underlyings configured; provide --underlyings or ALPACA_UNDERLYINGS");
    }
    if let Some(quantity) = args.quantity {
        if quantity == 0 {
            bail!("--quantity must be greater than zero");
        }
        config.quantity = quantity;
    }
    if let Some(strategies) = args.strategies.as_ref() {
        config.spread_kinds.clear();
        config.iron_condor_enabled = false;
        config.debit_kinds.clear();
        config.naked_kinds.clear();
        for strategy in strategies {
            match strategy.as_str() {
                "put" | "put_credit" => config.spread_kinds.push(CreditSpreadKind::Put),
                "call" | "call_credit" => config.spread_kinds.push(CreditSpreadKind::Call),
                "iron_condor" => config.iron_condor_enabled = true,
                "call_debit" => config.debit_kinds.push(DebitSpreadKind::Call),
                "put_debit" => config.debit_kinds.push(DebitSpreadKind::Put),
                "naked_call" => config.naked_kinds.push(NakedOptionKind::Call),
                "naked_put" => config.naked_kinds.push(NakedOptionKind::Put),
                "naked_call_1_3dte" | "naked_call_1_3" => {
                    config.naked_kinds.push(NakedOptionKind::CallOneToThreeDte);
                }
                "naked_put_1_3dte" | "naked_put_1_3" => {
                    config.naked_kinds.push(NakedOptionKind::PutOneToThreeDte);
                }
                other => bail!("unsupported strategy {other}"),
            }
        }
        config.spread_kinds.sort_by_key(|kind| match kind {
            CreditSpreadKind::Put => 0,
            CreditSpreadKind::Call => 1,
        });
        config.spread_kinds.dedup();
        config.debit_kinds.sort_by_key(|kind| match kind {
            DebitSpreadKind::Call => 0,
            DebitSpreadKind::Put => 1,
        });
        config.debit_kinds.dedup();
        config.naked_kinds.sort_by_key(|kind| match kind {
            NakedOptionKind::Call => 0,
            NakedOptionKind::Put => 1,
            NakedOptionKind::CallOneToThreeDte => 2,
            NakedOptionKind::PutOneToThreeDte => 3,
        });
        config.naked_kinds.dedup();
    }
    if enabled_strategy_names(config).is_empty() {
        bail!("no strategies configured; provide --strategies or ALPACA_STRATEGIES");
    }
    Ok(())
}

fn enabled_strategy_names(config: &OptionsEngineConfig) -> Vec<String> {
    let mut names = Vec::new();
    names.extend(config.spread_kinds.iter().map(|kind| credit_strategy_name(*kind).to_string()));
    if config.iron_condor_enabled {
        names.push("iron_condor".to_string());
    }
    names.extend(config.debit_kinds.iter().map(|kind| debit_strategy_name(*kind).to_string()));
    names.extend(config.naked_kinds.iter().map(|kind| naked_strategy_name(*kind).to_string()));
    names
}

fn credit_strategy_name(kind: CreditSpreadKind) -> &'static str {
    match kind {
        CreditSpreadKind::Put => "put_credit",
        CreditSpreadKind::Call => "call_credit",
    }
}

fn debit_strategy_name(kind: DebitSpreadKind) -> &'static str {
    match kind {
        DebitSpreadKind::Call => "call_debit",
        DebitSpreadKind::Put => "put_debit",
    }
}

fn naked_strategy_name(kind: NakedOptionKind) -> &'static str {
    match kind {
        NakedOptionKind::Call => "naked_call",
        NakedOptionKind::Put => "naked_put",
        NakedOptionKind::CallOneToThreeDte => "naked_call_1_3dte",
        NakedOptionKind::PutOneToThreeDte => "naked_put_1_3dte",
    }
}

fn parse_args() -> anyhow::Result<Args> {
    let mut start = None;
    let mut end = None;
    let mut entry_time = None;
    let mut exit_time = None;
    let mut underlyings = None;
    let mut strategies = None;
    let mut timeframe = "1Min".to_string();
    let mut option_feed = None;
    let mut stock_feed = None;
    let mut assumed_iv = 0.35;
    let mut synthetic_spread_pct = 0.05;
    let mut quantity = None;
    let mut json_output = false;

    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--start" => start = Some(parse_date(&next_value(&mut iter, "--start")?)?),
            "--end" => end = Some(parse_date(&next_value(&mut iter, "--end")?)?),
            "--entry-time" => entry_time = Some(parse_time(&next_value(&mut iter, "--entry-time")?)?),
            "--exit-time" => exit_time = Some(parse_time(&next_value(&mut iter, "--exit-time")?)?),
            "--underlyings" => underlyings = Some(split_csv(next_value(&mut iter, "--underlyings")?)),
            "--strategies" => strategies = Some(split_csv(next_value(&mut iter, "--strategies")?)),
            "--timeframe" => timeframe = next_value(&mut iter, "--timeframe")?,
            "--option-feed" => option_feed = Some(next_value(&mut iter, "--option-feed")?),
            "--stock-feed" => stock_feed = Some(next_value(&mut iter, "--stock-feed")?),
            "--assumed-iv" => {
                assumed_iv = next_value(&mut iter, "--assumed-iv")?
                    .parse::<f64>()
                    .context("--assumed-iv must be a decimal")?;
                if assumed_iv <= 0.0 {
                    bail!("--assumed-iv must be greater than zero");
                }
            }
            "--synthetic-spread-pct" => {
                synthetic_spread_pct = next_value(&mut iter, "--synthetic-spread-pct")?
                    .parse::<f64>()
                    .context("--synthetic-spread-pct must be a decimal")?;
                if synthetic_spread_pct < 0.0 {
                    bail!("--synthetic-spread-pct cannot be negative");
                }
            }
            "--quantity" | "--qty" => {
                quantity = Some(
                    next_value(&mut iter, "--quantity")?
                        .parse::<u64>()
                        .context("--quantity must be a positive integer")?,
                );
            }
            "--json" => json_output = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            unknown => bail!("unknown argument {unknown}"),
        }
    }

    Ok(Args {
        start: start.ok_or_else(|| anyhow!("missing --start"))?,
        end: end.ok_or_else(|| anyhow!("missing --end"))?,
        entry_time,
        exit_time,
        underlyings,
        strategies,
        timeframe,
        option_feed,
        stock_feed,
        assumed_iv,
        synthetic_spread_pct,
        quantity,
        json_output,
    })
}

fn parse_date(value: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("invalid date {value}; expected YYYY-MM-DD"))
}

fn parse_time(value: &str) -> anyhow::Result<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(value, "%H:%M"))
        .with_context(|| format!("invalid time {value}; expected HH:MM or HH:MM:SS"))
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    iter.next()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{flag} requires a value"))
}

fn split_csv(value: String) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn print_report(report: &BacktestReport) {
    println!(
        "Alpaca options strategy backtest {} -> {} entry={} exit={} strategies={} underlyings={} mark_source={}",
        report.start,
        report.end,
        report.entry_time,
        report.exit_time,
        report.strategies.join(","),
        report.underlyings.join(","),
        report.mark_source,
    );
    println!(
        "summary scan_days={} underlying_days={} selected={} closed={} wins={} win_rate={:.1}% total_pnl={:.2} avg_pnl={:.2}",
        report.summary.scan_days,
        report.summary.evaluated_underlying_days,
        report.summary.selected_trades,
        report.summary.closed_trades,
        report.summary.winning_trades,
        report.summary.win_rate * 100.0,
        report.summary.total_pnl,
        report.summary.average_pnl,
    );
    for (strategy, summary) in &report.summary.by_strategy {
        println!(
            "strategy={} selected={} closed={} wins={} win_rate={:.1}% total_pnl={:.2} avg_pnl={:.2}",
            strategy,
            summary.selected_trades,
            summary.closed_trades,
            summary.winning_trades,
            summary.win_rate * 100.0,
            summary.total_pnl,
            summary.average_pnl,
        );
    }
    for day in &report.days {
        if let Some(trade) = &day.selected {
            println!(
                "trade date={} underlying={} strategy={} score={:.1} premium={} {:.2} pnl={} status={}",
                trade.trade_date,
                trade.underlying,
                trade.strategy,
                trade.score,
                trade.premium_kind,
                trade.entry_premium,
                trade.pnl.map(|pnl| format!("{pnl:.2}")).unwrap_or_else(|| "-".to_string()),
                trade.exit_status,
            );
        }
    }
}

fn print_usage() {
    println!(
        "Usage: alpaca-options-backtest --start YYYY-MM-DD --end YYYY-MM-DD [--entry-time HH:MM] [--exit-time HH:MM] [--underlyings SPY,QQQ] [--strategies put_credit,call_credit,iron_condor,call_debit,put_debit,naked_call,naked_put] [--timeframe 1Min] [--option-feed indicative] [--stock-feed iex] [--assumed-iv 0.35] [--synthetic-spread-pct 0.05] [--quantity 1] [--json]"
    );
}
