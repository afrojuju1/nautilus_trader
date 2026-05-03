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

//! Account-engine runtime for the Alpaca index-credit strategy slice.
//!
//! This module owns the process loop and broker orchestration used by the installed
//! `alpaca-index-put-credit-entry` binary. The binary stays as a thin entrypoint so the live
//! runtime can be tested and evolved from library code.

use std::{cell::RefCell, env, rc::Rc, str::FromStr, time::Duration};

use crate::{
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    execution::AlpacaExecutionClient,
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{AlpacaOrder, OptionSnapshotsRequest},
    },
    index_credit::{IndexCreditConfig, SelectedEntry, select_index_credit_entry},
    management::credit_spread_close_reason,
    runtime::{
        StrategyState, StrategyStateEntry, credit_spread_strategy_name, emit_operator_event,
        load_strategy_state, save_strategy_state_atomic,
    },
    strategy::CreditSpreadKind,
    submit::{MlegSubmitLeg, MlegSubmitOrderListRequest, build_mleg_submit_order_list},
};
use chrono::{DateTime, Utc};
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
use serde_json::json;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, timeout},
};

const DEFAULT_EVENT_TIMEOUT_SECS: u64 = 20;
const STRATEGY_FAMILY: &str = "INDEX-PUT-CREDIT-ENTRY";

#[derive(Clone, Debug)]
struct SubmitOutcome {
    accepted: usize,
    rejected: usize,
    parent_order_id: Option<String>,
}

/// Runs the Alpaca index-credit account engine until configured shutdown.
///
/// # Errors
///
/// Returns an error if configuration parsing, broker I/O, selection, submission, cancellation,
/// state persistence, or execution-client lifecycle operations fail.
pub async fn run_index_credit_engine() -> anyhow::Result<()> {
    let config = IndexCreditConfig::from_env()?;
    let mut state = load_strategy_state(&config.state_path)?;

    println!(
        "index_credit_entry: underlyings={} strategies={} submit_enabled={} manage_enabled={} close_enabled={} kill_switch={} quantity={} state_path={}",
        config.underlyings.join(","),
        config
            .spread_kinds
            .iter()
            .map(|kind| strategy_name(*kind))
            .collect::<Vec<_>>()
            .join(","),
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
            "strategies": config.spread_kinds.iter().map(|kind| strategy_name(*kind)).collect::<Vec<_>>(),
            "submit_enabled": config.submit_enabled,
            "manage_enabled": config.manage_enabled,
            "close_enabled": config.close_enabled,
            "kill_switch": config.kill_switch,
            "quantity": config.quantity,
            "state_path": config.state_path.display().to_string(),
        }),
    );

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    let http_client = AlpacaHttpClient::from_data_config(&data_config)?;

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

        if manage_existing_entries(&http_client, &data_config, &config, &mut state).await? {
            save_strategy_state_atomic(&config.state_path, &state)?;
        }

        if config.kill_switch {
            println!("decision: skipped reason=kill_switch_enabled");
            emit_operator_event(
                "decision",
                json!({
                    "action": "skipped",
                    "reason": "kill_switch_enabled",
                    "trade_date": trade_date,
                }),
            );
        } else if !config.ignore_entry_window && !inside_entry_window(&config) {
            println!(
                "decision: skipped reason=outside_entry_window window={}-{} timezone={}",
                config.entry_start, config.entry_end, config.entry_timezone
            );
            emit_operator_event(
                "decision",
                json!({
                    "action": "skipped",
                    "reason": "outside_entry_window",
                    "window_start": config.entry_start.to_string(),
                    "window_end": config.entry_end.to_string(),
                    "timezone": config.entry_timezone.to_string(),
                    "trade_date": trade_date,
                }),
            );
        } else {
            let selected =
                select_index_credit_entry(&http_client, &data_config, &config, &state, &trade_date)
                    .await?;
            match selected {
                Some(entry) if config.submit_enabled => {
                    let order_list_id = order_list_id(&trade_date, &entry.underlying);
                    println!(
                        "decision: submit underlying={} short={} long={} credit={:.2} ror={:.1}% score={:.1} order_list_id={}",
                        entry.underlying,
                        entry.candidate.short.symbol,
                        entry.candidate.long.symbol,
                        entry.candidate.credit,
                        entry.candidate.return_on_risk * 100.0,
                        entry.candidate.score,
                        order_list_id,
                    );
                    emit_operator_event(
                        "decision",
                        json!({
                            "action": "submit",
                            "underlying": &entry.underlying,
                            "strategy": strategy_name(entry.kind),
                            "short_symbol": &entry.candidate.short.symbol,
                            "long_symbol": &entry.candidate.long.symbol,
                            "credit": entry.candidate.credit,
                            "return_on_risk": entry.candidate.return_on_risk,
                            "score": entry.candidate.score,
                            "order_list_id": &order_list_id,
                            "trade_date": trade_date,
                        }),
                    );
                    let outcome =
                        submit_entry(&entry, &order_list_id, config.quantity, &config).await?;
                    if outcome.accepted > 0 {
                        state.record_submission(
                            trade_date.clone(),
                            entry.underlying,
                            entry.kind,
                            config.quantity,
                            order_list_id,
                            &entry.candidate,
                            outcome.parent_order_id.clone(),
                        );
                        save_strategy_state_atomic(&config.state_path, &state)?;
                    }
                    println!(
                        "submit_result: accepted={} rejected={}",
                        outcome.accepted, outcome.rejected
                    );
                    emit_operator_event(
                        "submit_result",
                        json!({
                            "accepted": outcome.accepted,
                            "rejected": outcome.rejected,
                            "parent_order_id": outcome.parent_order_id,
                        }),
                    );
                }
                Some(entry) => {
                    println!(
                        "decision: dry_run underlying={} short={} long={} credit={:.2} ror={:.1}% score={:.1} reason=submission_disabled",
                        entry.underlying,
                        entry.candidate.short.symbol,
                        entry.candidate.long.symbol,
                        entry.candidate.credit,
                        entry.candidate.return_on_risk * 100.0,
                        entry.candidate.score,
                    );
                    emit_operator_event(
                        "decision",
                        json!({
                            "action": "dry_run",
                            "reason": "submission_disabled",
                            "underlying": &entry.underlying,
                            "strategy": strategy_name(entry.kind),
                            "short_symbol": &entry.candidate.short.symbol,
                            "long_symbol": &entry.candidate.long.symbol,
                            "credit": entry.candidate.credit,
                            "return_on_risk": entry.candidate.return_on_risk,
                            "score": entry.candidate.score,
                            "trade_date": trade_date,
                        }),
                    );
                }
                None => {
                    println!("decision: no_entry");
                    emit_operator_event(
                        "decision",
                        json!({
                            "action": "no_entry",
                            "trade_date": trade_date,
                        }),
                    );
                }
            }
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
    config: &IndexCreditConfig,
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
                    entry.closed = true;
                    entry.close_parent_order_id = order.id;
                    entry.closed_at_utc = Some(Utc::now().to_rfc3339());
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
                    entry.canceled = true;
                    changed = true;
                }
            }
            continue;
        }

        if matches!(
            entry_order.status.as_deref(),
            Some("canceled" | "expired" | "rejected")
        ) {
            entry.canceled = true;
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
            continue;
        };
        let close_reason = close_reason(config, entry, close_quote.debit);
        println!(
            "manage: position underlying={} strategy={} close_debit={:.2} entry_credit={:.2} reason={}",
            entry.underlying,
            entry.strategy,
            close_quote.debit,
            entry.credit,
            close_reason.as_deref().unwrap_or("none"),
        );

        let Some(close_reason) = close_reason else {
            continue;
        };
        if !(config.manage_enabled && config.close_enabled) {
            continue;
        }

        let close_order_list_id = close_order_list_id(entry);
        let outcome = submit_close_entry(entry, &close_quote, &close_order_list_id, config).await?;
        if outcome.accepted > 0 {
            entry.close_order_list_id = Some(close_order_list_id);
            entry.close_parent_order_id = outcome.parent_order_id;
            entry.close_reason = Some(close_reason);
            changed = true;
        }
    }

    Ok(changed)
}

#[derive(Clone, Copy, Debug)]
struct CloseQuote {
    short_ask: f64,
    long_bid: f64,
    debit: f64,
}

async fn close_quote(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    entry: &StrategyStateEntry,
) -> anyhow::Result<Option<CloseQuote>> {
    let mut request = OptionSnapshotsRequest::for_symbols([
        entry.short_symbol.clone(),
        entry.long_symbol.clone(),
    ]);
    request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&request).await?.snapshots;
    let short_quote = snapshots
        .get(&entry.short_symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref());
    let long_quote = snapshots
        .get(&entry.long_symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref());
    let (Some(short_ask), Some(long_bid)) = (
        short_quote.and_then(|quote| quote.ask_price),
        long_quote.and_then(|quote| quote.bid_price),
    ) else {
        return Ok(None);
    };
    let debit = short_ask - long_bid;
    if debit <= 0.0 {
        return Ok(None);
    }
    Ok(Some(CloseQuote {
        short_ask,
        long_bid,
        debit,
    }))
}

fn close_reason(
    config: &IndexCreditConfig,
    entry: &StrategyStateEntry,
    close_debit: f64,
) -> Option<String> {
    credit_spread_close_reason(&config.management_config(), entry, close_debit)
        .map(ToString::to_string)
}

async fn submit_entry(
    entry: &SelectedEntry,
    order_list_id: &str,
    quantity: u64,
    config: &IndexCreditConfig,
) -> anyhow::Result<SubmitOutcome> {
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

    let cmd = build_submit_order_list(
        entry,
        order_list_id,
        quantity,
        trader_id,
        Some(client_id),
        strategy_id,
    )?;
    client.submit_order_list(cmd)?;

    let (accepted, rejected) = collect_execution_events(&mut rx, 2).await;
    let parent_order_id = lookup_parent_order(&exec_config, order_list_id).await?;
    if config.cancel_after_accept && accepted > 0 {
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

async fn submit_close_entry(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    _config: &IndexCreditConfig,
) -> anyhow::Result<SubmitOutcome> {
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

    let cmd = build_close_submit_order_list(
        entry,
        quote,
        order_list_id,
        trader_id,
        Some(client_id),
        strategy_id,
    )?;
    client.submit_order_list(cmd)?;

    let (accepted, rejected) = collect_execution_events(&mut rx, 2).await;
    let parent_order_id = lookup_parent_order(&exec_config, order_list_id).await?;

    client.disconnect().await?;
    client.stop()?;

    Ok(SubmitOutcome {
        accepted,
        rejected,
        parent_order_id,
    })
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

fn build_close_submit_order_list(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    let order_list_id = OrderListId::from(order_list_id);
    let short_client_id = ClientOrderId::from(format!("{order_list_id}-short-close").as_str());
    let long_client_id = ClientOrderId::from(format!("{order_list_id}-long-close").as_str());
    let quantity = Quantity::new(entry.quantity as f64, 0);
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: vec![
            MlegSubmitLeg {
                client_order_id: short_client_id,
                instrument_id: alpaca_instrument_id(&entry.short_symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(quote.short_ask, 2),
                reduce_only: true,
            },
            MlegSubmitLeg {
                client_order_id: long_client_id,
                instrument_id: alpaca_instrument_id(&entry.long_symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(quote.long_bid, 2),
                reduce_only: true,
            },
        ],
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
            "ALPACA_INDEX_PUT_CREDIT_EVENT_TIMEOUT_SECS",
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
        "index-credit-entry-{trade_date}-{underlying}-{}",
        UUID4::new()
    )
}

fn close_order_list_id(entry: &StrategyStateEntry) -> String {
    format!(
        "index-credit-close-{}-{}-{}",
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

fn inside_entry_window(config: &IndexCreditConfig) -> bool {
    let now = Utc::now().with_timezone(&config.entry_timezone).time();
    config.entry_start <= now && now <= config.entry_end
}

fn market_trade_date(config: &IndexCreditConfig) -> String {
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
