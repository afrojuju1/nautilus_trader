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

//! Management runtime for the Alpaca options-engine strategy slice.
//!
//! This module owns the process loop and broker orchestration used by the installed
//! `alpaca-options-engine` binary for existing-entry management. New entries are owned by
//! the Nautilus live-node strategy path.

use std::{collections::BTreeSet, env, time::Duration};

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaOrder, ListActivitiesRequest, OptionSnapshotsRequest},
    },
    management::{credit_spread_close_reason, days_to_expiration, recorded_age_secs},
    options_entry_admission::EntryAdmissionConfig,
    options_runtime::OptionsEngineConfig,
    performance::{EntryOrderIds, collect_order_ids, entry_performance},
    runtime::{StrategyState, StrategyStateEntry, emit_operator_event},
    state_reconciliation::reconcile_strategy_state,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use nautilus_core::UUID4;
use serde_json::json;
use tokio::time::sleep;

mod submission;

#[cfg(test)]
use crate::state_reconciliation::{ReconciliationAction, reconciliation_action};
use submission::{cancel_parent_order_by_id, lookup_parent_order_snapshot, submit_close_entry};

const DEFAULT_EVENT_TIMEOUT_SECS: u64 = 20;
const STRATEGY_FAMILY: &str = "ALPACA-OPTIONS-ENGINE";
#[derive(Clone, Debug)]
struct SubmitOutcome {
    accepted: usize,
    parent_order_id: Option<String>,
}

/// Runs the Alpaca options-engine management runtime until configured shutdown.
///
/// # Errors
///
/// Returns an error if configuration parsing, broker I/O, management submission, cancellation,
/// state persistence, or execution-client lifecycle operations fail.
pub async fn run_options_engine() -> anyhow::Result<()> {
    let config = OptionsEngineConfig::from_env_with_storage().await?;
    let mut state = config.load_strategy_state().await?;

    println!(
        "options_engine_management: underlyings={} strategies={} submit_enabled={} manage_enabled={} close_enabled={} kill_switch={} quantity={} state_path={}",
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
            "candidate_ledger_max_candidates": config.candidate_ledger_max_candidates,
            "entry_owner": "nautilus_strategy",
        }),
    );

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    let http_client = AlpacaHttpClient::from_data_config(&data_config)?;

    if reconcile_strategy_state(&http_client, &mut state)
        .await?
        .changed
    {
        config.save_strategy_state(&state).await?;
    }

    let mut iteration = 1_u64;
    loop {
        let trade_date = EntryAdmissionConfig::from_engine_config(&config).market_trade_date();
        println!("management_iteration={iteration} trade_date={trade_date}");
        emit_operator_event(
            "management_iteration",
            json!({
                "iteration": iteration,
                "trade_date": trade_date,
            }),
        );

        if manage_existing_entries(&http_client, &data_config, &config, &mut state).await? {
            config.save_strategy_state(&state).await?;
        }

        if config.max_iterations != 0 && iteration >= config.max_iterations {
            break;
        }

        iteration = iteration.saturating_add(1);
        sleep(Duration::from_secs(config.interval_secs)).await;
    }

    Ok(())
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
            } else {
                println!(
                    "manage: close_order_missing order_list_id={}",
                    close_order_list_id
                );
                emit_operator_event(
                    "management_block",
                    json!({
                        "action": "close_blocked",
                        "reason": "close_order_missing",
                        "underlying": entry.underlying,
                        "strategy": entry.strategy,
                        "order_list_id": entry.order_list_id,
                        "close_order_list_id": close_order_list_id,
                    }),
                );
                entry.clear_close_submission();
                changed = true;
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
                entry.entry_debit().unwrap_or_default(),
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
    let Some(storage) = &config.storage_repository else {
        anyhow::bail!("storage is not connected");
    };
    let append = crate::storage::append_performance_ledger_record(
        storage,
        config.storage_account_id(),
        &ledger_date,
        &performance,
        None,
    )
    .await?;

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
    let entry_debit = entry.entry_debit()?;
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
                entry.entry_debit(),
                Some(close_credit),
                entry.entry_debit().map(|debit| close_credit - debit),
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

fn close_order_list_id(entry: &StrategyStateEntry) -> String {
    format!(
        "options-engine-close-{}-{}-{}",
        entry.trade_date,
        entry.underlying,
        UUID4::new()
    )
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use chrono::NaiveTime;
    use nautilus_model::{
        enums::OrderSide,
        identifiers::{ClientId, ClientOrderId, StrategyId, TraderId},
        types::{Price, Quantity},
    };

    use super::*;
    use crate::{
        candidate_engine::{
            CreditSpreadKind, DebitSpreadKind, DebitSpreadScannerConfig, IronCondorScannerConfig,
            NakedOptionCandidate, NakedOptionKind, NakedOptionScannerConfig,
            OptionCapitalRequirementModel, PutCreditScannerConfig, ScoredContract,
        },
        common::consts::ALPACA_CLIENT_ID,
        fleet::{AccountConfig, FleetConfig, FleetSection, ResolvedFleetConfig},
        options_entry_admission::{
            EntryAdmissionSnapshot, EntryGateDecision,
            UNCOVERED_OPTION_PERMISSION_REJECTION_REASON, admission_block_reason,
            entry_gate_decision, submission_block_for_selected as admission_block_for_selected,
        },
        options_runtime::{SelectedNakedOptionEntry, SelectedOptionsEntry},
        runtime::{
            credit_spread_strategy_name, debit_spread_strategy_name, naked_option_strategy_name,
            save_strategy_state_atomic,
        },
        storage::STORAGE_SCHEMA_DEFAULT,
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
            candidate_ledger_max_candidates: 10,
            scanner: PutCreditScannerConfig::default(),
            iron_condor_scanner: IronCondorScannerConfig::default(),
            debit_scanner: DebitSpreadScannerConfig::default(),
            naked_scanner: NakedOptionScannerConfig::default(),
            naked_1_3dte_scanner: NakedOptionScannerConfig::default(),
            fleet: None,
            fleet_account_id: None,
            fleet_policy_blocks: Vec::new(),
            storage_repository: None,
            storage_database_url: None,
            storage_schema: STORAGE_SCHEMA_DEFAULT.to_string(),
            storage_account_id: None,
        }
    }

    #[test]
    fn entry_gate_allows_inside_window() {
        let config = config_for_gate_tests();
        let now = DateTime::parse_from_rfc3339("2026-05-04T14:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let admission_config = EntryAdmissionConfig::from_engine_config(&config);

        assert_eq!(
            entry_gate_decision(&admission_config, now),
            EntryGateDecision::Continue
        );
    }

    #[test]
    fn entry_gate_blocks_outside_window_without_credentials() {
        let config = config_for_gate_tests();
        let now = DateTime::parse_from_rfc3339("2026-05-04T21:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let admission_config = EntryAdmissionConfig::from_engine_config(&config);

        assert_eq!(
            entry_gate_decision(&admission_config, now),
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
        let admission_config = EntryAdmissionConfig::from_engine_config(&config);

        assert_eq!(
            entry_gate_decision(&admission_config, now),
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
    fn close_quote_cushion_splits_across_iron_condor_short_legs() {
        let quote = CloseQuote {
            short_ask: 0.70,
            long_bid: 0.20,
            short_call_ask: Some(0.80),
            long_call_bid: Some(0.30),
            debit: 1.00,
        };

        let cushioned = quote.with_price_cushion(0.04);

        assert!((cushioned.short_ask - 0.72).abs() < 1e-9);
        assert!((cushioned.long_bid - 0.20).abs() < 1e-9);
        assert!((cushioned.short_call_ask.unwrap() - 0.82).abs() < 1e-9);
        assert!((cushioned.long_call_bid.unwrap() - 0.30).abs() < 1e-9);
        assert!((cushioned.debit - 1.04).abs() < 1e-9);
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
    fn fleet_underlying_limit_allows_same_underlying_below_limit() {
        let (config, dir) = config_with_fleet_underlying_limit(2);
        let selected = SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
            underlying: "SPY".to_string(),
            kind: NakedOptionKind::Call,
            candidate: naked_option_candidate(),
        });
        let admission_config = EntryAdmissionConfig::from_engine_config(&config);

        assert!(
            admission_block_for_selected(
                &admission_config,
                &StrategyState::default(),
                &selected,
                "2026-05-04",
                &EntryAdmissionSnapshot::default(),
            )
            .is_none()
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fleet_underlying_limit_blocks_at_configured_limit() {
        let (config, dir) = config_with_fleet_underlying_limit(1);
        let selected = SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
            underlying: "SPY".to_string(),
            kind: NakedOptionKind::Call,
            candidate: naked_option_candidate(),
        });
        let admission_config = EntryAdmissionConfig::from_engine_config(&config);

        let block = admission_block_for_selected(
            &admission_config,
            &StrategyState::default(),
            &selected,
            "2026-05-04",
            &EntryAdmissionSnapshot::default(),
        )
        .unwrap();

        assert_eq!(block.reason, "fleet_max_active_entries_per_underlying");
        assert_eq!(block.current, Some(1));
        assert_eq!(block.limit, Some(1));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn admission_block_reason_classifies_underlying_position_conflicts() {
        let reasons =
            vec!["existing open option position on underlying SLV: SLV260515P00067000".to_string()];

        assert_eq!(
            admission_block_reason(&reasons),
            "existing_underlying_position",
        );
    }

    #[test]
    fn admission_block_reason_classifies_working_underlying_orders() {
        let reasons =
            vec!["working order already references underlying SLV: SLV260515P00067000".to_string()];

        assert_eq!(admission_block_reason(&reasons), "working_underlying_order");
    }

    #[test]
    fn admission_block_reason_prefers_account_state_blocks() {
        let reasons = vec![
            "existing open option position on underlying SLV: SLV260515P00067000".to_string(),
            "account trading_blocked is true".to_string(),
        ];

        assert_eq!(admission_block_reason(&reasons), "account_not_tradable");
    }

    #[test]
    fn broker_permission_guard_blocks_naked_after_uncovered_rejection() {
        let mut rejected = state_entry();
        rejected.strategy = naked_option_strategy_name(NakedOptionKind::Call).to_string();
        rejected.long_symbol.clear();
        rejected.mark_canceled();
        rejected.close_reason = Some(UNCOVERED_OPTION_PERMISSION_REJECTION_REASON.to_string());
        let state = StrategyState {
            entries: vec![rejected],
        };
        let selected = SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
            underlying: "SPY".to_string(),
            kind: NakedOptionKind::Call,
            candidate: naked_option_candidate(),
        });

        let admission_config = EntryAdmissionConfig::from_engine_config(&config_for_gate_tests());
        let block = admission_block_for_selected(
            &admission_config,
            &state,
            &selected,
            "2026-05-04",
            &EntryAdmissionSnapshot::default(),
        )
        .unwrap();

        assert_eq!(block.reason, "broker_uncovered_option_permission");
        assert_eq!(block.details, vec!["alpaca_http_40310000"]);
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
    fn naked_option_order_builders_use_simple_open_and_close_orders() {
        let candidate = naked_option_candidate();
        let trader_id = TraderId::from("TRADER-001");
        let client_id = ClientId::from(ALPACA_CLIENT_ID);
        let strategy_id = StrategyId::from(STRATEGY_FAMILY);
        let selected = SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
            underlying: "SPY".to_string(),
            kind: NakedOptionKind::Put,
            candidate: candidate.clone(),
        });

        let open = submission::tests_support::build_open_order(
            &selected,
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
        entry.strategy = naked_option_strategy_name(NakedOptionKind::Put).into();
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

        let close = submission::tests_support::build_close_order(
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

    fn config_with_fleet_underlying_limit(limit: usize) -> (OptionsEngineConfig, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "nautilus-alpaca-fleet-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ));
        let state_path = dir.join("paper-main-state.json");
        save_strategy_state_atomic(
            &state_path,
            &StrategyState {
                entries: vec![state_entry()],
            },
        )
        .unwrap();

        let mut config = config_for_gate_tests();
        config.fleet = Some(ResolvedFleetConfig {
            path: dir.join("fleet.toml"),
            registry_dir: dir.clone(),
            config: FleetConfig {
                fleet: FleetSection {
                    max_active_entries_per_underlying: Some(limit),
                    ..FleetSection::default()
                },
                accounts: vec![AccountConfig {
                    id: "paper-main".to_string(),
                    enabled: true,
                    state_path: Some(state_path),
                    ..AccountConfig::default()
                }],
            },
        });
        (config, dir)
    }

    fn debit_state_entry_expiring_in_days(days: i64) -> StrategyStateEntry {
        let expiration = (Utc::now().date_naive() + chrono::Duration::days(days))
            .format("%y%m%d")
            .to_string();
        let mut entry = state_entry();
        entry.strategy = debit_spread_strategy_name(DebitSpreadKind::Call).to_string();
        entry.short_symbol = format!("SPY{expiration}C00713000");
        entry.long_symbol = format!("SPY{expiration}C00710000");
        entry.credit = 0.0;
        entry.debit = Some(1.00);
        entry
    }

    fn naked_option_candidate() -> NakedOptionCandidate {
        NakedOptionCandidate {
            short: ScoredContract {
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
            capital_requirement_model: OptionCapitalRequirementModel::CashSecuredPut,
            estimated_buying_power_requirement: 70_800.0,
            buying_power_usage_pct: Some(0.0708),
            return_on_buying_power: 0.001003,
            score: 72.0,
        }
    }
}
