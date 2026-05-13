use std::{
    collections::{BTreeMap, BTreeSet},
    env,
};

use anyhow::{Context, anyhow, bail};
use chrono::{Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaOptionBar, AlpacaOptionContract, AlpacaOptionGreeks, AlpacaOptionQuote,
            AlpacaOptionSnapshot, AlpacaOptionTrade, AlpacaStockBar, ListOptionContractsRequest,
            OptionBarsRequest, OptionTradesRequest, StockBarsRequest,
        },
    },
    options_runtime::{
        OptionsEngineConfig, SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry,
        SelectedNakedOptionEntry, SelectedOptionsEntry,
    },
    runtime_env::load_options_env_file,
    storage::{
        STORAGE_ACCOUNT_ID_DEFAULT, StorageRepository, read_backtest_market_cache,
        write_backtest_market_cache,
    },
    strategy::{
        CreditSpreadKind, DebitSpreadKind, NakedOptionCapitalContext, NakedOptionKind,
        scan_credit_spread_snapshot_at, scan_debit_spread_snapshot_at,
        scan_iron_condor_snapshots_at, scan_naked_option_snapshot_at,
    },
};
use nautilus_model::data::greeks::black_scholes_greeks;
use serde::{Serialize, de::DeserializeOwned};

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
    entry_source: Option<String>,
    exit_source: Option<String>,
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

    let mut config = OptionsEngineConfig::from_runtime_env_with_storage().await?;
    apply_backtest_overrides(&mut config, &args)?;
    let account_id = config
        .storage_account_id
        .clone()
        .unwrap_or_else(|| STORAGE_ACCOUNT_ID_DEFAULT.to_string());

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
                config.storage_repository.as_deref(),
                &account_id,
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
        mark_source: "historical_trade_or_bar_with_synthetic_quote",
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
    storage: Option<&StorageRepository>,
    account_id: &str,
) -> anyhow::Result<BacktestDay> {
    let entry_timestamp = timestamp_for(config, trade_date, entry_time)?;
    let exit_timestamp = timestamp_for(config, trade_date, exit_time)?;
    let entry_end = timestamp_plus_minutes(config, trade_date, entry_time, 1)?;
    let exit_end = timestamp_plus_minutes(config, trade_date, exit_time, 1)?;
    let (min_dte, max_dte) = scanner_dte_window(config);

    let contracts =
        load_contracts(client, storage, account_id, underlying, trade_date, min_dte, max_dte)
            .await?;
    let symbols = contracts
        .iter()
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();

    let underlying_price = load_underlying_price_at(
        client,
        storage,
        account_id,
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
            storage,
            account_id,
            symbols.clone(),
            timeframe,
            option_feed,
            &entry_timestamp,
            &entry_end,
        )
        .await?
    };
    let option_trades = if symbols.is_empty() {
        BTreeMap::new()
    } else {
        load_option_trades(
            client,
            storage,
            account_id,
            symbols.clone(),
            &entry_timestamp,
            &entry_end,
        )
        .await?
    };
    let snapshots = build_snapshot_map(
        &contracts,
        &option_bars,
        &option_trades,
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
                config,
                trade_date,
                &entry_timestamp,
                &exit_timestamp,
                &exit_end,
                timeframe,
                option_feed,
                &option_bars,
                &option_trades,
                storage,
                account_id,
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
    config: &OptionsEngineConfig,
    trade_date: NaiveDate,
    entry_timestamp: &str,
    exit_timestamp: &str,
    exit_end: &str,
    timeframe: &str,
    option_feed: &str,
    entry_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    entry_trades: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
    storage: Option<&StorageRepository>,
    account_id: &str,
) -> anyhow::Result<TradeBacktest> {
    let descriptor = entry.descriptor();
    let mut legs = selected_legs(&entry);
    for leg in &mut legs {
        if let Some(mark) = historical_mark(&leg.symbol, entry_bars, entry_trades) {
            leg.entry_close = Some(mark.price);
            leg.entry_source = Some(mark.source.to_string());
        }
    }

    let symbols = legs.iter().map(|leg| leg.symbol.clone()).collect::<Vec<_>>();
    let path_bars = load_option_bars(
        client,
        storage,
        account_id,
        symbols,
        timeframe,
        option_feed,
        entry_timestamp,
        exit_end,
    )
    .await?;
    let path_trades = load_option_trades(
        client,
        storage,
        account_id,
        legs.iter().map(|leg| leg.symbol.clone()).collect(),
        entry_timestamp,
        exit_end,
    )
    .await?;

    let entry_net_cashflow = match descriptor.premium_kind.as_str() {
        "credit" => descriptor.premium,
        "debit" => -descriptor.premium,
        _ => legs
            .iter()
            .filter_map(|leg| leg.entry_close.map(|price| leg.side.entry_sign() * price))
            .sum(),
    };
    let exit_plan = select_exit_plan(
        &legs,
        &path_bars,
        &path_trades,
        entry_timestamp,
        exit_timestamp,
        entry_net_cashflow,
        descriptor.premium_kind.as_str(),
        config,
    );

    let mut exit_net_cashflow = 0.0;
    let mut missing_exit = false;
    let mut resolved_exit_timestamp = exit_timestamp.to_string();
    let mut exit_status = "missing_exit_bar".to_string();
    if let Some(exit_plan) = exit_plan {
        resolved_exit_timestamp = exit_plan.timestamp;
        exit_status = exit_plan.reason;
        for leg in &mut legs {
            if let Some(mark) = exit_plan.marks.get(&leg.symbol) {
                leg.exit_close = Some(mark.price);
                leg.exit_source = Some(mark.source.to_string());
            }
        }
    }
    for leg in &legs {
        if let Some(exit_close) = leg.exit_close {
            exit_net_cashflow += leg.side.exit_sign() * exit_close;
        } else {
            missing_exit = true;
        }
    }

    let pnl_per_contract = entry_net_cashflow + exit_net_cashflow;
    let pnl =
        (!missing_exit).then_some(pnl_per_contract * OPTION_CONTRACT_MULTIPLIER * config.quantity as f64);

    Ok(TradeBacktest {
        strategy: descriptor.strategy.to_string(),
        underlying: descriptor.underlying,
        trade_date: trade_date.to_string(),
        entry_timestamp: entry_timestamp.to_string(),
        exit_timestamp: resolved_exit_timestamp,
        quantity: config.quantity,
        score: descriptor.score,
        premium_kind: descriptor.premium_kind.as_str().to_string(),
        entry_premium: descriptor.premium,
        entry_net_cashflow,
        exit_net_cashflow: (!missing_exit).then_some(exit_net_cashflow),
        pnl,
        legs,
        exit_status: if missing_exit {
            "missing_exit_bar".to_string()
        } else {
            exit_status
        },
    })
}

#[derive(Clone, Debug)]
struct ExitPlan {
    timestamp: String,
    reason: String,
    marks: BTreeMap<String, HistoricalMark>,
}

fn select_exit_plan(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    _path_trades: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
    entry_timestamp: &str,
    fallback_exit_timestamp: &str,
    entry_net_cashflow: f64,
    premium_kind: &str,
    config: &OptionsEngineConfig,
) -> Option<ExitPlan> {
    let mut fallback = None;
    for timestamp in common_bar_timestamps(legs, path_bars) {
        if timestamp.as_str() < entry_timestamp || timestamp.as_str() > fallback_exit_timestamp {
            continue;
        }
        let marks = bar_marks_at_timestamp(legs, path_bars, &timestamp)?;
        let exit_net_cashflow = exit_cashflow_from_marks(legs, &marks)?;
        let reason = management_exit_reason(
            entry_timestamp,
            &timestamp,
            entry_net_cashflow,
            exit_net_cashflow,
            premium_kind,
            config,
        );
        let plan = ExitPlan {
            timestamp,
            reason: reason.unwrap_or("fixed_exit").to_string(),
            marks,
        };
        if reason.is_some() {
            return Some(plan);
        }
        fallback = Some(plan);
    }
    fallback
}

fn common_bar_timestamps(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    for leg in legs {
        let mut timestamps = BTreeSet::new();
        if let Some(bars) = path_bars.get(&leg.symbol) {
            for bar in bars {
                if bar.close.is_some_and(|value| value > 0.0) {
                    if let Some(timestamp) = bar.timestamp.as_ref() {
                        timestamps.insert(timestamp.clone());
                    }
                }
            }
        }
        for timestamp in timestamps {
            *counts.entry(timestamp).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .filter_map(|(timestamp, count)| (count == legs.len()).then_some(timestamp))
        .collect()
}

fn bar_marks_at_timestamp(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    timestamp: &str,
) -> Option<BTreeMap<String, HistoricalMark>> {
    let mut marks = BTreeMap::new();
    for leg in legs {
        let bar = path_bars.get(&leg.symbol)?.iter().find(|bar| {
            bar.timestamp.as_deref() == Some(timestamp)
                && bar.close.is_some_and(|value| value > 0.0)
        })?;
        marks.insert(
            leg.symbol.clone(),
            HistoricalMark {
                price: bar.close?,
                timestamp: bar.timestamp.clone(),
                source: "bar_management",
                volume: bar.volume,
            },
        );
    }
    Some(marks)
}

fn exit_cashflow_from_marks(
    legs: &[BacktestLeg],
    marks: &BTreeMap<String, HistoricalMark>,
) -> Option<f64> {
    let mut cashflow = 0.0;
    for leg in legs {
        cashflow += leg.side.exit_sign() * marks.get(&leg.symbol)?.price;
    }
    Some(cashflow)
}

fn management_exit_reason(
    entry_timestamp: &str,
    timestamp: &str,
    entry_net_cashflow: f64,
    exit_net_cashflow: f64,
    premium_kind: &str,
    config: &OptionsEngineConfig,
) -> Option<&'static str> {
    if premium_kind == "credit" && entry_net_cashflow > 0.0 {
        let close_debit = -exit_net_cashflow;
        if close_debit > 0.0
            && close_debit <= entry_net_cashflow * config.profit_target_close_fraction
        {
            return Some("profit_target");
        }
        if config.stop_loss_close_multiple > 0.0
            && close_debit >= entry_net_cashflow * config.stop_loss_close_multiple
        {
            return Some("stop_loss");
        }
    }

    if config.max_hold_secs > 0 {
        if let Some(age_secs) = timestamp_age_secs(entry_timestamp, timestamp) {
            if age_secs >= config.max_hold_secs as i64 {
                return Some("max_hold");
            }
        }
    }

    None
}

fn timestamp_age_secs(entry_timestamp: &str, timestamp: &str) -> Option<i64> {
    let entry = chrono::DateTime::parse_from_rfc3339(entry_timestamp).ok()?;
    let current = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    Some(current.signed_duration_since(entry).num_seconds())
}

async fn load_contracts(
    client: &AlpacaHttpClient,
    storage: Option<&StorageRepository>,
    account_id: &str,
    underlying: &str,
    scan_date: NaiveDate,
    min_dte: i64,
    max_dte: i64,
) -> anyhow::Result<Vec<AlpacaOptionContract>> {
    let cache_key = format!("{underlying}|{scan_date}|{min_dte}|{max_dte}");
    if let Some(cached) =
        read_cache(storage, account_id, "option_contracts", &cache_key).await?
    {
        return Ok(cached);
    }

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
    write_cache(
        storage,
        account_id,
        "option_contracts",
        &cache_key,
        &contracts,
    )
    .await?;
    Ok(contracts)
}

async fn load_option_bars(
    client: &AlpacaHttpClient,
    storage: Option<&StorageRepository>,
    account_id: &str,
    symbols: Vec<String>,
    timeframe: &str,
    feed: &str,
    start: &str,
    end: &str,
) -> anyhow::Result<BTreeMap<String, Vec<AlpacaOptionBar>>> {
    if symbols.is_empty() {
        return Ok(BTreeMap::new());
    }
    let cache_key = market_data_cache_key(&symbols, &[timeframe, feed, start, end]);
    if let Some(cached) = read_cache(storage, account_id, "option_bars", &cache_key).await? {
        return Ok(cached);
    }
    let mut request = OptionBarsRequest::for_symbols(symbols, timeframe.to_string(), start.to_string());
    request.end = Some(end.to_string());
    request.feed = Some(feed.to_string());
    let bars = client.option_bars(&request).await?.bars;
    write_cache(storage, account_id, "option_bars", &cache_key, &bars).await?;
    Ok(bars)
}

async fn load_option_trades(
    client: &AlpacaHttpClient,
    storage: Option<&StorageRepository>,
    account_id: &str,
    symbols: Vec<String>,
    start: &str,
    end: &str,
) -> anyhow::Result<BTreeMap<String, Vec<AlpacaOptionTrade>>> {
    if symbols.is_empty() {
        return Ok(BTreeMap::new());
    }
    let cache_key = market_data_cache_key(&symbols, &[start, end]);
    if let Some(cached) = read_cache(storage, account_id, "option_trades", &cache_key).await? {
        return Ok(cached);
    }
    let mut request = OptionTradesRequest::for_symbols(symbols, start.to_string());
    request.end = Some(end.to_string());
    let trades = client.option_trades(&request).await?.trades;
    write_cache(storage, account_id, "option_trades", &cache_key, &trades).await?;
    Ok(trades)
}

async fn load_underlying_price_at(
    client: &AlpacaHttpClient,
    storage: Option<&StorageRepository>,
    account_id: &str,
    symbol: &str,
    timeframe: &str,
    feed: &str,
    start: &str,
    end: &str,
) -> anyhow::Result<Option<f64>> {
    let symbols = [symbol.to_string()];
    let cache_key = market_data_cache_key(&symbols, &[timeframe, feed, start, end]);
    if let Some(cached) =
        read_cache::<BTreeMap<String, Vec<AlpacaStockBar>>>(
            storage,
            account_id,
            "stock_bars",
            &cache_key,
        )
        .await?
    {
        return Ok(cached
            .get(symbol)
            .and_then(|bars| first_stock_bar_close(bars)));
    }
    let mut request = StockBarsRequest::for_symbols([symbol.to_string()], timeframe.to_string(), start.to_string());
    request.end = Some(end.to_string());
    request.feed = Some(feed.to_string());
    let response = client.stock_bars(&request).await?;
    write_cache(
        storage,
        account_id,
        "stock_bars",
        &cache_key,
        &response.bars,
    )
    .await?;
    Ok(response
        .bars
        .get(symbol)
        .and_then(|bars| first_stock_bar_close(bars)))
}

async fn read_cache<T>(
    storage: Option<&StorageRepository>,
    account_id: &str,
    cache_kind: &str,
    cache_key: &str,
) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let Some(storage) = storage else {
        return Ok(None);
    };
    read_backtest_market_cache(storage, account_id, cache_kind, cache_key).await
}

async fn write_cache<T>(
    storage: Option<&StorageRepository>,
    account_id: &str,
    cache_kind: &str,
    cache_key: &str,
    payload: &T,
) -> anyhow::Result<()>
where
    T: Serialize,
{
    let Some(storage) = storage else {
        return Ok(());
    };
    write_backtest_market_cache(storage, account_id, cache_kind, cache_key, payload).await
}

fn market_data_cache_key(symbols: &[String], parts: &[&str]) -> String {
    let mut symbols = symbols.to_vec();
    symbols.sort();
    symbols.dedup();
    format!("{}|{}", parts.join("|"), symbols.join(","))
}

fn build_snapshot_map(
    contracts: &[AlpacaOptionContract],
    bars_by_symbol: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    trades_by_symbol: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
    scan_date: NaiveDate,
    underlying_price: Option<f64>,
    assumed_iv: f64,
    synthetic_spread_pct: f64,
) -> BTreeMap<String, AlpacaOptionSnapshot> {
    let mut snapshots = BTreeMap::new();
    for contract in contracts {
        let Some(mark) = historical_mark(&contract.symbol, bars_by_symbol, trades_by_symbol)
        else {
            continue;
        };
        let (bid, ask, quote_size) = synthetic_quote(
            contract,
            scan_date,
            underlying_price,
            mark.price,
            mark.volume,
            synthetic_spread_pct,
        );
        let implied_volatility = underlying_price
            .and_then(|price| implied_volatility_from_mark(contract, scan_date, price, mark.price))
            .unwrap_or(assumed_iv);
        let greeks = underlying_price.and_then(|price| {
            calculated_greeks(contract, scan_date, price, implied_volatility)
        });
        snapshots.insert(
            contract.symbol.clone(),
            AlpacaOptionSnapshot {
                latest_quote: Some(AlpacaOptionQuote {
                    ask_price: Some(ask),
                    ask_size: Some(quote_size),
                    bid_price: Some(bid),
                    bid_size: Some(quote_size),
                    timestamp: mark.timestamp.clone(),
                }),
                latest_trade: None,
                minute_bar: bars_by_symbol
                    .get(&contract.symbol)
                    .and_then(|bars| first_option_bar(bars))
                    .cloned(),
                daily_bar: bars_by_symbol
                    .get(&contract.symbol)
                    .and_then(|bars| first_option_bar(bars))
                    .cloned(),
                prev_daily_bar: None,
                greeks,
                implied_volatility: Some(implied_volatility),
            },
        );
    }
    snapshots
}

#[derive(Clone, Debug)]
struct HistoricalMark {
    price: f64,
    timestamp: Option<String>,
    source: &'static str,
    volume: Option<u64>,
}

fn historical_mark(
    symbol: &str,
    bars_by_symbol: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    trades_by_symbol: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
) -> Option<HistoricalMark> {
    if let Some(trade) = trades_by_symbol
        .get(symbol)
        .and_then(|trades| first_option_trade(trades))
    {
        return Some(HistoricalMark {
            price: trade.price?,
            timestamp: trade.timestamp.clone(),
            source: "trade",
            volume: trade.size,
        });
    }

    let bar = bars_by_symbol
        .get(symbol)
        .and_then(|bars| first_option_bar(bars))?;
    Some(HistoricalMark {
        price: bar.close?,
        timestamp: bar.timestamp.clone(),
        source: "bar",
        volume: bar.volume,
    })
}

fn synthetic_quote(
    contract: &AlpacaOptionContract,
    scan_date: NaiveDate,
    underlying_price: Option<f64>,
    mark_price: f64,
    mark_volume: Option<u64>,
    base_spread_pct: f64,
) -> (f64, f64, u64) {
    let mut spread_pct = base_spread_pct.max(0.0);

    if mark_price < 0.25 {
        spread_pct += 0.25;
    } else if mark_price < 0.50 {
        spread_pct += 0.15;
    } else if mark_price < 1.00 {
        spread_pct += 0.08;
    }

    match mark_volume.unwrap_or(0) {
        0 => spread_pct += 0.20,
        1..=9 => spread_pct += 0.10,
        10..=49 => spread_pct += 0.05,
        _ => {}
    }

    let open_interest = contract
        .open_interest
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    match open_interest {
        0 => spread_pct += 0.08,
        1..=99 => spread_pct += 0.04,
        _ => {}
    }

    if let Some(dte) = days_to_expiration(contract, scan_date) {
        if dte <= 1 {
            spread_pct += 0.06;
        } else if dte <= 3 {
            spread_pct += 0.03;
        }
    }

    if let (Some(underlying_price), Ok(strike)) =
        (underlying_price, contract.strike_price.parse::<f64>())
    {
        if underlying_price > 0.0 && strike > 0.0 {
            let moneyness = ((strike / underlying_price) - 1.0).abs();
            if moneyness > 0.30 {
                spread_pct += 0.15;
            } else if moneyness > 0.15 {
                spread_pct += 0.06;
            }
        }
    }

    let spread_pct = spread_pct.clamp(0.01, 0.80);
    let tick = option_tick_size(contract, mark_price);
    let half_width = ((mark_price * spread_pct) / 2.0).max(tick);
    let bid = floor_to_tick((mark_price - half_width).max(tick), tick);
    let ask = ceil_to_tick((mark_price + half_width).max(bid + tick), tick);
    let quote_size = synthetic_quote_size(mark_volume);
    (bid, ask, quote_size)
}

fn days_to_expiration(contract: &AlpacaOptionContract, scan_date: NaiveDate) -> Option<i64> {
    let expiration = NaiveDate::parse_from_str(&contract.expiration_date, "%Y-%m-%d").ok()?;
    Some(expiration.signed_duration_since(scan_date).num_days())
}

fn option_tick_size(contract: &AlpacaOptionContract, mark_price: f64) -> f64 {
    if contract.ppind == Some(true) || mark_price < 3.0 {
        0.01
    } else {
        0.05
    }
}

fn floor_to_tick(value: f64, tick: f64) -> f64 {
    ((value / tick).floor() * tick * 100.0).round() / 100.0
}

fn ceil_to_tick(value: f64, tick: f64) -> f64 {
    ((value / tick).ceil() * tick * 100.0).round() / 100.0
}

fn synthetic_quote_size(mark_volume: Option<u64>) -> u64 {
    match mark_volume.unwrap_or(0) {
        0 => 1,
        1..=9 => 2,
        10..=49 => 5,
        _ => 10,
    }
}

fn implied_volatility_from_mark(
    contract: &AlpacaOptionContract,
    scan_date: NaiveDate,
    underlying_price: f64,
    option_price: f64,
) -> Option<f64> {
    let strike = contract.strike_price.parse::<f64>().ok()?;
    let years = years_to_expiration(contract, scan_date)?;
    if underlying_price <= 0.0 || strike <= 0.0 || option_price <= 0.0 || years <= 0.0 {
        return None;
    }

    let is_call = contract.option_type.eq_ignore_ascii_case("call");
    let intrinsic = if is_call {
        (underlying_price - strike).max(0.0)
    } else {
        (strike - underlying_price).max(0.0)
    };
    if option_price < intrinsic {
        return None;
    }

    let mut low = 0.0001;
    let mut high = 5.0;
    for _ in 0..80 {
        let mid = (low + high) / 2.0;
        let model = black_scholes_price(underlying_price, strike, years, mid, is_call);
        if model > option_price {
            high = mid;
        } else {
            low = mid;
        }
    }

    Some(((low + high) / 2.0).clamp(0.0001, 5.0))
}

fn calculated_greeks(
    contract: &AlpacaOptionContract,
    scan_date: NaiveDate,
    underlying_price: f64,
    implied_volatility: f64,
) -> Option<AlpacaOptionGreeks> {
    let strike = contract.strike_price.parse::<f64>().ok()?;
    let years = years_to_expiration(contract, scan_date)?;
    if underlying_price <= 0.0 || strike <= 0.0 || implied_volatility <= 0.0 || years <= 0.0 {
        return None;
    }
    let is_call = contract.option_type.eq_ignore_ascii_case("call");
    let greeks = black_scholes_greeks(
        underlying_price,
        SCANNER_RISK_FREE_RATE,
        SCANNER_RISK_FREE_RATE,
        implied_volatility,
        is_call,
        strike,
        years,
    );

    Some(AlpacaOptionGreeks {
        delta: Some(greeks.delta),
        gamma: Some(greeks.gamma),
        rho: Some(greeks.rho),
        theta: Some(greeks.theta),
        vega: Some(greeks.vega),
    })
}

fn years_to_expiration(contract: &AlpacaOptionContract, scan_date: NaiveDate) -> Option<f64> {
    let expiration = NaiveDate::parse_from_str(&contract.expiration_date, "%Y-%m-%d").ok()?;
    let dte = expiration.signed_duration_since(scan_date).num_days().max(1) as f64;
    Some(dte / DAYS_PER_YEAR)
}

fn black_scholes_price(
    underlying_price: f64,
    strike: f64,
    years: f64,
    implied_volatility: f64,
    is_call: bool,
) -> f64 {
    let volatility_sqrt_time = implied_volatility * years.sqrt();
    if volatility_sqrt_time <= 0.0 {
        return 0.0;
    }
    let d1 = ((underlying_price / strike).ln()
        + (SCANNER_RISK_FREE_RATE + 0.5 * implied_volatility * implied_volatility) * years)
        / volatility_sqrt_time;
    let d2 = d1 - volatility_sqrt_time;
    let discounted_strike = strike * (-SCANNER_RISK_FREE_RATE * years).exp();
    if is_call {
        underlying_price * normal_cdf(d1) - discounted_strike * normal_cdf(d2)
    } else {
        discounted_strike * normal_cdf(-d2) - underlying_price * normal_cdf(-d1)
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
                entry_source: None,
                exit_source: None,
            },
            BacktestLeg {
                symbol: entry.candidate.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
                entry_source: None,
                exit_source: None,
            },
        ],
        SelectedOptionsEntry::IronCondor(entry) => vec![
            BacktestLeg {
                symbol: entry.candidate.put.short.symbol.clone(),
                side: LegSide::Short,
                entry_close: None,
                exit_close: None,
                entry_source: None,
                exit_source: None,
            },
            BacktestLeg {
                symbol: entry.candidate.put.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
                entry_source: None,
                exit_source: None,
            },
            BacktestLeg {
                symbol: entry.candidate.call.short.symbol.clone(),
                side: LegSide::Short,
                entry_close: None,
                exit_close: None,
                entry_source: None,
                exit_source: None,
            },
            BacktestLeg {
                symbol: entry.candidate.call.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
                entry_source: None,
                exit_source: None,
            },
        ],
        SelectedOptionsEntry::Debit(entry) => vec![
            BacktestLeg {
                symbol: entry.candidate.long.symbol.clone(),
                side: LegSide::Long,
                entry_close: None,
                exit_close: None,
                entry_source: None,
                exit_source: None,
            },
            BacktestLeg {
                symbol: entry.candidate.short.symbol.clone(),
                side: LegSide::Short,
                entry_close: None,
                exit_close: None,
                entry_source: None,
                exit_source: None,
            },
        ],
        SelectedOptionsEntry::NakedOption(entry) => vec![BacktestLeg {
            symbol: entry.candidate.short.symbol.clone(),
            side: LegSide::Short,
            entry_close: None,
            exit_close: None,
            entry_source: None,
            exit_source: None,
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

fn first_option_trade(trades: &[AlpacaOptionTrade]) -> Option<&AlpacaOptionTrade> {
    trades
        .iter()
        .filter(|trade| trade.price.is_some_and(|value| value > 0.0))
        .min_by(|left, right| left.timestamp.cmp(&right.timestamp))
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
