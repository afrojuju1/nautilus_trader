//! Runtime config parsing and building for the Alpaca options runtime.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::NaiveTime;
use chrono_tz::Tz;
use nautilus_infrastructure::sql::operational::OPERATIONAL_SCHEMA_DEFAULT;
use nautilus_trading::options::candidates::{
    CreditSpreadKind, DebitSpreadKind, DebitSpreadScannerConfig, IronCondorScannerConfig,
    NakedOptionKind, NakedOptionScannerConfig, PutCreditScannerConfig,
};
use serde::Deserialize;

use crate::earnings::load_earnings_events_csv;
use crate::{fleet::load_fleet_config_from_env, runtime::StrategyState};

use super::AlpacaOptionsRuntimeConfig;

#[derive(Clone, Debug)]
struct StrategyFamilyConfig {
    credit_kinds: Vec<CreditSpreadKind>,
    iron_condor_enabled: bool,
    debit_kinds: Vec<DebitSpreadKind>,
    naked_kinds: Vec<NakedOptionKind>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct RuntimeConfigFile {
    extends: Option<PathBuf>,
    runtime: RuntimeSection,
    universe: UniverseSection,
    scanner: ScannerSection,
    iron_condor: IronCondorSection,
    debit_scanner: DebitScannerSection,
    naked_scanner: NakedScannerSection,
    naked_1_3dte_scanner: NakedScannerSection,
    management: ManagementSection,
    lifecycle: LifecycleSection,
    risk: RiskSection,
    event_shock: EventShockSection,
}

impl RuntimeConfigFile {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            extends: None,
            runtime: self.runtime.merge_parent(parent.runtime),
            universe: self.universe.merge_parent(parent.universe),
            scanner: self.scanner.merge_parent(parent.scanner),
            iron_condor: self.iron_condor.merge_parent(parent.iron_condor),
            debit_scanner: self.debit_scanner.merge_parent(parent.debit_scanner),
            naked_scanner: self.naked_scanner.merge_parent(parent.naked_scanner),
            naked_1_3dte_scanner: self
                .naked_1_3dte_scanner
                .merge_parent(parent.naked_1_3dte_scanner),
            management: self.management.merge_parent(parent.management),
            lifecycle: self.lifecycle.merge_parent(parent.lifecycle),
            risk: self.risk.merge_parent(parent.risk),
            event_shock: self.event_shock.merge_parent(parent.event_shock),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RuntimeSection {
    strategy_families: Vec<String>,
    dry_run_families: Vec<String>,
    max_iterations: Option<u64>,
    interval_secs: Option<u64>,
    open_orders: Option<bool>,
    close_orders: Option<bool>,
    force_flatten: Option<bool>,
    cancel_after_accept: Option<bool>,
    ignore_entry_window: Option<bool>,
    state_path: Option<PathBuf>,
    candidate_ledger_enabled: Option<bool>,
    candidate_ledger_max_candidates: Option<usize>,
}

impl RuntimeSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            strategy_families: merge_vec(self.strategy_families, parent.strategy_families),
            dry_run_families: merge_vec(self.dry_run_families, parent.dry_run_families),
            max_iterations: self.max_iterations.or(parent.max_iterations),
            interval_secs: self.interval_secs.or(parent.interval_secs),
            open_orders: self.open_orders.or(parent.open_orders),
            close_orders: self.close_orders.or(parent.close_orders),
            force_flatten: self.force_flatten.or(parent.force_flatten),
            cancel_after_accept: self.cancel_after_accept.or(parent.cancel_after_accept),
            ignore_entry_window: self.ignore_entry_window.or(parent.ignore_entry_window),
            state_path: self.state_path.or(parent.state_path),
            candidate_ledger_enabled: self
                .candidate_ledger_enabled
                .or(parent.candidate_ledger_enabled),
            candidate_ledger_max_candidates: self
                .candidate_ledger_max_candidates
                .or(parent.candidate_ledger_max_candidates),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UniverseSection {
    underlyings: Vec<String>,
    quantity: Option<u64>,
    entry_start: Option<String>,
    entry_end: Option<String>,
    entry_timezone: Option<String>,
}

impl UniverseSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            underlyings: merge_vec(self.underlyings, parent.underlyings),
            quantity: self.quantity.or(parent.quantity),
            entry_start: self.entry_start.or(parent.entry_start),
            entry_end: self.entry_end.or(parent.entry_end),
            entry_timezone: self.entry_timezone.or(parent.entry_timezone),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ScannerSection {
    min_dte: Option<i64>,
    max_dte: Option<i64>,
    short_delta_min: Option<f64>,
    short_delta_max: Option<f64>,
    widths: Option<Vec<f64>>,
    min_open_interest: Option<u64>,
    max_leg_spread_pct: Option<f64>,
    min_return_on_risk: Option<f64>,
    min_credit_to_width: Option<f64>,
}

impl ScannerSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_dte: self.min_dte.or(parent.min_dte),
            max_dte: self.max_dte.or(parent.max_dte),
            short_delta_min: self.short_delta_min.or(parent.short_delta_min),
            short_delta_max: self.short_delta_max.or(parent.short_delta_max),
            widths: self.widths.or(parent.widths),
            min_open_interest: self.min_open_interest.or(parent.min_open_interest),
            max_leg_spread_pct: self.max_leg_spread_pct.or(parent.max_leg_spread_pct),
            min_return_on_risk: self.min_return_on_risk.or(parent.min_return_on_risk),
            min_credit_to_width: self.min_credit_to_width.or(parent.min_credit_to_width),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IronCondorSection {
    min_return_on_risk: Option<f64>,
    wing_min_return_on_risk: Option<f64>,
    require_equal_widths: Option<bool>,
}

impl IronCondorSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_return_on_risk: self.min_return_on_risk.or(parent.min_return_on_risk),
            wing_min_return_on_risk: self
                .wing_min_return_on_risk
                .or(parent.wing_min_return_on_risk),
            require_equal_widths: self.require_equal_widths.or(parent.require_equal_widths),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DebitScannerSection {
    min_dte: Option<i64>,
    max_dte: Option<i64>,
    long_delta_min: Option<f64>,
    long_delta_max: Option<f64>,
    widths: Option<Vec<f64>>,
    min_open_interest: Option<u64>,
    max_leg_spread_pct: Option<f64>,
    max_debit_to_width: Option<f64>,
    min_debit_to_width: Option<f64>,
    min_reward_to_risk: Option<f64>,
}

impl DebitScannerSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_dte: self.min_dte.or(parent.min_dte),
            max_dte: self.max_dte.or(parent.max_dte),
            long_delta_min: self.long_delta_min.or(parent.long_delta_min),
            long_delta_max: self.long_delta_max.or(parent.long_delta_max),
            widths: self.widths.or(parent.widths),
            min_open_interest: self.min_open_interest.or(parent.min_open_interest),
            max_leg_spread_pct: self.max_leg_spread_pct.or(parent.max_leg_spread_pct),
            max_debit_to_width: self.max_debit_to_width.or(parent.max_debit_to_width),
            min_debit_to_width: self.min_debit_to_width.or(parent.min_debit_to_width),
            min_reward_to_risk: self.min_reward_to_risk.or(parent.min_reward_to_risk),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NakedScannerSection {
    min_dte: Option<i64>,
    max_dte: Option<i64>,
    short_delta_min: Option<f64>,
    short_delta_max: Option<f64>,
    min_open_interest: Option<u64>,
    max_spread_pct: Option<f64>,
    min_credit: Option<f64>,
    min_bid_size: Option<u64>,
    min_ask_size: Option<u64>,
    min_daily_volume: Option<u64>,
    min_implied_volatility: Option<f64>,
    max_implied_volatility: Option<f64>,
    min_annualized_premium_yield: Option<f64>,
    max_buying_power_usage_pct: Option<f64>,
    min_return_on_buying_power: Option<f64>,
    min_breakeven_pop: Option<f64>,
    max_probability_of_touch: Option<f64>,
    min_distance_to_breakeven_pct: Option<f64>,
    min_expected_move_coverage: Option<f64>,
    min_score: Option<f64>,
}

impl NakedScannerSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_dte: self.min_dte.or(parent.min_dte),
            max_dte: self.max_dte.or(parent.max_dte),
            short_delta_min: self.short_delta_min.or(parent.short_delta_min),
            short_delta_max: self.short_delta_max.or(parent.short_delta_max),
            min_open_interest: self.min_open_interest.or(parent.min_open_interest),
            max_spread_pct: self.max_spread_pct.or(parent.max_spread_pct),
            min_credit: self.min_credit.or(parent.min_credit),
            min_bid_size: self.min_bid_size.or(parent.min_bid_size),
            min_ask_size: self.min_ask_size.or(parent.min_ask_size),
            min_daily_volume: self.min_daily_volume.or(parent.min_daily_volume),
            min_implied_volatility: self
                .min_implied_volatility
                .or(parent.min_implied_volatility),
            max_implied_volatility: self
                .max_implied_volatility
                .or(parent.max_implied_volatility),
            min_annualized_premium_yield: self
                .min_annualized_premium_yield
                .or(parent.min_annualized_premium_yield),
            max_buying_power_usage_pct: self
                .max_buying_power_usage_pct
                .or(parent.max_buying_power_usage_pct),
            min_return_on_buying_power: self
                .min_return_on_buying_power
                .or(parent.min_return_on_buying_power),
            min_breakeven_pop: self.min_breakeven_pop.or(parent.min_breakeven_pop),
            max_probability_of_touch: self
                .max_probability_of_touch
                .or(parent.max_probability_of_touch),
            min_distance_to_breakeven_pct: self
                .min_distance_to_breakeven_pct
                .or(parent.min_distance_to_breakeven_pct),
            min_expected_move_coverage: self
                .min_expected_move_coverage
                .or(parent.min_expected_move_coverage),
            min_score: self.min_score.or(parent.min_score),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ManagementSection {
    stale_entry_secs: Option<u64>,
    stale_close_secs: Option<u64>,
    close_regular_hours_only: Option<bool>,
    close_start: Option<String>,
    close_end: Option<String>,
    close_price_cushion: Option<f64>,
    close_reprice_step: Option<f64>,
    max_close_price_cushion: Option<f64>,
    max_close_attempts: Option<u32>,
    close_reprice_cooldown_secs: Option<u64>,
    active_risk_candidate_quote_limit: Option<usize>,
    active_risk_quote_stale_secs: Option<u64>,
    profit_target_close_fraction: Option<f64>,
    stop_loss_close_multiple: Option<f64>,
    max_hold_secs: Option<u64>,
    expiration_exit_days: Option<i64>,
}

impl ManagementSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            stale_entry_secs: self.stale_entry_secs.or(parent.stale_entry_secs),
            stale_close_secs: self.stale_close_secs.or(parent.stale_close_secs),
            close_regular_hours_only: self
                .close_regular_hours_only
                .or(parent.close_regular_hours_only),
            close_start: self.close_start.or(parent.close_start),
            close_end: self.close_end.or(parent.close_end),
            close_price_cushion: self.close_price_cushion.or(parent.close_price_cushion),
            close_reprice_step: self.close_reprice_step.or(parent.close_reprice_step),
            max_close_price_cushion: self
                .max_close_price_cushion
                .or(parent.max_close_price_cushion),
            max_close_attempts: self.max_close_attempts.or(parent.max_close_attempts),
            close_reprice_cooldown_secs: self
                .close_reprice_cooldown_secs
                .or(parent.close_reprice_cooldown_secs),
            active_risk_candidate_quote_limit: self
                .active_risk_candidate_quote_limit
                .or(parent.active_risk_candidate_quote_limit),
            active_risk_quote_stale_secs: self
                .active_risk_quote_stale_secs
                .or(parent.active_risk_quote_stale_secs),
            profit_target_close_fraction: self
                .profit_target_close_fraction
                .or(parent.profit_target_close_fraction),
            stop_loss_close_multiple: self
                .stop_loss_close_multiple
                .or(parent.stop_loss_close_multiple),
            max_hold_secs: self.max_hold_secs.or(parent.max_hold_secs),
            expiration_exit_days: self.expiration_exit_days.or(parent.expiration_exit_days),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LifecycleSection {
    poll_secs: Option<u64>,
    activity_lookback_hours: Option<u64>,
    activity_block_hours: Option<u64>,
    expiration_entry_block_days: Option<i64>,
}

impl LifecycleSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            poll_secs: self.poll_secs.or(parent.poll_secs),
            activity_lookback_hours: self
                .activity_lookback_hours
                .or(parent.activity_lookback_hours),
            activity_block_hours: self.activity_block_hours.or(parent.activity_block_hours),
            expiration_entry_block_days: self
                .expiration_entry_block_days
                .or(parent.expiration_entry_block_days),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RiskSection {
    max_active_entries: Option<usize>,
    max_daily_submits: Option<usize>,
    max_open_orders: Option<usize>,
    max_active_entries_per_underlying: Option<usize>,
    max_active_entries_per_sector: Option<usize>,
    max_single_entry_risk_capital_usd: Option<f64>,
    max_portfolio_risk_capital_usd: Option<f64>,
    block_unestimated_risk_capital: Option<bool>,
    sectors: BTreeMap<String, String>,
}

impl RiskSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            max_active_entries: self.max_active_entries.or(parent.max_active_entries),
            max_daily_submits: self.max_daily_submits.or(parent.max_daily_submits),
            max_open_orders: self.max_open_orders.or(parent.max_open_orders),
            max_active_entries_per_underlying: self
                .max_active_entries_per_underlying
                .or(parent.max_active_entries_per_underlying),
            max_active_entries_per_sector: self
                .max_active_entries_per_sector
                .or(parent.max_active_entries_per_sector),
            max_single_entry_risk_capital_usd: self
                .max_single_entry_risk_capital_usd
                .or(parent.max_single_entry_risk_capital_usd),
            max_portfolio_risk_capital_usd: self
                .max_portfolio_risk_capital_usd
                .or(parent.max_portfolio_risk_capital_usd),
            block_unestimated_risk_capital: self
                .block_unestimated_risk_capital
                .or(parent.block_unestimated_risk_capital),
            sectors: merge_map(self.sectors, parent.sectors),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EventShockSection {
    earnings_events_path: Option<PathBuf>,
    require_earnings_events: Option<bool>,
    block_days_before_earnings: Option<i64>,
    block_days_after_earnings: Option<i64>,
}

impl EventShockSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            earnings_events_path: self.earnings_events_path.or(parent.earnings_events_path),
            require_earnings_events: self
                .require_earnings_events
                .or(parent.require_earnings_events),
            block_days_before_earnings: self
                .block_days_before_earnings
                .or(parent.block_days_before_earnings),
            block_days_after_earnings: self
                .block_days_after_earnings
                .or(parent.block_days_after_earnings),
        }
    }
}

pub(super) fn load_runtime_config_file_from_env() -> anyhow::Result<RuntimeConfigFile> {
    if let Some(path) = env::var_os("ALPACA_CONFIG_PATH") {
        let path = PathBuf::from(path);
        return load_runtime_config_file(&path, true);
    }
    if let Some(path) = current_fleet_account_config_path()? {
        return load_runtime_config_file(&path, true);
    }

    let path = default_config_path();
    if path.exists() {
        load_runtime_config_file(&path, false)
    } else {
        Ok(RuntimeConfigFile::default())
    }
}

fn current_fleet_account_config_path() -> anyhow::Result<Option<PathBuf>> {
    let Some(fleet) = load_fleet_config_from_env()? else {
        return Ok(None);
    };
    Ok(fleet
        .current_account()
        .and_then(|account| fleet.config_file(account)))
}

fn load_runtime_config_file(path: &Path, explicit: bool) -> anyhow::Result<RuntimeConfigFile> {
    match path.try_exists() {
        Ok(false) if !explicit => return Ok(RuntimeConfigFile::default()),
        Ok(_) => {}
        Err(error) => {
            anyhow::bail!(
                "failed to inspect Alpaca runtime config {}: {error}",
                path.display()
            );
        }
    }
    load_runtime_config_file_with_extends(path, &mut Vec::new())
}

fn load_runtime_config_file_with_extends(
    path: &Path,
    stack: &mut Vec<PathBuf>,
) -> anyhow::Result<RuntimeConfigFile> {
    let identity = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&identity) {
        let chain = stack
            .iter()
            .chain(std::iter::once(&identity))
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" -> ");
        anyhow::bail!("circular Alpaca runtime config inheritance: {chain}");
    }

    let raw = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "failed to read Alpaca runtime config {}: {error}",
            path.display()
        )
    })?;
    stack.push(identity);
    let result = (|| {
        let config = parse_runtime_config_at_path(&raw, path)?;
        if let Some(parent_path) = config.extends.as_deref() {
            let parent_path = resolve_config_extends_path(path, parent_path);
            let parent = load_runtime_config_file_with_extends(&parent_path, stack)?;
            Ok(config.merge_parent(parent))
        } else {
            Ok(config)
        }
    })();
    stack.pop();
    result
}

#[cfg(test)]
fn parse_runtime_config(raw: &str) -> anyhow::Result<RuntimeConfigFile> {
    toml::from_str(raw).map_err(|error| anyhow::anyhow!("invalid Alpaca runtime config: {error}"))
}

fn parse_runtime_config_at_path(raw: &str, path: &Path) -> anyhow::Result<RuntimeConfigFile> {
    toml::from_str(raw).map_err(|error| {
        anyhow::anyhow!("invalid Alpaca runtime config {}: {error}", path.display())
    })
}

pub(super) fn build_options_runtime_config(
    file: RuntimeConfigFile,
    cli_underlyings: Vec<String>,
) -> anyhow::Result<AlpacaOptionsRuntimeConfig> {
    reject_retired_order_capability_env_vars()?;
    let strategy_values = env::var("ALPACA_STRATEGY_FAMILIES")
        .ok()
        .map(|value| split_strings([value]))
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| {
            if file.runtime.strategy_families.is_empty() {
                vec!["put".to_string()]
            } else {
                file.runtime.strategy_families.clone()
            }
        });
    let strategy_config = strategy_family_config_from_values(strategy_values)?;
    let dry_run_strategy_values = env::var("ALPACA_DRY_RUN_FAMILIES")
        .ok()
        .map(|value| split_strings([value]))
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| file.runtime.dry_run_families.clone());
    let dry_run_strategy_config =
        dry_run_strategy_family_config_from_values(dry_run_strategy_values)?;
    let scanner = scanner_config_from_file(&file.scanner);
    let stale_entry_secs = file.management.stale_entry_secs.unwrap_or(900);
    let interval_secs = env_parse("ALPACA_INTERVAL_SECS")
        .or(file.runtime.interval_secs)
        .unwrap_or(300);
    let event_shock_earnings_events = load_event_shock_earnings_events(&file.event_shock)?;
    let close_price_cushion = env_parse("ALPACA_CLOSE_PRICE_CUSHION")
        .or(file.management.close_price_cushion)
        .unwrap_or(0.0)
        .max(0.0);
    let fleet = load_fleet_config_from_env()?;
    let mut config = AlpacaOptionsRuntimeConfig {
        underlyings: underlyings_from_sources(cli_underlyings, &file.universe),
        spread_kinds: strategy_config.credit_kinds,
        iron_condor_enabled: strategy_config.iron_condor_enabled,
        debit_kinds: strategy_config.debit_kinds,
        naked_kinds: strategy_config.naked_kinds,
        dry_run_spread_kinds: dry_run_strategy_config.credit_kinds,
        iron_condor_dry_run: dry_run_strategy_config.iron_condor_enabled,
        dry_run_debit_kinds: dry_run_strategy_config.debit_kinds,
        dry_run_naked_kinds: dry_run_strategy_config.naked_kinds,
        max_active_entries: env_parse("ALPACA_MAX_ACTIVE_ENTRIES").or(file.risk.max_active_entries),
        max_daily_submits: env_parse("ALPACA_MAX_DAILY_SUBMITS").or(file.risk.max_daily_submits),
        max_open_orders: env_parse("ALPACA_MAX_OPEN_ORDERS").or(file.risk.max_open_orders),
        max_active_entries_per_underlying: env_parse("ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING")
            .or(file.risk.max_active_entries_per_underlying),
        max_active_entries_per_sector: env_parse("ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR")
            .or(file.risk.max_active_entries_per_sector),
        max_single_entry_risk_capital_usd: env_parse("ALPACA_MAX_SINGLE_ENTRY_RISK_CAPITAL_USD")
            .or(file.risk.max_single_entry_risk_capital_usd)
            .filter(|value| value.is_finite() && *value > 0.0),
        max_portfolio_risk_capital_usd: env_parse("ALPACA_MAX_PORTFOLIO_RISK_CAPITAL_USD")
            .or(file.risk.max_portfolio_risk_capital_usd)
            .filter(|value| value.is_finite() && *value > 0.0),
        block_unestimated_risk_capital: env_bool("ALPACA_BLOCK_UNESTIMATED_RISK_CAPITAL")
            .or(file.risk.block_unestimated_risk_capital)
            .unwrap_or(true),
        event_shock_earnings_events,
        event_shock_block_days_before_earnings: env_parse(
            "ALPACA_EVENT_SHOCK_BLOCK_DAYS_BEFORE_EARNINGS",
        )
        .or(file.event_shock.block_days_before_earnings)
        .unwrap_or(1)
        .max(0),
        event_shock_block_days_after_earnings: env_parse(
            "ALPACA_EVENT_SHOCK_BLOCK_DAYS_AFTER_EARNINGS",
        )
        .or(file.event_shock.block_days_after_earnings)
        .unwrap_or(1)
        .max(0),
        sectors: sector_map_from_file(file.risk.sectors),
        max_iterations: env_parse("ALPACA_MAX_ITERATIONS")
            .or(file.runtime.max_iterations)
            .unwrap_or(1),
        interval_secs,
        quantity: env_parse("ALPACA_QTY")
            .or(file.universe.quantity)
            .unwrap_or(1),
        open_orders_enabled: env_bool("ALPACA_OPEN_ORDERS")
            .or(file.runtime.open_orders)
            .unwrap_or(false),
        force_flatten: env_bool("ALPACA_FORCE_FLATTEN")
            .or(file.runtime.force_flatten)
            .unwrap_or(false),
        cancel_after_accept: env_bool("ALPACA_CANCEL_AFTER_ACCEPT")
            .or(file.runtime.cancel_after_accept)
            .unwrap_or(false),
        stale_entry_secs,
        stale_close_secs: file.management.stale_close_secs.unwrap_or(stale_entry_secs),
        close_orders_enabled: env_bool("ALPACA_CLOSE_ORDERS")
            .or(file.runtime.close_orders)
            .unwrap_or(false),
        close_regular_hours_only: env_bool("ALPACA_CLOSE_REGULAR_HOURS_ONLY")
            .or(file.management.close_regular_hours_only)
            .unwrap_or(true),
        close_start: parse_time_value(file.management.close_start.as_deref(), "09:30")?,
        close_end: parse_time_value(file.management.close_end.as_deref(), "16:00")?,
        close_price_cushion,
        close_reprice_step: env_parse("ALPACA_CLOSE_REPRICE_STEP")
            .or(file.management.close_reprice_step)
            .unwrap_or(0.0)
            .max(0.0),
        max_close_price_cushion: env_parse("ALPACA_MAX_CLOSE_PRICE_CUSHION")
            .or(file.management.max_close_price_cushion)
            .unwrap_or(close_price_cushion)
            .max(close_price_cushion),
        max_close_attempts: env_parse("ALPACA_MAX_CLOSE_ATTEMPTS")
            .or(file.management.max_close_attempts)
            .unwrap_or(3),
        close_reprice_cooldown_secs: env_parse("ALPACA_CLOSE_REPRICE_COOLDOWN_SECS")
            .or(file.management.close_reprice_cooldown_secs)
            .unwrap_or(30),
        active_risk_candidate_quote_limit: env_parse("ALPACA_ACTIVE_RISK_CANDIDATE_QUOTE_LIMIT")
            .or(file.management.active_risk_candidate_quote_limit)
            .unwrap_or(5),
        active_risk_quote_stale_secs: env_parse("ALPACA_ACTIVE_RISK_QUOTE_STALE_SECS")
            .or(file.management.active_risk_quote_stale_secs)
            .unwrap_or(30),
        profit_target_close_fraction: file.management.profit_target_close_fraction.unwrap_or(0.50),
        stop_loss_close_multiple: file.management.stop_loss_close_multiple.unwrap_or(2.0),
        max_hold_secs: file.management.max_hold_secs.unwrap_or(0),
        expiration_exit_days: file.management.expiration_exit_days.unwrap_or(1),
        lifecycle_poll_secs: env_parse("ALPACA_LIFECYCLE_POLL_SECS")
            .or(file.lifecycle.poll_secs)
            .unwrap_or(interval_secs.max(60)),
        lifecycle_activity_lookback_hours: env_parse("ALPACA_LIFECYCLE_ACTIVITY_LOOKBACK_HOURS")
            .or(file.lifecycle.activity_lookback_hours)
            .unwrap_or(72),
        lifecycle_activity_block_hours: env_parse("ALPACA_LIFECYCLE_ACTIVITY_BLOCK_HOURS")
            .or(file.lifecycle.activity_block_hours)
            .unwrap_or(24),
        expiration_entry_block_days: env_parse("ALPACA_EXPIRATION_ENTRY_BLOCK_DAYS")
            .or(file.lifecycle.expiration_entry_block_days)
            .unwrap_or(0),
        ignore_entry_window: env_bool("ALPACA_IGNORE_ENTRY_WINDOW")
            .or(file.runtime.ignore_entry_window)
            .unwrap_or(false),
        entry_start: parse_time_value(file.universe.entry_start.as_deref(), "09:45")?,
        entry_end: parse_time_value(file.universe.entry_end.as_deref(), "14:30")?,
        entry_timezone: file
            .universe
            .entry_timezone
            .as_deref()
            .unwrap_or("America/New_York")
            .parse::<Tz>()?,
        state_path: env::var("ALPACA_STATE_PATH")
            .map(PathBuf::from)
            .ok()
            .or(file.runtime.state_path)
            .unwrap_or_else(default_state_path),
        candidate_ledger_enabled: file.runtime.candidate_ledger_enabled.unwrap_or(true),
        candidate_ledger_max_candidates: file.runtime.candidate_ledger_max_candidates.unwrap_or(10),
        iron_condor_scanner: iron_condor_scanner_config_from_file(&scanner, &file.iron_condor),
        debit_scanner: debit_scanner_config_from_file(&file.debit_scanner),
        naked_scanner: naked_scanner_config_from_file(&file.naked_scanner),
        naked_1_3dte_scanner: naked_1_3dte_scanner_config_from_file(&file.naked_1_3dte_scanner),
        scanner,
        fleet,
        fleet_account_id: None,
        fleet_policy_blocks: Vec::new(),
        operational_database_url: None,
        operational_repository: None,
        operational_schema: OPERATIONAL_SCHEMA_DEFAULT.to_string(),
        operational_account_id: None,
    };
    apply_fleet_policy(&mut config);
    Ok(config)
}

fn strategy_family_config_from_values(values: Vec<String>) -> anyhow::Result<StrategyFamilyConfig> {
    let mut kinds = Vec::new();
    let mut iron_condor_enabled = false;
    let mut debit_kinds = Vec::new();
    let mut naked_kinds = Vec::new();
    for raw in values
        .into_iter()
        .flat_map(|value| split_strings([value]))
        .map(|value| value.to_ascii_lowercase())
    {
        match raw.as_str() {
            "put" | "put_credit" => {
                kinds.push(CreditSpreadKind::Put);
            }
            "call" | "call_credit" => {
                kinds.push(CreditSpreadKind::Call);
            }
            "both" => {
                kinds.push(CreditSpreadKind::Put);
                kinds.push(CreditSpreadKind::Call);
            }
            "all" => {
                kinds.push(CreditSpreadKind::Put);
                kinds.push(CreditSpreadKind::Call);
                iron_condor_enabled = true;
            }
            "iron_condor" | "condor" => {
                iron_condor_enabled = true;
            }
            "call_debit" | "earnings_call_debit_entry" => {
                debit_kinds.push(DebitSpreadKind::Call);
            }
            "put_debit" | "earnings_put_debit_entry" => {
                debit_kinds.push(DebitSpreadKind::Put);
            }
            "debit" | "long_premium" | "directional" => {
                debit_kinds.push(DebitSpreadKind::Call);
                debit_kinds.push(DebitSpreadKind::Put);
            }
            "naked_call" | "short_call" => {
                naked_kinds.push(NakedOptionKind::Call);
            }
            "naked_put" | "short_put" => {
                naked_kinds.push(NakedOptionKind::Put);
            }
            "naked_call_1_3dte" | "short_call_1_3dte" => {
                naked_kinds.push(NakedOptionKind::CallOneToThreeDte);
            }
            "naked_put_1_3dte" | "short_put_1_3dte" => {
                naked_kinds.push(NakedOptionKind::PutOneToThreeDte);
            }
            "naked_1_3dte" | "undefined_risk_1_3dte" | "short_premium_1_3dte" => {
                naked_kinds.push(NakedOptionKind::CallOneToThreeDte);
                naked_kinds.push(NakedOptionKind::PutOneToThreeDte);
            }
            "naked" | "undefined_risk" | "short_premium_undefined" => {
                naked_kinds.push(NakedOptionKind::Call);
                naked_kinds.push(NakedOptionKind::Put);
            }
            other => anyhow::bail!("unsupported Alpaca strategy family value {other}"),
        }
    }
    if kinds.is_empty() && !iron_condor_enabled && debit_kinds.is_empty() && naked_kinds.is_empty()
    {
        kinds.push(CreditSpreadKind::Put);
    }
    kinds.sort_by_key(|kind| match kind {
        CreditSpreadKind::Put => 0,
        CreditSpreadKind::Call => 1,
    });
    kinds.dedup();
    debit_kinds.sort_by_key(|kind| match kind {
        DebitSpreadKind::Call => 0,
        DebitSpreadKind::Put => 1,
    });
    debit_kinds.dedup();
    naked_kinds.sort_by_key(|kind| match kind {
        NakedOptionKind::Call => 0,
        NakedOptionKind::Put => 1,
        NakedOptionKind::CallOneToThreeDte => 2,
        NakedOptionKind::PutOneToThreeDte => 3,
    });
    naked_kinds.dedup();
    Ok(StrategyFamilyConfig {
        credit_kinds: kinds,
        iron_condor_enabled,
        debit_kinds,
        naked_kinds,
    })
}

fn dry_run_strategy_family_config_from_values(
    values: Vec<String>,
) -> anyhow::Result<StrategyFamilyConfig> {
    if values.is_empty() {
        return Ok(StrategyFamilyConfig {
            credit_kinds: Vec::new(),
            iron_condor_enabled: false,
            debit_kinds: Vec::new(),
            naked_kinds: Vec::new(),
        });
    }
    strategy_family_config_from_values(values)
}

fn apply_fleet_policy(config: &mut AlpacaOptionsRuntimeConfig) {
    let Some(fleet) = &config.fleet else {
        return;
    };
    if let Some(account) = fleet.current_account() {
        config.fleet_account_id = Some(account.id.clone());
        if !account.enabled {
            config
                .fleet_policy_blocks
                .push(format!("fleet_account_disabled:{}", account.id));
        }
        if has_defined_risk_strategies(config) && !account.permissions.defined_risk {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_defined_risk_required:{}",
                account.id
            ));
        }
        if !config.debit_kinds.is_empty() && !account.permissions.long_premium {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_long_premium_required:{}",
                account.id
            ));
        }
        if has_undefined_risk_strategies(config) && !account.permissions.undefined_risk {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_undefined_risk_required:{}",
                account.id
            ));
        }
        if config.naked_kinds.iter().any(|kind| kind.is_call()) && !account.permissions.naked_calls
        {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_naked_calls_required:{}",
                account.id
            ));
        }
        if config.naked_kinds.iter().any(|kind| kind.is_put()) && !account.permissions.naked_puts {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_naked_puts_required:{}",
                account.id
            ));
        }
        if let Some(limit) = account.risk_budget.max_active_entries {
            config.max_active_entries = Some(min_limit(config.max_active_entries, limit));
        }
        if let Some(limit) = account.risk_budget.max_buying_power_pct {
            config.naked_scanner.max_buying_power_usage_pct =
                config.naked_scanner.max_buying_power_usage_pct.min(limit);
            config.naked_1_3dte_scanner.max_buying_power_usage_pct = config
                .naked_1_3dte_scanner
                .max_buying_power_usage_pct
                .min(limit);
        }
    } else {
        config
            .fleet_policy_blocks
            .push("fleet_account_unmatched".to_string());
    }

    if fleet.config.fleet.kill_switch {
        config
            .fleet_policy_blocks
            .push("fleet_kill_switch_enabled".to_string());
    }
    if !config.fleet_policy_blocks.is_empty() {
        config.open_orders_enabled = false;
    }
}

fn has_defined_risk_strategies(config: &AlpacaOptionsRuntimeConfig) -> bool {
    !config.spread_kinds.is_empty() || config.iron_condor_enabled
}

fn has_undefined_risk_strategies(config: &AlpacaOptionsRuntimeConfig) -> bool {
    !config.naked_kinds.is_empty()
}

fn min_limit(current: Option<usize>, fleet_limit: usize) -> usize {
    current.map_or(fleet_limit, |current| current.min(fleet_limit))
}

fn underlyings_from_sources(cli_underlyings: Vec<String>, config: &UniverseSection) -> Vec<String> {
    let args = split_strings(cli_underlyings);
    if !args.is_empty() {
        return args;
    }
    let configured = split_strings(config.underlyings.clone());
    if !configured.is_empty() {
        return configured;
    }
    default_underlyings()
}

fn split_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn merge_vec<T>(child: Vec<T>, parent: Vec<T>) -> Vec<T> {
    if child.is_empty() { parent } else { child }
}

fn merge_map<K: Ord, V>(child: BTreeMap<K, V>, parent: BTreeMap<K, V>) -> BTreeMap<K, V> {
    let mut merged = parent;
    merged.extend(child);
    merged
}

fn resolve_config_extends_path(config_path: &Path, parent_path: &Path) -> PathBuf {
    if parent_path.is_absolute() {
        parent_path.to_path_buf()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(parent_path)
    }
}

pub(super) fn no_candidate_reason(
    contract_count: usize,
    snapshot_count: usize,
    scoreable_count: usize,
) -> &'static str {
    if contract_count == 0 {
        "no_contracts"
    } else if snapshot_count == 0 {
        "no_snapshots"
    } else if scoreable_count == 0 {
        "no_scoreable_spreads"
    } else {
        "no_ranked_candidate"
    }
}

pub(super) fn account_options_buying_power(
    account: &crate::http::models::AlpacaAccount,
) -> Option<f64> {
    parse_account_amount(
        account
            .options_buying_power
            .as_deref()
            .or(account.buying_power.as_deref())
            .or(account.cash.as_deref()),
    )
}

fn parse_account_amount(value: Option<&str>) -> Option<f64> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.0)
}

pub(super) fn format_optional_pct(value: Option<f64>) -> String {
    value.map_or_else(
        || "n/a".to_string(),
        |value| format!("{:.2}%", value * 100.0),
    )
}

pub(super) fn format_rejection_counts(rejections: &BTreeMap<String, usize>) -> String {
    if rejections.is_empty() {
        return "none".to_string();
    }
    rejections
        .iter()
        .map(|(reason, count)| format!("{reason}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn active_underlying_count(state: &StrategyState, underlying: &str) -> usize {
    state
        .entries
        .iter()
        .filter(|entry| entry.is_active() && entry.underlying.eq_ignore_ascii_case(underlying))
        .count()
}

pub(crate) fn active_sector_count(
    state: &StrategyState,
    sectors: &BTreeMap<String, String>,
    sector: &str,
) -> usize {
    state
        .entries
        .iter()
        .filter(|entry| {
            entry.is_active()
                && sectors
                    .get(&entry.underlying.to_ascii_uppercase())
                    .is_some_and(|entry_sector| entry_sector == sector)
        })
        .count()
}

fn default_underlyings() -> Vec<String> {
    [
        "SPY", "QQQ", "IWM", "DIA", "GLD", "GDX", "SLV", "TLT", "XLE", "XLF", "XLK", "XLV", "XLY",
        "XLI", "XLP", "XLU", "XLB", "XLC", "SMH", "USO", "XOP", "XOM",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn sector_map_from_file(config: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let map = if config.is_empty() {
        default_sector_map()
    } else {
        config
    };
    map.into_iter()
        .map(|(underlying, sector)| (underlying.to_ascii_uppercase(), sector))
        .collect()
}

fn default_sector_map() -> BTreeMap<String, String> {
    [
        ("SPY", "broad_index"),
        ("QQQ", "broad_index"),
        ("IWM", "broad_index"),
        ("DIA", "broad_index"),
        ("GLD", "metals"),
        ("GDX", "metals"),
        ("SLV", "metals"),
        ("TLT", "rates"),
        ("XLE", "energy"),
        ("USO", "energy"),
        ("XOP", "energy"),
        ("XOM", "energy"),
        ("XLB", "materials"),
        ("XLC", "communication_services"),
        ("XLF", "financials"),
        ("XLK", "technology"),
        ("XLV", "healthcare"),
        ("XLY", "consumer_discretionary"),
        ("XLI", "industrials"),
        ("XLP", "consumer_staples"),
        ("XLU", "utilities"),
        ("SMH", "semiconductors"),
    ]
    .into_iter()
    .map(|(underlying, sector)| (underlying.to_string(), sector.to_string()))
    .collect()
}

fn scanner_config_from_file(config: &ScannerSection) -> PutCreditScannerConfig {
    PutCreditScannerConfig {
        min_dte: config.min_dte.unwrap_or(5),
        max_dte: config.max_dte.unwrap_or(10),
        short_delta_min: config.short_delta_min.unwrap_or(0.18),
        short_delta_max: config.short_delta_max.unwrap_or(0.28),
        widths: config.widths.clone().unwrap_or_else(|| vec![2.0, 3.0, 5.0]),
        min_open_interest: config.min_open_interest.unwrap_or(200),
        max_leg_spread_pct: config.max_leg_spread_pct.unwrap_or(0.15),
        min_return_on_risk: config.min_return_on_risk.unwrap_or(0.13),
        min_credit_to_width: config.min_credit_to_width.unwrap_or(0.08),
    }
}

fn iron_condor_scanner_config_from_file(
    scanner: &PutCreditScannerConfig,
    config: &IronCondorSection,
) -> IronCondorScannerConfig {
    let mut credit = scanner.clone();
    credit.min_return_on_risk = config.wing_min_return_on_risk.unwrap_or(0.10);
    IronCondorScannerConfig {
        credit,
        min_return_on_risk: config.min_return_on_risk.unwrap_or(0.18),
        require_equal_widths: config.require_equal_widths.unwrap_or(true),
    }
}

fn debit_scanner_config_from_file(config: &DebitScannerSection) -> DebitSpreadScannerConfig {
    DebitSpreadScannerConfig {
        min_dte: config.min_dte.unwrap_or(5),
        max_dte: config.max_dte.unwrap_or(45),
        long_delta_min: config.long_delta_min.unwrap_or(0.45),
        long_delta_max: config.long_delta_max.unwrap_or(0.65),
        widths: config.widths.clone().unwrap_or_else(|| vec![2.0, 3.0, 5.0]),
        min_open_interest: config.min_open_interest.unwrap_or(200),
        max_leg_spread_pct: config.max_leg_spread_pct.unwrap_or(0.15),
        max_debit_to_width: config.max_debit_to_width.unwrap_or(0.55),
        min_debit_to_width: config.min_debit_to_width.unwrap_or(0.20),
        min_reward_to_risk: config.min_reward_to_risk.unwrap_or(0.75),
    }
}

fn naked_scanner_config_from_file(config: &NakedScannerSection) -> NakedOptionScannerConfig {
    NakedOptionScannerConfig {
        min_dte: config.min_dte.unwrap_or(5),
        max_dte: config.max_dte.unwrap_or(14),
        short_delta_min: config.short_delta_min.unwrap_or(0.10),
        short_delta_max: config.short_delta_max.unwrap_or(0.20),
        min_open_interest: config.min_open_interest.unwrap_or(500),
        max_spread_pct: config.max_spread_pct.unwrap_or(0.12),
        min_credit: config.min_credit.unwrap_or(0.25),
        min_bid_size: config.min_bid_size.unwrap_or(1),
        min_ask_size: config.min_ask_size.unwrap_or(1),
        min_daily_volume: config.min_daily_volume.unwrap_or(1),
        min_implied_volatility: config.min_implied_volatility.unwrap_or(0.0),
        max_implied_volatility: config.max_implied_volatility.unwrap_or(1.50),
        min_annualized_premium_yield: config.min_annualized_premium_yield.unwrap_or(0.10),
        max_buying_power_usage_pct: config.max_buying_power_usage_pct.unwrap_or(0.10),
        min_return_on_buying_power: config.min_return_on_buying_power.unwrap_or(0.0005),
        min_breakeven_pop: config.min_breakeven_pop.unwrap_or(0.65),
        max_probability_of_touch: config.max_probability_of_touch.unwrap_or(0.70),
        min_distance_to_breakeven_pct: config.min_distance_to_breakeven_pct.unwrap_or(0.005),
        min_expected_move_coverage: config.min_expected_move_coverage.unwrap_or(0.75),
        min_score: config.min_score.unwrap_or(55.0),
    }
}

fn naked_1_3dte_scanner_config_from_file(config: &NakedScannerSection) -> NakedOptionScannerConfig {
    NakedOptionScannerConfig {
        min_dte: config.min_dte.unwrap_or(1),
        max_dte: config.max_dte.unwrap_or(3),
        short_delta_min: config.short_delta_min.unwrap_or(0.06),
        short_delta_max: config.short_delta_max.unwrap_or(0.14),
        min_open_interest: config.min_open_interest.unwrap_or(300),
        max_spread_pct: config.max_spread_pct.unwrap_or(0.08),
        min_credit: config.min_credit.unwrap_or(0.12),
        min_bid_size: config.min_bid_size.unwrap_or(1),
        min_ask_size: config.min_ask_size.unwrap_or(1),
        min_daily_volume: config.min_daily_volume.unwrap_or(100),
        min_implied_volatility: config.min_implied_volatility.unwrap_or(0.12),
        max_implied_volatility: config.max_implied_volatility.unwrap_or(1.00),
        min_annualized_premium_yield: config.min_annualized_premium_yield.unwrap_or(0.12),
        max_buying_power_usage_pct: config.max_buying_power_usage_pct.unwrap_or(0.10),
        min_return_on_buying_power: config.min_return_on_buying_power.unwrap_or(0.0004),
        min_breakeven_pop: config.min_breakeven_pop.unwrap_or(0.72),
        max_probability_of_touch: config.max_probability_of_touch.unwrap_or(0.40),
        min_distance_to_breakeven_pct: config.min_distance_to_breakeven_pct.unwrap_or(0.004),
        min_expected_move_coverage: config.min_expected_move_coverage.unwrap_or(1.10),
        min_score: config.min_score.unwrap_or(72.0),
    }
}

fn parse_time_value(value: Option<&str>, default: &str) -> anyhow::Result<NaiveTime> {
    Ok(NaiveTime::parse_from_str(
        value.unwrap_or(default),
        "%H:%M",
    )?)
}

fn load_event_shock_earnings_events(
    config: &EventShockSection,
) -> anyhow::Result<Vec<crate::earnings::EarningsEvent>> {
    let path = env::var_os("ALPACA_EVENT_SHOCK_EARNINGS_EVENTS_PATH")
        .map(PathBuf::from)
        .or_else(|| config.earnings_events_path.clone());
    let required = env_bool("ALPACA_EVENT_SHOCK_REQUIRE_EARNINGS_EVENTS")
        .or(config.require_earnings_events)
        .unwrap_or(false);

    let Some(path) = path else {
        anyhow::ensure!(
            !required,
            "event_shock.require_earnings_events is true but no earnings_events_path is configured"
        );
        return Ok(Vec::new());
    };

    load_earnings_events_csv(&path).map_err(|error| {
        anyhow::anyhow!(
            "failed to load event-shock earnings events {}: {error}",
            path.display()
        )
    })
}

fn default_config_path() -> PathBuf {
    if let Some(value) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(value)
            .join("nautilus-trader")
            .join("alpaca")
            .join("options.toml");
    }
    if let Some(value) = env::var_os("HOME") {
        return PathBuf::from(value)
            .join(".config")
            .join("nautilus-trader")
            .join("alpaca")
            .join("options.toml");
    }
    PathBuf::from("options.toml")
}

fn default_state_path() -> PathBuf {
    if let Some(value) = env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(value)
            .join("nautilus_trader")
            .join("alpaca_options_state.json");
    }
    if let Some(value) = env::var_os("HOME") {
        return PathBuf::from(value)
            .join(".local")
            .join("state")
            .join("nautilus_trader")
            .join("alpaca_options_state.json");
    }
    PathBuf::from("alpaca_options_state.json")
}

fn env_bool(name: &str) -> Option<bool> {
    env::var(name).ok().and_then(|value| {
        let value = value.trim();
        if matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON") {
            Some(true)
        } else if matches!(value, "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF") {
            Some(false)
        } else {
            None
        }
    })
}

fn reject_retired_order_capability_env_vars() -> anyhow::Result<()> {
    let present = [
        "ALPACA_SUBMIT",
        "ALPACA_MANAGE",
        "ALPACA_CLOSE",
        "ALPACA_KILL_SWITCH",
    ]
    .into_iter()
    .filter(|name| env::var_os(name).is_some())
    .collect::<Vec<_>>();
    if present.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "retired Alpaca order capability env vars are set: {}; use ALPACA_OPEN_ORDERS and ALPACA_CLOSE_ORDERS",
        present.join(", ")
    );
}

fn env_parse<T>(name: &str) -> Option<T>
where
    T: FromStr,
{
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<T>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_strings_splits_args_and_csv() {
        assert_eq!(
            split_strings(["SPY, QQQ".to_string(), "IWM".to_string()]),
            vec!["SPY", "QQQ", "IWM"],
        );
    }

    #[test]
    fn runtime_config_inherits_parent_sections() {
        let parent = parse_runtime_config(
            r#"
[runtime]
max_iterations = 0
candidate_ledger_enabled = true
candidate_ledger_max_candidates = 5

[universe]
underlyings = ["SPY", "GLD"]
quantity = 1

[naked_scanner]
max_buying_power_usage_pct = 0.10
min_score = 70.0

[risk]
max_active_entries_per_underlying = 1
max_portfolio_risk_capital_usd = 1000.0
block_unestimated_risk_capital = true

[event_shock]
earnings_events_path = "/tmp/earnings_events_approved.csv"
block_days_before_earnings = 2

[risk.sectors]
SPY = "broad_index"
GLD = "metals"
"#,
        )
        .unwrap();
        let child = parse_runtime_config(
            r#"
extends = "base.toml"

[runtime]
strategy_families = ["naked_put"]
candidate_ledger_max_candidates = 20

[naked_scanner]
max_buying_power_usage_pct = 0.03

[risk]
max_active_entries = 3
max_single_entry_risk_capital_usd = 400.0

[risk.sectors]
GDX = "metals"
"#,
        )
        .unwrap();

        let merged = child.merge_parent(parent);

        assert!(merged.extends.is_none());
        assert_eq!(merged.runtime.strategy_families, vec!["naked_put"]);
        assert_eq!(merged.runtime.max_iterations, Some(0));
        assert_eq!(merged.runtime.candidate_ledger_enabled, Some(true));
        assert_eq!(merged.runtime.candidate_ledger_max_candidates, Some(20));
        assert_eq!(merged.universe.underlyings, vec!["SPY", "GLD"]);
        assert_eq!(merged.universe.quantity, Some(1));
        assert_eq!(merged.naked_scanner.max_buying_power_usage_pct, Some(0.03),);
        assert_eq!(merged.naked_scanner.min_score, Some(70.0));
        assert_eq!(merged.risk.max_active_entries, Some(3));
        assert_eq!(merged.risk.max_active_entries_per_underlying, Some(1));
        assert_eq!(merged.risk.max_single_entry_risk_capital_usd, Some(400.0));
        assert_eq!(merged.risk.max_portfolio_risk_capital_usd, Some(1000.0));
        assert_eq!(merged.risk.block_unestimated_risk_capital, Some(true));
        assert_eq!(
            merged.event_shock.earnings_events_path,
            Some(PathBuf::from("/tmp/earnings_events_approved.csv")),
        );
        assert_eq!(merged.event_shock.block_days_before_earnings, Some(2));
        assert_eq!(
            merged.risk.sectors.get("SPY").map(String::as_str),
            Some("broad_index"),
        );
        assert_eq!(
            merged.risk.sectors.get("GDX").map(String::as_str),
            Some("metals"),
        );
    }

    #[test]
    fn parse_runtime_config_reads_strategy_scanner_and_management_sections() {
        let config = parse_runtime_config(
            r#"
[runtime]
strategy_families = ["put", "iron_condor"]
dry_run_families = ["iron_condor"]
max_iterations = 0
open_orders = false
close_orders = true
state_path = "/tmp/alpaca-state.json"
candidate_ledger_enabled = true
candidate_ledger_max_candidates = 7

[universe]
underlyings = ["SPY", "QQQ"]
quantity = 2
entry_start = "09:45"
entry_end = "14:30"
entry_timezone = "America/New_York"

[scanner]
min_dte = 7
max_dte = 14
short_delta_min = 0.18
short_delta_max = 0.28
widths = [2.0, 5.0]
min_open_interest = 300
max_leg_spread_pct = 0.10
min_return_on_risk = 0.15
min_credit_to_width = 0.09

[iron_condor]
min_return_on_risk = 0.20
wing_min_return_on_risk = 0.11
require_equal_widths = true

[debit_scanner]
min_dte = 10
max_dte = 35
long_delta_min = 0.45
long_delta_max = 0.60
widths = [3.0, 5.0]
min_open_interest = 250
max_leg_spread_pct = 0.12
max_debit_to_width = 0.50
min_debit_to_width = 0.25
min_reward_to_risk = 0.80

[naked_scanner]
min_dte = 5
max_dte = 14
short_delta_min = 0.08
short_delta_max = 0.16
min_open_interest = 500
max_spread_pct = 0.08
min_credit = 0.20
min_bid_size = 1
min_ask_size = 1
min_daily_volume = 50
min_implied_volatility = 0.15
max_implied_volatility = 0.90
min_annualized_premium_yield = 0.10
max_buying_power_usage_pct = 0.10
min_return_on_buying_power = 0.0005
min_breakeven_pop = 0.70
max_probability_of_touch = 0.45
min_distance_to_breakeven_pct = 0.005
min_expected_move_coverage = 1.00
min_score = 70.0

[naked_1_3dte_scanner]
min_dte = 1
max_dte = 3
short_delta_min = 0.06
short_delta_max = 0.14
min_open_interest = 300
max_spread_pct = 0.08
min_credit = 0.12
min_bid_size = 1
min_ask_size = 1
min_daily_volume = 100
min_implied_volatility = 0.12
max_implied_volatility = 1.00
min_annualized_premium_yield = 0.12
max_buying_power_usage_pct = 0.10
min_return_on_buying_power = 0.0004
min_breakeven_pop = 0.72
max_probability_of_touch = 0.40
min_distance_to_breakeven_pct = 0.004
min_expected_move_coverage = 1.10
min_score = 72.0

[risk]
max_active_entries = 1
max_daily_submits = 1
max_open_orders = 1
max_active_entries_per_underlying = 1
max_active_entries_per_sector = 3
max_single_entry_risk_capital_usd = 500.0
max_portfolio_risk_capital_usd = 1500.0
block_unestimated_risk_capital = false

[risk.sectors]
SPY = "broad_index"
QQQ = "broad_index"

[management]
stale_entry_secs = 600
stale_close_secs = 120
close_regular_hours_only = true
close_start = "09:30"
close_end = "16:00"
close_price_cushion = 0.02
close_reprice_step = 0.01
max_close_price_cushion = 0.05
max_close_attempts = 4
close_reprice_cooldown_secs = 45
profit_target_close_fraction = 0.45
stop_loss_close_multiple = 1.8
max_hold_secs = 3600
expiration_exit_days = 2

[event_shock]
earnings_events_path = "/tmp/earnings_events_approved.csv"
require_earnings_events = true
block_days_before_earnings = 3
block_days_after_earnings = 2
"#,
        )
        .unwrap();

        assert_eq!(config.runtime.strategy_families, vec!["put", "iron_condor"]);
        assert_eq!(config.runtime.dry_run_families, vec!["iron_condor"]);
        assert_eq!(config.runtime.open_orders, Some(false));
        assert_eq!(config.runtime.close_orders, Some(true));
        assert_eq!(config.runtime.candidate_ledger_enabled, Some(true));
        assert_eq!(config.runtime.candidate_ledger_max_candidates, Some(7));
        assert_eq!(config.universe.underlyings, vec!["SPY", "QQQ"]);
        assert_eq!(config.scanner.widths, Some(vec![2.0, 5.0]));
        assert_eq!(config.scanner.min_credit_to_width, Some(0.09));
        assert_eq!(config.iron_condor.min_return_on_risk, Some(0.20));
        assert_eq!(config.debit_scanner.widths, Some(vec![3.0, 5.0]));
        assert_eq!(config.debit_scanner.min_debit_to_width, Some(0.25));
        assert_eq!(config.naked_scanner.min_dte, Some(5));
        assert_eq!(config.naked_scanner.max_spread_pct, Some(0.08));
        assert_eq!(config.naked_scanner.min_credit, Some(0.20));
        assert_eq!(config.naked_scanner.min_daily_volume, Some(50));
        assert_eq!(config.naked_scanner.min_implied_volatility, Some(0.15));
        assert_eq!(
            config.naked_scanner.min_annualized_premium_yield,
            Some(0.10)
        );
        assert_eq!(config.naked_scanner.max_buying_power_usage_pct, Some(0.10));
        assert_eq!(
            config.naked_scanner.min_return_on_buying_power,
            Some(0.0005)
        );
        assert_eq!(config.naked_scanner.min_breakeven_pop, Some(0.70));
        assert_eq!(config.naked_scanner.max_probability_of_touch, Some(0.45));
        assert_eq!(
            config.naked_scanner.min_distance_to_breakeven_pct,
            Some(0.005)
        );
        assert_eq!(config.naked_scanner.min_expected_move_coverage, Some(1.00));
        assert_eq!(config.naked_scanner.min_score, Some(70.0));
        assert_eq!(config.naked_1_3dte_scanner.min_dte, Some(1));
        assert_eq!(config.naked_1_3dte_scanner.max_dte, Some(3));
        assert_eq!(config.naked_1_3dte_scanner.min_credit, Some(0.12));
        assert_eq!(
            config.naked_1_3dte_scanner.max_buying_power_usage_pct,
            Some(0.10)
        );
        assert_eq!(
            config.naked_1_3dte_scanner.min_return_on_buying_power,
            Some(0.0004)
        );
        assert_eq!(
            config.naked_1_3dte_scanner.max_probability_of_touch,
            Some(0.40)
        );
        assert_eq!(
            config.naked_1_3dte_scanner.min_expected_move_coverage,
            Some(1.10)
        );
        assert_eq!(config.naked_1_3dte_scanner.min_score, Some(72.0));
        assert_eq!(config.risk.max_active_entries, Some(1));
        assert_eq!(config.risk.max_daily_submits, Some(1));
        assert_eq!(config.risk.max_open_orders, Some(1));
        assert_eq!(config.risk.max_active_entries_per_underlying, Some(1));
        assert_eq!(config.risk.max_active_entries_per_sector, Some(3));
        assert_eq!(config.risk.max_single_entry_risk_capital_usd, Some(500.0));
        assert_eq!(config.risk.max_portfolio_risk_capital_usd, Some(1500.0));
        assert_eq!(config.risk.block_unestimated_risk_capital, Some(false));
        assert_eq!(
            config.risk.sectors.get("SPY").map(String::as_str),
            Some("broad_index"),
        );
        assert_eq!(config.management.stale_entry_secs, Some(600));
        assert_eq!(config.management.stale_close_secs, Some(120));
        assert_eq!(config.management.close_regular_hours_only, Some(true));
        assert_eq!(config.management.close_start.as_deref(), Some("09:30"));
        assert_eq!(config.management.close_end.as_deref(), Some("16:00"));
        assert_eq!(config.management.close_price_cushion, Some(0.02));
        assert_eq!(config.management.close_reprice_step, Some(0.01));
        assert_eq!(config.management.max_close_price_cushion, Some(0.05));
        assert_eq!(config.management.max_close_attempts, Some(4));
        assert_eq!(config.management.close_reprice_cooldown_secs, Some(45));
        assert_eq!(
            config.event_shock.earnings_events_path,
            Some(PathBuf::from("/tmp/earnings_events_approved.csv")),
        );
        assert_eq!(config.event_shock.require_earnings_events, Some(true));
        assert_eq!(config.event_shock.block_days_before_earnings, Some(3));
        assert_eq!(config.event_shock.block_days_after_earnings, Some(2));
    }

    #[test]
    fn strategy_config_accepts_combined_four_leg_strategy() {
        let config =
            strategy_family_config_from_values(vec!["both,iron_condor".to_string()]).unwrap();

        assert_eq!(
            config.credit_kinds,
            vec![CreditSpreadKind::Put, CreditSpreadKind::Call],
        );
        assert!(config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert!(config.naked_kinds.is_empty());
    }

    #[test]
    fn strategy_config_accepts_debit_strategies() {
        let config =
            strategy_family_config_from_values(vec!["call_debit,put_debit".to_string()]).unwrap();

        assert!(config.credit_kinds.is_empty());
        assert!(!config.iron_condor_enabled);
        assert_eq!(
            config.debit_kinds,
            vec![DebitSpreadKind::Call, DebitSpreadKind::Put],
        );
        assert!(config.naked_kinds.is_empty());
    }

    #[test]
    fn strategy_config_accepts_naked_strategies() {
        let config = strategy_family_config_from_values(vec![
            "naked_call,naked_put,naked_call_1_3dte,naked_put_1_3dte".to_string(),
        ])
        .unwrap();

        assert!(config.credit_kinds.is_empty());
        assert!(!config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert_eq!(
            config.naked_kinds,
            vec![
                NakedOptionKind::Call,
                NakedOptionKind::Put,
                NakedOptionKind::CallOneToThreeDte,
                NakedOptionKind::PutOneToThreeDte,
            ],
        );
    }

    #[test]
    fn dry_run_strategy_config_has_no_default_strategy() {
        let config = dry_run_strategy_family_config_from_values(Vec::new()).unwrap();

        assert!(config.credit_kinds.is_empty());
        assert!(!config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert!(config.naked_kinds.is_empty());

        let config = dry_run_strategy_family_config_from_values(vec!["put".to_string()]).unwrap();
        assert_eq!(config.credit_kinds, vec![CreditSpreadKind::Put]);
        assert!(!config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert!(config.naked_kinds.is_empty());
    }
}
