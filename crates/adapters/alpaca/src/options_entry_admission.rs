//! Entry-admission gates shared by Alpaca options runtimes.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;

use crate::{
    earnings::EarningsEvent,
    options_runtime::{
        AlpacaOptionsRuntimeConfig, SelectedOptionsEntry, active_sector_count,
        active_underlying_count,
    },
    runtime::StrategyState,
};

/// Terminal close reason recorded when Alpaca rejects uncovered naked-option permissions.
pub const UNCOVERED_OPTION_PERMISSION_REJECTION_REASON: &str =
    "entry_rejected_uncovered_option_permission";

/// Action mode for a selected entry candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryMode {
    /// Submit the entry to the broker.
    Submit,
    /// Record the candidate without broker submission.
    DryRun,
}

impl EntryMode {
    /// Returns the stable operator action label.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::DryRun => "dry_run",
        }
    }

    /// Returns whether this mode allows broker submission.
    #[must_use]
    pub const fn is_submit(self) -> bool {
        matches!(self, Self::Submit)
    }
}

/// Account-level entry gate decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryGateDecision {
    /// New entries may continue through candidate admission.
    Continue,
    /// New entries are blocked by the kill switch.
    KillSwitch,
    /// New entries are blocked outside the entry window.
    OutsideEntryWindow,
}

/// Candidate submit block with stable operator diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmissionBlock {
    /// Stable block reason.
    pub reason: String,
    /// Current observed count, if the block is count based.
    pub current: Option<usize>,
    /// Configured limit, if the block is count based.
    pub limit: Option<usize>,
    /// Additional diagnostic details.
    pub details: Vec<String>,
}

/// Live runtime snapshot used by entry admission.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EntryAdmissionSnapshot {
    /// Current open order count from broker/runtime state.
    pub open_order_count: usize,
    /// Broker/runtime admission reasons for the selected candidate.
    pub broker_admission_reasons: Vec<String>,
}

/// Strategy-owned entry admission configuration derived from [`AlpacaOptionsRuntimeConfig`].
#[derive(Clone, Debug)]
pub struct EntryAdmissionConfig {
    /// Whether live submission is globally enabled.
    pub submit_enabled: bool,
    /// Strategy names that are intentionally scanned but not submitted.
    pub dry_run_strategy_family_names: BTreeMap<String, ()>,
    /// Whether new entries are blocked.
    pub kill_switch: bool,
    /// Whether the entry window should be ignored.
    pub ignore_entry_window: bool,
    /// Entry window start.
    pub entry_start: NaiveTime,
    /// Entry window end.
    pub entry_end: NaiveTime,
    /// Entry window timezone.
    pub entry_timezone: Tz,
    /// Maximum active strategy entries.
    pub max_active_entries: Option<usize>,
    /// Maximum accepted strategy submissions for one trade date.
    pub max_daily_submits: Option<usize>,
    /// Maximum working broker orders before new entries are blocked.
    pub max_open_orders: Option<usize>,
    /// Maximum active entries for one underlying.
    pub max_active_entries_per_underlying: Option<usize>,
    /// Maximum active entries for one sector/correlation group.
    pub max_active_entries_per_sector: Option<usize>,
    /// Strategy quantity for selected-entry risk-capital estimates.
    pub quantity: u64,
    /// Maximum risk-capital estimate for one selected entry in USD.
    pub max_single_entry_risk_capital_usd: Option<f64>,
    /// Maximum active plus selected portfolio risk-capital estimate in USD.
    pub max_portfolio_risk_capital_usd: Option<f64>,
    /// Whether configured risk-capital limits block entries when risk cannot be estimated.
    pub block_unestimated_risk_capital: bool,
    /// Approved earnings events used by the event-shock admission guard.
    pub event_shock_earnings_events: Vec<EarningsEvent>,
    /// Calendar days before an earnings report to block new entries.
    pub event_shock_block_days_before_earnings: i64,
    /// Calendar days after an earnings report to block new entries.
    pub event_shock_block_days_after_earnings: i64,
    /// Underlying to sector/correlation-group mapping.
    pub sectors: BTreeMap<String, String>,
    /// Fleet-wide maximum active entries.
    pub fleet_max_active_entries: Option<usize>,
    /// Fleet-wide current active entries.
    pub fleet_active_entries: usize,
    /// Fleet maximum active entries for one underlying.
    pub fleet_max_active_entries_per_underlying: Option<usize>,
    /// Fleet current active entries by underlying.
    pub fleet_active_entries_by_underlying: BTreeMap<String, usize>,
    /// Fleet maximum active entries for one sector/correlation group.
    pub fleet_max_active_entries_per_sector: Option<usize>,
    /// Fleet underlying to sector/correlation-group mapping.
    pub fleet_sectors: BTreeMap<String, String>,
    /// Fleet current active entries by sector/correlation group.
    pub fleet_active_entries_by_sector: BTreeMap<String, usize>,
    /// Broker account-level reasons captured before live submit is enabled.
    pub account_admission_reasons: Vec<String>,
}

impl EntryAdmissionConfig {
    /// Builds entry-admission config from the Alpaca options runtime config.
    #[must_use]
    pub fn from_runtime_config(engine: &AlpacaOptionsRuntimeConfig) -> Self {
        let fleet_exposure = engine.fleet.as_ref().map(|fleet| fleet.exposure());
        let fleet_section = engine.fleet.as_ref().map(|fleet| &fleet.config.fleet);
        Self {
            submit_enabled: engine.submit_enabled,
            dry_run_strategy_family_names: engine
                .dry_run_strategy_family_names()
                .into_iter()
                .map(|name| (name.to_string(), ()))
                .collect(),
            kill_switch: engine.kill_switch,
            ignore_entry_window: engine.ignore_entry_window,
            entry_start: engine.entry_start,
            entry_end: engine.entry_end,
            entry_timezone: engine.entry_timezone,
            max_active_entries: engine.max_active_entries,
            max_daily_submits: engine.max_daily_submits,
            max_open_orders: engine.max_open_orders,
            max_active_entries_per_underlying: engine.max_active_entries_per_underlying,
            max_active_entries_per_sector: engine.max_active_entries_per_sector,
            quantity: engine.quantity,
            max_single_entry_risk_capital_usd: engine.max_single_entry_risk_capital_usd,
            max_portfolio_risk_capital_usd: engine.max_portfolio_risk_capital_usd,
            block_unestimated_risk_capital: engine.block_unestimated_risk_capital,
            event_shock_earnings_events: engine.event_shock_earnings_events.clone(),
            event_shock_block_days_before_earnings: engine.event_shock_block_days_before_earnings,
            event_shock_block_days_after_earnings: engine.event_shock_block_days_after_earnings,
            sectors: engine.sectors.clone(),
            fleet_max_active_entries: fleet_section.and_then(|fleet| fleet.max_active_entries),
            fleet_active_entries: fleet_exposure
                .as_ref()
                .map_or(0, |exposure| exposure.active_entries),
            fleet_max_active_entries_per_underlying: fleet_section
                .and_then(|fleet| fleet.max_active_entries_per_underlying),
            fleet_active_entries_by_underlying: fleet_exposure
                .as_ref()
                .map_or_else(BTreeMap::new, |exposure| {
                    exposure.active_entries_by_underlying.clone()
                }),
            fleet_max_active_entries_per_sector: fleet_section
                .and_then(|fleet| fleet.max_active_entries_per_sector),
            fleet_sectors: fleet_section.map_or_else(BTreeMap::new, |fleet| fleet.sectors.clone()),
            fleet_active_entries_by_sector: fleet_exposure
                .as_ref()
                .map_or_else(BTreeMap::new, |exposure| {
                    exposure.active_entries_by_sector.clone()
                }),
            account_admission_reasons: Vec::new(),
        }
    }

    /// Returns the current market trade date in the configured entry timezone.
    #[must_use]
    pub fn market_trade_date(&self) -> String {
        Utc::now()
            .with_timezone(&self.entry_timezone)
            .date_naive()
            .to_string()
    }
}

impl Default for EntryAdmissionConfig {
    fn default() -> Self {
        Self {
            submit_enabled: true,
            dry_run_strategy_family_names: BTreeMap::new(),
            kill_switch: false,
            ignore_entry_window: true,
            entry_start: NaiveTime::MIN,
            entry_end: NaiveTime::from_hms_opt(23, 59, 59).expect("valid terminal day time"),
            entry_timezone: chrono_tz::UTC,
            max_active_entries: None,
            max_daily_submits: None,
            max_open_orders: None,
            max_active_entries_per_underlying: None,
            max_active_entries_per_sector: None,
            quantity: 1,
            max_single_entry_risk_capital_usd: None,
            max_portfolio_risk_capital_usd: None,
            block_unestimated_risk_capital: true,
            event_shock_earnings_events: Vec::new(),
            event_shock_block_days_before_earnings: 1,
            event_shock_block_days_after_earnings: 1,
            sectors: BTreeMap::new(),
            fleet_max_active_entries: None,
            fleet_active_entries: 0,
            fleet_max_active_entries_per_underlying: None,
            fleet_active_entries_by_underlying: BTreeMap::new(),
            fleet_max_active_entries_per_sector: None,
            fleet_sectors: BTreeMap::new(),
            fleet_active_entries_by_sector: BTreeMap::new(),
            account_admission_reasons: Vec::new(),
        }
    }
}

/// Returns whether a selected entry may submit live orders under runtime dry-run gates.
#[must_use]
pub fn selected_submit_enabled(
    config: &EntryAdmissionConfig,
    selected: &SelectedOptionsEntry,
) -> bool {
    config.submit_enabled
        && !config
            .dry_run_strategy_family_names
            .contains_key(selected.strategy_name())
}

/// Returns the entry action mode for a selected entry.
#[must_use]
pub fn selected_entry_mode(
    config: &EntryAdmissionConfig,
    selected: &SelectedOptionsEntry,
) -> EntryMode {
    if selected_submit_enabled(config, selected) {
        EntryMode::Submit
    } else {
        EntryMode::DryRun
    }
}

/// Returns the account-level entry gate decision for `now`.
#[must_use]
pub fn entry_gate_decision(config: &EntryAdmissionConfig, now: DateTime<Utc>) -> EntryGateDecision {
    if config.kill_switch {
        EntryGateDecision::KillSwitch
    } else if !config.ignore_entry_window && !inside_entry_window_at(config, now) {
        EntryGateDecision::OutsideEntryWindow
    } else {
        EntryGateDecision::Continue
    }
}

/// Returns a selected-entry submission block, if admission should prevent broker submission.
#[must_use]
pub fn submission_block_for_selected(
    config: &EntryAdmissionConfig,
    state: &StrategyState,
    selected: &SelectedOptionsEntry,
    trade_date: &str,
    snapshot: &EntryAdmissionSnapshot,
) -> Option<SubmissionBlock> {
    broker_permission_block_for_selected(state, selected)
        .or_else(|| risk_gate_decision(config, state, trade_date, snapshot).into_submission_block())
        .or_else(|| event_shock_block(config, trade_date, selected.underlying()))
        .or_else(|| portfolio_risk_capital_block(config, state, selected))
        .or_else(|| per_underlying_block(config, state, selected.underlying()))
        .or_else(|| per_sector_block(config, state, selected.underlying()))
        .or_else(|| fleet_underlying_limit_block(config, selected.underlying()))
        .or_else(|| fleet_sector_limit_block(config, selected.underlying()))
        .or_else(|| daily_duplicate_block(state, trade_date, selected.underlying()))
        .or_else(|| broker_admission_block(&snapshot.broker_admission_reasons))
}

/// Returns whether an Alpaca rejection reason is the uncovered-option permission rejection.
#[must_use]
pub fn is_uncovered_option_permission_rejection(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    reason.contains("40310000")
        && reason.contains("not eligible to trade uncovered option contracts")
}

/// Classifies detailed broker/runtime admission reasons into a stable block reason.
#[must_use]
pub fn admission_block_reason(reasons: &[String]) -> &'static str {
    if reasons.iter().any(|reason| {
        matches!(
            reason.as_str(),
            "account trading_blocked is true"
                | "account_blocked is true"
                | "trade_suspended_by_user is true"
        ) || reason.starts_with("account status is ")
    }) {
        "account_not_tradable"
    } else if reasons.iter().any(|reason| {
        reason.starts_with("options_trading_level_")
            || reason.starts_with("options_approved_level_")
            || reason.starts_with("max_options_trading_level_")
            || reason == "options_buying_power_non_positive"
    }) {
        "options_level_insufficient"
    } else if reasons.iter().any(|reason| {
        reason.starts_with("lifecycle_activity_poll_failed")
            || reason.starts_with("assignment activity ")
            || reason.starts_with("exercise activity ")
    }) {
        "account_lifecycle_event"
    } else if reasons
        .iter()
        .any(|reason| reason == "candidate option symbols must resolve to one underlying")
    {
        "invalid_candidate_symbols"
    } else if reasons
        .iter()
        .any(|reason| reason.starts_with("existing open position on candidate leg "))
    {
        "existing_candidate_leg_position"
    } else if reasons
        .iter()
        .any(|reason| reason.starts_with("existing open option position on underlying "))
    {
        "existing_underlying_position"
    } else if reasons
        .iter()
        .any(|reason| reason.starts_with("working order already references candidate leg "))
    {
        "working_candidate_leg_order"
    } else if reasons
        .iter()
        .any(|reason| reason.starts_with("working order already references underlying "))
    {
        "working_underlying_order"
    } else {
        "broker_admission_rejected"
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RiskGateDecision {
    Continue,
    MaxActiveEntries { current: usize, limit: usize },
    MaxDailySubmits { current: usize, limit: usize },
    MaxOpenOrders { current: usize, limit: usize },
    FleetMaxActiveEntries { current: usize, limit: usize },
}

impl RiskGateDecision {
    fn into_submission_block(self) -> Option<SubmissionBlock> {
        match self {
            Self::Continue => None,
            Self::MaxActiveEntries { current, limit } => Some(SubmissionBlock {
                reason: "risk_max_active_entries".to_string(),
                current: Some(current),
                limit: Some(limit),
                details: Vec::new(),
            }),
            Self::MaxDailySubmits { current, limit } => Some(SubmissionBlock {
                reason: "risk_max_daily_submits".to_string(),
                current: Some(current),
                limit: Some(limit),
                details: Vec::new(),
            }),
            Self::MaxOpenOrders { current, limit } => Some(SubmissionBlock {
                reason: "risk_max_open_orders".to_string(),
                current: Some(current),
                limit: Some(limit),
                details: Vec::new(),
            }),
            Self::FleetMaxActiveEntries { current, limit } => Some(SubmissionBlock {
                reason: "fleet_max_active_entries".to_string(),
                current: Some(current),
                limit: Some(limit),
                details: Vec::new(),
            }),
        }
    }
}

fn risk_gate_decision(
    config: &EntryAdmissionConfig,
    state: &StrategyState,
    trade_date: &str,
    snapshot: &EntryAdmissionSnapshot,
) -> RiskGateDecision {
    if let Some(limit) = config.max_active_entries {
        let current = state
            .entries
            .iter()
            .filter(|entry| entry.is_active())
            .count();
        if current >= limit {
            return RiskGateDecision::MaxActiveEntries { current, limit };
        }
    }

    if let Some(limit) = config.max_daily_submits {
        let current = state.risk_counted_daily_submits(trade_date);
        if current >= limit {
            return RiskGateDecision::MaxDailySubmits { current, limit };
        }
    }

    if let Some(limit) = config.max_open_orders {
        let current = snapshot.open_order_count;
        if current >= limit {
            return RiskGateDecision::MaxOpenOrders { current, limit };
        }
    }

    if let Some(limit) = config.fleet_max_active_entries {
        let current = config.fleet_active_entries;
        if current >= limit {
            return RiskGateDecision::FleetMaxActiveEntries { current, limit };
        }
    }

    RiskGateDecision::Continue
}

fn broker_permission_block_for_selected(
    state: &StrategyState,
    selected: &SelectedOptionsEntry,
) -> Option<SubmissionBlock> {
    if selected.is_naked_option()
        && state.has_canceled_naked_entry_with_close_reason(
            UNCOVERED_OPTION_PERMISSION_REJECTION_REASON,
        )
    {
        Some(SubmissionBlock {
            reason: "broker_uncovered_option_permission".to_string(),
            current: None,
            limit: None,
            details: vec!["alpaca_http_40310000".to_string()],
        })
    } else {
        None
    }
}

fn per_underlying_block(
    config: &EntryAdmissionConfig,
    state: &StrategyState,
    underlying: &str,
) -> Option<SubmissionBlock> {
    let limit = config.max_active_entries_per_underlying?;
    let current = active_underlying_count(state, underlying);
    (current >= limit).then(|| SubmissionBlock {
        reason: "risk_max_active_entries_per_underlying".to_string(),
        current: Some(current),
        limit: Some(limit),
        details: Vec::new(),
    })
}

fn portfolio_risk_capital_block(
    config: &EntryAdmissionConfig,
    state: &StrategyState,
    selected: &SelectedOptionsEntry,
) -> Option<SubmissionBlock> {
    let has_limit = config.max_single_entry_risk_capital_usd.is_some()
        || config.max_portfolio_risk_capital_usd.is_some();
    if !has_limit {
        return None;
    }

    let candidate_risk = selected.risk_capital_usd(config.quantity);
    let Some(candidate_risk) = candidate_risk else {
        return config
            .block_unestimated_risk_capital
            .then(|| SubmissionBlock {
                reason: "risk_capital_unestimated".to_string(),
                current: None,
                limit: None,
                details: vec![
                    "scope=selected_entry".to_string(),
                    format!("strategy={}", selected.strategy_name()),
                    format!("underlying={}", selected.underlying()),
                ],
            });
    };

    if let Some(limit) = config.max_single_entry_risk_capital_usd
        && candidate_risk > limit
    {
        return Some(SubmissionBlock {
            reason: "risk_max_single_entry_risk_capital".to_string(),
            current: None,
            limit: None,
            details: vec![
                format!("candidate_risk_capital_usd={candidate_risk:.2}"),
                format!("limit_usd={limit:.2}"),
            ],
        });
    }

    let Some(limit) = config.max_portfolio_risk_capital_usd else {
        return None;
    };

    let active = active_risk_capital(state);
    if active.unknown_count > 0 && config.block_unestimated_risk_capital {
        return Some(SubmissionBlock {
            reason: "risk_capital_unestimated".to_string(),
            current: None,
            limit: None,
            details: vec![
                "scope=active_entries".to_string(),
                format!("unknown_active_entries={}", active.unknown_count),
                format!("known_active_risk_capital_usd={:.2}", active.known_usd),
                format!("limit_usd={limit:.2}"),
            ],
        });
    }

    let projected = active.known_usd + candidate_risk;
    (projected > limit).then(|| SubmissionBlock {
        reason: "risk_max_portfolio_risk_capital".to_string(),
        current: None,
        limit: None,
        details: vec![
            format!("active_risk_capital_usd={:.2}", active.known_usd),
            format!("candidate_risk_capital_usd={candidate_risk:.2}"),
            format!("projected_risk_capital_usd={projected:.2}"),
            format!("limit_usd={limit:.2}"),
        ],
    })
}

fn event_shock_block(
    config: &EntryAdmissionConfig,
    trade_date: &str,
    underlying: &str,
) -> Option<SubmissionBlock> {
    if config.event_shock_earnings_events.is_empty() {
        return None;
    }

    let trade_date = NaiveDate::parse_from_str(trade_date, "%Y-%m-%d").ok()?;
    let underlying = underlying.to_ascii_uppercase();
    config
        .event_shock_earnings_events
        .iter()
        .filter(|event| event.underlying == underlying)
        .find_map(|event| {
            let days_to_report = event
                .report_date
                .signed_duration_since(trade_date)
                .num_days();
            (-config.event_shock_block_days_after_earnings <= days_to_report
                && days_to_report <= config.event_shock_block_days_before_earnings)
                .then(|| SubmissionBlock {
                    reason: "event_shock_earnings".to_string(),
                    current: None,
                    limit: None,
                    details: vec![
                        format!("underlying={}", event.underlying),
                        format!("report_date={}", event.report_date),
                        format!("timing={}", event.timing.as_str()),
                        format!("source={}", event.source),
                        format!("days_to_report={days_to_report}"),
                    ],
                })
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ActiveRiskCapital {
    known_usd: f64,
    unknown_count: usize,
}

fn active_risk_capital(state: &StrategyState) -> ActiveRiskCapital {
    state.entries.iter().filter(|entry| entry.is_active()).fold(
        ActiveRiskCapital::default(),
        |mut active, entry| {
            if let Some(value) = entry.risk_capital_usd_estimate() {
                active.known_usd += value;
            } else {
                active.unknown_count = active.unknown_count.saturating_add(1);
            }
            active
        },
    )
}

fn per_sector_block(
    config: &EntryAdmissionConfig,
    state: &StrategyState,
    underlying: &str,
) -> Option<SubmissionBlock> {
    let limit = config.max_active_entries_per_sector?;
    let sector = config.sectors.get(&underlying.to_ascii_uppercase())?;
    let current = active_sector_count(state, &config.sectors, sector);
    (current >= limit).then(|| SubmissionBlock {
        reason: "risk_max_active_entries_per_sector".to_string(),
        current: Some(current),
        limit: Some(limit),
        details: vec![format!("sector={sector}")],
    })
}

fn fleet_underlying_limit_block(
    config: &EntryAdmissionConfig,
    underlying: &str,
) -> Option<SubmissionBlock> {
    let limit = config.fleet_max_active_entries_per_underlying?;
    let current = config
        .fleet_active_entries_by_underlying
        .get(&underlying.to_ascii_uppercase())
        .copied()
        .unwrap_or(0);
    (current >= limit).then(|| SubmissionBlock {
        reason: "fleet_max_active_entries_per_underlying".to_string(),
        current: Some(current),
        limit: Some(limit),
        details: Vec::new(),
    })
}

fn fleet_sector_limit_block(
    config: &EntryAdmissionConfig,
    underlying: &str,
) -> Option<SubmissionBlock> {
    let limit = config.fleet_max_active_entries_per_sector?;
    let sector = config
        .fleet_sectors
        .get(&underlying.to_ascii_uppercase())?
        .clone();
    let current = config
        .fleet_active_entries_by_sector
        .get(&sector)
        .copied()
        .unwrap_or(0);
    (current >= limit).then(|| SubmissionBlock {
        reason: "fleet_max_active_entries_per_sector".to_string(),
        current: Some(current),
        limit: Some(limit),
        details: vec![format!("sector={sector}")],
    })
}

fn daily_duplicate_block(
    state: &StrategyState,
    trade_date: &str,
    underlying: &str,
) -> Option<SubmissionBlock> {
    state
        .has_risk_counted_submitted_underlying_today(trade_date, underlying)
        .then(|| SubmissionBlock {
            reason: "daily_duplicate_state".to_string(),
            current: None,
            limit: None,
            details: vec!["scope=same_day_underlying_reentry".to_string()],
        })
}

fn broker_admission_block(reasons: &[String]) -> Option<SubmissionBlock> {
    (!reasons.is_empty()).then(|| SubmissionBlock {
        reason: admission_block_reason(reasons).to_string(),
        current: None,
        limit: None,
        details: reasons.to_vec(),
    })
}

fn inside_entry_window_at(config: &EntryAdmissionConfig, now: DateTime<Utc>) -> bool {
    let now = now.with_timezone(&config.entry_timezone).time();
    config.entry_start <= now && now <= config.entry_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        candidate_engine::{ScoredContract, SpreadCandidate},
        earnings::{EarningsEvent, EarningsTiming},
        options_entry::{SelectedEntry, SelectedOptionsEntry},
        runtime::{StrategyStateEntryDraft, credit_spread_strategy_name},
    };

    #[test]
    fn portfolio_risk_capital_blocks_projected_excess() {
        let config = EntryAdmissionConfig {
            quantity: 1,
            max_portfolio_risk_capital_usd: Some(600.0),
            ..EntryAdmissionConfig::default()
        };
        let mut state = StrategyState::default();
        state.record_entry_submission(active_draft("QQQ", Some(300.0)));
        let selected = selected_credit_entry("SPY", 3.50);

        let block = submission_block_for_selected(
            &config,
            &state,
            &selected,
            "2026-05-04",
            &EntryAdmissionSnapshot::default(),
        )
        .expect("projected risk capital should block");

        assert_eq!(block.reason, "risk_max_portfolio_risk_capital");
        assert!(
            block
                .details
                .contains(&"active_risk_capital_usd=300.00".to_string())
        );
        assert!(
            block
                .details
                .contains(&"candidate_risk_capital_usd=350.00".to_string())
        );
        assert!(
            block
                .details
                .contains(&"projected_risk_capital_usd=650.00".to_string())
        );
    }

    #[test]
    fn single_entry_risk_capital_blocks_large_candidate() {
        let config = EntryAdmissionConfig {
            quantity: 2,
            max_single_entry_risk_capital_usd: Some(400.0),
            ..EntryAdmissionConfig::default()
        };
        let state = StrategyState::default();
        let selected = selected_credit_entry("SPY", 2.50);

        let block = submission_block_for_selected(
            &config,
            &state,
            &selected,
            "2026-05-04",
            &EntryAdmissionSnapshot::default(),
        )
        .expect("single-entry risk capital should block");

        assert_eq!(block.reason, "risk_max_single_entry_risk_capital");
        assert!(
            block
                .details
                .contains(&"candidate_risk_capital_usd=500.00".to_string())
        );
    }

    #[test]
    fn configured_portfolio_cap_blocks_unknown_active_risk() {
        let config = EntryAdmissionConfig {
            quantity: 1,
            max_portfolio_risk_capital_usd: Some(1_000.0),
            block_unestimated_risk_capital: true,
            ..EntryAdmissionConfig::default()
        };
        let mut state = StrategyState::default();
        let mut draft = active_draft("QQQ", None);
        draft.strategy = "naked_call".to_string();
        draft.short_symbol = "QQQ260515C00430000".to_string();
        draft.long_symbol = String::new();
        state.record_entry_submission(draft);
        let selected = selected_credit_entry("SPY", 1.00);

        let block = submission_block_for_selected(
            &config,
            &state,
            &selected,
            "2026-05-04",
            &EntryAdmissionSnapshot::default(),
        )
        .expect("unknown active risk capital should block when configured");

        assert_eq!(block.reason, "risk_capital_unestimated");
        assert!(block.details.contains(&"scope=active_entries".to_string()));
        assert!(
            block
                .details
                .contains(&"unknown_active_entries=1".to_string())
        );
    }

    #[test]
    fn configured_earnings_event_blocks_selected_underlying() {
        let config = EntryAdmissionConfig {
            event_shock_earnings_events: vec![EarningsEvent {
                underlying: "SPY".to_string(),
                report_date: NaiveDate::from_ymd_opt(2026, 5, 5).unwrap(),
                timing: EarningsTiming::AfterClose,
                source: "approved_csv".to_string(),
            }],
            event_shock_block_days_before_earnings: 2,
            event_shock_block_days_after_earnings: 1,
            ..EntryAdmissionConfig::default()
        };
        let state = StrategyState::default();
        let selected = selected_credit_entry("SPY", 1.00);

        let block = submission_block_for_selected(
            &config,
            &state,
            &selected,
            "2026-05-04",
            &EntryAdmissionSnapshot::default(),
        )
        .expect("configured earnings event should block");

        assert_eq!(block.reason, "event_shock_earnings");
        assert!(
            block
                .details
                .contains(&"report_date=2026-05-05".to_string())
        );
        assert!(block.details.contains(&"timing=after_close".to_string()));
        assert!(block.details.contains(&"days_to_report=1".to_string()));
    }

    fn selected_credit_entry(underlying: &str, max_loss: f64) -> SelectedOptionsEntry {
        SelectedOptionsEntry::Credit(SelectedEntry {
            underlying: underlying.to_string(),
            kind: crate::candidate_engine::CreditSpreadKind::Put,
            candidate: SpreadCandidate {
                short: scored_contract(&format!("{underlying}260515P00400000"), 400.0),
                long: scored_contract(&format!("{underlying}260515P00395000"), 395.0),
                width: 5.0,
                credit: 5.0 - max_loss,
                max_loss,
                return_on_risk: 0.25,
                score: 70.0,
            },
        })
    }

    fn active_draft(underlying: &str, risk_capital_usd: Option<f64>) -> StrategyStateEntryDraft {
        StrategyStateEntryDraft {
            trade_date: "2026-05-03".to_string(),
            underlying: underlying.to_string(),
            strategy: credit_spread_strategy_name(crate::candidate_engine::CreditSpreadKind::Put)
                .to_string(),
            order_list_id: format!("{underlying}-entry"),
            short_symbol: format!("{underlying}260515P00400000"),
            long_symbol: format!("{underlying}260515P00395000"),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: 1.00,
            debit: None,
            risk_capital_usd,
            score: 70.0,
            parent_order_id: Some(format!("{underlying}-parent")),
            submitted_at_utc: Some("2026-05-03T14:00:00Z".to_string()),
        }
    }

    fn scored_contract(symbol: &str, strike: f64) -> ScoredContract {
        ScoredContract {
            symbol: symbol.to_string(),
            expiration_date: "2026-05-15".to_string(),
            dte: 11,
            strike,
            bid: 1.0,
            ask: 1.1,
            delta_abs: 0.20,
            spread_pct: 0.05,
            bid_size: 10,
            ask_size: 10,
            volume: 100,
            open_interest: 1_000,
            implied_volatility: Some(0.2),
            metrics: None,
        }
    }
}
