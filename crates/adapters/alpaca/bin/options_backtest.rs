use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    io::Write,
};

use anyhow::{Context, anyhow, bail};
use chrono::{Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaOptionBar, AlpacaOptionContract, AlpacaOptionGreeks, AlpacaOptionQuote,
            AlpacaOptionSnapshot, AlpacaOptionTrade, AlpacaStockBar, ListOptionContractsRequest,
            MarketCalendarRequest, OptionBarsRequest, OptionTradesRequest, StockBarsRequest,
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
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::time::{Duration as TokioDuration, sleep};

const DAYS_PER_YEAR: f64 = 365.25;
const SCANNER_RISK_FREE_RATE: f64 = 0.0425;
const OPTION_CONTRACT_MULTIPLIER: f64 = 100.0;
const EXIT_FALLBACK_MAX_DISTANCE_SECS: i64 = 30 * 60;
const CONTRACT_LIST_MAX_ATTEMPTS: usize = 5;

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
    entry_mark_window_mins: i64,
    historical_min_open_interest: u64,
    missing_open_interest: u64,
    historical_max_leg_spread_pct: f64,
    sweep_synthetic_spread_pct: Vec<f64>,
    sweep_short_delta_min: Vec<f64>,
    sweep_short_delta_max: Vec<f64>,
    sweep_min_return_on_risk: Vec<f64>,
    profit_target_close_fraction: Option<f64>,
    stop_loss_close_multiple: Option<f64>,
    max_hold_secs: Option<u64>,
    sweep_profit_target_close_fraction: Vec<f64>,
    sweep_stop_loss_close_multiple: Vec<f64>,
    sweep_max_hold_mins: Vec<u64>,
    breakdown_period: BreakdownPeriod,
    unclosed_valuation: UnclosedValuation,
    min_exit_leg_bars: usize,
    require_exit_common_timestamp: bool,
    trade_export_csv: Option<String>,
    trade_export_json: Option<String>,
    quantity: Option<u64>,
    json_output: bool,
}

#[derive(Clone, Copy, Debug)]
enum UnclosedValuation {
    Ignore,
    Conservative,
    WorstObserved,
}

impl UnclosedValuation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Conservative => "conservative",
            Self::WorstObserved => "worst_observed",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum BreakdownPeriod {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl BreakdownPeriod {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Quarter => "quarter",
            Self::Year => "year",
        }
    }
}

#[derive(Debug, Serialize)]
struct BacktestReport {
    variant: String,
    start: String,
    end: String,
    breakdown_period: &'static str,
    unclosed_valuation: &'static str,
    entry_time: String,
    exit_time: String,
    timeframe: String,
    option_feed: String,
    stock_feed: String,
    mark_source: &'static str,
    assumed_iv: f64,
    synthetic_spread_pct: f64,
    entry_mark_window_mins: i64,
    historical_min_open_interest: u64,
    missing_open_interest: u64,
    historical_max_leg_spread_pct: f64,
    underlyings: Vec<String>,
    strategies: Vec<String>,
    candidate_card: CandidateCard,
    summary: BacktestSummary,
    days: Vec<BacktestDay>,
}

#[derive(Default, Debug, Serialize)]
struct BacktestSummary {
    scan_days: usize,
    evaluated_underlying_days: usize,
    selected_trades: usize,
    closed_trades: usize,
    unclosed_trades: usize,
    winning_trades: usize,
    total_pnl: f64,
    average_pnl: f64,
    win_rate: f64,
    accounted: RiskMetrics,
    by_strategy: BTreeMap<String, StrategySummary>,
    by_period: BTreeMap<String, PeriodSummary>,
    by_exit_status: BTreeMap<String, usize>,
    by_data_quality_rejection: BTreeMap<String, usize>,
}

#[derive(Default, Debug, Serialize)]
struct StrategySummary {
    selected_trades: usize,
    closed_trades: usize,
    unclosed_trades: usize,
    winning_trades: usize,
    total_pnl: f64,
    average_pnl: f64,
    win_rate: f64,
    accounted: RiskMetrics,
    by_exit_status: BTreeMap<String, usize>,
}

#[derive(Default, Debug, Serialize)]
struct PeriodSummary {
    scan_days: usize,
    evaluated_underlying_days: usize,
    selected_trades: usize,
    closed_trades: usize,
    unclosed_trades: usize,
    winning_trades: usize,
    total_pnl: f64,
    average_pnl: f64,
    win_rate: f64,
    accounted: RiskMetrics,
    by_strategy: BTreeMap<String, StrategySummary>,
    by_exit_status: BTreeMap<String, usize>,
    by_data_quality_rejection: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct CandidateCard {
    variant: String,
    strategies: Vec<String>,
    underlyings: Vec<String>,
    selected_trades: usize,
    closed_trades: usize,
    unclosed_trades: usize,
    data_quality_rejections: usize,
    accounted_pnl: f64,
    average_pnl: f64,
    win_rate: f64,
    profit_factor: Option<f64>,
    max_drawdown: f64,
    worst_trade: Option<f64>,
    active_periods: usize,
    positive_periods: usize,
    negative_periods: usize,
    verdict: String,
    verdict_reason: String,
}

#[derive(Debug, Serialize)]
struct RiskMetrics {
    trades: usize,
    winning_trades: usize,
    losing_trades: usize,
    gross_profit: f64,
    gross_loss: f64,
    total_pnl: f64,
    average_pnl: f64,
    win_rate: f64,
    profit_factor: Option<f64>,
    expectancy: f64,
    best_trade: Option<f64>,
    worst_trade: Option<f64>,
    max_drawdown: f64,
    #[serde(skip)]
    equity: f64,
    #[serde(skip)]
    peak_equity: f64,
}

impl Default for RiskMetrics {
    fn default() -> Self {
        Self {
            trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            gross_profit: 0.0,
            gross_loss: 0.0,
            total_pnl: 0.0,
            average_pnl: 0.0,
            win_rate: 0.0,
            profit_factor: None,
            expectancy: 0.0,
            best_trade: None,
            worst_trade: None,
            max_drawdown: 0.0,
            equity: 0.0,
            peak_equity: 0.0,
        }
    }
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
    data_quality_rejection: Option<String>,
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
    conservative_pnl: Option<f64>,
    conservative_status: Option<String>,
    worst_observed_pnl: Option<f64>,
    worst_observed_status: Option<String>,
    closed: bool,
    legs: Vec<BacktestLeg>,
    exit_status: String,
    exit_diagnostic: String,
    exit_missing_leg_symbols: Vec<String>,
    exit_bar_counts: BTreeMap<String, usize>,
    exit_common_timestamps: usize,
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

#[derive(Debug, Serialize)]
struct TradeExportRecord {
    variant: String,
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
    conservative_pnl: Option<f64>,
    conservative_status: Option<String>,
    worst_observed_pnl: Option<f64>,
    worst_observed_status: Option<String>,
    accounted_pnl: f64,
    closed: bool,
    exit_status: String,
    exit_diagnostic: String,
    exit_missing_leg_symbols: Vec<String>,
    exit_bar_counts: BTreeMap<String, usize>,
    exit_common_timestamps: usize,
    legs: Vec<BacktestLeg>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum LegSide {
    Long,
    Short,
}

impl LegSide {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
        }
    }

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

#[derive(Clone, Debug)]
struct SweepVariant {
    label: String,
    synthetic_spread_pct: f64,
    short_delta_min: Option<f64>,
    short_delta_max: Option<f64>,
    min_return_on_risk: Option<f64>,
    profit_target_close_fraction: Option<f64>,
    stop_loss_close_multiple: Option<f64>,
    max_hold_secs: Option<u64>,
}

#[derive(Default, Debug, Deserialize)]
struct BacktestProfilePatch {
    start: Option<String>,
    end: Option<String>,
    entry_time: Option<String>,
    exit_time: Option<String>,
    underlyings: Option<Vec<String>>,
    strategies: Option<Vec<String>>,
    timeframe: Option<String>,
    option_feed: Option<String>,
    stock_feed: Option<String>,
    assumed_iv: Option<f64>,
    synthetic_spread_pct: Option<f64>,
    entry_mark_window_mins: Option<i64>,
    historical_min_open_interest: Option<u64>,
    missing_open_interest: Option<u64>,
    historical_max_leg_spread_pct: Option<f64>,
    sweep_synthetic_spread_pct: Option<Vec<f64>>,
    sweep_short_delta_min: Option<Vec<f64>>,
    sweep_short_delta_max: Option<Vec<f64>>,
    sweep_min_return_on_risk: Option<Vec<f64>>,
    profit_target_close_fraction: Option<f64>,
    stop_loss_close_multiple: Option<f64>,
    max_hold_mins: Option<u64>,
    max_hold_secs: Option<u64>,
    sweep_profit_target_close_fraction: Option<Vec<f64>>,
    sweep_stop_loss_close_multiple: Option<Vec<f64>>,
    sweep_max_hold_mins: Option<Vec<u64>>,
    breakdown_period: Option<String>,
    unclosed_valuation: Option<String>,
    min_exit_leg_bars: Option<usize>,
    require_exit_common_timestamp: Option<bool>,
    quantity: Option<u64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_options_env_file()?;

    let args = parse_args()?;
    if args.end < args.start {
        bail!("--end must be on or after --start");
    }

    let variants = sweep_variants(&args);
    let mut reports = Vec::new();
    for variant in variants {
        reports.push(run_backtest(args.clone(), variant).await?);
    }

    if let Some(path) = args.trade_export_csv.as_deref() {
        write_trade_csv_export(path, &reports)?;
    }
    if let Some(path) = args.trade_export_json.as_deref() {
        write_trade_json_export(path, &reports)?;
    }

    if args.json_output {
        if reports.len() == 1 {
            println!("{}", serde_json::to_string_pretty(&reports[0])?);
        } else {
            println!("{}", serde_json::to_string_pretty(&reports)?);
        }
    } else if reports.len() == 1 {
        print_report(&reports[0]);
    } else {
        print_sweep_report(&reports);
    }

    Ok(())
}

async fn run_backtest(mut args: Args, variant: SweepVariant) -> anyhow::Result<BacktestReport> {
    args.synthetic_spread_pct = variant.synthetic_spread_pct;
    let mut config = OptionsEngineConfig::from_runtime_env_with_storage().await?;
    apply_backtest_overrides(&mut config, &args)?;
    apply_sweep_variant(&mut config, &variant);
    apply_historical_scanner_overrides(&mut config, &args);
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
    let trade_dates = load_trading_dates(&client, args.start, args.end).await?;

    let mut days = Vec::new();
    let total_underlying_days = trade_dates.len() * config.underlyings.len();
    let mut evaluated_underlying_days = 0_usize;
    eprintln!(
        "backtest start variant={} strategies={} underlyings={} trading_days={} underlying_days={}",
        variant.label,
        enabled_strategy_names(&config).join(","),
        config.underlyings.join(","),
        trade_dates.len(),
        total_underlying_days,
    );
    for trade_date in trade_dates {
        for underlying in config.underlyings.clone() {
            evaluated_underlying_days += 1;
            if evaluated_underlying_days == 1
                || evaluated_underlying_days % 50 == 0
                || evaluated_underlying_days == total_underlying_days
            {
                eprintln!(
                    "backtest progress variant={} {}/{} date={} underlying={}",
                    variant.label,
                    evaluated_underlying_days,
                    total_underlying_days,
                    trade_date,
                    underlying,
                );
            }
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
                args.entry_mark_window_mins,
                args.missing_open_interest,
                args.min_exit_leg_bars,
                args.require_exit_common_timestamp,
                config.storage_repository.as_deref(),
                &account_id,
            )
            .await?;
            days.push(result);
        }
    }

    let summary = summarize(&days, args.breakdown_period, args.unclosed_valuation);
    let strategies = enabled_strategy_names(&config);
    let candidate_card = build_candidate_card(
        &variant.label,
        config.underlyings.clone(),
        strategies.clone(),
        &summary,
    );
    Ok(BacktestReport {
        variant: variant.label,
        start: args.start.to_string(),
        end: args.end.to_string(),
        breakdown_period: args.breakdown_period.as_str(),
        unclosed_valuation: args.unclosed_valuation.as_str(),
        entry_time: entry_time.format("%H:%M:%S").to_string(),
        exit_time: exit_time.format("%H:%M:%S").to_string(),
        timeframe: args.timeframe,
        option_feed,
        stock_feed,
        mark_source: "historical_trade_or_bar_with_synthetic_quote",
        assumed_iv: args.assumed_iv,
        synthetic_spread_pct: args.synthetic_spread_pct,
        entry_mark_window_mins: args.entry_mark_window_mins,
        historical_min_open_interest: args.historical_min_open_interest,
        missing_open_interest: args.missing_open_interest,
        historical_max_leg_spread_pct: args.historical_max_leg_spread_pct,
        underlyings: config.underlyings.clone(),
        strategies,
        candidate_card,
        summary,
        days,
    })
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
    entry_mark_window_mins: i64,
    missing_open_interest: u64,
    min_exit_leg_bars: usize,
    require_exit_common_timestamp: bool,
    storage: Option<&StorageRepository>,
    account_id: &str,
) -> anyhow::Result<BacktestDay> {
    let entry_timestamp = timestamp_for(config, trade_date, entry_time)?;
    let exit_timestamp = timestamp_for(config, trade_date, exit_time)?;
    let entry_start = timestamp_plus_minutes(config, trade_date, entry_time, -entry_mark_window_mins)?;
    let entry_end = timestamp_plus_minutes(config, trade_date, entry_time, entry_mark_window_mins + 1)?;
    let exit_end = timestamp_plus_minutes(config, trade_date, exit_time, 1)?;
    let (min_dte, max_dte) = scanner_dte_window(config);

    let mut contracts =
        load_contracts(client, storage, account_id, underlying, trade_date, min_dte, max_dte)
            .await?;
    normalize_historical_open_interest(&mut contracts, missing_open_interest);
    let symbols = contracts
        .iter()
        .filter(|contract| is_standard_alpaca_option_symbol(&contract.symbol))
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();

    let underlying_price = load_underlying_price_at(
        client,
        storage,
        account_id,
        underlying,
        timeframe,
        stock_feed,
        &entry_start,
        &entry_end,
        &entry_timestamp,
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
            &entry_start,
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
            &entry_start,
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
        &entry_timestamp,
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

    let (selected, data_quality_rejection) = match selected {
        Some(entry) => {
            let trade = simulate_selected_entry(
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
            .await?;
            if let Some(reason) =
                data_quality_rejection(&trade, min_exit_leg_bars, require_exit_common_timestamp)
            {
                (None, Some(reason))
            } else {
                (Some(trade), None)
            }
        }
        None => (None, None),
    };

    Ok(BacktestDay {
        trade_date: trade_date.to_string(),
        underlying: underlying.to_string(),
        contracts: contracts.len(),
        snapshots: snapshots.len(),
        underlying_price,
        diagnostics,
        selected,
        data_quality_rejection,
    })
}

fn data_quality_rejection(
    trade: &TradeBacktest,
    min_exit_leg_bars: usize,
    require_exit_common_timestamp: bool,
) -> Option<String> {
    if min_exit_leg_bars > 0 {
        if let Some((symbol, count)) = trade
            .exit_bar_counts
            .iter()
            .find(|(_, count)| **count < min_exit_leg_bars)
        {
            return Some(format!(
                "exit_leg_bars_below_min symbol={symbol} count={count} min={min_exit_leg_bars}"
            ));
        }
    }
    if require_exit_common_timestamp && trade.exit_common_timestamps == 0 {
        return Some("missing_common_exit_timestamp".to_string());
    }
    None
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
        if let Some(mark) =
            historical_mark_near(&leg.symbol, entry_bars, entry_trades, entry_timestamp)
        {
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
    let exit_common_timestamps =
        common_bar_timestamps_in_window(&legs, &path_bars, entry_timestamp, exit_timestamp).len();

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
    let exit_bar_counts = exit_bar_counts_by_leg(&legs, &path_bars, entry_timestamp, exit_timestamp);
    let mut exit_missing_leg_symbols = exit_bar_counts
        .iter()
        .filter_map(|(symbol, count)| (*count == 0).then_some(symbol.clone()))
        .collect::<Vec<_>>();
    let mut exit_status = if exit_missing_leg_symbols.is_empty() {
        "missing_common_exit_timestamp".to_string()
    } else {
        "missing_exit_leg_bar".to_string()
    };
    let mut exit_diagnostic = if exit_missing_leg_symbols.is_empty() {
        "no common exit timestamp across all legs before fallback exit".to_string()
    } else {
        format!(
            "missing usable exit bars for {} leg(s)",
            exit_missing_leg_symbols.len()
        )
    };
    if let Some(exit_plan) = exit_plan {
        resolved_exit_timestamp = exit_plan.timestamp;
        exit_status = exit_plan.reason;
        exit_diagnostic = "closed from complete leg marks".to_string();
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
    if missing_exit {
        let missing_exit_prices = legs
            .iter()
            .filter_map(|leg| leg.exit_close.is_none().then_some(leg.symbol.clone()))
            .collect::<Vec<_>>();
        if exit_missing_leg_symbols.is_empty() {
            exit_missing_leg_symbols = missing_exit_prices;
        }
    } else {
        exit_missing_leg_symbols.clear();
    }

    let pnl_per_contract = entry_net_cashflow + exit_net_cashflow;
    let pnl =
        (!missing_exit).then_some(pnl_per_contract * OPTION_CONTRACT_MULTIPLIER * config.quantity as f64);
    let closed = !missing_exit;
    let conservative_pnl = if closed {
        pnl
    } else {
        conservative_unclosed_pnl(
            &legs,
            entry_net_cashflow,
            descriptor.premium_kind.as_str(),
            config.quantity,
        )
    };
    let conservative_status = (!closed).then(|| {
        if conservative_pnl.is_some() {
            "max_loss".to_string()
        } else {
            "unvalued".to_string()
        }
    });
    let worst_observed_pnl = if closed {
        pnl
    } else {
        worst_observed_unclosed_pnl(
            &legs,
            &path_bars,
            entry_net_cashflow,
            descriptor.premium_kind.as_str(),
            config.quantity,
        )
        .or(conservative_pnl)
    };
    let worst_observed_status = (!closed).then(|| {
        if worst_observed_pnl.is_some() {
            "worst_observed_or_max_loss".to_string()
        } else {
            "unvalued".to_string()
        }
    });

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
        conservative_pnl,
        conservative_status,
        worst_observed_pnl,
        worst_observed_status,
        closed,
        legs,
        exit_status: if missing_exit {
            exit_status
        } else {
            exit_status
        },
        exit_diagnostic,
        exit_missing_leg_symbols,
        exit_bar_counts,
        exit_common_timestamps,
    })
}

#[derive(Clone, Debug)]
struct ExitPlan {
    timestamp: String,
    reason: String,
    marks: BTreeMap<String, HistoricalMark>,
}

fn conservative_unclosed_pnl(
    legs: &[BacktestLeg],
    entry_net_cashflow: f64,
    premium_kind: &str,
    quantity: u64,
) -> Option<f64> {
    match premium_kind {
        "credit" => conservative_credit_spread_pnl(legs, entry_net_cashflow, quantity),
        "debit" => Some(entry_net_cashflow * OPTION_CONTRACT_MULTIPLIER * quantity as f64),
        _ => None,
    }
}

fn conservative_credit_spread_pnl(
    legs: &[BacktestLeg],
    entry_net_cashflow: f64,
    quantity: u64,
) -> Option<f64> {
    if legs.len() != 2 {
        return None;
    }
    let mut strikes = legs
        .iter()
        .filter_map(|leg| option_strike_from_symbol(&leg.symbol))
        .collect::<Vec<_>>();
    if strikes.len() != 2 {
        return None;
    }
    strikes.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let width = strikes[1] - strikes[0];
    if width <= 0.0 {
        return None;
    }
    let max_loss_per_contract = (width - entry_net_cashflow).max(0.0);
    Some(-max_loss_per_contract * OPTION_CONTRACT_MULTIPLIER * quantity as f64)
}

fn option_strike_from_symbol(symbol: &str) -> Option<f64> {
    let strike = symbol.get(symbol.len().checked_sub(8)?..)?;
    let strike = strike.parse::<u64>().ok()?;
    Some(strike as f64 / 1000.0)
}

fn worst_observed_unclosed_pnl(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    entry_net_cashflow: f64,
    premium_kind: &str,
    quantity: u64,
) -> Option<f64> {
    if premium_kind != "credit" || legs.len() != 2 {
        return None;
    }
    let short_leg = legs.iter().find(|leg| matches!(leg.side, LegSide::Short))?;
    let long_leg = legs.iter().find(|leg| matches!(leg.side, LegSide::Long))?;
    let short_high = observed_high_close(&short_leg.symbol, path_bars)?;
    let long_low = observed_low_close(&long_leg.symbol, path_bars)?;
    let worst_close_debit = (short_high - long_low).max(0.0);
    let observed_pnl =
        (entry_net_cashflow - worst_close_debit) * OPTION_CONTRACT_MULTIPLIER * quantity as f64;
    let max_loss_pnl = conservative_credit_spread_pnl(legs, entry_net_cashflow, quantity)?;
    Some(observed_pnl.max(max_loss_pnl))
}

fn observed_high_close(
    symbol: &str,
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
) -> Option<f64> {
    path_bars
        .get(symbol)?
        .iter()
        .filter_map(|bar| bar.close.filter(|value| *value > 0.0))
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
}

fn observed_low_close(
    symbol: &str,
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
) -> Option<f64> {
    path_bars
        .get(symbol)?
        .iter()
        .filter_map(|bar| bar.close.filter(|value| *value > 0.0))
        .min_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
}

fn select_exit_plan(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    path_trades: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
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
    fallback.or_else(|| {
        nearest_exit_fallback_plan(legs, path_bars, path_trades, fallback_exit_timestamp)
    })
}

fn nearest_exit_fallback_plan(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    path_trades: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
    fallback_exit_timestamp: &str,
) -> Option<ExitPlan> {
    let mut marks = BTreeMap::new();
    for leg in legs {
        let mark = historical_mark_near(&leg.symbol, path_bars, path_trades, fallback_exit_timestamp)?;
        let timestamp = mark.timestamp.as_deref()?;
        let distance = timestamp_distance_secs(timestamp, fallback_exit_timestamp)?;
        if distance > EXIT_FALLBACK_MAX_DISTANCE_SECS {
            return None;
        }
        marks.insert(leg.symbol.clone(), mark);
    }
    Some(ExitPlan {
        timestamp: fallback_exit_timestamp.to_string(),
        reason: "nearest_exit_fallback".to_string(),
        marks,
    })
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

fn common_bar_timestamps_in_window(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    entry_timestamp: &str,
    fallback_exit_timestamp: &str,
) -> Vec<String> {
    common_bar_timestamps(legs, path_bars)
        .into_iter()
        .filter(|timestamp| {
            timestamp.as_str() >= entry_timestamp && timestamp.as_str() <= fallback_exit_timestamp
        })
        .collect()
}

fn exit_bar_counts_by_leg(
    legs: &[BacktestLeg],
    path_bars: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    entry_timestamp: &str,
    fallback_exit_timestamp: &str,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for leg in legs {
        let count = path_bars
            .get(&leg.symbol)
            .map(|bars| {
                bars.iter()
                    .filter(|bar| {
                        bar.timestamp
                            .as_deref()
                            .is_some_and(|timestamp| {
                                timestamp >= entry_timestamp && timestamp <= fallback_exit_timestamp
                            })
                            && bar.close.is_some_and(|value| value > 0.0)
                    })
                    .count()
            })
            .unwrap_or(0);
        counts.insert(leg.symbol.clone(), count);
    }
    counts
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
        let mut response = list_option_contracts_with_retry(
            client,
            &request,
            underlying,
            status,
        )
        .await?;
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

async fn list_option_contracts_with_retry(
    client: &AlpacaHttpClient,
    request: &ListOptionContractsRequest,
    underlying: &str,
    status: &str,
) -> anyhow::Result<nautilus_alpaca::http::models::OptionContractsResponse> {
    let mut last_error = None;
    for attempt in 1..=CONTRACT_LIST_MAX_ATTEMPTS {
        match client.list_option_contracts(request).await {
            Ok(response) => return Ok(response),
            Err(error) => {
                let message = error.to_string();
                if !is_retryable_alpaca_error(&message) || attempt == CONTRACT_LIST_MAX_ATTEMPTS {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to load {status} contracts for {underlying} after {attempt} attempt(s)"
                        )
                    });
                }
                last_error = Some(message);
                let delay_ms = 500_u64 * 2_u64.pow((attempt - 1) as u32);
                eprintln!(
                    "retrying option contracts underlying={} status={} attempt={}/{} delay_ms={}",
                    underlying,
                    status,
                    attempt + 1,
                    CONTRACT_LIST_MAX_ATTEMPTS,
                    delay_ms,
                );
                sleep(TokioDuration::from_millis(delay_ms)).await;
            }
        }
    }
    bail!(
        "failed to load {status} contracts for {underlying}; last error={}",
        last_error.unwrap_or_else(|| "unknown".to_string())
    )
}

fn is_retryable_alpaca_error(message: &str) -> bool {
    message.contains("HTTP 429")
        || message.contains("HTTP 500")
        || message.contains("HTTP 502")
        || message.contains("HTTP 503")
        || message.contains("HTTP 504")
        || message.contains("internal server error")
        || message.contains("timed out")
        || message.contains("connection")
}

fn normalize_historical_open_interest(
    contracts: &mut [AlpacaOptionContract],
    missing_open_interest: u64,
) {
    for contract in contracts {
        if contract.open_interest.is_none() {
            contract.open_interest = Some(missing_open_interest.to_string());
        }
    }
}

fn is_standard_alpaca_option_symbol(symbol: &str) -> bool {
    if symbol.starts_with(|value: char| value.is_ascii_digit()) {
        return false;
    }
    if symbol.len() < 16 || symbol.len() > 20 {
        return false;
    }
    let side_index = symbol.len() - 9;
    let side = symbol.as_bytes()[side_index] as char;
    if !matches!(side, 'C' | 'P') {
        return false;
    }
    let (prefix, strike_with_side) = symbol.split_at(side_index);
    let strike = &strike_with_side[1..];
    if strike.len() != 8 || !strike.chars().all(|value| value.is_ascii_digit()) {
        return false;
    }
    if prefix.len() < 7 || prefix.len() > 12 {
        return false;
    }
    let root_len = prefix.len() - 6;
    let (root, expiration) = prefix.split_at(root_len);
    !root.is_empty()
        && root.len() <= 5
        && root.chars().all(|value| value.is_ascii_uppercase())
        && expiration.chars().all(|value| value.is_ascii_digit())
}

async fn load_option_bars(
    client: &AlpacaHttpClient,
    storage: Option<&StorageRepository>,
    account_id: &str,
    symbols: Vec<String>,
    timeframe: &str,
    _feed: &str,
    start: &str,
    end: &str,
) -> anyhow::Result<BTreeMap<String, Vec<AlpacaOptionBar>>> {
    if symbols.is_empty() {
        return Ok(BTreeMap::new());
    }
    let cache_key = market_data_cache_key(&symbols, &["standard-v2", timeframe, start, end]);
    if let Some(cached) = read_cache(storage, account_id, "option_bars", &cache_key).await? {
        return Ok(cached);
    }
    let mut request = OptionBarsRequest::for_symbols(symbols, timeframe.to_string(), start.to_string());
    request.end = Some(end.to_string());
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
    let cache_key = market_data_cache_key(&symbols, &["standard-v2", start, end]);
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
    target_timestamp: &str,
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
            .and_then(|bars| stock_bar_close_near(bars, target_timestamp)));
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
        .and_then(|bars| stock_bar_close_near(bars, target_timestamp)))
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
    let raw = format!("{}|{}", parts.join("|"), symbols.join(","));
    let mut hasher = DefaultHasher::new();
    raw.hash(&mut hasher);
    format!("{}|{:016x}", parts.join("|"), hasher.finish())
}

fn build_snapshot_map(
    contracts: &[AlpacaOptionContract],
    bars_by_symbol: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    trades_by_symbol: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
    scan_date: NaiveDate,
    underlying_price: Option<f64>,
    assumed_iv: f64,
    synthetic_spread_pct: f64,
    target_timestamp: &str,
) -> BTreeMap<String, AlpacaOptionSnapshot> {
    let mut snapshots = BTreeMap::new();
    for contract in contracts {
        let Some(mark) =
            historical_mark_near(&contract.symbol, bars_by_symbol, trades_by_symbol, target_timestamp)
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

fn historical_mark_near(
    symbol: &str,
    bars_by_symbol: &BTreeMap<String, Vec<AlpacaOptionBar>>,
    trades_by_symbol: &BTreeMap<String, Vec<AlpacaOptionTrade>>,
    target_timestamp: &str,
) -> Option<HistoricalMark> {
    if let Some(trade) = trades_by_symbol
        .get(symbol)
        .and_then(|trades| nearest_option_trade(trades, target_timestamp))
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
        .and_then(|bars| nearest_option_bar(bars, target_timestamp))?;
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
        rho: None,
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

fn nearest_option_bar<'a>(
    bars: &'a [AlpacaOptionBar],
    target_timestamp: &str,
) -> Option<&'a AlpacaOptionBar> {
    bars.iter()
        .filter(|bar| bar.close.is_some_and(|value| value > 0.0))
        .filter_map(|bar| timestamp_distance_secs(bar.timestamp.as_deref()?, target_timestamp).map(|distance| (distance, bar)))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, bar)| bar)
}

fn nearest_option_trade<'a>(
    trades: &'a [AlpacaOptionTrade],
    target_timestamp: &str,
) -> Option<&'a AlpacaOptionTrade> {
    trades
        .iter()
        .filter(|trade| trade.price.is_some_and(|value| value > 0.0))
        .filter_map(|trade| timestamp_distance_secs(trade.timestamp.as_deref()?, target_timestamp).map(|distance| (distance, trade)))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, trade)| trade)
}

fn stock_bar_close_near(bars: &[AlpacaStockBar], target_timestamp: &str) -> Option<f64> {
    bars.iter()
        .filter_map(|bar| {
            let close = bar.close.filter(|value| *value > 0.0)?;
            let distance = timestamp_distance_secs(bar.timestamp.as_deref()?, target_timestamp)?;
            Some((distance, close))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, close)| close)
}

fn timestamp_distance_secs(timestamp: &str, target_timestamp: &str) -> Option<i64> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let target = chrono::DateTime::parse_from_rfc3339(target_timestamp).ok()?;
    Some((timestamp - target).num_seconds().abs())
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

async fn load_trading_dates(
    client: &AlpacaHttpClient,
    start: NaiveDate,
    end: NaiveDate,
) -> anyhow::Result<Vec<NaiveDate>> {
    let request = MarketCalendarRequest::new(start.to_string(), end.to_string());
    let mut dates = client
        .market_calendar(&request)
        .await?
        .into_iter()
        .filter_map(|day| NaiveDate::parse_from_str(&day.date, "%Y-%m-%d").ok())
        .collect::<Vec<_>>();
    dates.sort();
    dates.dedup();
    Ok(dates)
}

fn summarize(
    days: &[BacktestDay],
    breakdown_period: BreakdownPeriod,
    unclosed_valuation: UnclosedValuation,
) -> BacktestSummary {
    let mut summary = BacktestSummary {
        scan_days: days
            .iter()
            .map(|day| day.trade_date.clone())
            .collect::<BTreeSet<_>>()
            .len(),
        evaluated_underlying_days: days.len(),
        ..BacktestSummary::default()
    };
    let mut period_scan_dates = BTreeMap::<String, BTreeSet<String>>::new();

    for day in days {
        let period = period_key(&day.trade_date, breakdown_period);
        let period_summary = summary.by_period.entry(period.clone()).or_default();
        period_summary.evaluated_underlying_days += 1;
        period_scan_dates
            .entry(period)
            .or_default()
            .insert(day.trade_date.clone());
        if let Some(reason) = day.data_quality_rejection.as_ref() {
            *summary
                .by_data_quality_rejection
                .entry(reason.clone())
                .or_insert(0) += 1;
            *period_summary
                .by_data_quality_rejection
                .entry(reason.clone())
                .or_insert(0) += 1;
        }

        let Some(trade) = day.selected.as_ref() else {
            continue;
        };
        let accounted_pnl = accounted_pnl_for_trade(trade, unclosed_valuation);
        summary.selected_trades += 1;
        period_summary.selected_trades += 1;
        update_risk_metrics(&mut summary.accounted, accounted_pnl);
        update_risk_metrics(&mut period_summary.accounted, accounted_pnl);
        *summary
            .by_exit_status
            .entry(trade.exit_status.clone())
            .or_insert(0) += 1;
        *period_summary
            .by_exit_status
            .entry(trade.exit_status.clone())
            .or_insert(0) += 1;
        let strategy = summary.by_strategy.entry(trade.strategy.clone()).or_default();
        strategy.selected_trades += 1;
        update_risk_metrics(&mut strategy.accounted, accounted_pnl);
        *strategy
            .by_exit_status
            .entry(trade.exit_status.clone())
            .or_insert(0) += 1;
        let period_strategy = period_summary
            .by_strategy
            .entry(trade.strategy.clone())
            .or_default();
        period_strategy.selected_trades += 1;
        update_risk_metrics(&mut period_strategy.accounted, accounted_pnl);
        *period_strategy
            .by_exit_status
            .entry(trade.exit_status.clone())
            .or_insert(0) += 1;
        if let Some(pnl) = trade.pnl {
            summary.closed_trades += 1;
            strategy.closed_trades += 1;
            period_summary.closed_trades += 1;
            period_strategy.closed_trades += 1;
            summary.total_pnl += pnl;
            strategy.total_pnl += pnl;
            period_summary.total_pnl += pnl;
            period_strategy.total_pnl += pnl;
            if pnl > 0.0 {
                summary.winning_trades += 1;
                strategy.winning_trades += 1;
                period_summary.winning_trades += 1;
                period_strategy.winning_trades += 1;
            }
        } else {
            summary.unclosed_trades += 1;
            strategy.unclosed_trades += 1;
            period_summary.unclosed_trades += 1;
            period_strategy.unclosed_trades += 1;
        }
    }

    for (period, scan_dates) in period_scan_dates {
        if let Some(period_summary) = summary.by_period.get_mut(&period) {
            period_summary.scan_days = scan_dates.len();
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
        finalize_risk_metrics(&mut strategy.accounted);
    }
    for period in summary.by_period.values_mut() {
        if period.closed_trades > 0 {
            period.average_pnl = period.total_pnl / period.closed_trades as f64;
            period.win_rate = period.winning_trades as f64 / period.closed_trades as f64;
        }
        finalize_risk_metrics(&mut period.accounted);
        for strategy in period.by_strategy.values_mut() {
            if strategy.closed_trades > 0 {
                strategy.average_pnl = strategy.total_pnl / strategy.closed_trades as f64;
                strategy.win_rate = strategy.winning_trades as f64 / strategy.closed_trades as f64;
            }
            finalize_risk_metrics(&mut strategy.accounted);
        }
    }
    finalize_risk_metrics(&mut summary.accounted);
    summary
}

fn build_candidate_card(
    variant: &str,
    underlyings: Vec<String>,
    strategies: Vec<String>,
    summary: &BacktestSummary,
) -> CandidateCard {
    let data_quality_rejections = summary.by_data_quality_rejection.values().sum();
    let active_periods = summary
        .by_period
        .values()
        .filter(|period| period.selected_trades > 0)
        .count();
    let positive_periods = summary
        .by_period
        .values()
        .filter(|period| period.accounted.total_pnl > 0.0)
        .count();
    let negative_periods = summary
        .by_period
        .values()
        .filter(|period| period.accounted.total_pnl < 0.0)
        .count();
    let profit_factor = summary.accounted.profit_factor.unwrap_or(f64::INFINITY);
    let (verdict, verdict_reason) = if summary.selected_trades >= 20
        && summary.unclosed_trades == 0
        && summary.accounted.total_pnl > 0.0
        && profit_factor >= 1.5
        && summary.accounted.max_drawdown <= 100.0
        && negative_periods <= positive_periods
    {
        ("candidate", "passes sample, PnL, drawdown, profit-factor, and data-closure checks")
    } else if summary.selected_trades >= 10
        && summary.unclosed_trades == 0
        && summary.accounted.total_pnl > 0.0
    {
        ("watchlist", "positive but misses one or more candidate thresholds")
    } else {
        ("reject", "insufficient sample, negative expectancy, unresolved exits, or weak risk metrics")
    };

    CandidateCard {
        variant: variant.to_string(),
        strategies,
        underlyings,
        selected_trades: summary.selected_trades,
        closed_trades: summary.closed_trades,
        unclosed_trades: summary.unclosed_trades,
        data_quality_rejections,
        accounted_pnl: summary.accounted.total_pnl,
        average_pnl: summary.accounted.average_pnl,
        win_rate: summary.accounted.win_rate,
        profit_factor: summary.accounted.profit_factor,
        max_drawdown: summary.accounted.max_drawdown,
        worst_trade: summary.accounted.worst_trade,
        active_periods,
        positive_periods,
        negative_periods,
        verdict: verdict.to_string(),
        verdict_reason: verdict_reason.to_string(),
    }
}

fn accounted_pnl_for_trade(trade: &TradeBacktest, unclosed_valuation: UnclosedValuation) -> f64 {
    if let Some(pnl) = trade.pnl {
        return pnl;
    }
    match unclosed_valuation {
        UnclosedValuation::Ignore => 0.0,
        UnclosedValuation::Conservative => trade.conservative_pnl.unwrap_or(0.0),
        UnclosedValuation::WorstObserved => trade
            .worst_observed_pnl
            .or(trade.conservative_pnl)
            .unwrap_or(0.0),
    }
}

fn update_risk_metrics(metrics: &mut RiskMetrics, pnl: f64) {
    metrics.trades += 1;
    metrics.total_pnl += pnl;
    if pnl > 0.0 {
        metrics.winning_trades += 1;
        metrics.gross_profit += pnl;
    } else if pnl < 0.0 {
        metrics.losing_trades += 1;
        metrics.gross_loss += -pnl;
    }
    metrics.best_trade = Some(metrics.best_trade.map_or(pnl, |value| value.max(pnl)));
    metrics.worst_trade = Some(metrics.worst_trade.map_or(pnl, |value| value.min(pnl)));
    metrics.equity += pnl;
    metrics.peak_equity = metrics.peak_equity.max(metrics.equity);
    metrics.max_drawdown = metrics.max_drawdown.max(metrics.peak_equity - metrics.equity);
}

fn finalize_risk_metrics(metrics: &mut RiskMetrics) {
    if metrics.trades == 0 {
        return;
    }
    metrics.average_pnl = metrics.total_pnl / metrics.trades as f64;
    metrics.expectancy = metrics.average_pnl;
    metrics.win_rate = metrics.winning_trades as f64 / metrics.trades as f64;
    metrics.profit_factor = if metrics.gross_loss > 0.0 {
        Some(metrics.gross_profit / metrics.gross_loss)
    } else if metrics.gross_profit > 0.0 {
        None
    } else {
        Some(0.0)
    };
}

fn period_key(trade_date: &str, breakdown_period: BreakdownPeriod) -> String {
    let Ok(date) = NaiveDate::parse_from_str(trade_date, "%Y-%m-%d") else {
        return trade_date.to_string();
    };
    match breakdown_period {
        BreakdownPeriod::Day => date.to_string(),
        BreakdownPeriod::Week => {
            let week = date.iso_week();
            format!("{:04}-W{:02}", week.year(), week.week())
        }
        BreakdownPeriod::Month => format!("{:04}-{:02}", date.year(), date.month()),
        BreakdownPeriod::Quarter => {
            let quarter = ((date.month() - 1) / 3) + 1;
            format!("{:04}-Q{quarter}", date.year())
        }
        BreakdownPeriod::Year => date.year().to_string(),
    }
}

fn sweep_variants(args: &Args) -> Vec<SweepVariant> {
    let synthetic_spreads = if args.sweep_synthetic_spread_pct.is_empty() {
        vec![args.synthetic_spread_pct]
    } else {
        args.sweep_synthetic_spread_pct.clone()
    };
    let short_delta_mins = if args.sweep_short_delta_min.is_empty() {
        vec![None]
    } else {
        args.sweep_short_delta_min.iter().copied().map(Some).collect()
    };
    let short_delta_maxes = if args.sweep_short_delta_max.is_empty() {
        vec![None]
    } else {
        args.sweep_short_delta_max.iter().copied().map(Some).collect()
    };
    let min_return_on_risks = if args.sweep_min_return_on_risk.is_empty() {
        vec![None]
    } else {
        args.sweep_min_return_on_risk
            .iter()
            .copied()
            .map(Some)
            .collect()
    };
    let profit_target_close_fractions = if args.sweep_profit_target_close_fraction.is_empty() {
        vec![args.profit_target_close_fraction]
    } else {
        args.sweep_profit_target_close_fraction
            .iter()
            .copied()
            .map(Some)
            .collect()
    };
    let stop_loss_close_multiples = if args.sweep_stop_loss_close_multiple.is_empty() {
        vec![args.stop_loss_close_multiple]
    } else {
        args.sweep_stop_loss_close_multiple
            .iter()
            .copied()
            .map(Some)
            .collect()
    };
    let max_hold_secs_values = if args.sweep_max_hold_mins.is_empty() {
        vec![args.max_hold_secs]
    } else {
        args.sweep_max_hold_mins
            .iter()
            .copied()
            .map(|value| Some(value * 60))
            .collect()
    };

    let mut variants = Vec::new();
    for synthetic_spread_pct in synthetic_spreads {
        for short_delta_min in &short_delta_mins {
            for short_delta_max in &short_delta_maxes {
                for min_return_on_risk in &min_return_on_risks {
                    for profit_target_close_fraction in &profit_target_close_fractions {
                        for stop_loss_close_multiple in &stop_loss_close_multiples {
                            for max_hold_secs in &max_hold_secs_values {
                                let mut label_parts =
                                    vec![format!("spread={synthetic_spread_pct:.4}")];
                                if let Some(value) = short_delta_min {
                                    label_parts.push(format!("short_delta_min={value:.3}"));
                                }
                                if let Some(value) = short_delta_max {
                                    label_parts.push(format!("short_delta_max={value:.3}"));
                                }
                                if let Some(value) = min_return_on_risk {
                                    label_parts.push(format!("min_ror={value:.3}"));
                                }
                                if let Some(value) = profit_target_close_fraction {
                                    label_parts.push(format!("profit={value:.3}"));
                                }
                                if let Some(value) = stop_loss_close_multiple {
                                    label_parts.push(format!("stop={value:.3}"));
                                }
                                if let Some(value) = max_hold_secs {
                                    label_parts.push(format!("max_hold_mins={}", value / 60));
                                }
                                variants.push(SweepVariant {
                                    label: label_parts.join(","),
                                    synthetic_spread_pct,
                                    short_delta_min: *short_delta_min,
                                    short_delta_max: *short_delta_max,
                                    min_return_on_risk: *min_return_on_risk,
                                    profit_target_close_fraction: *profit_target_close_fraction,
                                    stop_loss_close_multiple: *stop_loss_close_multiple,
                                    max_hold_secs: *max_hold_secs,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    if variants.len() == 1
        && args.sweep_synthetic_spread_pct.is_empty()
        && args.sweep_short_delta_min.is_empty()
        && args.sweep_short_delta_max.is_empty()
        && args.sweep_min_return_on_risk.is_empty()
        && args.sweep_profit_target_close_fraction.is_empty()
        && args.sweep_stop_loss_close_multiple.is_empty()
        && args.sweep_max_hold_mins.is_empty()
    {
        variants[0].label = "base".to_string();
    }
    variants
}

fn apply_historical_scanner_overrides(config: &mut OptionsEngineConfig, args: &Args) {
    config.scanner.min_open_interest = args.historical_min_open_interest;
    config.iron_condor_scanner.credit.min_open_interest = args.historical_min_open_interest;
    config.debit_scanner.min_open_interest = args.historical_min_open_interest;
    config.naked_scanner.min_open_interest = args.historical_min_open_interest;
    config.naked_1_3dte_scanner.min_open_interest = args.historical_min_open_interest;

    config.scanner.max_leg_spread_pct = args.historical_max_leg_spread_pct;
    config.iron_condor_scanner.credit.max_leg_spread_pct = args.historical_max_leg_spread_pct;
    config.debit_scanner.max_leg_spread_pct = args.historical_max_leg_spread_pct;
    config.naked_scanner.max_spread_pct = args.historical_max_leg_spread_pct;
    config.naked_1_3dte_scanner.max_spread_pct = args.historical_max_leg_spread_pct;
}

fn apply_sweep_variant(config: &mut OptionsEngineConfig, variant: &SweepVariant) {
    if let Some(value) = variant.short_delta_min {
        config.scanner.short_delta_min = value;
        config.iron_condor_scanner.credit.short_delta_min = value;
        config.naked_scanner.short_delta_min = value;
        config.naked_1_3dte_scanner.short_delta_min = value;
    }
    if let Some(value) = variant.short_delta_max {
        config.scanner.short_delta_max = value;
        config.iron_condor_scanner.credit.short_delta_max = value;
        config.naked_scanner.short_delta_max = value;
        config.naked_1_3dte_scanner.short_delta_max = value;
    }
    if let Some(value) = variant.min_return_on_risk {
        config.scanner.min_return_on_risk = value;
        config.iron_condor_scanner.credit.min_return_on_risk = value;
        config.iron_condor_scanner.min_return_on_risk = value;
    }
    if let Some(value) = variant.profit_target_close_fraction {
        config.profit_target_close_fraction = value;
    }
    if let Some(value) = variant.stop_loss_close_multiple {
        config.stop_loss_close_multiple = value;
    }
    if let Some(value) = variant.max_hold_secs {
        config.max_hold_secs = value;
    }
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
    if let Some(value) = args.profit_target_close_fraction {
        if value <= 0.0 {
            bail!("--profit-target-close-fraction must be greater than zero");
        }
        config.profit_target_close_fraction = value;
    }
    if let Some(value) = args.stop_loss_close_multiple {
        if value < 0.0 {
            bail!("--stop-loss-close-multiple cannot be negative");
        }
        config.stop_loss_close_multiple = value;
    }
    if let Some(value) = args.max_hold_secs {
        config.max_hold_secs = value;
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

fn expanded_cli_args() -> anyhow::Result<Vec<String>> {
    let raw = env::args().skip(1).collect::<Vec<_>>();
    let mut profile = None;
    let mut profile_file = None;
    let mut filtered = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--profile" => {
                let value = raw
                    .get(index + 1)
                    .ok_or_else(|| anyhow!("--profile requires a value"))?;
                profile = Some(value.clone());
                index += 2;
            }
            "--profile-file" => {
                let value = raw
                    .get(index + 1)
                    .ok_or_else(|| anyhow!("--profile-file requires a value"))?;
                profile_file = Some(value.clone());
                index += 2;
            }
            _ => {
                filtered.push(raw[index].clone());
                index += 1;
            }
        }
    }

    let mut expanded = Vec::new();
    if let Some(profile) = profile {
        expanded.extend(builtin_profile_args(&profile)?);
    }
    if let Some(path) = profile_file {
        expanded.extend(profile_file_args(&path)?);
    }
    expanded.extend(filtered);
    Ok(expanded)
}

fn builtin_profile_args(name: &str) -> anyhow::Result<Vec<String>> {
    let mut args = common_profile_args();
    match name {
        "put_credit_spy_strict" => {
            push_arg(&mut args, "--underlyings", "SPY");
            push_arg(&mut args, "--strategies", "put_credit");
            push_arg(&mut args, "--synthetic-spread-pct", "0.05");
            push_arg(&mut args, "--sweep-min-return-on-risk", "0.16");
        }
        "call_credit_qqq_strict" => {
            push_arg(&mut args, "--underlyings", "QQQ");
            push_arg(&mut args, "--strategies", "call_credit");
            push_arg(&mut args, "--synthetic-spread-pct", "0.03");
            push_arg(&mut args, "--sweep-min-return-on-risk", "0.20");
        }
        "iron_condor_spy_sweep" => {
            push_arg(&mut args, "--underlyings", "SPY");
            push_arg(&mut args, "--strategies", "iron_condor");
            push_arg(&mut args, "--sweep-synthetic-spread-pct", "0.03,0.05,0.08");
            push_arg(&mut args, "--sweep-min-return-on-risk", "0.13,0.16,0.20");
        }
        "call_debit_spy_sweep" => {
            push_arg(&mut args, "--underlyings", "SPY");
            push_arg(&mut args, "--strategies", "call_debit");
            push_arg(&mut args, "--sweep-synthetic-spread-pct", "0.03,0.05,0.08");
        }
        "put_debit_spy_sweep" => {
            push_arg(&mut args, "--underlyings", "SPY");
            push_arg(&mut args, "--strategies", "put_debit");
            push_arg(&mut args, "--sweep-synthetic-spread-pct", "0.03,0.05,0.08");
        }
        "debit_spreads_spy_sweep" => {
            push_arg(&mut args, "--underlyings", "SPY");
            push_arg(&mut args, "--strategies", "call_debit,put_debit");
            push_arg(&mut args, "--sweep-synthetic-spread-pct", "0.03,0.05,0.08");
        }
        other => bail!("unknown backtest profile {other}"),
    }
    Ok(args)
}

fn common_profile_args() -> Vec<String> {
    let mut args = Vec::new();
    push_arg(&mut args, "--start", "2024-02-01");
    push_arg(&mut args, "--end", "2026-02-28");
    push_arg(&mut args, "--entry-time", "09:45");
    push_arg(&mut args, "--exit-time", "15:45");
    push_arg(&mut args, "--profit-target-close-fraction", "0.50");
    push_arg(&mut args, "--stop-loss-close-multiple", "2.00");
    push_arg(&mut args, "--max-hold-mins", "240");
    push_arg(&mut args, "--breakdown-period", "month");
    push_arg(&mut args, "--unclosed-valuation", "worst_observed");
    args.push("--require-exit-common-timestamp".to_string());
    args
}

fn profile_file_args(path: &str) -> anyhow::Result<Vec<String>> {
    let raw = fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
    let profile = serde_json::from_str::<BacktestProfilePatch>(&raw)
        .with_context(|| format!("failed to parse JSON profile {path}"))?;
    Ok(profile_patch_args(profile))
}

fn profile_patch_args(profile: BacktestProfilePatch) -> Vec<String> {
    let mut args = Vec::new();
    push_opt_arg(&mut args, "--start", profile.start);
    push_opt_arg(&mut args, "--end", profile.end);
    push_opt_arg(&mut args, "--entry-time", profile.entry_time);
    push_opt_arg(&mut args, "--exit-time", profile.exit_time);
    push_opt_csv(&mut args, "--underlyings", profile.underlyings);
    push_opt_csv(&mut args, "--strategies", profile.strategies);
    push_opt_arg(&mut args, "--timeframe", profile.timeframe);
    push_opt_arg(&mut args, "--option-feed", profile.option_feed);
    push_opt_arg(&mut args, "--stock-feed", profile.stock_feed);
    push_opt_arg(&mut args, "--assumed-iv", profile.assumed_iv.map(|value| value.to_string()));
    push_opt_arg(
        &mut args,
        "--synthetic-spread-pct",
        profile.synthetic_spread_pct.map(|value| value.to_string()),
    );
    push_opt_arg(
        &mut args,
        "--entry-mark-window-mins",
        profile.entry_mark_window_mins.map(|value| value.to_string()),
    );
    push_opt_arg(
        &mut args,
        "--historical-min-open-interest",
        profile.historical_min_open_interest.map(|value| value.to_string()),
    );
    push_opt_arg(
        &mut args,
        "--missing-open-interest",
        profile.missing_open_interest.map(|value| value.to_string()),
    );
    push_opt_arg(
        &mut args,
        "--historical-max-leg-spread-pct",
        profile.historical_max_leg_spread_pct.map(|value| value.to_string()),
    );
    push_opt_float_csv(
        &mut args,
        "--sweep-synthetic-spread-pct",
        profile.sweep_synthetic_spread_pct,
    );
    push_opt_float_csv(&mut args, "--sweep-short-delta-min", profile.sweep_short_delta_min);
    push_opt_float_csv(&mut args, "--sweep-short-delta-max", profile.sweep_short_delta_max);
    push_opt_float_csv(
        &mut args,
        "--sweep-min-return-on-risk",
        profile.sweep_min_return_on_risk,
    );
    push_opt_arg(
        &mut args,
        "--profit-target-close-fraction",
        profile.profit_target_close_fraction.map(|value| value.to_string()),
    );
    push_opt_arg(
        &mut args,
        "--stop-loss-close-multiple",
        profile.stop_loss_close_multiple.map(|value| value.to_string()),
    );
    push_opt_arg(
        &mut args,
        "--max-hold-mins",
        profile.max_hold_mins.map(|value| value.to_string()),
    );
    push_opt_arg(
        &mut args,
        "--max-hold-secs",
        profile.max_hold_secs.map(|value| value.to_string()),
    );
    push_opt_float_csv(
        &mut args,
        "--sweep-profit-target-close-fraction",
        profile.sweep_profit_target_close_fraction,
    );
    push_opt_float_csv(
        &mut args,
        "--sweep-stop-loss-close-multiple",
        profile.sweep_stop_loss_close_multiple,
    );
    push_opt_u64_csv(&mut args, "--sweep-max-hold-mins", profile.sweep_max_hold_mins);
    push_opt_arg(&mut args, "--breakdown-period", profile.breakdown_period);
    push_opt_arg(&mut args, "--unclosed-valuation", profile.unclosed_valuation);
    push_opt_arg(
        &mut args,
        "--min-exit-leg-bars",
        profile.min_exit_leg_bars.map(|value| value.to_string()),
    );
    push_opt_arg(&mut args, "--quantity", profile.quantity.map(|value| value.to_string()));
    if profile.require_exit_common_timestamp.unwrap_or(false) {
        args.push("--require-exit-common-timestamp".to_string());
    }
    args
}

fn push_arg(args: &mut Vec<String>, flag: &str, value: &str) {
    args.push(flag.to_string());
    args.push(value.to_string());
}

fn push_opt_arg(args: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        push_arg(args, flag, &value);
    }
}

fn push_opt_csv(args: &mut Vec<String>, flag: &str, values: Option<Vec<String>>) {
    if let Some(values) = values {
        push_arg(args, flag, &values.join(","));
    }
}

fn push_opt_float_csv(args: &mut Vec<String>, flag: &str, values: Option<Vec<f64>>) {
    if let Some(values) = values {
        let value = values
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        push_arg(args, flag, &value);
    }
}

fn push_opt_u64_csv(args: &mut Vec<String>, flag: &str, values: Option<Vec<u64>>) {
    if let Some(values) = values {
        let value = values
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        push_arg(args, flag, &value);
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
    let mut entry_mark_window_mins = 10;
    let mut historical_min_open_interest = 0;
    let mut missing_open_interest = 0;
    let mut historical_max_leg_spread_pct = 0.80;
    let mut sweep_synthetic_spread_pct = Vec::new();
    let mut sweep_short_delta_min = Vec::new();
    let mut sweep_short_delta_max = Vec::new();
    let mut sweep_min_return_on_risk = Vec::new();
    let mut profit_target_close_fraction = None;
    let mut stop_loss_close_multiple = None;
    let mut max_hold_secs = None;
    let mut sweep_profit_target_close_fraction = Vec::new();
    let mut sweep_stop_loss_close_multiple = Vec::new();
    let mut sweep_max_hold_mins = Vec::new();
    let mut breakdown_period = BreakdownPeriod::Month;
    let mut unclosed_valuation = UnclosedValuation::Ignore;
    let mut min_exit_leg_bars = 0;
    let mut require_exit_common_timestamp = false;
    let mut trade_export_csv = None;
    let mut trade_export_json = None;
    let mut quantity = None;
    let mut json_output = false;

    let mut iter = expanded_cli_args()?.into_iter();
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
            "--entry-mark-window-mins" => {
                entry_mark_window_mins = next_value(&mut iter, "--entry-mark-window-mins")?
                    .parse::<i64>()
                    .context("--entry-mark-window-mins must be a non-negative integer")?;
                if entry_mark_window_mins < 0 {
                    bail!("--entry-mark-window-mins cannot be negative");
                }
            }
            "--historical-min-open-interest" => {
                historical_min_open_interest =
                    next_value(&mut iter, "--historical-min-open-interest")?
                        .parse::<u64>()
                        .context("--historical-min-open-interest must be a non-negative integer")?;
            }
            "--missing-open-interest" => {
                missing_open_interest = next_value(&mut iter, "--missing-open-interest")?
                    .parse::<u64>()
                    .context("--missing-open-interest must be a non-negative integer")?;
            }
            "--historical-max-leg-spread-pct" => {
                historical_max_leg_spread_pct =
                    next_value(&mut iter, "--historical-max-leg-spread-pct")?
                        .parse::<f64>()
                        .context("--historical-max-leg-spread-pct must be a decimal")?;
                if historical_max_leg_spread_pct <= 0.0 {
                    bail!("--historical-max-leg-spread-pct must be greater than zero");
                }
            }
            "--sweep-synthetic-spread-pct" => {
                sweep_synthetic_spread_pct =
                    parse_float_csv(next_value(&mut iter, "--sweep-synthetic-spread-pct")?)?;
            }
            "--sweep-short-delta-min" => {
                sweep_short_delta_min =
                    parse_float_csv(next_value(&mut iter, "--sweep-short-delta-min")?)?;
            }
            "--sweep-short-delta-max" => {
                sweep_short_delta_max =
                    parse_float_csv(next_value(&mut iter, "--sweep-short-delta-max")?)?;
            }
            "--sweep-min-return-on-risk" => {
                sweep_min_return_on_risk =
                    parse_float_csv(next_value(&mut iter, "--sweep-min-return-on-risk")?)?;
            }
            "--profit-target-close-fraction" => {
                let value = next_value(&mut iter, "--profit-target-close-fraction")?
                    .parse::<f64>()
                    .context("--profit-target-close-fraction must be a decimal")?;
                if value <= 0.0 {
                    bail!("--profit-target-close-fraction must be greater than zero");
                }
                profit_target_close_fraction = Some(value);
            }
            "--stop-loss-close-multiple" => {
                let value = next_value(&mut iter, "--stop-loss-close-multiple")?
                    .parse::<f64>()
                    .context("--stop-loss-close-multiple must be a decimal")?;
                if value < 0.0 {
                    bail!("--stop-loss-close-multiple cannot be negative");
                }
                stop_loss_close_multiple = Some(value);
            }
            "--max-hold-mins" => {
                max_hold_secs = Some(
                    next_value(&mut iter, "--max-hold-mins")?
                        .parse::<u64>()
                        .context("--max-hold-mins must be a non-negative integer")?
                        * 60,
                );
            }
            "--max-hold-secs" => {
                max_hold_secs = Some(
                    next_value(&mut iter, "--max-hold-secs")?
                        .parse::<u64>()
                        .context("--max-hold-secs must be a non-negative integer")?,
                );
            }
            "--sweep-profit-target-close-fraction" => {
                sweep_profit_target_close_fraction = parse_float_csv(next_value(
                    &mut iter,
                    "--sweep-profit-target-close-fraction",
                )?)?;
            }
            "--sweep-stop-loss-close-multiple" => {
                sweep_stop_loss_close_multiple =
                    parse_float_csv(next_value(&mut iter, "--sweep-stop-loss-close-multiple")?)?;
            }
            "--sweep-max-hold-mins" => {
                sweep_max_hold_mins =
                    parse_u64_csv(next_value(&mut iter, "--sweep-max-hold-mins")?)?;
            }
            "--breakdown-period" => {
                breakdown_period =
                    parse_breakdown_period(&next_value(&mut iter, "--breakdown-period")?)?;
            }
            "--unclosed-valuation" | "--unresolved-exit-policy" => {
                unclosed_valuation =
                    parse_unclosed_valuation(&next_value(&mut iter, "--unclosed-valuation")?)?;
            }
            "--min-exit-leg-bars" => {
                min_exit_leg_bars = next_value(&mut iter, "--min-exit-leg-bars")?
                    .parse::<usize>()
                    .context("--min-exit-leg-bars must be a non-negative integer")?;
            }
            "--require-exit-common-timestamp" => {
                require_exit_common_timestamp = true;
            }
            "--trade-export" | "--trade-export-csv" => {
                trade_export_csv = Some(next_value(&mut iter, "--trade-export-csv")?);
            }
            "--trade-export-json" => {
                trade_export_json = Some(next_value(&mut iter, "--trade-export-json")?);
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
        entry_mark_window_mins,
        historical_min_open_interest,
        missing_open_interest,
        historical_max_leg_spread_pct,
        sweep_synthetic_spread_pct,
        sweep_short_delta_min,
        sweep_short_delta_max,
        sweep_min_return_on_risk,
        profit_target_close_fraction,
        stop_loss_close_multiple,
        max_hold_secs,
        sweep_profit_target_close_fraction,
        sweep_stop_loss_close_multiple,
        sweep_max_hold_mins,
        breakdown_period,
        unclosed_valuation,
        min_exit_leg_bars,
        require_exit_common_timestamp,
        trade_export_csv,
        trade_export_json,
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

fn parse_breakdown_period(value: &str) -> anyhow::Result<BreakdownPeriod> {
    match value.to_ascii_lowercase().as_str() {
        "day" | "daily" => Ok(BreakdownPeriod::Day),
        "week" | "weekly" => Ok(BreakdownPeriod::Week),
        "month" | "monthly" => Ok(BreakdownPeriod::Month),
        "quarter" | "quarterly" => Ok(BreakdownPeriod::Quarter),
        "year" | "yearly" | "annual" | "annually" => Ok(BreakdownPeriod::Year),
        _ => bail!("--breakdown-period must be one of day, week, month, quarter, year"),
    }
}

fn parse_unclosed_valuation(value: &str) -> anyhow::Result<UnclosedValuation> {
    match value.to_ascii_lowercase().as_str() {
        "ignore" | "none" => Ok(UnclosedValuation::Ignore),
        "conservative" | "max_loss" | "max-loss" | "pessimistic" => {
            Ok(UnclosedValuation::Conservative)
        }
        "worst_observed" | "worst-observed" | "observed" => Ok(UnclosedValuation::WorstObserved),
        _ => bail!("--unclosed-valuation must be one of ignore, conservative, worst_observed"),
    }
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

fn parse_float_csv(value: String) -> anyhow::Result<Vec<f64>> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<f64>()
                .with_context(|| format!("invalid numeric sweep value {value}"))
        })
        .collect()
}

fn parse_u64_csv(value: String) -> anyhow::Result<Vec<u64>> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<u64>()
                .with_context(|| format!("invalid integer sweep value {value}"))
        })
        .collect()
}

fn trade_export_records(reports: &[BacktestReport]) -> Vec<TradeExportRecord> {
    let mut records = Vec::new();
    for report in reports {
        for day in &report.days {
            let Some(trade) = day.selected.as_ref() else {
                continue;
            };
            records.push(TradeExportRecord {
                variant: report.variant.clone(),
                strategy: trade.strategy.clone(),
                underlying: trade.underlying.clone(),
                trade_date: trade.trade_date.clone(),
                entry_timestamp: trade.entry_timestamp.clone(),
                exit_timestamp: trade.exit_timestamp.clone(),
                quantity: trade.quantity,
                score: trade.score,
                premium_kind: trade.premium_kind.clone(),
                entry_premium: trade.entry_premium,
                entry_net_cashflow: trade.entry_net_cashflow,
                exit_net_cashflow: trade.exit_net_cashflow,
                pnl: trade.pnl,
                conservative_pnl: trade.conservative_pnl,
                conservative_status: trade.conservative_status.clone(),
                worst_observed_pnl: trade.worst_observed_pnl,
                worst_observed_status: trade.worst_observed_status.clone(),
                accounted_pnl: accounted_pnl_for_trade(trade, report_unclosed_valuation(report)),
                closed: trade.closed,
                exit_status: trade.exit_status.clone(),
                exit_diagnostic: trade.exit_diagnostic.clone(),
                exit_missing_leg_symbols: trade.exit_missing_leg_symbols.clone(),
                exit_bar_counts: trade.exit_bar_counts.clone(),
                exit_common_timestamps: trade.exit_common_timestamps,
                legs: trade.legs.clone(),
            });
        }
    }
    records
}

fn report_unclosed_valuation(report: &BacktestReport) -> UnclosedValuation {
    match report.unclosed_valuation {
        "conservative" => UnclosedValuation::Conservative,
        "worst_observed" => UnclosedValuation::WorstObserved,
        _ => UnclosedValuation::Ignore,
    }
}

fn write_trade_json_export(path: &str, reports: &[BacktestReport]) -> anyhow::Result<()> {
    let records = trade_export_records(reports);
    let file = File::create(path).with_context(|| format!("failed to create {path}"))?;
    serde_json::to_writer_pretty(file, &records)
        .with_context(|| format!("failed to write JSON trade export {path}"))
}

fn write_trade_csv_export(path: &str, reports: &[BacktestReport]) -> anyhow::Result<()> {
    let records = trade_export_records(reports);
    let mut file = File::create(path).with_context(|| format!("failed to create {path}"))?;
    writeln!(
        file,
        "variant,strategy,underlying,trade_date,entry_timestamp,exit_timestamp,quantity,score,premium_kind,entry_premium,entry_net_cashflow,exit_net_cashflow,pnl,conservative_pnl,conservative_status,worst_observed_pnl,worst_observed_status,accounted_pnl,closed,exit_status,exit_diagnostic,exit_missing_leg_symbols,exit_bar_counts,exit_common_timestamps,legs"
    )?;
    for record in records {
        writeln!(
            file,
            "{},{},{},{},{},{},{},{:.4},{},{:.4},{:.4},{},{},{},{},{},{:.2},{},{},{},{},{},{},{},{}",
            csv_cell(&record.variant),
            csv_cell(&record.strategy),
            csv_cell(&record.underlying),
            csv_cell(&record.trade_date),
            csv_cell(&record.entry_timestamp),
            csv_cell(&record.exit_timestamp),
            record.quantity,
            record.score,
            csv_cell(&record.premium_kind),
            record.entry_premium,
            record.entry_net_cashflow,
            record
                .exit_net_cashflow
                .map(|value| format!("{value:.4}"))
                .unwrap_or_default(),
            record.pnl.map(|value| format!("{value:.2}")).unwrap_or_default(),
            record
                .conservative_pnl
                .map(|value| format!("{value:.2}"))
                .unwrap_or_default(),
            csv_cell(record.conservative_status.as_deref().unwrap_or_default()),
            record
                .worst_observed_pnl
                .map(|value| format!("{value:.2}"))
                .unwrap_or_default(),
            csv_cell(record.worst_observed_status.as_deref().unwrap_or_default()),
            record.accounted_pnl,
            record.closed,
            csv_cell(&record.exit_status),
            csv_cell(&record.exit_diagnostic),
            csv_cell(&record.exit_missing_leg_symbols.join("|")),
            csv_cell(&format_exit_bar_counts(&record.exit_bar_counts)),
            record.exit_common_timestamps,
            csv_cell(&format_legs(&record.legs)),
        )?;
    }
    Ok(())
}

fn format_exit_bar_counts(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(symbol, count)| format!("{symbol}:{count}"))
        .collect::<Vec<_>>()
        .join("|")
}

fn format_legs(legs: &[BacktestLeg]) -> String {
    legs.iter()
        .map(|leg| {
            format!(
                "{}:{}:entry={}:exit={}",
                leg.symbol,
                leg.side.as_str(),
                leg.entry_close
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "-".to_string()),
                leg.exit_close
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "-".to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn csv_cell(value: &str) -> String {
    if value.contains(|ch| matches!(ch, ',' | '"' | '\n' | '\r')) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn print_report(report: &BacktestReport) {
    println!(
        "Alpaca options strategy backtest variant={} {} -> {} period={} unclosed_valuation={} entry={} exit={} strategies={} underlyings={} mark_source={}",
        report.variant,
        report.start,
        report.end,
        report.breakdown_period,
        report.unclosed_valuation,
        report.entry_time,
        report.exit_time,
        report.strategies.join(","),
        report.underlyings.join(","),
        report.mark_source,
    );
    println!(
        "summary scan_days={} underlying_days={} selected={} closed={} unclosed={} wins={} win_rate={:.1}% total_pnl={:.2} avg_pnl={:.2} accounted_pnl={:.2} accounted_avg={:.2} accounted_win_rate={:.1}% profit_factor={} max_dd={:.2} worst_trade={}",
        report.summary.scan_days,
        report.summary.evaluated_underlying_days,
        report.summary.selected_trades,
        report.summary.closed_trades,
        report.summary.unclosed_trades,
        report.summary.winning_trades,
        report.summary.win_rate * 100.0,
        report.summary.total_pnl,
        report.summary.average_pnl,
        report.summary.accounted.total_pnl,
        report.summary.accounted.average_pnl,
        report.summary.accounted.win_rate * 100.0,
        report
            .summary
            .accounted
            .profit_factor
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "inf".to_string()),
        report.summary.accounted.max_drawdown,
        report
            .summary
            .accounted
            .worst_trade
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "-".to_string()),
    );
    for (strategy, summary) in &report.summary.by_strategy {
        println!(
            "strategy={} selected={} closed={} unclosed={} wins={} win_rate={:.1}% total_pnl={:.2} avg_pnl={:.2}",
            strategy,
            summary.selected_trades,
            summary.closed_trades,
            summary.unclosed_trades,
            summary.winning_trades,
            summary.win_rate * 100.0,
            summary.total_pnl,
            summary.average_pnl,
        );
    }
    for (status, count) in &report.summary.by_exit_status {
        println!("exit_status={} count={}", status, count);
    }
    for (reason, count) in &report.summary.by_data_quality_rejection {
        println!("data_quality_rejection={} count={}", reason, count);
    }
    println!(
        "candidate verdict={} pnl={:.2} pf={} max_dd={:.2} trades={} rejected={} reason={}",
        report.candidate_card.verdict,
        report.candidate_card.accounted_pnl,
        report
            .candidate_card
            .profit_factor
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "inf".to_string()),
        report.candidate_card.max_drawdown,
        report.candidate_card.selected_trades,
        report.candidate_card.data_quality_rejections,
        report.candidate_card.verdict_reason,
    );
    for (period, summary) in &report.summary.by_period {
        println!(
            "period={} scan_days={} underlying_days={} selected={} closed={} unclosed={} wins={} win_rate={:.1}% total_pnl={:.2} avg_pnl={:.2} accounted_pnl={:.2} max_dd={:.2}",
            period,
            summary.scan_days,
            summary.evaluated_underlying_days,
            summary.selected_trades,
            summary.closed_trades,
            summary.unclosed_trades,
            summary.winning_trades,
            summary.win_rate * 100.0,
            summary.total_pnl,
            summary.average_pnl,
            summary.accounted.total_pnl,
            summary.accounted.max_drawdown,
        );
    }
    for day in &report.days {
        if let Some(trade) = &day.selected {
            println!(
                "trade date={} underlying={} strategy={} score={:.1} premium={} {:.2} pnl={} status={} diagnostic={}",
                trade.trade_date,
                trade.underlying,
                trade.strategy,
                trade.score,
                trade.premium_kind,
                trade.entry_premium,
                trade.pnl.map(|pnl| format!("{pnl:.2}")).unwrap_or_else(|| "-".to_string()),
                trade.exit_status,
                trade.exit_diagnostic,
            );
        }
    }
}

fn print_sweep_report(reports: &[BacktestReport]) {
    println!("Alpaca options strategy backtest sweep results:");
    let mut ranked = reports.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .summary
            .accounted
            .total_pnl
            .partial_cmp(&left.summary.accounted.total_pnl)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for report in ranked {
        println!(
            "variant={} selected={} closed={} unclosed={} win_rate={:.1}% total_pnl={:.2} avg_pnl={:.2} accounted_pnl={:.2} profit_factor={} max_dd={:.2}",
            report.variant,
            report.summary.selected_trades,
            report.summary.closed_trades,
            report.summary.unclosed_trades,
            report.summary.win_rate * 100.0,
            report.summary.total_pnl,
            report.summary.average_pnl,
            report.summary.accounted.total_pnl,
            report
                .summary
                .accounted
                .profit_factor
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| "inf".to_string()),
            report.summary.accounted.max_drawdown,
        );
        for (period, summary) in &report.summary.by_period {
            if summary.selected_trades == 0 && summary.closed_trades == 0 {
                continue;
            }
            println!(
                "  period={} selected={} closed={} unclosed={} win_rate={:.1}% total_pnl={:.2} avg_pnl={:.2} accounted_pnl={:.2}",
                period,
                summary.selected_trades,
                summary.closed_trades,
                summary.unclosed_trades,
                summary.win_rate * 100.0,
                summary.total_pnl,
                summary.average_pnl,
                summary.accounted.total_pnl,
            );
        }
    }

    println!("Candidate cards ranked by accounted PnL:");
    let mut cards = reports.iter().collect::<Vec<_>>();
    cards.sort_by(|left, right| {
        right
            .candidate_card
            .accounted_pnl
            .partial_cmp(&left.candidate_card.accounted_pnl)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for report in cards {
        let card = &report.candidate_card;
        println!(
            "card verdict={} variant={} strategies={} underlyings={} trades={} closed={} unclosed={} rejected={} pnl={:.2} avg={:.2} win_rate={:.1}% pf={} max_dd={:.2} worst={} active_periods={} pos_periods={} neg_periods={} reason={}",
            card.verdict,
            card.variant,
            card.strategies.join(","),
            card.underlyings.join(","),
            card.selected_trades,
            card.closed_trades,
            card.unclosed_trades,
            card.data_quality_rejections,
            card.accounted_pnl,
            card.average_pnl,
            card.win_rate * 100.0,
            card
                .profit_factor
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| "inf".to_string()),
            card.max_drawdown,
            card
                .worst_trade
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| "-".to_string()),
            card.active_periods,
            card.positive_periods,
            card.negative_periods,
            card.verdict_reason,
        );
    }
}

fn print_usage() {
    println!(
        "Usage: alpaca-options-backtest [--profile name] [--profile-file profile.json] --start YYYY-MM-DD --end YYYY-MM-DD [--entry-time HH:MM] [--exit-time HH:MM] [--underlyings SPY,QQQ] [--strategies put_credit,call_credit,iron_condor,call_debit,put_debit,naked_call,naked_put] [--timeframe 1Min] [--option-feed indicative] [--stock-feed iex] [--assumed-iv 0.35] [--synthetic-spread-pct 0.05] [--entry-mark-window-mins 10] [--historical-min-open-interest 0] [--missing-open-interest 0] [--historical-max-leg-spread-pct 0.80] [--profit-target-close-fraction 0.50] [--stop-loss-close-multiple 2.00] [--max-hold-mins 180] [--sweep-synthetic-spread-pct 0.03,0.05,0.08] [--sweep-short-delta-min 0.10,0.15] [--sweep-short-delta-max 0.20,0.25] [--sweep-min-return-on-risk 0.04,0.08,0.13] [--sweep-profit-target-close-fraction 0.35,0.50] [--sweep-stop-loss-close-multiple 1.50,2.00] [--sweep-max-hold-mins 120,240] [--breakdown-period day|week|month|quarter|year] [--unclosed-valuation ignore|conservative|worst_observed] [--min-exit-leg-bars 10] [--require-exit-common-timestamp] [--trade-export-csv trades.csv] [--trade-export-json trades.json] [--quantity 1] [--json]"
    );
}
