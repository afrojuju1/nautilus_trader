//! Broker submission helpers for the Alpaca options engine.

use std::{cell::RefCell, env, rc::Rc, str::FromStr, time::Duration};

use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{
        ExecutionEvent,
        execution::{SubmitOrder, SubmitOrderList},
    },
};
use nautilus_core::time::get_atomic_clock_realtime;
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType, OrderSide},
    events::OrderEventAny,
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId, Venue,
    },
    types::{Price, Quantity},
};
use tokio::{
    sync::mpsc,
    time::{Instant, timeout},
};

use crate::{
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::AlpacaExecClientConfig,
    execution::AlpacaExecutionClient,
    http::{client::AlpacaHttpClient, error::Error, models::AlpacaOrder},
    options_runtime::{OptionsEngineConfig, SelectedOptionsEntry},
    order_plan::{OrderLegSpec, SubmitPlan},
    runtime::StrategyStateEntry,
    submit::{
        MlegSubmitLeg, MlegSubmitOrderListRequest, SimpleSubmitOrderRequest,
        build_mleg_submit_order_list, build_simple_submit_order,
    },
};

use super::{CloseQuote, DEFAULT_EVENT_TIMEOUT_SECS, STRATEGY_FAMILY, SubmitOutcome};

pub(super) async fn submit_selected_entry(
    entry: &SelectedOptionsEntry,
    order_list_id: &str,
    quantity: u64,
    config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_order_plan(
        selected_entry_submit_plan(entry, order_list_id, quantity)?,
        config.cancel_after_accept,
    )
    .await
}

pub(super) async fn submit_close_entry(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    _config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_order_plan(close_entry_submit_plan(entry, quote, order_list_id), false).await
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

    let (accepted, rejected, rejection_reasons) =
        collect_execution_events(&mut rx, expected_events).await;
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
        rejection_reasons,
    })
}

async fn submit_order_plan(
    plan: SubmitPlan,
    cancel_after_accept: bool,
) -> anyhow::Result<SubmitOutcome> {
    let order_list_id = plan.client_order_id.clone();
    let expected_events = plan.expected_events();
    submit_with_execution_session(
        &order_list_id,
        expected_events,
        cancel_after_accept,
        |client, trader_id, client_id, strategy_id| {
            if plan.is_single_leg() {
                client.submit_order(build_simple_submit_order_from_plan(
                    &plan,
                    trader_id,
                    client_id,
                    strategy_id,
                )?)?;
            } else {
                client.submit_order_list(build_mleg_submit_order_list_from_plan(
                    &plan,
                    trader_id,
                    client_id,
                    strategy_id,
                )?)?;
            }
            Ok(())
        },
    )
    .await
}

fn selected_entry_submit_plan(
    entry: &SelectedOptionsEntry,
    order_list_id: &str,
    quantity: u64,
) -> anyhow::Result<SubmitPlan> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }
    Ok(match entry {
        SelectedOptionsEntry::Credit(entry) => SubmitPlan::new(
            order_list_id,
            vec![
                OrderLegSpec::new(
                    "short",
                    &entry.candidate.short.symbol,
                    OrderSide::Sell,
                    quantity,
                    entry.candidate.short.bid,
                    false,
                ),
                OrderLegSpec::new(
                    "long",
                    &entry.candidate.long.symbol,
                    OrderSide::Buy,
                    quantity,
                    entry.candidate.long.ask,
                    false,
                ),
            ],
        ),
        SelectedOptionsEntry::IronCondor(entry) => SubmitPlan::new(
            order_list_id,
            vec![
                OrderLegSpec::new(
                    "short-put",
                    &entry.candidate.put.short.symbol,
                    OrderSide::Sell,
                    quantity,
                    entry.candidate.put.short.bid,
                    false,
                ),
                OrderLegSpec::new(
                    "long-put",
                    &entry.candidate.put.long.symbol,
                    OrderSide::Buy,
                    quantity,
                    entry.candidate.put.long.ask,
                    false,
                ),
                OrderLegSpec::new(
                    "short-call",
                    &entry.candidate.call.short.symbol,
                    OrderSide::Sell,
                    quantity,
                    entry.candidate.call.short.bid,
                    false,
                ),
                OrderLegSpec::new(
                    "long-call",
                    &entry.candidate.call.long.symbol,
                    OrderSide::Buy,
                    quantity,
                    entry.candidate.call.long.ask,
                    false,
                ),
            ],
        ),
        SelectedOptionsEntry::Debit(entry) => SubmitPlan::new(
            order_list_id,
            vec![
                OrderLegSpec::new(
                    "long",
                    &entry.candidate.long.symbol,
                    OrderSide::Buy,
                    quantity,
                    entry.candidate.long.ask,
                    false,
                ),
                OrderLegSpec::new(
                    "short",
                    &entry.candidate.short.symbol,
                    OrderSide::Sell,
                    quantity,
                    entry.candidate.short.bid,
                    false,
                ),
            ],
        ),
        SelectedOptionsEntry::NakedOption(entry) => SubmitPlan::new(
            order_list_id,
            vec![OrderLegSpec::new(
                "entry",
                &entry.candidate.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.short.bid,
                false,
            )],
        ),
    })
}

fn close_entry_submit_plan(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    client_order_id: &str,
) -> SubmitPlan {
    let mut legs = vec![OrderLegSpec::new(
        "short-close",
        &entry.short_symbol,
        OrderSide::Buy,
        entry.quantity,
        quote.short_ask,
        true,
    )];
    if !entry.long_symbol.is_empty() {
        legs.push(OrderLegSpec::new(
            "long-close",
            &entry.long_symbol,
            OrderSide::Sell,
            entry.quantity,
            quote.long_bid,
            true,
        ));
    }
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
        legs.push(OrderLegSpec::new(
            "short-call-close",
            short_call_symbol,
            OrderSide::Buy,
            entry.quantity,
            short_call_ask,
            true,
        ));
        legs.push(OrderLegSpec::new(
            "long-call-close",
            long_call_symbol,
            OrderSide::Sell,
            entry.quantity,
            long_call_bid,
            true,
        ));
    }
    SubmitPlan::new(client_order_id, legs)
}

fn build_simple_submit_order_from_plan(
    plan: &SubmitPlan,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrder> {
    let Some(leg) = plan.legs.first() else {
        anyhow::bail!("single-leg submit plan missing leg");
    };
    build_simple_submit_order(SimpleSubmitOrderRequest {
        trader_id,
        client_id,
        strategy_id,
        client_order_id: ClientOrderId::from(plan.client_order_id.as_str()),
        instrument_id: alpaca_instrument_id(&leg.symbol)?,
        order_side: leg.side,
        quantity: Quantity::new(leg.quantity as f64, 0),
        limit_price: Price::new(leg.limit_price, 2),
        reduce_only: leg.reduce_only,
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

fn build_mleg_submit_order_list_from_plan(
    plan: &SubmitPlan,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    let order_list_id = OrderListId::from(plan.client_order_id.as_str());
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: plan
            .legs
            .iter()
            .map(|leg| {
                Ok(MlegSubmitLeg {
                    client_order_id: ClientOrderId::from(
                        format!("{}-{}", plan.client_order_id, leg.label).as_str(),
                    ),
                    instrument_id: alpaca_instrument_id(&leg.symbol)?,
                    order_side: leg.side,
                    quantity: Quantity::new(leg.quantity as f64, 0),
                    limit_price: Price::new(leg.limit_price, 2),
                    reduce_only: leg.reduce_only,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

async fn collect_execution_events(
    rx: &mut mpsc::UnboundedReceiver<ExecutionEvent>,
    leg_count: usize,
) -> (usize, usize, Vec<String>) {
    let mut accepted = 0;
    let mut rejected = 0;
    let mut rejection_reasons = Vec::new();
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
                OrderEventAny::Rejected(_) => {
                    rejected += 1;
                    if let Some(reason) = order_event_reason(&order_event) {
                        rejection_reasons.push(reason);
                    }
                }
                _ => {}
            }
            if accepted + rejected >= leg_count {
                break;
            }
        }
    }

    (accepted, rejected, rejection_reasons)
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

fn order_event_reason(order_event: &OrderEventAny) -> Option<String> {
    order_event
        .clone()
        .into_boxed()
        .reason()
        .map(|reason| reason.to_string())
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

pub(super) async fn lookup_parent_order_snapshot(
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
    let Some(order_id) = parent_order_id else {
        return Ok(());
    };
    let client = AlpacaHttpClient::from_exec_config(config)?;
    cancel_parent_order_by_id(&client, Some(order_id)).await
}

pub(super) async fn cancel_parent_order_by_id(
    client: &AlpacaHttpClient,
    parent_order_id: Option<&str>,
) -> anyhow::Result<()> {
    let Some(order_id) = parent_order_id else {
        return Ok(());
    };
    match client.cancel_order(order_id).await {
        Ok(()) => Ok(()),
        Err(Error::HttpStatus { status, .. }) if status == 404 => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn alpaca_instrument_id(symbol: &str) -> anyhow::Result<InstrumentId> {
    Ok(InstrumentId::from_str(&format!("{symbol}.{ALPACA_VENUE}"))?)
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
pub(crate) mod tests_support {
    use super::*;

    pub(crate) fn build_open_order(
        entry: &SelectedOptionsEntry,
        client_order_id: &str,
        quantity: u64,
        trader_id: TraderId,
        client_id: Option<ClientId>,
        strategy_id: StrategyId,
    ) -> anyhow::Result<SubmitOrder> {
        let plan = selected_entry_submit_plan(entry, client_order_id, quantity)?;
        build_simple_submit_order_from_plan(&plan, trader_id, client_id, strategy_id)
    }

    pub(crate) fn build_close_order(
        entry: &StrategyStateEntry,
        quote: &CloseQuote,
        client_order_id: &str,
        trader_id: TraderId,
        client_id: Option<ClientId>,
        strategy_id: StrategyId,
    ) -> anyhow::Result<SubmitOrder> {
        let plan = close_entry_submit_plan(entry, quote, client_order_id);
        build_simple_submit_order_from_plan(&plan, trader_id, client_id, strategy_id)
    }
}
