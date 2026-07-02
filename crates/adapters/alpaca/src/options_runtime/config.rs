//! Runtime config parsing and building for the Alpaca options runtime.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use nautilus_infrastructure::sql::operational::OPERATIONAL_SCHEMA_DEFAULT;
use nautilus_trading::{
    options::candidates::{
        DebitSpreadScannerConfig, IronCondorScannerConfig, NakedOptionScannerConfig,
        PutCreditScannerConfig,
    },
    scheduled_events::{
        ApprovedScheduledEventLoadReport, ApprovedScheduledEventLoadRequest,
        SCHEDULED_EVENT_CATALOG_ENV, ScheduledEventLoadFreshness,
        default_scheduled_event_catalog_path, load_approved_scheduled_events,
    },
};
use serde::Deserialize;

use crate::earnings::{
    EARNINGS_EVENT_TYPE, earnings_events_from_approved_scheduled_events, load_earnings_events_csv,
};
use crate::{fleet::load_fleet_config_from_env, runtime::StrategyState};

use super::{
    AlpacaOptionsRuntimeConfig, AlpacaOptionsStrategyFamily, AlpacaOptionsStrategyMode,
    AlpacaOptionsStrategyProfile, AlpacaOptionsStrategyRiskOverrides,
    AlpacaOptionsStrategyScannerConfig, EventShockRuntimeStatus,
};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct RuntimeConfigFile {
    extends: Option<PathBuf>,
    runtime: RuntimeSection,
    strategies: Vec<StrategyBlockSection>,
    universe_groups: BTreeMap<String, UniverseGroupSection>,
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
            strategies: merge_vec(self.strategies, parent.strategies),
            universe_groups: merge_map(self.universe_groups, parent.universe_groups),
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

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StrategyBlockSection {
    id: Option<String>,
    family: Option<String>,
    mode: Option<String>,
    universe_groups: Vec<String>,
    underlyings: Vec<String>,
    include_underlyings: Vec<String>,
    exclude_underlyings: Vec<String>,
    quantity: Option<u64>,
    scanner: StrategyScannerSection,
    risk: StrategyRiskOverrideSection,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UniverseGroupSection {
    members: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StrategyScannerSection {
    min_dte: Option<i64>,
    max_dte: Option<i64>,
    short_delta_min: Option<f64>,
    short_delta_max: Option<f64>,
    long_delta_min: Option<f64>,
    long_delta_max: Option<f64>,
    widths: Option<Vec<f64>>,
    min_open_interest: Option<u64>,
    max_leg_spread_pct: Option<f64>,
    min_return_on_risk: Option<f64>,
    min_credit_to_width: Option<f64>,
    wing_min_return_on_risk: Option<f64>,
    require_equal_widths: Option<bool>,
    max_debit_to_width: Option<f64>,
    min_debit_to_width: Option<f64>,
    min_reward_to_risk: Option<f64>,
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

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StrategyRiskOverrideSection {
    max_active_entries: Option<usize>,
    max_daily_submits: Option<usize>,
    max_active_entries_per_underlying: Option<usize>,
    max_single_entry_risk_capital_usd: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UniverseSection {
    quantity: Option<u64>,
    entry_start: Option<String>,
    entry_end: Option<String>,
    entry_timezone: Option<String>,
}

impl UniverseSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
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
#[serde(default, deny_unknown_fields)]
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
    scheduled_event_catalog_path: Option<PathBuf>,
    earnings_events_path: Option<PathBuf>,
    require_earnings_events: Option<bool>,
    allow_csv_bridge: Option<bool>,
    stale_after_days: Option<i64>,
    horizon_days: Option<i64>,
    block_days_before_earnings: Option<i64>,
    block_days_after_earnings: Option<i64>,
}

impl EventShockSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            scheduled_event_catalog_path: self
                .scheduled_event_catalog_path
                .or(parent.scheduled_event_catalog_path),
            earnings_events_path: self.earnings_events_path.or(parent.earnings_events_path),
            require_earnings_events: self
                .require_earnings_events
                .or(parent.require_earnings_events),
            allow_csv_bridge: self.allow_csv_bridge.or(parent.allow_csv_bridge),
            stale_after_days: self.stale_after_days.or(parent.stale_after_days),
            horizon_days: self.horizon_days.or(parent.horizon_days),
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
    reject_retired_strategy_composition_env_vars()?;
    anyhow::ensure!(
        split_strings(cli_underlyings).is_empty(),
        "positional Alpaca underlyings are retired; configure profile-level underlyings or universe_groups"
    );
    let scanner = scanner_config_from_file(&file.scanner);
    let iron_condor_scanner = iron_condor_scanner_config_from_file(&scanner, &file.iron_condor);
    let debit_scanner = debit_scanner_config_from_file(&file.debit_scanner);
    let naked_scanner = naked_scanner_config_from_file(&file.naked_scanner);
    let naked_1_3dte_scanner = naked_1_3dte_scanner_config_from_file(&file.naked_1_3dte_scanner);
    let stale_entry_secs = file.management.stale_entry_secs.unwrap_or(900);
    let interval_secs = env_parse("ALPACA_INTERVAL_SECS")
        .or(file.runtime.interval_secs)
        .unwrap_or(300);
    let universe_groups = universe_groups_from_file(file.universe_groups)?;
    let default_quantity = file.universe.quantity.unwrap_or(1);
    anyhow::ensure!(
        default_quantity > 0,
        "Alpaca strategy quantity must be positive"
    );
    let strategy_profiles = strategy_profiles_from_file(
        file.strategies,
        &universe_groups,
        default_quantity,
        &scanner,
        &iron_condor_scanner,
        &debit_scanner,
        &naked_scanner,
        &naked_1_3dte_scanner,
    )?;
    let quantity = strategy_profiles
        .first()
        .map_or(default_quantity, |profile| profile.quantity);
    let profile_underlyings = underlyings_from_profiles(&strategy_profiles);
    let event_shock_block_days_before_earnings =
        env_parse("ALPACA_EVENT_SHOCK_BLOCK_DAYS_BEFORE_EARNINGS")
            .or(file.event_shock.block_days_before_earnings)
            .unwrap_or(1)
            .max(0);
    let event_shock_block_days_after_earnings =
        env_parse("ALPACA_EVENT_SHOCK_BLOCK_DAYS_AFTER_EARNINGS")
            .or(file.event_shock.block_days_after_earnings)
            .unwrap_or(1)
            .max(0);
    let entry_timezone = file
        .universe
        .entry_timezone
        .as_deref()
        .unwrap_or("America/New_York")
        .parse::<Tz>()?;
    let as_of_utc = Utc::now();
    let event_shock_load = load_event_shock_earnings_events(
        &file.event_shock,
        &profile_underlyings,
        as_of_utc.with_timezone(&entry_timezone).date_naive(),
        as_of_utc,
        event_shock_block_days_before_earnings,
        event_shock_block_days_after_earnings,
    )?;
    let close_price_cushion = env_parse("ALPACA_CLOSE_PRICE_CUSHION")
        .or(file.management.close_price_cushion)
        .unwrap_or(0.0)
        .max(0.0);
    let fleet = load_fleet_config_from_env()?;
    let mut config = AlpacaOptionsRuntimeConfig {
        underlyings: profile_underlyings,
        universe_groups,
        strategy_profiles,
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
        event_shock_earnings_events: event_shock_load.events,
        event_shock: event_shock_load.status,
        event_shock_block_days_before_earnings,
        event_shock_block_days_after_earnings,
        sectors: sector_map_from_file(file.risk.sectors),
        max_iterations: env_parse("ALPACA_MAX_ITERATIONS")
            .or(file.runtime.max_iterations)
            .unwrap_or(1),
        interval_secs,
        quantity,
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
        entry_timezone,
        state_path: env::var("ALPACA_STATE_PATH")
            .map(PathBuf::from)
            .ok()
            .or(file.runtime.state_path)
            .unwrap_or_else(default_state_path),
        candidate_ledger_enabled: file.runtime.candidate_ledger_enabled.unwrap_or(true),
        candidate_ledger_max_candidates: file.runtime.candidate_ledger_max_candidates.unwrap_or(10),
        iron_condor_scanner,
        debit_scanner,
        naked_scanner,
        naked_1_3dte_scanner,
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

fn strategy_profiles_from_file(
    strategies: Vec<StrategyBlockSection>,
    universe_groups: &BTreeMap<String, Vec<String>>,
    default_quantity: u64,
    credit_scanner: &PutCreditScannerConfig,
    iron_condor_scanner: &IronCondorScannerConfig,
    debit_scanner: &DebitSpreadScannerConfig,
    naked_scanner: &NakedOptionScannerConfig,
    naked_1_3dte_scanner: &NakedOptionScannerConfig,
) -> anyhow::Result<Vec<AlpacaOptionsStrategyProfile>> {
    anyhow::ensure!(
        !strategies.is_empty(),
        "Alpaca options runtime requires at least one explicit [[strategies]] profile"
    );

    let mut profiles = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut family_underlyings = std::collections::BTreeSet::new();
    for strategy in strategies {
        let id = strategy
            .id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| anyhow::anyhow!("every [[strategies]] block requires a non-empty id"))?;
        anyhow::ensure!(
            ids.insert(id.clone()),
            "duplicate Alpaca strategy profile id {id}"
        );
        let family = strategy
            .family
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Alpaca strategy profile {id} requires a family field"))?
            .parse::<AlpacaOptionsStrategyFamily>()
            .map_err(|error| anyhow::anyhow!("{error} in profile {id}"))?;
        let mode = strategy
            .mode
            .as_deref()
            .unwrap_or(AlpacaOptionsStrategyMode::Live.as_str())
            .parse::<AlpacaOptionsStrategyMode>()
            .map_err(|error| anyhow::anyhow!("{error} in profile {id}"))?;
        let universe_groups_requested = split_strings(strategy.universe_groups.clone());
        let include_underlyings = normalized_underlyings(
            split_strings(strategy.underlyings.clone())
                .into_iter()
                .chain(split_strings(strategy.include_underlyings.clone()).into_iter()),
        );
        let exclude_underlyings =
            normalized_underlyings(split_strings(strategy.exclude_underlyings.clone()));
        let underlyings = expanded_strategy_underlyings(
            &id,
            &universe_groups_requested,
            &include_underlyings,
            &exclude_underlyings,
            universe_groups,
        )?;
        anyhow::ensure!(
            !underlyings.is_empty(),
            "Alpaca strategy profile {id} resolved no underlyings"
        );
        let quantity = strategy.quantity.unwrap_or(default_quantity);
        anyhow::ensure!(
            quantity > 0,
            "Alpaca strategy profile {id} quantity must be positive"
        );
        for underlying in &underlyings {
            let key = (family, underlying.clone());
            anyhow::ensure!(
                family_underlyings.insert(key),
                "duplicate Alpaca strategy profile for family {} and underlying {}",
                family.as_str(),
                underlying
            );
        }
        let scanner = strategy_scanner_config(
            family,
            strategy.scanner,
            credit_scanner,
            iron_condor_scanner,
            debit_scanner,
            naked_scanner,
            naked_1_3dte_scanner,
        );
        let risk = AlpacaOptionsStrategyRiskOverrides {
            max_active_entries: strategy.risk.max_active_entries,
            max_daily_submits: strategy.risk.max_daily_submits,
            max_active_entries_per_underlying: strategy.risk.max_active_entries_per_underlying,
            max_single_entry_risk_capital_usd: strategy.risk.max_single_entry_risk_capital_usd,
        };
        profiles.push(AlpacaOptionsStrategyProfile {
            id,
            family,
            mode,
            universe_groups: universe_groups_requested,
            include_underlyings,
            exclude_underlyings,
            underlyings,
            quantity,
            scanner,
            risk,
        });
    }

    Ok(profiles)
}

fn strategy_scanner_config(
    family: AlpacaOptionsStrategyFamily,
    scanner: StrategyScannerSection,
    credit_scanner: &PutCreditScannerConfig,
    iron_condor_scanner: &IronCondorScannerConfig,
    debit_scanner: &DebitSpreadScannerConfig,
    naked_scanner: &NakedOptionScannerConfig,
    naked_1_3dte_scanner: &NakedOptionScannerConfig,
) -> AlpacaOptionsStrategyScannerConfig {
    match family {
        AlpacaOptionsStrategyFamily::PutCredit | AlpacaOptionsStrategyFamily::CallCredit => {
            AlpacaOptionsStrategyScannerConfig::Credit(apply_credit_scanner_overrides(
                credit_scanner.clone(),
                &scanner,
            ))
        }
        AlpacaOptionsStrategyFamily::IronCondor => AlpacaOptionsStrategyScannerConfig::IronCondor(
            apply_iron_condor_overrides(iron_condor_scanner.clone(), &scanner),
        ),
        AlpacaOptionsStrategyFamily::PutDebit | AlpacaOptionsStrategyFamily::CallDebit => {
            AlpacaOptionsStrategyScannerConfig::Debit(apply_debit_scanner_overrides(
                debit_scanner.clone(),
                &scanner,
            ))
        }
        AlpacaOptionsStrategyFamily::NakedPut | AlpacaOptionsStrategyFamily::NakedCall => {
            AlpacaOptionsStrategyScannerConfig::Naked(apply_naked_scanner_overrides(
                naked_scanner.clone(),
                &scanner,
            ))
        }
        AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte
        | AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte => {
            AlpacaOptionsStrategyScannerConfig::Naked(apply_naked_scanner_overrides(
                naked_1_3dte_scanner.clone(),
                &scanner,
            ))
        }
    }
}

fn apply_credit_scanner_overrides(
    mut config: PutCreditScannerConfig,
    overrides: &StrategyScannerSection,
) -> PutCreditScannerConfig {
    if let Some(value) = overrides.min_dte {
        config.min_dte = value;
    }
    if let Some(value) = overrides.max_dte {
        config.max_dte = value;
    }
    if let Some(value) = overrides.short_delta_min {
        config.short_delta_min = value;
    }
    if let Some(value) = overrides.short_delta_max {
        config.short_delta_max = value;
    }
    if let Some(value) = overrides.widths.clone() {
        config.widths = value;
    }
    if let Some(value) = overrides.min_open_interest {
        config.min_open_interest = value;
    }
    if let Some(value) = overrides.max_leg_spread_pct {
        config.max_leg_spread_pct = value;
    }
    if let Some(value) = overrides.min_return_on_risk {
        config.min_return_on_risk = value;
    }
    if let Some(value) = overrides.min_credit_to_width {
        config.min_credit_to_width = value;
    }
    config
}

fn apply_iron_condor_overrides(
    mut config: IronCondorScannerConfig,
    overrides: &StrategyScannerSection,
) -> IronCondorScannerConfig {
    if let Some(value) = overrides.min_dte {
        config.credit.min_dte = value;
    }
    if let Some(value) = overrides.max_dte {
        config.credit.max_dte = value;
    }
    if let Some(value) = overrides.short_delta_min {
        config.credit.short_delta_min = value;
    }
    if let Some(value) = overrides.short_delta_max {
        config.credit.short_delta_max = value;
    }
    if let Some(value) = overrides.widths.clone() {
        config.credit.widths = value;
    }
    if let Some(value) = overrides.min_open_interest {
        config.credit.min_open_interest = value;
    }
    if let Some(value) = overrides.max_leg_spread_pct {
        config.credit.max_leg_spread_pct = value;
    }
    if let Some(value) = overrides.min_return_on_risk {
        config.min_return_on_risk = value;
    }
    if let Some(value) = overrides.wing_min_return_on_risk {
        config.credit.min_return_on_risk = value;
    }
    if let Some(value) = overrides.require_equal_widths {
        config.require_equal_widths = value;
    }
    config
}

fn apply_debit_scanner_overrides(
    mut config: DebitSpreadScannerConfig,
    overrides: &StrategyScannerSection,
) -> DebitSpreadScannerConfig {
    if let Some(value) = overrides.min_dte {
        config.min_dte = value;
    }
    if let Some(value) = overrides.max_dte {
        config.max_dte = value;
    }
    if let Some(value) = overrides.long_delta_min {
        config.long_delta_min = value;
    }
    if let Some(value) = overrides.long_delta_max {
        config.long_delta_max = value;
    }
    if let Some(value) = overrides.widths.clone() {
        config.widths = value;
    }
    if let Some(value) = overrides.min_open_interest {
        config.min_open_interest = value;
    }
    if let Some(value) = overrides.max_leg_spread_pct {
        config.max_leg_spread_pct = value;
    }
    if let Some(value) = overrides.max_debit_to_width {
        config.max_debit_to_width = value;
    }
    if let Some(value) = overrides.min_debit_to_width {
        config.min_debit_to_width = value;
    }
    if let Some(value) = overrides.min_reward_to_risk {
        config.min_reward_to_risk = value;
    }
    config
}

fn apply_naked_scanner_overrides(
    mut config: NakedOptionScannerConfig,
    overrides: &StrategyScannerSection,
) -> NakedOptionScannerConfig {
    if let Some(value) = overrides.min_dte {
        config.min_dte = value;
    }
    if let Some(value) = overrides.max_dte {
        config.max_dte = value;
    }
    if let Some(value) = overrides.short_delta_min {
        config.short_delta_min = value;
    }
    if let Some(value) = overrides.short_delta_max {
        config.short_delta_max = value;
    }
    if let Some(value) = overrides.min_open_interest {
        config.min_open_interest = value;
    }
    if let Some(value) = overrides.max_spread_pct {
        config.max_spread_pct = value;
    }
    if let Some(value) = overrides.min_credit {
        config.min_credit = value;
    }
    if let Some(value) = overrides.min_bid_size {
        config.min_bid_size = value;
    }
    if let Some(value) = overrides.min_ask_size {
        config.min_ask_size = value;
    }
    if let Some(value) = overrides.min_daily_volume {
        config.min_daily_volume = value;
    }
    if let Some(value) = overrides.min_implied_volatility {
        config.min_implied_volatility = value;
    }
    if let Some(value) = overrides.max_implied_volatility {
        config.max_implied_volatility = value;
    }
    if let Some(value) = overrides.min_annualized_premium_yield {
        config.min_annualized_premium_yield = value;
    }
    if let Some(value) = overrides.max_buying_power_usage_pct {
        config.max_buying_power_usage_pct = value;
    }
    if let Some(value) = overrides.min_return_on_buying_power {
        config.min_return_on_buying_power = value;
    }
    if let Some(value) = overrides.min_breakeven_pop {
        config.min_breakeven_pop = value;
    }
    if let Some(value) = overrides.max_probability_of_touch {
        config.max_probability_of_touch = value;
    }
    if let Some(value) = overrides.min_distance_to_breakeven_pct {
        config.min_distance_to_breakeven_pct = value;
    }
    if let Some(value) = overrides.min_expected_move_coverage {
        config.min_expected_move_coverage = value;
    }
    if let Some(value) = overrides.min_score {
        config.min_score = value;
    }
    config
}

fn underlyings_from_profiles(profiles: &[AlpacaOptionsStrategyProfile]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    profiles
        .iter()
        .flat_map(|profile| profile.underlyings.iter())
        .filter_map(|underlying| {
            if seen.insert(underlying.clone()) {
                Some(underlying.clone())
            } else {
                None
            }
        })
        .collect()
}

fn universe_groups_from_file(
    groups: BTreeMap<String, UniverseGroupSection>,
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let mut resolved = BTreeMap::new();
    for (name, group) in groups {
        let name = name.trim().to_string();
        anyhow::ensure!(!name.is_empty(), "universe group names must not be empty");
        let members = normalized_underlyings(split_strings(group.members));
        anyhow::ensure!(
            !members.is_empty(),
            "universe group {name} must contain at least one member"
        );
        resolved.insert(name, members);
    }
    Ok(resolved)
}

fn expanded_strategy_underlyings(
    profile_id: &str,
    group_names: &[String],
    include_underlyings: &[String],
    exclude_underlyings: &[String],
    universe_groups: &BTreeMap<String, Vec<String>>,
) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(
        !group_names.is_empty() || !include_underlyings.is_empty(),
        "Alpaca strategy profile {profile_id} requires universe_groups, underlyings, or include_underlyings"
    );

    let mut underlyings = Vec::new();
    for group_name in group_names {
        let Some(members) = universe_groups.get(group_name) else {
            anyhow::bail!(
                "Alpaca strategy profile {profile_id} references unknown universe group {group_name}"
            );
        };
        for member in members {
            push_unique_underlying(&mut underlyings, member.clone());
        }
    }
    for underlying in include_underlyings {
        push_unique_underlying(&mut underlyings, underlying.clone());
    }

    if !exclude_underlyings.is_empty() {
        underlyings.retain(|underlying| !exclude_underlyings.contains(underlying));
    }

    anyhow::ensure!(
        !underlyings.is_empty(),
        "Alpaca strategy profile {profile_id} resolved no underlyings after excludes"
    );
    Ok(underlyings)
}

fn normalized_underlyings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().to_ascii_uppercase();
        if !value.is_empty() {
            push_unique_underlying(&mut normalized, value);
        }
    }
    normalized
}

fn push_unique_underlying(underlyings: &mut Vec<String>, underlying: String) {
    if !underlyings.contains(&underlying) {
        underlyings.push(underlying);
    }
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
        if has_long_premium_profiles(config) && !account.permissions.long_premium {
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
        if has_naked_call_profiles(config) && !account.permissions.naked_calls {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_naked_calls_required:{}",
                account.id
            ));
        }
        if has_naked_put_profiles(config) && !account.permissions.naked_puts {
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
    has_live_open_profile(config, |family| {
        matches!(
            family,
            AlpacaOptionsStrategyFamily::PutCredit
                | AlpacaOptionsStrategyFamily::CallCredit
                | AlpacaOptionsStrategyFamily::IronCondor
        )
    })
}

fn has_undefined_risk_strategies(config: &AlpacaOptionsRuntimeConfig) -> bool {
    has_live_open_profile(config, is_naked_family)
}

fn has_long_premium_profiles(config: &AlpacaOptionsRuntimeConfig) -> bool {
    has_live_open_profile(config, |family| {
        matches!(
            family,
            AlpacaOptionsStrategyFamily::PutDebit | AlpacaOptionsStrategyFamily::CallDebit
        )
    })
}

fn has_naked_call_profiles(config: &AlpacaOptionsRuntimeConfig) -> bool {
    has_live_open_profile(config, |family| {
        matches!(
            family,
            AlpacaOptionsStrategyFamily::NakedCall
                | AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte
        )
    })
}

fn has_naked_put_profiles(config: &AlpacaOptionsRuntimeConfig) -> bool {
    has_live_open_profile(config, |family| {
        matches!(
            family,
            AlpacaOptionsStrategyFamily::NakedPut
                | AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte
        )
    })
}

fn has_live_open_profile(
    config: &AlpacaOptionsRuntimeConfig,
    predicate: impl Fn(AlpacaOptionsStrategyFamily) -> bool,
) -> bool {
    config.open_orders_enabled
        && config.strategy_profiles.iter().any(|profile| {
            matches!(profile.mode, AlpacaOptionsStrategyMode::Live) && predicate(profile.family)
        })
}

fn is_naked_family(family: AlpacaOptionsStrategyFamily) -> bool {
    matches!(
        family,
        AlpacaOptionsStrategyFamily::NakedPut
            | AlpacaOptionsStrategyFamily::NakedCall
            | AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte
            | AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte
    )
}

fn min_limit(current: Option<usize>, fleet_limit: usize) -> usize {
    current.map_or(fleet_limit, |current| current.min(fleet_limit))
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

struct EventShockLoad {
    events: Vec<crate::earnings::EarningsEvent>,
    status: EventShockRuntimeStatus,
}

fn load_event_shock_earnings_events(
    config: &EventShockSection,
    underlyings: &[String],
    as_of_date: NaiveDate,
    as_of_utc: DateTime<Utc>,
    block_days_before_earnings: i64,
    block_days_after_earnings: i64,
) -> anyhow::Result<EventShockLoad> {
    let catalog_path = event_shock_catalog_path(config);
    let mut request =
        ApprovedScheduledEventLoadRequest::new(EARNINGS_EVENT_TYPE, as_of_date, as_of_utc);
    request.lookback_days = block_days_after_earnings;
    request.horizon_days = env_parse("ALPACA_EVENT_SHOCK_HORIZON_DAYS")
        .or(config.horizon_days)
        .unwrap_or(90)
        .max(block_days_before_earnings);
    request.underlyings = underlyings.to_vec();
    request.stale_after_days = env_parse("ALPACA_EVENT_SHOCK_STALE_AFTER_DAYS")
        .or(config.stale_after_days)
        .unwrap_or(1)
        .max(0);

    let report = load_approved_scheduled_events(&catalog_path, &request).map_err(|error| {
        anyhow::anyhow!(
            "failed to load approved scheduled earnings events from {}: {error}",
            catalog_path.display()
        )
    })?;
    let csv_bridge_enabled = env_bool("ALPACA_EVENT_SHOCK_ALLOW_CSV_BRIDGE")
        .or(config.allow_csv_bridge)
        .unwrap_or(false);
    let csv_bridge_path = env::var_os("ALPACA_EVENT_SHOCK_EARNINGS_EVENTS_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| config.earnings_events_path.clone());
    let required = env_bool("ALPACA_EVENT_SHOCK_REQUIRE_EARNINGS_EVENTS")
        .or(config.require_earnings_events)
        .unwrap_or(false);

    let mut events = if report.freshness == ScheduledEventLoadFreshness::Fresh {
        earnings_events_from_approved_scheduled_events(&report.events)
    } else {
        Vec::new()
    };
    let mut source = if report.freshness == ScheduledEventLoadFreshness::Fresh {
        "scheduled_event_catalog".to_string()
    } else {
        "none".to_string()
    };
    let mut dry_run_only = report.freshness != ScheduledEventLoadFreshness::Fresh;

    if events.is_empty()
        && csv_bridge_enabled
        && let Some(path) = csv_bridge_path.as_ref()
    {
        events = load_earnings_events_csv(path).map_err(|error| {
            anyhow::anyhow!(
                "failed to load event-shock CSV bridge events {}: {error}",
                path.display()
            )
        })?;
        source = "csv_bridge".to_string();
        dry_run_only = true;
    }

    Ok(EventShockLoad {
        status: event_shock_status(
            source,
            catalog_path,
            events.len(),
            &report,
            csv_bridge_enabled,
            csv_bridge_path,
            dry_run_only,
            required,
        ),
        events,
    })
}

fn event_shock_catalog_path(config: &EventShockSection) -> PathBuf {
    env::var_os(SCHEDULED_EVENT_CATALOG_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| config.scheduled_event_catalog_path.clone())
        .unwrap_or_else(default_scheduled_event_catalog_path)
}

fn event_shock_status(
    source: String,
    catalog_path: PathBuf,
    event_count: usize,
    report: &ApprovedScheduledEventLoadReport,
    csv_bridge_enabled: bool,
    csv_bridge_path: Option<PathBuf>,
    dry_run_only: bool,
    required: bool,
) -> EventShockRuntimeStatus {
    EventShockRuntimeStatus {
        source,
        scheduled_event_catalog_path: catalog_path,
        event_count,
        catalog_event_count: report.event_count,
        source_set: report.source_set.clone(),
        policy_versions: report.policy_versions.clone(),
        coverage_start: report.coverage_start,
        coverage_end: report.coverage_end,
        freshness: report.freshness.as_str().to_string(),
        unavailable_reason: report.unavailable_reason.clone(),
        rejected_count: report.rejected_count,
        csv_bridge_enabled,
        csv_bridge_path,
        dry_run_only,
        required,
    }
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

fn reject_retired_strategy_composition_env_vars() -> anyhow::Result<()> {
    let present = [
        "ALPACA_STRATEGY_FAMILIES",
        "ALPACA_DRY_RUN_FAMILIES",
        "ALPACA_QTY",
    ]
    .into_iter()
    .filter(|name| env::var_os(name).is_some())
    .collect::<Vec<_>>();
    if present.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "retired Alpaca strategy composition env vars are set: {}; configure [[strategies]] blocks in ALPACA_CONFIG_PATH instead",
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
pub(super) fn runtime_config_file_for_tests() -> RuntimeConfigFile {
    let mut file = RuntimeConfigFile::default();
    file.universe.quantity = Some(1);
    file.strategies.push(runtime_test_strategy_block(
        "put_credit_test",
        "put_credit",
        AlpacaOptionsStrategyMode::Live,
        ["SPY"],
    ));
    file
}

#[cfg(test)]
fn runtime_test_strategy_block(
    id: impl Into<String>,
    family: impl Into<String>,
    mode: AlpacaOptionsStrategyMode,
    underlyings: impl IntoIterator<Item = impl Into<String>>,
) -> StrategyBlockSection {
    StrategyBlockSection {
        id: Some(id.into()),
        family: Some(family.into()),
        mode: Some(mode.as_str().to_string()),
        universe_groups: Vec::new(),
        underlyings: underlyings.into_iter().map(Into::into).collect(),
        include_underlyings: Vec::new(),
        exclude_underlyings: Vec::new(),
        quantity: Some(1),
        scanner: StrategyScannerSection::default(),
        risk: StrategyRiskOverrideSection::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options_runtime::{AlpacaOptionsStrategyScannerConfig, EventShockRuntimeStatus};

    #[test]
    fn split_strings_splits_args_and_csv() {
        assert_eq!(
            split_strings(["SPY, QQQ".to_string(), "IWM".to_string()]),
            vec!["SPY", "QQQ", "IWM"],
        );
    }

    #[test]
    fn strategy_profiles_require_explicit_profile() {
        let error = build_strategy_profiles(Vec::new(), BTreeMap::new())
            .expect_err("empty strategy profile list should fail");

        assert!(
            error
                .to_string()
                .contains("requires at least one explicit [[strategies]] profile")
        );
    }

    #[test]
    fn strategy_profiles_expand_groups_includes_and_excludes() {
        let groups = BTreeMap::from([(
            "core_liquid_options".to_string(),
            vec!["SPY".to_string(), "QQQ".to_string(), "IWM".to_string()],
        )]);
        let mut strategy = strategy_block(
            "put_credit_core",
            "put_credit",
            AlpacaOptionsStrategyMode::Live,
            Vec::<String>::new(),
        );
        strategy.universe_groups = vec!["core_liquid_options".to_string()];
        strategy.include_underlyings = vec!["GLD".to_string(), "spy".to_string()];
        strategy.exclude_underlyings = vec!["QQQ".to_string()];

        let profiles = build_strategy_profiles(vec![strategy], groups).unwrap();

        assert_eq!(profiles.len(), 1);
        assert_eq!(
            profiles[0].underlyings,
            vec!["SPY".to_string(), "IWM".to_string(), "GLD".to_string()],
        );
        assert_eq!(
            profiles[0].universe_groups,
            vec!["core_liquid_options".to_string()]
        );
        assert_eq!(
            profiles[0].include_underlyings,
            vec!["GLD".to_string(), "SPY".to_string()]
        );
        assert_eq!(profiles[0].exclude_underlyings, vec!["QQQ".to_string()]);
    }

    #[test]
    fn strategy_profiles_reject_unknown_universe_group() {
        let mut strategy = strategy_block(
            "put_credit_missing",
            "put_credit",
            AlpacaOptionsStrategyMode::Live,
            Vec::<String>::new(),
        );
        strategy.universe_groups = vec!["missing_group".to_string()];

        let error = build_strategy_profiles(vec![strategy], BTreeMap::new())
            .expect_err("unknown group should fail");

        assert!(
            error
                .to_string()
                .contains("references unknown universe group missing_group")
        );
    }

    #[test]
    fn strategy_profiles_reject_empty_expansion_after_excludes() {
        let groups = BTreeMap::from([("core_liquid_options".to_string(), vec!["SPY".to_string()])]);
        let mut strategy = strategy_block(
            "put_credit_empty",
            "put_credit",
            AlpacaOptionsStrategyMode::Live,
            Vec::<String>::new(),
        );
        strategy.universe_groups = vec!["core_liquid_options".to_string()];
        strategy.exclude_underlyings = vec!["SPY".to_string()];

        let error = build_strategy_profiles(vec![strategy], groups)
            .expect_err("exclude-empty universe should fail");

        assert!(
            error
                .to_string()
                .contains("resolved no underlyings after excludes")
        );
    }

    #[test]
    fn runtime_config_rejects_positional_underlyings() {
        let error =
            build_options_runtime_config(runtime_config_file_for_tests(), vec!["SPY".to_string()])
                .expect_err("positional underlyings should fail");

        assert!(
            error
                .to_string()
                .contains("positional Alpaca underlyings are retired")
        );
    }

    #[test]
    fn dry_run_undefined_risk_profiles_do_not_require_live_permission() {
        let config = minimal_policy_config(vec![
            test_strategy_profile(
                "iron_condor_live",
                AlpacaOptionsStrategyFamily::IronCondor,
                AlpacaOptionsStrategyMode::Live,
                ["SPY"],
            ),
            test_strategy_profile(
                "naked_put_watch",
                AlpacaOptionsStrategyFamily::NakedPut,
                AlpacaOptionsStrategyMode::DryRun,
                ["SPY"],
            ),
            test_strategy_profile(
                "naked_call_watch",
                AlpacaOptionsStrategyFamily::NakedCall,
                AlpacaOptionsStrategyMode::DryRun,
                ["SPY"],
            ),
        ]);

        assert!(has_defined_risk_strategies(&config));
        assert!(!has_undefined_risk_strategies(&config));
        assert!(!has_naked_put_profiles(&config));
        assert!(!has_naked_call_profiles(&config));
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
	quantity = 1

	[universe_groups.core]
	members = ["SPY", "GLD"]

[naked_scanner]
max_buying_power_usage_pct = 0.10
min_score = 70.0

[risk]
max_active_entries_per_underlying = 1
max_portfolio_risk_capital_usd = 1000.0
block_unestimated_risk_capital = true

[event_shock]
scheduled_event_catalog_path = "/tmp/scheduled_events/catalog"
earnings_events_path = "/tmp/earnings_events_approved.csv"
allow_csv_bridge = true
stale_after_days = 2
horizon_days = 60
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
candidate_ledger_max_candidates = 20

[[strategies]]
id = "naked_put_watch"
family = "naked_put"
mode = "dry_run"
underlyings = ["GLD"]

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
        assert_eq!(merged.strategies.len(), 1);
        assert_eq!(merged.strategies[0].id.as_deref(), Some("naked_put_watch"));
        assert_eq!(merged.strategies[0].family.as_deref(), Some("naked_put"));
        assert_eq!(merged.strategies[0].mode.as_deref(), Some("dry_run"));
        assert_eq!(merged.runtime.max_iterations, Some(0));
        assert_eq!(merged.runtime.candidate_ledger_enabled, Some(true));
        assert_eq!(merged.runtime.candidate_ledger_max_candidates, Some(20));
        assert_eq!(
            merged
                .universe_groups
                .get("core")
                .map(|group| group.members.clone()),
            Some(vec!["SPY".to_string(), "GLD".to_string()]),
        );
        assert_eq!(merged.universe.quantity, Some(1));
        assert_eq!(merged.naked_scanner.max_buying_power_usage_pct, Some(0.03),);
        assert_eq!(merged.naked_scanner.min_score, Some(70.0));
        assert_eq!(merged.risk.max_active_entries, Some(3));
        assert_eq!(merged.risk.max_active_entries_per_underlying, Some(1));
        assert_eq!(merged.risk.max_single_entry_risk_capital_usd, Some(400.0));
        assert_eq!(merged.risk.max_portfolio_risk_capital_usd, Some(1000.0));
        assert_eq!(merged.risk.block_unestimated_risk_capital, Some(true));
        assert_eq!(
            merged.event_shock.scheduled_event_catalog_path,
            Some(PathBuf::from("/tmp/scheduled_events/catalog")),
        );
        assert_eq!(
            merged.event_shock.earnings_events_path,
            Some(PathBuf::from("/tmp/earnings_events_approved.csv")),
        );
        assert_eq!(merged.event_shock.allow_csv_bridge, Some(true));
        assert_eq!(merged.event_shock.stale_after_days, Some(2));
        assert_eq!(merged.event_shock.horizon_days, Some(60));
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
max_iterations = 0
open_orders = false
close_orders = true
state_path = "/tmp/alpaca-state.json"
candidate_ledger_enabled = true
candidate_ledger_max_candidates = 7

[[strategies]]
id = "put_credit_spy"
family = "put_credit"
mode = "live"
underlyings = ["SPY"]
quantity = 2

[[strategies]]
id = "iron_condor_qqq_watch"
family = "iron_condor"
mode = "dry_run"
underlyings = ["QQQ"]
quantity = 2

[strategies.scanner]
min_return_on_risk = 0.21

[strategies.risk]
max_active_entries = 1
max_daily_submits = 1

	[universe]
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
scheduled_event_catalog_path = "/tmp/scheduled_events/catalog"
earnings_events_path = "/tmp/earnings_events_approved.csv"
require_earnings_events = true
allow_csv_bridge = true
stale_after_days = 2
horizon_days = 60
block_days_before_earnings = 3
block_days_after_earnings = 2
"#,
        )
        .unwrap();

        assert_eq!(config.strategies.len(), 2);
        assert_eq!(config.strategies[0].id.as_deref(), Some("put_credit_spy"));
        assert_eq!(config.strategies[0].family.as_deref(), Some("put_credit"));
        assert_eq!(config.strategies[0].mode.as_deref(), Some("live"));
        assert_eq!(config.strategies[0].underlyings, vec!["SPY"]);
        assert_eq!(
            config.strategies[1].id.as_deref(),
            Some("iron_condor_qqq_watch")
        );
        assert_eq!(config.strategies[1].family.as_deref(), Some("iron_condor"));
        assert_eq!(config.strategies[1].mode.as_deref(), Some("dry_run"));
        assert_eq!(config.strategies[1].scanner.min_return_on_risk, Some(0.21));
        assert_eq!(config.strategies[1].risk.max_active_entries, Some(1));
        assert_eq!(config.strategies[1].risk.max_daily_submits, Some(1));
        assert_eq!(config.runtime.open_orders, Some(false));
        assert_eq!(config.runtime.close_orders, Some(true));
        assert_eq!(config.runtime.candidate_ledger_enabled, Some(true));
        assert_eq!(config.runtime.candidate_ledger_max_candidates, Some(7));
        assert_eq!(config.universe.quantity, Some(2));
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
            config.event_shock.scheduled_event_catalog_path,
            Some(PathBuf::from("/tmp/scheduled_events/catalog")),
        );
        assert_eq!(
            config.event_shock.earnings_events_path,
            Some(PathBuf::from("/tmp/earnings_events_approved.csv")),
        );
        assert_eq!(config.event_shock.require_earnings_events, Some(true));
        assert_eq!(config.event_shock.allow_csv_bridge, Some(true));
        assert_eq!(config.event_shock.stale_after_days, Some(2));
        assert_eq!(config.event_shock.horizon_days, Some(60));
        assert_eq!(config.event_shock.block_days_before_earnings, Some(3));
        assert_eq!(config.event_shock.block_days_after_earnings, Some(2));
    }

    fn build_strategy_profiles(
        strategies: Vec<StrategyBlockSection>,
        universe_groups: BTreeMap<String, Vec<String>>,
    ) -> anyhow::Result<Vec<AlpacaOptionsStrategyProfile>> {
        strategy_profiles_from_file(
            strategies,
            &universe_groups,
            1,
            &PutCreditScannerConfig::default(),
            &IronCondorScannerConfig::default(),
            &DebitSpreadScannerConfig::default(),
            &NakedOptionScannerConfig::default(),
            &NakedOptionScannerConfig::default(),
        )
    }

    fn strategy_block(
        id: impl Into<String>,
        family: impl Into<String>,
        mode: AlpacaOptionsStrategyMode,
        underlyings: impl IntoIterator<Item = impl Into<String>>,
    ) -> StrategyBlockSection {
        StrategyBlockSection {
            id: Some(id.into()),
            family: Some(family.into()),
            mode: Some(mode.as_str().to_string()),
            universe_groups: Vec::new(),
            underlyings: underlyings.into_iter().map(Into::into).collect(),
            include_underlyings: Vec::new(),
            exclude_underlyings: Vec::new(),
            quantity: Some(1),
            scanner: StrategyScannerSection::default(),
            risk: StrategyRiskOverrideSection::default(),
        }
    }

    fn test_strategy_profile(
        id: impl Into<String>,
        family: AlpacaOptionsStrategyFamily,
        mode: AlpacaOptionsStrategyMode,
        underlyings: impl IntoIterator<Item = impl Into<String>>,
    ) -> AlpacaOptionsStrategyProfile {
        AlpacaOptionsStrategyProfile {
            id: id.into(),
            family,
            mode,
            universe_groups: Vec::new(),
            include_underlyings: Vec::new(),
            exclude_underlyings: Vec::new(),
            underlyings: underlyings.into_iter().map(Into::into).collect(),
            quantity: 1,
            scanner: scanner_for_family(family),
            risk: AlpacaOptionsStrategyRiskOverrides::default(),
        }
    }

    fn scanner_for_family(
        family: AlpacaOptionsStrategyFamily,
    ) -> AlpacaOptionsStrategyScannerConfig {
        match family {
            AlpacaOptionsStrategyFamily::PutCredit | AlpacaOptionsStrategyFamily::CallCredit => {
                AlpacaOptionsStrategyScannerConfig::Credit(PutCreditScannerConfig::default())
            }
            AlpacaOptionsStrategyFamily::IronCondor => {
                AlpacaOptionsStrategyScannerConfig::IronCondor(IronCondorScannerConfig::default())
            }
            AlpacaOptionsStrategyFamily::PutDebit | AlpacaOptionsStrategyFamily::CallDebit => {
                AlpacaOptionsStrategyScannerConfig::Debit(DebitSpreadScannerConfig::default())
            }
            AlpacaOptionsStrategyFamily::NakedPut
            | AlpacaOptionsStrategyFamily::NakedCall
            | AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte
            | AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte => {
                AlpacaOptionsStrategyScannerConfig::Naked(NakedOptionScannerConfig::default())
            }
        }
    }

    fn minimal_policy_config(
        strategy_profiles: Vec<AlpacaOptionsStrategyProfile>,
    ) -> AlpacaOptionsRuntimeConfig {
        AlpacaOptionsRuntimeConfig {
            underlyings: Vec::new(),
            universe_groups: BTreeMap::new(),
            strategy_profiles,
            max_active_entries: None,
            max_daily_submits: None,
            max_open_orders: None,
            max_active_entries_per_underlying: None,
            max_active_entries_per_sector: None,
            max_single_entry_risk_capital_usd: None,
            max_portfolio_risk_capital_usd: None,
            block_unestimated_risk_capital: true,
            event_shock_earnings_events: Vec::new(),
            event_shock: EventShockRuntimeStatus {
                source: "test".to_string(),
                scheduled_event_catalog_path: PathBuf::new(),
                event_count: 0,
                catalog_event_count: 0,
                source_set: Vec::new(),
                policy_versions: Vec::new(),
                coverage_start: None,
                coverage_end: None,
                freshness: "unavailable".to_string(),
                unavailable_reason: None,
                rejected_count: 0,
                csv_bridge_enabled: false,
                csv_bridge_path: None,
                dry_run_only: false,
                required: false,
            },
            event_shock_block_days_before_earnings: 1,
            event_shock_block_days_after_earnings: 1,
            sectors: BTreeMap::new(),
            max_iterations: 1,
            interval_secs: 300,
            quantity: 1,
            open_orders_enabled: true,
            force_flatten: false,
            cancel_after_accept: false,
            stale_entry_secs: 900,
            stale_close_secs: 900,
            close_orders_enabled: false,
            close_regular_hours_only: true,
            close_start: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            close_end: NaiveTime::from_hms_opt(16, 0, 0).unwrap(),
            close_price_cushion: 0.0,
            close_reprice_step: 0.0,
            max_close_price_cushion: 0.0,
            max_close_attempts: 3,
            close_reprice_cooldown_secs: 30,
            active_risk_candidate_quote_limit: 5,
            active_risk_quote_stale_secs: 30,
            profit_target_close_fraction: 0.5,
            stop_loss_close_multiple: 2.0,
            max_hold_secs: 0,
            expiration_exit_days: 1,
            lifecycle_poll_secs: 300,
            lifecycle_activity_lookback_hours: 72,
            lifecycle_activity_block_hours: 24,
            expiration_entry_block_days: 0,
            ignore_entry_window: false,
            entry_start: NaiveTime::from_hms_opt(9, 45, 0).unwrap(),
            entry_end: NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            entry_timezone: "America/New_York".parse().unwrap(),
            state_path: PathBuf::new(),
            candidate_ledger_enabled: true,
            candidate_ledger_max_candidates: 10,
            scanner: PutCreditScannerConfig::default(),
            iron_condor_scanner: IronCondorScannerConfig::default(),
            debit_scanner: DebitSpreadScannerConfig::default(),
            naked_scanner: NakedOptionScannerConfig::default(),
            naked_1_3dte_scanner: NakedOptionScannerConfig::default(),
            fleet: None,
            fleet_account_id: None,
            fleet_policy_blocks: Vec::new(),
            operational_repository: None,
            operational_database_url: None,
            operational_schema: OPERATIONAL_SCHEMA_DEFAULT.to_string(),
            operational_account_id: None,
        }
    }
}
