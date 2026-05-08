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

//! Account-engine runtime for the Alpaca options-engine strategy slice.
//!
//! This module owns the process loop and broker orchestration used by the installed
//! `alpaca-options-engine` binary. The binary stays as a thin entrypoint so the live
//! runtime can be tested and evolved from library code.

use std::{
    cell::RefCell, collections::BTreeSet, env, future::Future, path::PathBuf, pin::Pin, rc::Rc,
    str::FromStr, time::Duration,
};

use crate::{
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    execution::{AlpacaExecutionClient, check_option_spread_entry_admission},
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{
            AlpacaOrder, AlpacaPosition, ListActivitiesRequest, ListOrdersRequest,
            OptionSnapshotsRequest,
        },
    },
    management::{credit_spread_close_reason, days_to_expiration, recorded_age_secs},
    options_runtime::{
        OptionsEngineConfig, SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry,
        SelectedNakedOptionEntry, SelectedOptionsEntry, active_sector_count,
        active_underlying_count, candidate_alert_identity_key, candidate_alert_key,
        credit_candidate_ledger_payload, debit_candidate_ledger_payload,
        fleet_active_underlying_count, fleet_has_active_underlying_elsewhere,
        fleet_sector_limit_state, iron_condor_candidate_ledger_payload,
        naked_candidate_ledger_payload, select_options_entry,
    },
    performance::{
        EntryOrderIds, append_performance_ledger_record, collect_order_ids,
        default_performance_ledger_dir, entry_performance,
    },
    runtime::{
        StrategyState, StrategyStateEntry, StrategyStateEntryDraft, credit_spread_strategy_name,
        debit_spread_strategy_name, emit_operator_event, load_strategy_state,
        naked_option_strategy_name, save_strategy_state_atomic,
    },
    strategy::{CreditSpreadKind, DebitSpreadCandidate, IronCondorCandidate, NakedOptionCandidate},
    submit::{
        MlegSubmitLeg, MlegSubmitOrderListRequest, SimpleSubmitOrderRequest,
        build_mleg_submit_order_list, build_simple_submit_order,
    },
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{ExecutionEvent, execution::SubmitOrderList},
};
use nautilus_core::{UUID4, time::get_atomic_clock_realtime};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType, OrderSide},
    events::OrderEventAny,
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId, Venue,
    },
    types::{Price, Quantity},
};
use serde_json::{Value, json};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, timeout},
};

const DEFAULT_EVENT_TIMEOUT_SECS: u64 = 20;
const STRATEGY_FAMILY: &str = "ALPACA-OPTIONS-ENGINE";
const SELECTED_CANDIDATE_ALERT: &str = "selected_candidate";
const CANDIDATE_SUBMIT_REJECTED_ALERT: &str = "candidate_submit_rejected";

#[derive(Clone, Debug)]
struct SubmitOutcome {
    accepted: usize,
    rejected: usize,
    parent_order_id: Option<String>,
}

#[derive(Clone, Debug)]
struct SubmissionBlock {
    reason: String,
    current: Option<usize>,
    limit: Option<usize>,
    details: Vec<String>,
}

/// Strategy decision emitted into the Alpaca account engine.
#[derive(Clone, Debug)]
pub enum StrategyDecision {
    /// No broker action should be taken because an account-level gate blocked entries.
    Skip {
        /// Stable skip reason.
        reason: &'static str,
    },
    /// No eligible candidate was found.
    NoEntry,
    /// New entries are blocked by account-level risk caps.
    RiskBlocked {
        /// Stable risk reason.
        reason: &'static str,
        /// Current observed count.
        current: usize,
        /// Configured limit.
        limit: usize,
    },
    /// Candidate was selected by discovery, but submit admission blocked broker action.
    SelectedBlocked {
        /// Selected strategy candidate.
        entry: SelectedOptionsEntry,
        /// Stable block reason.
        reason: String,
        /// Current observed count, if the block is count based.
        current: Option<usize>,
        /// Configured limit, if the block is count based.
        limit: Option<usize>,
        /// Additional diagnostic details.
        details: Vec<String>,
    },
    /// Candidate was selected for either broker submission or dry-run recording.
    Selected {
        /// Selected strategy candidate.
        entry: SelectedOptionsEntry,
        /// Entry action mode.
        mode: EntryMode,
    },
}

/// Action mode for a selected entry candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryMode {
    /// Submit the entry to the broker.
    Submit,
    /// Record the candidate without broker submission.
    DryRun,
}

impl EntryMode {
    fn action(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::DryRun => "dry_run",
        }
    }

    fn is_submit(self) -> bool {
        self == Self::Submit
    }
}

/// Account-engine context passed to hosted strategies for one iteration.
#[derive(Debug)]
pub struct AccountEngineContext<'a> {
    client: &'a AlpacaHttpClient,
    data_config: &'a AlpacaDataClientConfig,
    config: &'a OptionsEngineConfig,
    state: &'a StrategyState,
    trade_date: &'a str,
}

impl<'a> AccountEngineContext<'a> {
    fn new(
        client: &'a AlpacaHttpClient,
        data_config: &'a AlpacaDataClientConfig,
        config: &'a OptionsEngineConfig,
        state: &'a StrategyState,
        trade_date: &'a str,
    ) -> Self {
        Self {
            client,
            data_config,
            config,
            state,
            trade_date,
        }
    }

    /// Returns the market trade date for this iteration.
    #[must_use]
    pub fn trade_date(&self) -> &str {
        self.trade_date
    }
}

/// Strategy interface hosted by the single Alpaca account engine.
pub trait StrategyRuntime {
    /// Stable strategy runtime name.
    fn name(&self) -> &'static str;

    /// Evaluates one strategy iteration and emits an account-engine decision.
    ///
    /// # Errors
    ///
    /// Returns an error if strategy evaluation needs broker data and broker I/O fails.
    fn evaluate<'a>(
        &'a self,
        context: AccountEngineContext<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<StrategyDecision>> + 'a>>;
}

/// Options strategy implementation hosted by the Alpaca account engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct OptionsRuntimeStrategy;

impl StrategyRuntime for OptionsRuntimeStrategy {
    fn name(&self) -> &'static str {
        "options_engine"
    }

    fn evaluate<'a>(
        &'a self,
        context: AccountEngineContext<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<StrategyDecision>> + 'a>> {
        Box::pin(async move {
            match entry_gate_decision(context.config, Utc::now()) {
                EntryGateDecision::KillSwitch => {
                    return Ok(StrategyDecision::Skip {
                        reason: "kill_switch_enabled",
                    });
                }
                EntryGateDecision::OutsideEntryWindow => {
                    return Ok(StrategyDecision::Skip {
                        reason: "outside_entry_window",
                    });
                }
                EntryGateDecision::Continue => {}
            }

            let selected = select_options_entry(
                context.client,
                context.data_config,
                context.config,
                context.state,
                context.trade_date,
            )
            .await?;
            let Some(selected) = selected else {
                return Ok(StrategyDecision::NoEntry);
            };
            if selected_submit_enabled(context.config, &selected)
                && let Some(block) = submission_block_for_selected(&context, &selected).await?
            {
                return Ok(StrategyDecision::SelectedBlocked {
                    entry: selected,
                    reason: block.reason,
                    current: block.current,
                    limit: block.limit,
                    details: block.details,
                });
            }
            Ok(selected_strategy_decision(context.config, selected))
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryGateDecision {
    Continue,
    KillSwitch,
    OutsideEntryWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RiskGateDecision {
    Continue,
    MaxActiveEntries { current: usize, limit: usize },
    MaxDailySubmits { current: usize, limit: usize },
    MaxOpenOrders { current: usize, limit: usize },
    FleetMaxActiveEntries { current: usize, limit: usize },
}

fn selected_submit_enabled(config: &OptionsEngineConfig, selected: &SelectedOptionsEntry) -> bool {
    match selected {
        SelectedOptionsEntry::Credit(entry) => config.credit_submit_enabled(entry.kind),
        SelectedOptionsEntry::IronCondor(_) => config.iron_condor_submit_enabled(),
        SelectedOptionsEntry::Debit(entry) => config.debit_submit_enabled(entry.kind),
        SelectedOptionsEntry::NakedOption(entry) => config.naked_submit_enabled(entry.kind),
    }
}

fn selected_strategy_decision(
    config: &OptionsEngineConfig,
    selected: SelectedOptionsEntry,
) -> StrategyDecision {
    let mode = if selected_submit_enabled(config, &selected) {
        EntryMode::Submit
    } else {
        EntryMode::DryRun
    };
    StrategyDecision::Selected {
        entry: selected,
        mode,
    }
}

async fn submission_block_for_selected(
    context: &AccountEngineContext<'_>,
    selected: &SelectedOptionsEntry,
) -> anyhow::Result<Option<SubmissionBlock>> {
    if let Some(block) = risk_gate_decision(context).await?.into_submission_block() {
        return Ok(Some(block));
    }

    let underlying = selected.underlying();
    if let Some(limit) = context.config.max_active_entries_per_underlying {
        let current = active_underlying_count(context.state, underlying);
        if current >= limit {
            return Ok(Some(SubmissionBlock {
                reason: "risk_max_active_entries_per_underlying".to_string(),
                current: Some(current),
                limit: Some(limit),
                details: Vec::new(),
            }));
        }
    }
    if let Some(limit) = context.config.max_active_entries_per_sector
        && let Some(sector) = context.config.sector_for(underlying)
    {
        let current = active_sector_count(context.state, &context.config.sectors, sector);
        if current >= limit {
            return Ok(Some(SubmissionBlock {
                reason: "risk_max_active_entries_per_sector".to_string(),
                current: Some(current),
                limit: Some(limit),
                details: vec![format!("sector={sector}")],
            }));
        }
    }
    if let Some(limit) = context
        .config
        .fleet
        .as_ref()
        .and_then(|fleet| fleet.config.fleet.max_active_entries_per_underlying)
    {
        let current = fleet_active_underlying_count(context.config, underlying);
        if current >= limit {
            return Ok(Some(SubmissionBlock {
                reason: "fleet_max_active_entries_per_underlying".to_string(),
                current: Some(current),
                limit: Some(limit),
                details: Vec::new(),
            }));
        }
    }
    if let Some((sector, current, limit)) = fleet_sector_limit_state(context.config, underlying)
        && current >= limit
    {
        return Ok(Some(SubmissionBlock {
            reason: "fleet_max_active_entries_per_sector".to_string(),
            current: Some(current),
            limit: Some(limit),
            details: vec![format!("sector={sector}")],
        }));
    }
    if context
        .state
        .has_risk_counted_submitted_underlying_today(context.trade_date, underlying)
    {
        return Ok(Some(SubmissionBlock {
            reason: "daily_duplicate_state".to_string(),
            current: None,
            limit: None,
            details: vec!["scope=same_day_underlying_reentry".to_string()],
        }));
    }
    if fleet_has_active_underlying_elsewhere(context.config, underlying) {
        return Ok(Some(SubmissionBlock {
            reason: "fleet_duplicate_underlying".to_string(),
            current: None,
            limit: None,
            details: Vec::new(),
        }));
    }

    let account = context.client.account().await?;
    let positions = context.client.positions().await?;
    let open_orders = context
        .client
        .orders(&ListOrdersRequest::open_nested())
        .await?;
    let symbols = selected.option_symbols();
    let admission =
        check_option_spread_entry_admission(&account, &positions, &open_orders, &symbols);
    if admission.allowed {
        return Ok(None);
    }
    Ok(Some(SubmissionBlock {
        reason: "broker_admission_rejected".to_string(),
        current: None,
        limit: None,
        details: admission.reasons,
    }))
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

/// Runs the Alpaca options-engine account engine until configured shutdown.
///
/// # Errors
///
/// Returns an error if configuration parsing, broker I/O, selection, submission, cancellation,
/// state persistence, or execution-client lifecycle operations fail.
pub async fn run_options_engine() -> anyhow::Result<()> {
    let config = OptionsEngineConfig::from_env()?;
    let mut state = load_strategy_state(&config.state_path)?;
    let strategy = OptionsRuntimeStrategy;

    println!(
        "options_engine_entry: underlyings={} strategies={} submit_enabled={} manage_enabled={} close_enabled={} kill_switch={} quantity={} state_path={}",
        config.underlyings.join(","),
        config.enabled_strategy_names().join(","),
        config.submit_enabled,
        config.manage_enabled,
        config.close_enabled,
        config.kill_switch,
        config.quantity,
        config.state_path.display(),
    );
    emit_operator_event(
        "runner_start",
        json!({
            "underlyings": &config.underlyings,
            "strategies": config.enabled_strategy_names(),
            "dry_run_strategies": config.dry_run_strategy_names(),
            "submit_enabled": config.submit_enabled,
            "manage_enabled": config.manage_enabled,
            "close_enabled": config.close_enabled,
            "kill_switch": config.kill_switch,
            "quantity": config.quantity,
            "max_active_entries": config.max_active_entries,
            "max_daily_submits": config.max_daily_submits,
            "max_open_orders": config.max_open_orders,
            "max_active_entries_per_underlying": config.max_active_entries_per_underlying,
            "max_active_entries_per_sector": config.max_active_entries_per_sector,
            "sectors": &config.sectors,
            "fleet_account_id": &config.fleet_account_id,
            "fleet_policy_blocks": &config.fleet_policy_blocks,
            "stale_close_secs": config.stale_close_secs,
            "close_regular_hours_only": config.close_regular_hours_only,
            "close_start": config.close_start.to_string(),
            "close_end": config.close_end.to_string(),
            "close_price_cushion": config.close_price_cushion,
            "max_close_attempts": config.max_close_attempts,
            "close_reprice_cooldown_secs": config.close_reprice_cooldown_secs,
            "state_path": config.state_path.display().to_string(),
            "candidate_ledger_enabled": config.candidate_ledger_enabled,
            "candidate_ledger_dir": config.candidate_ledger_dir.display().to_string(),
            "candidate_ledger_max_candidates": config.candidate_ledger_max_candidates,
            "hosted_strategy": strategy.name(),
        }),
    );

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    let http_client = AlpacaHttpClient::from_data_config(&data_config)?;

    if reconcile_strategy_state(&http_client, &mut state).await? {
        save_strategy_state_atomic(&config.state_path, &state)?;
    }

    let mut iteration = 1_u64;
    loop {
        let trade_date = market_trade_date(&config);
        println!("strategy_iteration={iteration} trade_date={trade_date}");
        emit_operator_event(
            "strategy_iteration",
            json!({
                "iteration": iteration,
                "trade_date": trade_date,
            }),
        );
        record_scan_started(&config, &trade_date, iteration, strategy.name());

        if manage_existing_entries(&http_client, &data_config, &config, &mut state).await? {
            save_strategy_state_atomic(&config.state_path, &state)?;
        }

        let decision = strategy
            .evaluate(AccountEngineContext::new(
                &http_client,
                &data_config,
                &config,
                &state,
                &trade_date,
            ))
            .await?;
        if apply_strategy_decision(decision, &config, &mut state, &trade_date).await? {
            save_strategy_state_atomic(&config.state_path, &state)?;
        }

        if config.max_iterations != 0 && iteration >= config.max_iterations {
            break;
        }

        iteration = iteration.saturating_add(1);
        sleep(Duration::from_secs(config.interval_secs)).await;
    }

    Ok(())
}

fn record_scan_started(
    config: &OptionsEngineConfig,
    trade_date: &str,
    iteration: u64,
    hosted_strategy: &str,
) {
    config.record_candidate_ledger(
        trade_date,
        "scan_started",
        json!({
            "iteration": iteration,
            "hosted_strategy": hosted_strategy,
            "underlyings": &config.underlyings,
            "strategies": config.enabled_strategy_names(),
            "dry_run_strategies": config.dry_run_strategy_names(),
            "entry_window": {
                "start": config.entry_start.to_string(),
                "end": config.entry_end.to_string(),
                "timezone": config.entry_timezone.to_string(),
                "ignore": config.ignore_entry_window,
            },
        }),
    );
    config.record_candidate_ledger(
        trade_date,
        "threshold_snapshot",
        candidate_ledger_threshold_snapshot(config),
    );
}

fn candidate_ledger_threshold_snapshot(config: &OptionsEngineConfig) -> serde_json::Value {
    json!({
        "underlyings": &config.underlyings,
        "strategies": config.enabled_strategy_names(),
        "dry_run_strategies": config.dry_run_strategy_names(),
        "quantity": config.quantity,
        "submit_enabled": config.submit_enabled,
        "manage_enabled": config.manage_enabled,
        "close_enabled": config.close_enabled,
        "kill_switch": config.kill_switch,
        "risk": {
            "max_active_entries": config.max_active_entries,
            "max_daily_submits": config.max_daily_submits,
            "max_open_orders": config.max_open_orders,
            "max_active_entries_per_underlying": config.max_active_entries_per_underlying,
            "max_active_entries_per_sector": config.max_active_entries_per_sector,
            "sectors": &config.sectors,
        },
        "credit_scanner": {
            "min_dte": config.scanner.min_dte,
            "max_dte": config.scanner.max_dte,
            "short_delta_min": config.scanner.short_delta_min,
            "short_delta_max": config.scanner.short_delta_max,
            "widths": &config.scanner.widths,
            "min_open_interest": config.scanner.min_open_interest,
            "max_leg_spread_pct": config.scanner.max_leg_spread_pct,
            "min_return_on_risk": config.scanner.min_return_on_risk,
            "min_credit_to_width": config.scanner.min_credit_to_width,
        },
        "iron_condor_scanner": {
            "min_return_on_risk": config.iron_condor_scanner.min_return_on_risk,
            "require_equal_widths": config.iron_condor_scanner.require_equal_widths,
        },
        "debit_scanner": {
            "min_dte": config.debit_scanner.min_dte,
            "max_dte": config.debit_scanner.max_dte,
            "long_delta_min": config.debit_scanner.long_delta_min,
            "long_delta_max": config.debit_scanner.long_delta_max,
            "widths": &config.debit_scanner.widths,
            "min_open_interest": config.debit_scanner.min_open_interest,
            "max_leg_spread_pct": config.debit_scanner.max_leg_spread_pct,
            "max_debit_to_width": config.debit_scanner.max_debit_to_width,
            "min_debit_to_width": config.debit_scanner.min_debit_to_width,
            "min_reward_to_risk": config.debit_scanner.min_reward_to_risk,
        },
        "naked_scanner": naked_scanner_threshold_snapshot(&config.naked_scanner),
        "naked_1_3dte_scanner": naked_scanner_threshold_snapshot(&config.naked_1_3dte_scanner),
    })
}

fn naked_scanner_threshold_snapshot(
    scanner: &crate::strategy::NakedOptionScannerConfig,
) -> serde_json::Value {
    json!({
        "min_dte": scanner.min_dte,
        "max_dte": scanner.max_dte,
        "short_delta_min": scanner.short_delta_min,
        "short_delta_max": scanner.short_delta_max,
        "min_open_interest": scanner.min_open_interest,
        "max_spread_pct": scanner.max_spread_pct,
        "min_credit": scanner.min_credit,
        "min_bid_size": scanner.min_bid_size,
        "min_ask_size": scanner.min_ask_size,
        "min_daily_volume": scanner.min_daily_volume,
        "min_implied_volatility": scanner.min_implied_volatility,
        "max_implied_volatility": scanner.max_implied_volatility,
        "min_annualized_premium_yield": scanner.min_annualized_premium_yield,
        "max_buying_power_usage_pct": scanner.max_buying_power_usage_pct,
        "min_return_on_buying_power": scanner.min_return_on_buying_power,
        "min_breakeven_pop": scanner.min_breakeven_pop,
        "max_probability_of_touch": scanner.max_probability_of_touch,
        "min_distance_to_breakeven_pct": scanner.min_distance_to_breakeven_pct,
        "min_expected_move_coverage": scanner.min_expected_move_coverage,
        "min_score": scanner.min_score,
    })
}

fn record_decision_event(
    config: &OptionsEngineConfig,
    trade_date: &str,
    payload: serde_json::Value,
) {
    emit_operator_event("decision", payload.clone());
    config.record_candidate_ledger(trade_date, "decision", payload);
}

fn record_submit_result_event(
    config: &OptionsEngineConfig,
    trade_date: &str,
    payload: serde_json::Value,
) {
    emit_operator_event("submit_result", payload.clone());
    config.record_candidate_ledger(trade_date, "submit_result", payload);
}

fn record_selected_candidate_alert(
    config: &OptionsEngineConfig,
    trade_date: &str,
    identity_key: &str,
    payload: Value,
) {
    config.record_candidate_alert_ledger(
        trade_date,
        SELECTED_CANDIDATE_ALERT,
        "info",
        candidate_alert_key(SELECTED_CANDIDATE_ALERT, identity_key),
        payload,
    );
}

fn record_submit_rejected_candidate_alert(
    config: &OptionsEngineConfig,
    trade_date: &str,
    identity_key: &str,
    mut payload: Value,
    outcome: &SubmitOutcome,
    terminal_rejection_recorded: Option<bool>,
) {
    insert_value_field(&mut payload, "accepted", Value::from(outcome.accepted));
    insert_value_field(&mut payload, "rejected", Value::from(outcome.rejected));
    insert_value_field(
        &mut payload,
        "parent_order_id",
        outcome
            .parent_order_id
            .as_ref()
            .map_or(Value::Null, |value| Value::String(value.clone())),
    );
    if let Some(recorded) = terminal_rejection_recorded {
        insert_value_field(
            &mut payload,
            "terminal_rejection_recorded",
            Value::Bool(recorded),
        );
    }
    config.record_candidate_alert_ledger(
        trade_date,
        CANDIDATE_SUBMIT_REJECTED_ALERT,
        "warning",
        candidate_alert_key(CANDIDATE_SUBMIT_REJECTED_ALERT, identity_key),
        payload,
    );
}

fn selected_credit_alert_payload(
    entry: &SelectedEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let strategy = strategy_name(entry.kind);
    let identity_key = candidate_alert_identity_key(
        strategy,
        &entry.underlying,
        &[&entry.candidate.short.symbol, &entry.candidate.long.symbol],
    );
    let mut payload =
        credit_candidate_ledger_payload(&entry.underlying, strategy, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn selected_iron_condor_alert_payload(
    entry: &SelectedIronCondorEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let identity_key = candidate_alert_identity_key(
        "iron_condor",
        &entry.underlying,
        &[
            &entry.candidate.put.short.symbol,
            &entry.candidate.put.long.symbol,
            &entry.candidate.call.short.symbol,
            &entry.candidate.call.long.symbol,
        ],
    );
    let mut payload =
        iron_condor_candidate_ledger_payload(&entry.underlying, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn selected_debit_alert_payload(
    entry: &SelectedDebitEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let strategy = debit_spread_strategy_name(entry.kind);
    let identity_key = candidate_alert_identity_key(
        strategy,
        &entry.underlying,
        &[&entry.candidate.long.symbol, &entry.candidate.short.symbol],
    );
    let mut payload =
        debit_candidate_ledger_payload(&entry.underlying, strategy, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn selected_naked_alert_payload(
    entry: &SelectedNakedOptionEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let strategy = naked_option_strategy_name(entry.kind);
    let identity_key = candidate_alert_identity_key(
        strategy,
        &entry.underlying,
        &[&entry.candidate.short.symbol],
    );
    let mut payload =
        naked_candidate_ledger_payload(&entry.underlying, strategy, None, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn selected_entry_alert_payload(
    entry: &SelectedOptionsEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    match entry {
        SelectedOptionsEntry::Credit(entry) => {
            selected_credit_alert_payload(entry, trade_date, action, order_list_id)
        }
        SelectedOptionsEntry::IronCondor(entry) => {
            selected_iron_condor_alert_payload(entry, trade_date, action, order_list_id)
        }
        SelectedOptionsEntry::Debit(entry) => {
            selected_debit_alert_payload(entry, trade_date, action, order_list_id)
        }
        SelectedOptionsEntry::NakedOption(entry) => {
            selected_naked_alert_payload(entry, trade_date, action, order_list_id)
        }
    }
}

fn insert_selected_alert_fields(
    payload: &mut Value,
    identity_key: &str,
    action: &str,
    trade_date: &str,
    order_list_id: Option<&str>,
) {
    insert_string_field(payload, "candidate_identity_key", identity_key.to_string());
    insert_string_field(payload, "action", action.to_string());
    insert_string_field(payload, "trade_date", trade_date.to_string());
    if let Some(order_list_id) = order_list_id {
        insert_string_field(payload, "order_list_id", order_list_id.to_string());
    }
}

fn insert_string_field(payload: &mut Value, key: &str, value: String) {
    insert_value_field(payload, key, Value::String(value));
}

fn insert_value_field(payload: &mut Value, key: &str, value: Value) {
    if let Value::Object(fields) = payload {
        fields.insert(key.to_string(), value);
    }
}

async fn apply_strategy_decision(
    decision: StrategyDecision,
    config: &OptionsEngineConfig,
    state: &mut StrategyState,
    trade_date: &str,
) -> anyhow::Result<bool> {
    match decision {
        StrategyDecision::Skip {
            reason: "outside_entry_window",
        } => {
            println!(
                "decision: skipped reason=outside_entry_window window={}-{} timezone={}",
                config.entry_start, config.entry_end, config.entry_timezone
            );
            record_decision_event(
                config,
                trade_date,
                json!({
                    "action": "skipped",
                    "reason": "outside_entry_window",
                    "window_start": config.entry_start.to_string(),
                    "window_end": config.entry_end.to_string(),
                    "timezone": config.entry_timezone.to_string(),
                    "trade_date": trade_date,
                }),
            );
            Ok(false)
        }
        StrategyDecision::Skip { reason } => {
            println!("decision: skipped reason={reason}");
            record_decision_event(
                config,
                trade_date,
                json!({
                    "action": "skipped",
                    "reason": reason,
                    "trade_date": trade_date,
                }),
            );
            Ok(false)
        }
        StrategyDecision::RiskBlocked {
            reason,
            current,
            limit,
        } => {
            println!("decision: skipped reason={reason} current={current} limit={limit}");
            record_decision_event(
                config,
                trade_date,
                json!({
                    "action": "skipped",
                    "reason": reason,
                    "current": current,
                    "limit": limit,
                    "trade_date": trade_date,
                }),
            );
            Ok(false)
        }
        StrategyDecision::SelectedBlocked {
            entry,
            reason,
            current,
            limit,
            details,
        } => {
            let (candidate_identity_key, mut candidate_alert_payload) =
                selected_entry_alert_payload(&entry, trade_date, "selected_but_blocked", None);
            insert_string_field(&mut candidate_alert_payload, "reason", reason.clone());
            insert_value_field(
                &mut candidate_alert_payload,
                "current",
                current.map_or(Value::Null, Value::from),
            );
            insert_value_field(
                &mut candidate_alert_payload,
                "limit",
                limit.map_or(Value::Null, Value::from),
            );
            insert_value_field(&mut candidate_alert_payload, "details", json!(&details));
            println!(
                "decision: selected_but_blocked underlying={} reason={} current={} limit={} details={} score={:.1}",
                entry.underlying(),
                reason,
                current
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                limit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                if details.is_empty() {
                    "none".to_string()
                } else {
                    details.join(" | ")
                },
                entry.score(),
            );
            record_decision_event(config, trade_date, candidate_alert_payload.clone());
            record_selected_candidate_alert(
                config,
                trade_date,
                &candidate_identity_key,
                candidate_alert_payload,
            );
            Ok(false)
        }
        StrategyDecision::Selected { entry, mode } => {
            apply_selected_entry_decision(entry, mode, config, state, trade_date).await
        }
        StrategyDecision::NoEntry => {
            println!("decision: no_entry");
            record_decision_event(
                config,
                trade_date,
                json!({
                    "action": "no_entry",
                    "trade_date": trade_date,
                }),
            );
            Ok(false)
        }
    }
}

async fn apply_selected_entry_decision(
    entry: SelectedOptionsEntry,
    mode: EntryMode,
    config: &OptionsEngineConfig,
    state: &mut StrategyState,
    trade_date: &str,
) -> anyhow::Result<bool> {
    let order_list_id = mode
        .is_submit()
        .then(|| order_list_id(trade_date, entry.underlying()));
    let (candidate_identity_key, mut candidate_alert_payload) =
        selected_entry_alert_payload(&entry, trade_date, mode.action(), order_list_id.as_deref());
    if mode == EntryMode::DryRun {
        insert_string_field(
            &mut candidate_alert_payload,
            "reason",
            "submission_disabled".to_string(),
        );
    }

    println!(
        "decision: {} underlying={} strategy={} symbols={} {}={:.2} score={:.1}{}",
        mode.action(),
        entry.underlying(),
        entry.strategy_name(),
        entry.option_symbols().join(","),
        entry.entry_premium_kind(),
        entry.entry_premium(),
        entry.score(),
        order_list_id
            .as_ref()
            .map(|value| format!(" order_list_id={value}"))
            .unwrap_or_else(|| " reason=submission_disabled".to_string()),
    );
    record_decision_event(config, trade_date, candidate_alert_payload.clone());
    record_selected_candidate_alert(
        config,
        trade_date,
        &candidate_identity_key,
        candidate_alert_payload.clone(),
    );

    if mode == EntryMode::DryRun {
        return Ok(false);
    }

    let Some(order_list_id) = order_list_id else {
        anyhow::bail!("submit mode missing order list ID");
    };
    let outcome = submit_selected_entry(&entry, &order_list_id, config.quantity, config).await?;
    let terminal_rejection =
        entry.is_naked_option() && outcome.accepted == 0 && outcome.rejected > 0;
    if outcome.accepted > 0 || terminal_rejection {
        state.record_entry_submission(selected_entry_state_draft(
            &entry,
            trade_date,
            &order_list_id,
            config.quantity,
            outcome.parent_order_id.clone(),
        ));
        if terminal_rejection && let Some(entry) = state.entries.last_mut() {
            entry.mark_canceled();
            entry.close_reason = Some("entry_rejected".to_string());
        }
    }

    println!(
        "submit_result: accepted={} rejected={}",
        outcome.accepted, outcome.rejected
    );
    let mut submit_payload = json!({
        "accepted": outcome.accepted,
        "rejected": outcome.rejected,
        "parent_order_id": outcome.parent_order_id.clone(),
        "underlying": entry.underlying(),
        "strategy": entry.strategy_name(),
    });
    if entry.is_naked_option() {
        insert_value_field(
            &mut submit_payload,
            "terminal_rejection_recorded",
            Value::Bool(terminal_rejection),
        );
    }
    record_submit_result_event(config, trade_date, submit_payload);
    if outcome.rejected > 0 {
        record_submit_rejected_candidate_alert(
            config,
            trade_date,
            &candidate_identity_key,
            candidate_alert_payload,
            &outcome,
            entry.is_naked_option().then_some(terminal_rejection),
        );
    }
    Ok(outcome.accepted > 0 || terminal_rejection)
}

fn selected_entry_state_draft(
    entry: &SelectedOptionsEntry,
    trade_date: &str,
    order_list_id: &str,
    quantity: u64,
    parent_order_id: Option<String>,
) -> StrategyStateEntryDraft {
    match entry {
        SelectedOptionsEntry::Credit(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            underlying: entry.underlying.clone(),
            strategy: credit_spread_strategy_name(entry.kind).to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.short.symbol.clone(),
            long_symbol: entry.candidate.long.symbol.clone(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: entry.candidate.credit,
            debit: None,
            score: entry.candidate.score,
            parent_order_id,
        },
        SelectedOptionsEntry::IronCondor(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            underlying: entry.underlying.clone(),
            strategy: "iron_condor".to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.put.short.symbol.clone(),
            long_symbol: entry.candidate.put.long.symbol.clone(),
            short_call_symbol: Some(entry.candidate.call.short.symbol.clone()),
            long_call_symbol: Some(entry.candidate.call.long.symbol.clone()),
            quantity,
            credit: entry.candidate.credit,
            debit: None,
            score: entry.candidate.score,
            parent_order_id,
        },
        SelectedOptionsEntry::Debit(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            underlying: entry.underlying.clone(),
            strategy: debit_spread_strategy_name(entry.kind).to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.short.symbol.clone(),
            long_symbol: entry.candidate.long.symbol.clone(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: -entry.candidate.debit,
            debit: Some(entry.candidate.debit),
            score: entry.candidate.score,
            parent_order_id,
        },
        SelectedOptionsEntry::NakedOption(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            underlying: entry.underlying.clone(),
            strategy: naked_option_strategy_name(entry.kind).to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.short.symbol.clone(),
            long_symbol: String::new(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: entry.candidate.credit,
            debit: None,
            score: entry.candidate.score,
            parent_order_id,
        },
    }
}

async fn risk_gate_decision(
    context: &AccountEngineContext<'_>,
) -> anyhow::Result<RiskGateDecision> {
    if let Some(limit) = context.config.max_active_entries {
        let current = context
            .state
            .entries
            .iter()
            .filter(|entry| entry.is_active())
            .count();
        if current >= limit {
            return Ok(RiskGateDecision::MaxActiveEntries { current, limit });
        }
    }

    if let Some(limit) = context.config.max_daily_submits {
        let current = context.state.risk_counted_daily_submits(context.trade_date);
        if current >= limit {
            return Ok(RiskGateDecision::MaxDailySubmits { current, limit });
        }
    }

    if let Some(limit) = context.config.max_open_orders {
        let current = context
            .client
            .orders(&ListOrdersRequest::open_nested())
            .await?
            .len();
        if current >= limit {
            return Ok(RiskGateDecision::MaxOpenOrders { current, limit });
        }
    }

    if let Some(fleet) = context.config.fleet.as_ref()
        && let Some(limit) = fleet.config.fleet.max_active_entries
    {
        let current = fleet.exposure().active_entries;
        if current >= limit {
            return Ok(RiskGateDecision::FleetMaxActiveEntries { current, limit });
        }
    }

    Ok(RiskGateDecision::Continue)
}

async fn reconcile_strategy_state(
    client: &AlpacaHttpClient,
    state: &mut StrategyState,
) -> anyhow::Result<bool> {
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    let position_symbols = position_symbols(&positions);
    let open_order_symbols = order_symbols(&open_orders);
    let active_symbols = active_state_symbols(state);
    let unmanaged_symbols = position_symbols
        .difference(&active_symbols)
        .cloned()
        .collect::<Vec<_>>();
    if !unmanaged_symbols.is_empty() {
        println!(
            "reconcile: unmanaged_positions symbols={}",
            unmanaged_symbols.join(","),
        );
        emit_operator_event(
            "reconciliation_warning",
            json!({
                "reason": "unmanaged_positions",
                "symbols": unmanaged_symbols,
            }),
        );
    }

    let mut changed = false;
    for entry in state.entries.iter_mut().filter(|entry| entry.is_active()) {
        let mut action = reconciliation_action(entry, &position_symbols, &open_order_symbols, None);
        if action == ReconciliationAction::MarkClosed {
            let order_status = lookup_parent_order_snapshot(client, &entry.order_list_id)
                .await?
                .and_then(|order| order.status);
            action = reconciliation_action(
                entry,
                &position_symbols,
                &open_order_symbols,
                order_status.as_deref(),
            );
        }

        match action {
            ReconciliationAction::None => {}
            ReconciliationAction::MarkClosed => {
                println!(
                    "reconcile: mark_closed underlying={} order_list_id={} reason=broker_flat",
                    entry.underlying, entry.order_list_id,
                );
                emit_operator_event(
                    "reconciliation_repair",
                    json!({
                        "action": "mark_closed",
                        "reason": "broker_flat",
                        "underlying": entry.underlying,
                        "order_list_id": entry.order_list_id,
                    }),
                );
                entry.mark_closed(None);
                changed = true;
            }
            ReconciliationAction::MarkCanceled => {
                println!(
                    "reconcile: mark_canceled underlying={} order_list_id={} reason=entry_terminal_without_position",
                    entry.underlying, entry.order_list_id,
                );
                emit_operator_event(
                    "reconciliation_repair",
                    json!({
                        "action": "mark_canceled",
                        "reason": "entry_terminal_without_position",
                        "underlying": entry.underlying,
                        "order_list_id": entry.order_list_id,
                    }),
                );
                entry.mark_canceled();
                changed = true;
            }
            ReconciliationAction::PartialPosition => {
                println!(
                    "reconcile: partial_position underlying={} symbols={}",
                    entry.underlying,
                    entry.symbols().join(","),
                );
                emit_operator_event(
                    "reconciliation_warning",
                    json!({
                        "reason": "partial_position",
                        "underlying": entry.underlying,
                        "symbols": entry.symbols(),
                    }),
                );
            }
        }
    }

    Ok(changed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconciliationAction {
    None,
    MarkClosed,
    MarkCanceled,
    PartialPosition,
}

fn reconciliation_action(
    entry: &StrategyStateEntry,
    position_symbols: &BTreeSet<String>,
    open_order_symbols: &BTreeSet<String>,
    entry_order_status: Option<&str>,
) -> ReconciliationAction {
    let symbols = entry.symbols();
    let position_matches = symbols
        .iter()
        .filter(|symbol| {
            position_symbols
                .iter()
                .any(|candidate| candidate == *symbol)
        })
        .count();
    let open_order_matches = symbols
        .iter()
        .filter(|symbol| {
            open_order_symbols
                .iter()
                .any(|candidate| candidate == *symbol)
        })
        .count();

    if position_matches == 0 && open_order_matches == 0 {
        if matches!(
            entry_order_status,
            Some("canceled" | "expired" | "rejected")
        ) {
            ReconciliationAction::MarkCanceled
        } else {
            ReconciliationAction::MarkClosed
        }
    } else if position_matches > 0 && position_matches < symbols.len() {
        ReconciliationAction::PartialPosition
    } else {
        ReconciliationAction::None
    }
}

fn active_state_symbols(state: &StrategyState) -> BTreeSet<String> {
    state
        .entries
        .iter()
        .filter(|entry| entry.is_active())
        .flat_map(|entry| entry.symbols().into_iter().map(ToString::to_string))
        .collect()
}

fn position_symbols(positions: &[AlpacaPosition]) -> BTreeSet<String> {
    positions
        .iter()
        .filter_map(|position| position.symbol.clone())
        .collect()
}

fn order_symbols(orders: &[AlpacaOrder]) -> BTreeSet<String> {
    orders.iter().flat_map(AlpacaOrder::symbols).collect()
}

async fn manage_existing_entries(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &OptionsEngineConfig,
    state: &mut StrategyState,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for entry in state
        .entries
        .iter_mut()
        .filter(|entry| entry.submitted && !entry.closed && !entry.canceled)
    {
        if let Some(close_order_list_id) = entry.close_order_list_id.as_deref() {
            if let Some(order) = lookup_parent_order_snapshot(client, close_order_list_id).await? {
                println!(
                    "manage: close_order order_list_id={} status={}",
                    close_order_list_id,
                    order.status.as_deref().unwrap_or("unknown"),
                );
                if order.status.as_deref() == Some("filled") {
                    entry.mark_closed(order.id.clone());
                    record_realized_performance_ledger(client, config, entry, &order).await;
                    changed = true;
                } else if order.is_working() {
                    let stale = config.stale_close_secs > 0
                        && order_age_secs(&order).is_some_and(|age| age >= config.stale_close_secs);
                    if stale {
                        println!(
                            "manage: stale_close order_list_id={} status={} manage_enabled={}",
                            close_order_list_id,
                            order.status.as_deref().unwrap_or("unknown"),
                            config.manage_enabled,
                        );
                        if config.manage_enabled && config.close_enabled {
                            cancel_parent_order_by_id(client, order.id.as_deref()).await?;
                            entry.clear_close_submission();
                            changed = true;
                        }
                    }
                } else if matches!(
                    order.status.as_deref(),
                    Some("canceled" | "expired" | "rejected")
                ) {
                    entry.clear_close_submission();
                    changed = true;
                }
            }
            continue;
        }

        let Some(entry_order) = lookup_parent_order_snapshot(client, &entry.order_list_id).await?
        else {
            println!(
                "manage: entry_order_missing order_list_id={}",
                entry.order_list_id
            );
            continue;
        };

        if entry_order.is_working() {
            let stale = config.stale_entry_secs > 0
                && order_age_secs(&entry_order).is_some_and(|age| age >= config.stale_entry_secs);
            if stale {
                println!(
                    "manage: stale_entry order_list_id={} status={} manage_enabled={}",
                    entry.order_list_id,
                    entry_order.status.as_deref().unwrap_or("unknown"),
                    config.manage_enabled,
                );
                if config.manage_enabled {
                    cancel_parent_order_by_id(client, entry_order.id.as_deref()).await?;
                    entry.mark_canceled();
                    changed = true;
                }
            }
            continue;
        }

        if matches!(
            entry_order.status.as_deref(),
            Some("canceled" | "expired" | "rejected")
        ) {
            entry.mark_canceled();
            changed = true;
            println!(
                "manage: entry_terminal order_list_id={} status={}",
                entry.order_list_id,
                entry_order.status.as_deref().unwrap_or("unknown"),
            );
            continue;
        }

        let Some(close_quote) = close_quote(client, data_config, entry).await? else {
            println!(
                "manage: close_quote_unavailable short={} long={}",
                entry.short_symbol, entry.long_symbol
            );
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "close_quote_unavailable",
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "short_symbol": entry.short_symbol,
                    "long_symbol": entry.long_symbol,
                }),
            );
            continue;
        };
        let close_reason = close_reason(config, entry, close_quote.debit);
        emit_management_snapshot(entry, &close_quote, close_reason.as_deref());
        if entry.is_debit_spread() {
            println!(
                "manage: position underlying={} strategy={} close_credit={:.2} entry_debit={:.2} reason={}",
                entry.underlying,
                entry.strategy,
                -close_quote.debit,
                entry.debit.unwrap_or_default(),
                close_reason.as_deref().unwrap_or("none"),
            );
        } else {
            println!(
                "manage: position underlying={} strategy={} close_debit={:.2} entry_credit={:.2} reason={}",
                entry.underlying,
                entry.strategy,
                close_quote.debit,
                entry.credit,
                close_reason.as_deref().unwrap_or("none"),
            );
        }

        let Some(close_reason) = close_reason else {
            continue;
        };
        if !(config.manage_enabled && config.close_enabled) {
            let reason = if !config.manage_enabled {
                "management_disabled"
            } else {
                "close_disabled"
            };
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": reason,
                    "underlying": entry.underlying,
                    "trigger": close_reason,
                    "manage_enabled": config.manage_enabled,
                    "close_enabled": config.close_enabled,
                }),
            );
            continue;
        }
        if close_attempts_exhausted(config, entry) {
            println!(
                "manage: close_blocked underlying={} trigger={} reason=max_close_attempts attempts={} limit={}",
                entry.underlying, close_reason, entry.close_attempts, config.max_close_attempts,
            );
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "max_close_attempts",
                    "underlying": entry.underlying,
                    "trigger": close_reason,
                    "attempts": entry.close_attempts,
                    "limit": config.max_close_attempts,
                }),
            );
            continue;
        }
        if let Some(remaining_secs) = close_reprice_cooldown_remaining_secs(config, entry) {
            println!(
                "manage: close_blocked underlying={} trigger={} reason=close_reprice_cooldown remaining_secs={}",
                entry.underlying, close_reason, remaining_secs,
            );
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "close_reprice_cooldown",
                    "underlying": entry.underlying,
                    "trigger": close_reason,
                    "remaining_secs": remaining_secs,
                }),
            );
            continue;
        }
        if close_submission_gate_decision(config, Utc::now())
            == CloseSubmissionGateDecision::OutsideCloseWindow
        {
            println!(
                "manage: close_blocked underlying={} trigger={} reason=outside_close_window window={}-{} timezone={}",
                entry.underlying,
                close_reason,
                config.close_start,
                config.close_end,
                config.entry_timezone,
            );
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "outside_close_window",
                    "underlying": entry.underlying,
                    "trigger": close_reason,
                    "window_start": config.close_start.to_string(),
                    "window_end": config.close_end.to_string(),
                    "timezone": config.entry_timezone.to_string(),
                }),
            );
            continue;
        }

        let close_order_list_id = close_order_list_id(entry);
        let submit_quote = close_quote.with_price_cushion(config.close_price_cushion);
        if config.close_price_cushion > 0.0 {
            if entry.is_debit_spread() {
                println!(
                    "manage: close_limit underlying={} raw_credit={:.2} cushion={:.2} limit_credit={:.2}",
                    entry.underlying,
                    -close_quote.debit,
                    config.close_price_cushion,
                    -submit_quote.debit,
                );
            } else {
                println!(
                    "manage: close_limit underlying={} raw_debit={:.2} cushion={:.2} limit_debit={:.2}",
                    entry.underlying,
                    close_quote.debit,
                    config.close_price_cushion,
                    submit_quote.debit,
                );
            }
        }
        let outcome =
            submit_close_entry(entry, &submit_quote, &close_order_list_id, config).await?;
        if outcome.accepted > 0 {
            entry.record_close_submission(
                close_order_list_id,
                outcome.parent_order_id,
                close_reason,
            );
            changed = true;
        }
    }

    Ok(changed)
}

async fn record_realized_performance_ledger(
    client: &AlpacaHttpClient,
    config: &OptionsEngineConfig,
    entry: &StrategyStateEntry,
    close_order: &AlpacaOrder,
) {
    if let Err(error) = append_realized_performance_ledger(client, config, entry, close_order).await
    {
        emit_operator_event(
            "performance_ledger_error",
            json!({
                "reason": "append_failed",
                "underlying": entry.underlying,
                "strategy": entry.strategy,
                "order_list_id": entry.order_list_id,
                "close_order_list_id": entry.close_order_list_id,
                "error": error.to_string(),
            }),
        );
    }
}

async fn append_realized_performance_ledger(
    client: &AlpacaHttpClient,
    config: &OptionsEngineConfig,
    entry: &StrategyStateEntry,
    close_order: &AlpacaOrder,
) -> anyhow::Result<()> {
    let open_order = lookup_parent_order_snapshot(client, &entry.order_list_id).await?;
    let mut order_ids = EntryOrderIds::default();
    if let Some(order) = open_order.as_ref() {
        collect_order_ids(order, &mut order_ids.open);
    }
    collect_order_ids(close_order, &mut order_ids.close);
    add_state_order_id(entry.parent_order_id.as_deref(), &mut order_ids.open);
    add_state_order_id(entry.close_parent_order_id.as_deref(), &mut order_ids.close);

    let mut request = ListActivitiesRequest::option_reconciliation();
    request.direction = Some("asc".to_string());
    request.after = Some(performance_activity_after_timestamp(entry));
    let activities = client.account_activities_all(&request).await?;
    let performance = entry_performance(entry, &order_ids, &activities, &[]);
    let ledger_date = Utc::now()
        .with_timezone(&config.entry_timezone)
        .date_naive()
        .to_string();
    let append = append_performance_ledger_record(
        &performance_ledger_dir(config),
        &ledger_date,
        config.fleet_account_id.as_deref(),
        &performance,
    )?;

    emit_operator_event(
        "performance_ledger",
        json!({
            "ledger_path": append.path,
            "appended": append.appended,
            "record_key": append.record_key,
            "underlying": performance.underlying,
            "strategy": performance.strategy,
            "status": performance.status,
            "realized_pnl": performance.realized_pnl,
            "open_cashflow": performance.open.cashflow,
            "close_cashflow": performance.close.cashflow,
            "warnings": performance.warnings,
        }),
    );

    Ok(())
}

fn add_state_order_id(order_id: Option<&str>, order_ids: &mut BTreeSet<String>) {
    if let Some(order_id) = order_id.filter(|value| !value.trim().is_empty()) {
        order_ids.insert(order_id.to_string());
    }
}

fn performance_activity_after_timestamp(entry: &StrategyStateEntry) -> String {
    DateTime::parse_from_rfc3339(&entry.recorded_at_utc)
        .ok()
        .and_then(|recorded| {
            recorded
                .with_timezone(&Utc)
                .checked_sub_signed(ChronoDuration::days(1))
        })
        .unwrap_or_else(|| Utc::now() - ChronoDuration::days(30))
        .to_rfc3339()
}

fn performance_ledger_dir(config: &OptionsEngineConfig) -> PathBuf {
    env::var("ALPACA_PERFORMANCE_LEDGER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            default_performance_ledger_dir(&config.state_path, config.fleet_account_id.as_deref())
        })
}

#[derive(Clone, Copy, Debug)]
struct CloseQuote {
    short_ask: f64,
    long_bid: f64,
    short_call_ask: Option<f64>,
    long_call_bid: Option<f64>,
    debit: f64,
}

impl CloseQuote {
    fn with_price_cushion(self, cushion: f64) -> Self {
        let cushion = cushion.max(0.0);
        if cushion == 0.0 {
            return self;
        }

        let mut quote = self;
        let short_leg_count = if quote.short_call_ask.is_some() {
            2.0
        } else {
            1.0
        };
        let per_short_leg_cushion = cushion / short_leg_count;
        quote.short_ask += per_short_leg_cushion;
        if let Some(short_call_ask) = quote.short_call_ask.as_mut() {
            *short_call_ask += per_short_leg_cushion;
        }
        quote.debit += cushion;
        quote
    }
}

async fn close_quote(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    entry: &StrategyStateEntry,
) -> anyhow::Result<Option<CloseQuote>> {
    let mut request =
        OptionSnapshotsRequest::for_symbols(entry.symbols().into_iter().map(ToString::to_string));
    request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&request).await?.snapshots;
    let short_quote = snapshots
        .get(&entry.short_symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref());
    let Some(short_ask) = short_quote.and_then(|quote| quote.ask_price) else {
        return Ok(None);
    };
    if entry.is_naked_option() {
        if short_ask <= 0.0 {
            return Ok(None);
        }
        return Ok(Some(CloseQuote {
            short_ask,
            long_bid: 0.0,
            short_call_ask: None,
            long_call_bid: None,
            debit: short_ask,
        }));
    }
    let long_quote = snapshots
        .get(&entry.long_symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref());
    let Some(long_bid) = long_quote.and_then(|quote| quote.bid_price) else {
        return Ok(None);
    };
    let mut debit = if entry.is_debit_spread() {
        let credit = long_bid - short_ask;
        if credit <= 0.0 {
            return Ok(None);
        }
        -credit
    } else {
        short_ask - long_bid
    };
    let mut short_call_ask = None;
    let mut long_call_bid = None;
    if let (Some(short_call_symbol), Some(long_call_symbol)) = (
        entry.short_call_symbol.as_deref(),
        entry.long_call_symbol.as_deref(),
    ) {
        let short_call_quote = snapshots
            .get(short_call_symbol)
            .and_then(|snapshot| snapshot.latest_quote.as_ref());
        let long_call_quote = snapshots
            .get(long_call_symbol)
            .and_then(|snapshot| snapshot.latest_quote.as_ref());
        let (Some(call_short_ask), Some(call_long_bid)) = (
            short_call_quote.and_then(|quote| quote.ask_price),
            long_call_quote.and_then(|quote| quote.bid_price),
        ) else {
            return Ok(None);
        };
        debit += call_short_ask - call_long_bid;
        short_call_ask = Some(call_short_ask);
        long_call_bid = Some(call_long_bid);
    }
    if !entry.is_debit_spread() && debit <= 0.0 {
        return Ok(None);
    }
    Ok(Some(CloseQuote {
        short_ask,
        long_bid,
        short_call_ask,
        long_call_bid,
        debit,
    }))
}

fn close_reason(
    config: &OptionsEngineConfig,
    entry: &StrategyStateEntry,
    close_debit: f64,
) -> Option<String> {
    if entry.is_debit_spread() {
        debit_spread_close_reason(config, entry, -close_debit)
    } else {
        credit_spread_close_reason(&config.management_config(), entry, close_debit)
            .map(ToString::to_string)
    }
}

fn debit_spread_close_reason(
    config: &OptionsEngineConfig,
    entry: &StrategyStateEntry,
    close_credit: f64,
) -> Option<String> {
    if config.force_flatten {
        return Some("manual_flatten".to_string());
    }
    let entry_debit = entry.debit?;
    if close_credit >= entry_debit * (1.0 + config.profit_target_close_fraction.max(0.0)) {
        return Some("profit_target".to_string());
    }
    if config.stop_loss_close_multiple > 0.0
        && close_credit <= entry_debit / config.stop_loss_close_multiple
    {
        return Some("stop_loss".to_string());
    }
    if config.max_hold_secs > 0
        && DateTime::parse_from_rfc3339(&entry.recorded_at_utc)
            .map(|recorded| {
                Utc::now()
                    .signed_duration_since(recorded.with_timezone(&Utc))
                    .num_seconds()
                    >= config.max_hold_secs as i64
            })
            .unwrap_or(false)
    {
        return Some("max_hold".to_string());
    }
    if config.expiration_exit_days >= 0
        && days_to_expiration(&entry.short_symbol)
            .or_else(|| days_to_expiration(&entry.long_symbol))
            .is_some_and(|days| days <= config.expiration_exit_days)
    {
        return Some("expiration_risk".to_string());
    }
    None
}

fn emit_management_snapshot(
    entry: &StrategyStateEntry,
    close_quote: &CloseQuote,
    close_reason: Option<&str>,
) {
    let (net_premium_kind, entry_net_premium, close_net_premium, unrealized_pnl) =
        if entry.is_debit_spread() {
            let close_credit = -close_quote.debit;
            (
                "debit",
                entry.debit,
                Some(close_credit),
                entry.debit.map(|debit| close_credit - debit),
            )
        } else {
            (
                "credit",
                Some(entry.credit),
                Some(close_quote.debit),
                Some(entry.credit - close_quote.debit),
            )
        };
    let unrealized_pnl_fraction = unrealized_pnl.and_then(|pnl| {
        entry_net_premium.and_then(|basis| if basis > 0.0 { Some(pnl / basis) } else { None })
    });
    emit_operator_event(
        "management_snapshot",
        json!({
            "underlying": entry.underlying,
            "strategy": entry.strategy,
            "net_premium_kind": net_premium_kind,
            "entry_net_premium": entry_net_premium,
            "close_net_premium": close_net_premium,
            "unrealized_pnl": unrealized_pnl,
            "unrealized_pnl_fraction": unrealized_pnl_fraction,
            "close_reason": close_reason,
            "close_attempts": entry.close_attempts,
            "days_to_expiration": days_to_expiration(&entry.short_symbol)
                .or_else(|| days_to_expiration(&entry.long_symbol)),
            "hold_secs": recorded_age_secs(entry),
        }),
    );
}

async fn submit_selected_entry(
    entry: &SelectedOptionsEntry,
    order_list_id: &str,
    quantity: u64,
    config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    match entry {
        SelectedOptionsEntry::Credit(entry) => {
            submit_entry(entry, order_list_id, quantity, config).await
        }
        SelectedOptionsEntry::IronCondor(entry) => {
            submit_iron_condor_entry(entry, order_list_id, quantity, config).await
        }
        SelectedOptionsEntry::Debit(entry) => {
            submit_debit_entry(entry, order_list_id, quantity, config).await
        }
        SelectedOptionsEntry::NakedOption(entry) => {
            submit_naked_option_entry(entry, order_list_id, quantity, config).await
        }
    }
}

async fn submit_with_execution_session<F>(
    order_list_id: &str,
    expected_events: usize,
    cancel_after_accept: bool,
    submit: F,
) -> anyhow::Result<SubmitOutcome>
where
    F: FnOnce(
        &mut AlpacaExecutionClient,
        TraderId,
        Option<ClientId>,
        StrategyId,
    ) -> anyhow::Result<()>,
{
    let exec_config = exec_config_from_env();
    let (tx, mut rx) = mpsc::unbounded_channel();
    replace_exec_event_sender(tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let trader_id = TraderId::from("TRADER-001");
    let client_id = ClientId::from(ALPACA_CLIENT_ID);
    let account_id = AccountId::from("ALPACA-001");
    let strategy_id = StrategyId::from(STRATEGY_FAMILY);
    let core = ExecutionClientCore::new(
        trader_id,
        client_id,
        Venue::new(ALPACA_VENUE),
        OmsType::Netting,
        account_id,
        AccountType::Margin,
        None,
        cache,
    );
    let mut client = AlpacaExecutionClient::new(core, exec_config.clone())?;
    client.start()?;
    client.connect().await?;

    submit(&mut client, trader_id, Some(client_id), strategy_id)?;

    let (accepted, rejected) = collect_execution_events(&mut rx, expected_events).await;
    let parent_order_id = lookup_parent_order(&exec_config, order_list_id).await?;
    if cancel_after_accept && accepted > 0 {
        cancel_parent_order(&exec_config, parent_order_id.as_deref()).await?;
    }

    client.disconnect().await?;
    client.stop()?;

    Ok(SubmitOutcome {
        accepted,
        rejected,
        parent_order_id,
    })
}

async fn submit_entry(
    entry: &SelectedEntry,
    order_list_id: &str,
    quantity: u64,
    config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_with_execution_session(
        order_list_id,
        2,
        config.cancel_after_accept,
        |client, trader_id, client_id, strategy_id| {
            let cmd = build_submit_order_list(
                entry,
                order_list_id,
                quantity,
                trader_id,
                client_id,
                strategy_id,
            )?;
            client.submit_order_list(cmd)?;
            Ok(())
        },
    )
    .await
}

async fn submit_iron_condor_entry(
    entry: &SelectedIronCondorEntry,
    order_list_id: &str,
    quantity: u64,
    config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_with_execution_session(
        order_list_id,
        4,
        config.cancel_after_accept,
        |client, trader_id, client_id, strategy_id| {
            let cmd = build_iron_condor_submit_order_list(
                &entry.candidate,
                order_list_id,
                quantity,
                trader_id,
                client_id,
                strategy_id,
            )?;
            client.submit_order_list(cmd)?;
            Ok(())
        },
    )
    .await
}

async fn submit_debit_entry(
    entry: &SelectedDebitEntry,
    order_list_id: &str,
    quantity: u64,
    config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_with_execution_session(
        order_list_id,
        2,
        config.cancel_after_accept,
        |client, trader_id, client_id, strategy_id| {
            let cmd = build_debit_submit_order_list(
                &entry.candidate,
                order_list_id,
                quantity,
                trader_id,
                client_id,
                strategy_id,
            )?;
            client.submit_order_list(cmd)?;
            Ok(())
        },
    )
    .await
}

async fn submit_naked_option_entry(
    entry: &SelectedNakedOptionEntry,
    order_list_id: &str,
    quantity: u64,
    config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_with_execution_session(
        order_list_id,
        1,
        config.cancel_after_accept,
        |client, trader_id, client_id, strategy_id| {
            let cmd = build_naked_option_submit_order(
                &entry.candidate,
                order_list_id,
                quantity,
                trader_id,
                client_id,
                strategy_id,
            )?;
            client.submit_order(cmd)?;
            Ok(())
        },
    )
    .await
}

async fn submit_close_entry(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    _config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_with_execution_session(
        order_list_id,
        entry.symbols().len(),
        false,
        |client, trader_id, client_id, strategy_id| {
            if entry.is_naked_option() {
                let cmd = build_naked_option_close_order(
                    entry,
                    quote,
                    order_list_id,
                    trader_id,
                    client_id,
                    strategy_id,
                )?;
                client.submit_order(cmd)?;
            } else {
                let cmd = build_close_submit_order_list(
                    entry,
                    quote,
                    order_list_id,
                    trader_id,
                    client_id,
                    strategy_id,
                )?;
                client.submit_order_list(cmd)?;
            }
            Ok(())
        },
    )
    .await
}

fn build_submit_order_list(
    entry: &SelectedEntry,
    order_list_id: &str,
    quantity: u64,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }

    let order_list_id = OrderListId::from(order_list_id);
    let short_client_id = ClientOrderId::from(format!("{order_list_id}-short").as_str());
    let long_client_id = ClientOrderId::from(format!("{order_list_id}-long").as_str());
    let quantity = Quantity::new(quantity as f64, 0);
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: vec![
            MlegSubmitLeg {
                client_order_id: short_client_id,
                instrument_id: alpaca_instrument_id(&entry.candidate.short.symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(entry.candidate.short.bid, 2),
                reduce_only: false,
            },
            MlegSubmitLeg {
                client_order_id: long_client_id,
                instrument_id: alpaca_instrument_id(&entry.candidate.long.symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(entry.candidate.long.ask, 2),
                reduce_only: false,
            },
        ],
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

fn build_iron_condor_submit_order_list(
    candidate: &IronCondorCandidate,
    order_list_id: &str,
    quantity: u64,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }

    let order_list_id = OrderListId::from(order_list_id);
    let quantity = Quantity::new(quantity as f64, 0);
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: vec![
            MlegSubmitLeg {
                client_order_id: ClientOrderId::from(format!("{order_list_id}-short-put").as_str()),
                instrument_id: alpaca_instrument_id(&candidate.put.short.symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(candidate.put.short.bid, 2),
                reduce_only: false,
            },
            MlegSubmitLeg {
                client_order_id: ClientOrderId::from(format!("{order_list_id}-long-put").as_str()),
                instrument_id: alpaca_instrument_id(&candidate.put.long.symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(candidate.put.long.ask, 2),
                reduce_only: false,
            },
            MlegSubmitLeg {
                client_order_id: ClientOrderId::from(
                    format!("{order_list_id}-short-call").as_str(),
                ),
                instrument_id: alpaca_instrument_id(&candidate.call.short.symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(candidate.call.short.bid, 2),
                reduce_only: false,
            },
            MlegSubmitLeg {
                client_order_id: ClientOrderId::from(format!("{order_list_id}-long-call").as_str()),
                instrument_id: alpaca_instrument_id(&candidate.call.long.symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(candidate.call.long.ask, 2),
                reduce_only: false,
            },
        ],
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

fn build_debit_submit_order_list(
    candidate: &DebitSpreadCandidate,
    order_list_id: &str,
    quantity: u64,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }

    let order_list_id = OrderListId::from(order_list_id);
    let quantity = Quantity::new(quantity as f64, 0);
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: vec![
            MlegSubmitLeg {
                client_order_id: ClientOrderId::from(format!("{order_list_id}-long").as_str()),
                instrument_id: alpaca_instrument_id(&candidate.long.symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(candidate.long.ask, 2),
                reduce_only: false,
            },
            MlegSubmitLeg {
                client_order_id: ClientOrderId::from(format!("{order_list_id}-short").as_str()),
                instrument_id: alpaca_instrument_id(&candidate.short.symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(candidate.short.bid, 2),
                reduce_only: false,
            },
        ],
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

fn build_naked_option_submit_order(
    candidate: &NakedOptionCandidate,
    client_order_id: &str,
    quantity: u64,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<nautilus_common::messages::execution::SubmitOrder> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }

    build_simple_submit_order(SimpleSubmitOrderRequest {
        trader_id,
        client_id,
        strategy_id,
        client_order_id: ClientOrderId::from(client_order_id),
        instrument_id: alpaca_instrument_id(&candidate.short.symbol)?,
        order_side: OrderSide::Sell,
        quantity: Quantity::new(quantity as f64, 0),
        limit_price: Price::new(candidate.short.bid, 2),
        reduce_only: false,
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

fn build_naked_option_close_order(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    client_order_id: &str,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<nautilus_common::messages::execution::SubmitOrder> {
    build_simple_submit_order(SimpleSubmitOrderRequest {
        trader_id,
        client_id,
        strategy_id,
        client_order_id: ClientOrderId::from(client_order_id),
        instrument_id: alpaca_instrument_id(&entry.short_symbol)?,
        order_side: OrderSide::Buy,
        quantity: Quantity::new(entry.quantity as f64, 0),
        limit_price: Price::new(quote.short_ask, 2),
        reduce_only: true,
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

fn build_close_submit_order_list(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    let order_list_id = OrderListId::from(order_list_id);
    let quantity = Quantity::new(entry.quantity as f64, 0);
    let mut legs = vec![
        MlegSubmitLeg {
            client_order_id: ClientOrderId::from(format!("{order_list_id}-short-close").as_str()),
            instrument_id: alpaca_instrument_id(&entry.short_symbol)?,
            order_side: OrderSide::Buy,
            quantity,
            limit_price: Price::new(quote.short_ask, 2),
            reduce_only: true,
        },
        MlegSubmitLeg {
            client_order_id: ClientOrderId::from(format!("{order_list_id}-long-close").as_str()),
            instrument_id: alpaca_instrument_id(&entry.long_symbol)?,
            order_side: OrderSide::Sell,
            quantity,
            limit_price: Price::new(quote.long_bid, 2),
            reduce_only: true,
        },
    ];
    if let (
        Some(short_call_symbol),
        Some(long_call_symbol),
        Some(short_call_ask),
        Some(long_call_bid),
    ) = (
        entry.short_call_symbol.as_deref(),
        entry.long_call_symbol.as_deref(),
        quote.short_call_ask,
        quote.long_call_bid,
    ) {
        legs.push(MlegSubmitLeg {
            client_order_id: ClientOrderId::from(
                format!("{order_list_id}-short-call-close").as_str(),
            ),
            instrument_id: alpaca_instrument_id(short_call_symbol)?,
            order_side: OrderSide::Buy,
            quantity,
            limit_price: Price::new(short_call_ask, 2),
            reduce_only: true,
        });
        legs.push(MlegSubmitLeg {
            client_order_id: ClientOrderId::from(
                format!("{order_list_id}-long-call-close").as_str(),
            ),
            instrument_id: alpaca_instrument_id(long_call_symbol)?,
            order_side: OrderSide::Sell,
            quantity,
            limit_price: Price::new(long_call_bid, 2),
            reduce_only: true,
        });
    }
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs,
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

async fn collect_execution_events(
    rx: &mut mpsc::UnboundedReceiver<ExecutionEvent>,
    leg_count: usize,
) -> (usize, usize) {
    let mut accepted = 0;
    let mut rejected = 0;
    let deadline = Instant::now()
        + Duration::from_secs(env_parse(
            "ALPACA_EVENT_TIMEOUT_SECS",
            DEFAULT_EVENT_TIMEOUT_SECS,
        ));

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(Some(event)) = timeout(remaining, rx.recv()).await else {
            break;
        };

        if let ExecutionEvent::Order(order_event) = event {
            print_order_event(&order_event);
            match order_event {
                OrderEventAny::Accepted(_) => accepted += 1,
                OrderEventAny::Rejected(_) => rejected += 1,
                _ => {}
            }
            if accepted + rejected >= leg_count {
                break;
            }
        }
    }

    (accepted, rejected)
}

fn print_order_event(order_event: &OrderEventAny) {
    let event_type = order_event.event_type();
    let event = order_event.clone().into_boxed();
    println!(
        "execution_event: order type={event_type:?} client_order_id={} instrument_id={} venue_order_id={} reason={}",
        event.client_order_id(),
        event.instrument_id(),
        event
            .venue_order_id()
            .map_or_else(|| "None".to_string(), |value| value.to_string()),
        event
            .reason()
            .map_or_else(|| "None".to_string(), |value| value.to_string()),
    );
}

async fn lookup_parent_order(
    config: &AlpacaExecClientConfig,
    order_list_id: &str,
) -> anyhow::Result<Option<String>> {
    let client = AlpacaHttpClient::from_exec_config(config)?;
    match client.order_by_client_order_id(order_list_id, true).await {
        Ok(order) => Ok(order.id),
        Err(Error::HttpStatus { status, .. }) if status == 404 => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn lookup_parent_order_snapshot(
    client: &AlpacaHttpClient,
    order_list_id: &str,
) -> anyhow::Result<Option<AlpacaOrder>> {
    match client.order_by_client_order_id(order_list_id, true).await {
        Ok(order) => Ok(Some(order)),
        Err(Error::HttpStatus { status, .. }) if status == 404 => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn cancel_parent_order(
    config: &AlpacaExecClientConfig,
    parent_order_id: Option<&str>,
) -> anyhow::Result<()> {
    let Some(parent_order_id) = parent_order_id else {
        println!("cleanup: skipped reason=no_parent_order_id");
        return Ok(());
    };

    let client = AlpacaHttpClient::from_exec_config(config)?;
    client.cancel_order(parent_order_id).await?;
    println!("cleanup: cancel_requested parent_order_id={parent_order_id}");
    Ok(())
}

async fn cancel_parent_order_by_id(
    client: &AlpacaHttpClient,
    parent_order_id: Option<&str>,
) -> anyhow::Result<()> {
    let Some(parent_order_id) = parent_order_id else {
        println!("cleanup: skipped reason=no_parent_order_id");
        return Ok(());
    };
    client.cancel_order(parent_order_id).await?;
    println!("cleanup: cancel_requested parent_order_id={parent_order_id}");
    Ok(())
}

fn alpaca_instrument_id(symbol: &str) -> anyhow::Result<InstrumentId> {
    Ok(InstrumentId::from_str(&format!("{symbol}.{ALPACA_VENUE}"))?)
}

fn order_list_id(trade_date: &str, underlying: &str) -> String {
    format!(
        "options-engine-entry-{trade_date}-{underlying}-{}",
        UUID4::new()
    )
}

fn close_order_list_id(entry: &StrategyStateEntry) -> String {
    format!(
        "options-engine-close-{}-{}-{}",
        entry.trade_date,
        entry.underlying,
        UUID4::new()
    )
}

fn strategy_name(kind: CreditSpreadKind) -> &'static str {
    credit_spread_strategy_name(kind)
}

fn order_age_secs(order: &AlpacaOrder) -> Option<u64> {
    order
        .submitted_at
        .as_deref()
        .or(order.created_at.as_deref())
        .and_then(age_secs_from_rfc3339)
}

fn age_secs_from_rfc3339(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| {
            Utc::now()
                .signed_duration_since(timestamp.with_timezone(&Utc))
                .to_std()
                .ok()
        })
        .map(|duration| duration.as_secs())
}

fn inside_entry_window_at(config: &OptionsEngineConfig, now: DateTime<Utc>) -> bool {
    let now = now.with_timezone(&config.entry_timezone).time();
    config.entry_start <= now && now <= config.entry_end
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseSubmissionGateDecision {
    Continue,
    OutsideCloseWindow,
}

fn close_submission_gate_decision(
    config: &OptionsEngineConfig,
    now: DateTime<Utc>,
) -> CloseSubmissionGateDecision {
    if config.force_flatten
        || !config.close_regular_hours_only
        || inside_close_window_at(config, now)
    {
        CloseSubmissionGateDecision::Continue
    } else {
        CloseSubmissionGateDecision::OutsideCloseWindow
    }
}

fn inside_close_window_at(config: &OptionsEngineConfig, now: DateTime<Utc>) -> bool {
    let now = now.with_timezone(&config.entry_timezone).time();
    config.close_start <= now && now <= config.close_end
}

fn close_attempts_exhausted(config: &OptionsEngineConfig, entry: &StrategyStateEntry) -> bool {
    !config.force_flatten
        && config.max_close_attempts > 0
        && entry.close_attempts >= config.max_close_attempts
}

fn close_reprice_cooldown_remaining_secs(
    config: &OptionsEngineConfig,
    entry: &StrategyStateEntry,
) -> Option<u64> {
    if config.force_flatten || config.close_reprice_cooldown_secs == 0 {
        return None;
    }

    let age = entry
        .last_close_submitted_at_utc
        .as_deref()
        .and_then(age_secs_from_rfc3339)?;
    (age < config.close_reprice_cooldown_secs).then_some(config.close_reprice_cooldown_secs - age)
}

fn entry_gate_decision(config: &OptionsEngineConfig, now: DateTime<Utc>) -> EntryGateDecision {
    if config.kill_switch {
        EntryGateDecision::KillSwitch
    } else if !config.ignore_entry_window && !inside_entry_window_at(config, now) {
        EntryGateDecision::OutsideEntryWindow
    } else {
        EntryGateDecision::Continue
    }
}

fn market_trade_date(config: &OptionsEngineConfig) -> String {
    Utc::now()
        .with_timezone(&config.entry_timezone)
        .date_naive()
        .to_string()
}

fn exec_config_from_env() -> AlpacaExecClientConfig {
    let mut config = AlpacaExecClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.trade_updates_ws_url = env::var("ALPACA_TRADE_UPDATES_WS_URL").ok();
    config.external_order_filtering = false;
    config
}

fn env_parse<T>(name: &str, default: T) -> T
where
    T: FromStr,
{
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use chrono::NaiveTime;

    use super::*;
    use crate::strategy::{
        DebitSpreadScannerConfig, IronCondorScannerConfig, PutCreditScannerConfig,
    };

    fn config_for_gate_tests() -> OptionsEngineConfig {
        OptionsEngineConfig {
            underlyings: vec!["SPY".to_string()],
            spread_kinds: vec![CreditSpreadKind::Put],
            iron_condor_enabled: false,
            debit_kinds: Vec::new(),
            naked_kinds: Vec::new(),
            dry_run_spread_kinds: Vec::new(),
            iron_condor_dry_run: false,
            dry_run_debit_kinds: Vec::new(),
            dry_run_naked_kinds: Vec::new(),
            max_active_entries: None,
            max_daily_submits: None,
            max_open_orders: None,
            max_active_entries_per_underlying: None,
            max_active_entries_per_sector: None,
            sectors: BTreeMap::new(),
            max_iterations: 1,
            interval_secs: 300,
            quantity: 1,
            submit_enabled: false,
            manage_enabled: false,
            kill_switch: false,
            force_flatten: false,
            cancel_after_accept: false,
            stale_entry_secs: 900,
            stale_close_secs: 120,
            close_enabled: false,
            close_regular_hours_only: true,
            close_start: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            close_end: NaiveTime::from_hms_opt(16, 0, 0).unwrap(),
            close_price_cushion: 0.02,
            max_close_attempts: 3,
            close_reprice_cooldown_secs: 30,
            profit_target_close_fraction: 0.50,
            stop_loss_close_multiple: 2.0,
            max_hold_secs: 0,
            expiration_exit_days: 1,
            ignore_entry_window: false,
            entry_start: NaiveTime::from_hms_opt(9, 45, 0).unwrap(),
            entry_end: NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            entry_timezone: "America/New_York".parse().unwrap(),
            state_path: PathBuf::from("state.json"),
            candidate_ledger_enabled: false,
            candidate_ledger_dir: PathBuf::from("candidate-ledger"),
            candidate_ledger_max_candidates: 10,
            scanner: PutCreditScannerConfig::default(),
            iron_condor_scanner: IronCondorScannerConfig::default(),
            debit_scanner: DebitSpreadScannerConfig::default(),
            naked_scanner: crate::strategy::NakedOptionScannerConfig::default(),
            naked_1_3dte_scanner: crate::strategy::NakedOptionScannerConfig::default(),
            fleet: None,
            fleet_account_id: None,
            fleet_policy_blocks: Vec::new(),
        }
    }

    #[test]
    fn entry_gate_allows_inside_window() {
        let config = config_for_gate_tests();
        let now = DateTime::parse_from_rfc3339("2026-05-04T14:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            entry_gate_decision(&config, now),
            EntryGateDecision::Continue
        );
    }

    #[test]
    fn entry_gate_blocks_outside_window_without_credentials() {
        let config = config_for_gate_tests();
        let now = DateTime::parse_from_rfc3339("2026-05-04T21:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            entry_gate_decision(&config, now),
            EntryGateDecision::OutsideEntryWindow
        );
    }

    #[test]
    fn entry_gate_prefers_kill_switch() {
        let mut config = config_for_gate_tests();
        config.kill_switch = true;
        let now = DateTime::parse_from_rfc3339("2026-05-04T21:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            entry_gate_decision(&config, now),
            EntryGateDecision::KillSwitch
        );
    }

    #[test]
    fn close_submission_gate_blocks_after_regular_hours() {
        let config = config_for_gate_tests();
        let now = DateTime::parse_from_rfc3339("2026-05-04T21:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            close_submission_gate_decision(&config, now),
            CloseSubmissionGateDecision::OutsideCloseWindow,
        );
    }

    #[test]
    fn close_submission_gate_allows_force_flatten_after_regular_hours() {
        let mut config = config_for_gate_tests();
        config.force_flatten = true;
        let now = DateTime::parse_from_rfc3339("2026-05-04T21:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            close_submission_gate_decision(&config, now),
            CloseSubmissionGateDecision::Continue,
        );
    }

    #[test]
    fn close_quote_cushion_increases_credit_spread_limit_debit() {
        let quote = CloseQuote {
            short_ask: 0.70,
            long_bid: 0.20,
            short_call_ask: None,
            long_call_bid: None,
            debit: 0.50,
        };

        let cushioned = quote.with_price_cushion(0.02);

        assert!((cushioned.short_ask - 0.72).abs() < 1e-9);
        assert!((cushioned.long_bid - 0.20).abs() < 1e-9);
        assert!((cushioned.debit - 0.52).abs() < 1e-9);
    }

    #[test]
    fn reconciliation_action_marks_closed_when_broker_is_flat() {
        assert_eq!(
            reconciliation_action(
                &state_entry(),
                &BTreeSet::new(),
                &BTreeSet::new(),
                Some("filled"),
            ),
            ReconciliationAction::MarkClosed,
        );
    }

    #[test]
    fn reconciliation_action_marks_canceled_for_terminal_entry_without_position() {
        assert_eq!(
            reconciliation_action(
                &state_entry(),
                &BTreeSet::new(),
                &BTreeSet::new(),
                Some("canceled"),
            ),
            ReconciliationAction::MarkCanceled,
        );
    }

    #[test]
    fn reconciliation_action_detects_partial_position_match() {
        let position_symbols = BTreeSet::from(["SPY260512P00708000".to_string()]);

        assert_eq!(
            reconciliation_action(&state_entry(), &position_symbols, &BTreeSet::new(), None,),
            ReconciliationAction::PartialPosition,
        );
    }

    #[test]
    fn close_attempt_limit_blocks_non_forced_submission() {
        let config = config_for_gate_tests();
        let mut entry = state_entry();
        entry.close_attempts = config.max_close_attempts;

        assert!(close_attempts_exhausted(&config, &entry));

        let mut forced = config_for_gate_tests();
        forced.force_flatten = true;
        assert!(!close_attempts_exhausted(&forced, &entry));
    }

    #[test]
    fn close_reprice_cooldown_reports_remaining_time() {
        let mut config = config_for_gate_tests();
        config.close_reprice_cooldown_secs = 60;
        let mut entry = state_entry();
        entry.last_close_submitted_at_utc = Some(Utc::now().to_rfc3339());

        assert!(close_reprice_cooldown_remaining_secs(&config, &entry).is_some());
    }

    #[test]
    fn debit_close_reason_detects_expiration_risk() {
        let mut config = config_for_gate_tests();
        config.expiration_exit_days = 1;
        config.profit_target_close_fraction = 10.0;
        config.stop_loss_close_multiple = 0.0;
        let entry = debit_state_entry_expiring_in_days(1);

        assert_eq!(
            debit_spread_close_reason(&config, &entry, 1.0),
            Some("expiration_risk".to_string()),
        );
    }

    #[test]
    fn debit_close_reason_uses_manual_flatten_reason() {
        let mut config = config_for_gate_tests();
        config.force_flatten = true;
        let entry = debit_state_entry_expiring_in_days(30);

        assert_eq!(
            debit_spread_close_reason(&config, &entry, 1.0),
            Some("manual_flatten".to_string()),
        );
    }

    #[test]
    fn options_engine_strategy_has_stable_host_name() {
        let strategy = OptionsRuntimeStrategy;

        assert_eq!(strategy.name(), "options_engine");
    }

    #[test]
    fn naked_option_order_builders_use_simple_open_and_close_orders() {
        let candidate = naked_option_candidate();
        let trader_id = TraderId::from("TRADER-001");
        let client_id = ClientId::from(ALPACA_CLIENT_ID);
        let strategy_id = StrategyId::from(STRATEGY_FAMILY);

        let open = build_naked_option_submit_order(
            &candidate,
            "open-list-1",
            2,
            trader_id,
            Some(client_id),
            strategy_id,
        )
        .unwrap();

        assert_eq!(open.client_order_id, ClientOrderId::from("open-list-1"));
        assert_eq!(open.order_init.order_side, OrderSide::Sell);
        assert!(!open.order_init.reduce_only);
        assert_eq!(open.order_init.quantity, Quantity::new(2.0, 0));
        assert_eq!(open.order_init.price, Some(Price::new(0.71, 2)));

        let mut entry = state_entry();
        entry.strategy = naked_option_strategy_name(crate::strategy::NakedOptionKind::Put).into();
        entry.short_symbol = candidate.short.symbol.clone();
        entry.long_symbol.clear();
        entry.quantity = 2;
        let quote = CloseQuote {
            short_ask: 0.92,
            long_bid: 0.0,
            short_call_ask: None,
            long_call_bid: None,
            debit: 0.92,
        };

        let close = build_naked_option_close_order(
            &entry,
            &quote,
            "close-list-1",
            trader_id,
            Some(client_id),
            strategy_id,
        )
        .unwrap();

        assert_eq!(close.client_order_id, ClientOrderId::from("close-list-1"));
        assert_eq!(close.order_init.order_side, OrderSide::Buy);
        assert!(close.order_init.reduce_only);
        assert_eq!(close.order_init.quantity, Quantity::new(2.0, 0));
        assert_eq!(close.order_init.price, Some(Price::new(0.92, 2)));
    }

    fn state_entry() -> StrategyStateEntry {
        StrategyStateEntry {
            trade_date: "2026-05-04".to_string(),
            underlying: "SPY".to_string(),
            strategy: credit_spread_strategy_name(CreditSpreadKind::Put).to_string(),
            order_list_id: "open-list-1".to_string(),
            short_symbol: "SPY260512P00708000".to_string(),
            long_symbol: "SPY260512P00705000".to_string(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: 0.50,
            debit: None,
            score: 60.0,
            parent_order_id: Some("open-parent-1".to_string()),
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            close_attempts: 0,
            last_close_submitted_at_utc: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: "2026-05-04T14:00:00Z".to_string(),
            closed_at_utc: None,
        }
    }

    fn debit_state_entry_expiring_in_days(days: i64) -> StrategyStateEntry {
        let expiration = (Utc::now().date_naive() + chrono::Duration::days(days))
            .format("%y%m%d")
            .to_string();
        let mut entry = state_entry();
        entry.strategy =
            debit_spread_strategy_name(crate::strategy::DebitSpreadKind::Call).to_string();
        entry.short_symbol = format!("SPY{expiration}C00713000");
        entry.long_symbol = format!("SPY{expiration}C00710000");
        entry.credit = 0.0;
        entry.debit = Some(1.00);
        entry
    }

    fn naked_option_candidate() -> NakedOptionCandidate {
        NakedOptionCandidate {
            short: crate::strategy::ScoredContract {
                symbol: "SPY260512P00708000".to_string(),
                expiration_date: "2026-05-12".to_string(),
                dte: 7,
                strike: 708.0,
                bid: 0.71,
                ask: 0.92,
                delta_abs: 0.16,
                spread_pct: 0.08,
                bid_size: 10,
                ask_size: 10,
                volume: 100,
                open_interest: 1_200,
                implied_volatility: Some(0.22),
                metrics: None,
            },
            credit: 0.71,
            capital_requirement_model:
                crate::strategy::OptionCapitalRequirementModel::CashSecuredPut,
            estimated_buying_power_requirement: 70_800.0,
            buying_power_usage_pct: Some(0.0708),
            return_on_buying_power: 0.001003,
            score: 72.0,
        }
    }
}
