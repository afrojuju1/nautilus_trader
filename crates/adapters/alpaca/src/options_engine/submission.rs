//! Broker submission helpers for the Alpaca options engine.

use std::{cell::RefCell, env, rc::Rc, str::FromStr, time::Duration};

use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    factories::OrderFactory,
    live::{clock::LiveClock, runner::replace_exec_event_sender},
    messages::{
        ExecutionEvent,
        execution::{SubmitOrder, SubmitOrderList},
    },
};
use nautilus_core::{UUID4, time::get_atomic_clock_realtime};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType, OrderSide, TimeInForce},
    events::{OrderEventAny, OrderInitialized},
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId, Venue,
    },
    orders::{OrderAny, OrderList},
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
    runtime::StrategyStateEntry,
};

use super::{CloseQuote, DEFAULT_EVENT_TIMEOUT_SECS, STRATEGY_FAMILY, SubmitOutcome};

pub(super) async fn submit_selected_entry(
    entry: &SelectedOptionsEntry,
    order_list_id: &str,
    quantity: u64,
    config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_with_execution_session(
        order_list_id,
        config.cancel_after_accept,
        |client, trader_id, client_id, strategy_id| {
            let orders =
                selected_entry_orders(entry, order_list_id, quantity, trader_id, strategy_id)?;
            submit_orders(
                client,
                order_list_id,
                orders,
                trader_id,
                client_id,
                strategy_id,
            )
        },
    )
    .await
}

pub(super) async fn submit_close_entry(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    _config: &OptionsEngineConfig,
) -> anyhow::Result<SubmitOutcome> {
    submit_with_execution_session(
        order_list_id,
        false,
        |client, trader_id, client_id, strategy_id| {
            let orders = close_entry_orders(entry, quote, order_list_id, trader_id, strategy_id)?;
            submit_orders(
                client,
                order_list_id,
                orders,
                trader_id,
                client_id,
                strategy_id,
            )
        },
    )
    .await
}

async fn submit_with_execution_session<F>(
    order_list_id: &str,
    cancel_after_accept: bool,
    submit: F,
) -> anyhow::Result<SubmitOutcome>
where
    F: FnOnce(
        &mut AlpacaExecutionClient,
        TraderId,
        Option<ClientId>,
        StrategyId,
    ) -> anyhow::Result<usize>,
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

    let expected_events = submit(&mut client, trader_id, Some(client_id), strategy_id)?;

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

fn submit_orders(
    client: &mut AlpacaExecutionClient,
    order_list_id: &str,
    mut orders: Vec<OrderAny>,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<usize> {
    if orders.is_empty() {
        anyhow::bail!("cannot submit empty order set");
    }

    let expected_events = orders.len();
    if orders.len() == 1 {
        let order = orders.remove(0);
        client.submit_order(submit_order_from_order(&order, trader_id, client_id))?;
    } else {
        client.submit_order_list(submit_order_list_from_orders(
            order_list_id,
            &mut orders,
            trader_id,
            client_id,
            strategy_id,
        )?)?;
    }
    Ok(expected_events)
}

fn submit_order_from_order(
    order: &OrderAny,
    trader_id: TraderId,
    client_id: Option<ClientId>,
) -> SubmitOrder {
    SubmitOrder::from_order(
        order,
        trader_id,
        client_id,
        None,
        UUID4::new(),
        get_atomic_clock_realtime().get_time_ns(),
    )
}

fn submit_order_list_from_orders(
    order_list_id: &str,
    orders: &mut [OrderAny],
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    if orders.is_empty() {
        anyhow::bail!("cannot build empty order list");
    }

    let order_list_id = OrderListId::from(order_list_id);
    for order in orders.iter_mut() {
        order.set_order_list_id(order_list_id);
    }

    let ts_init = get_atomic_clock_realtime().get_time_ns();
    let order_list = OrderList::from_orders(orders, ts_init);
    order_list.validate()?;
    let order_inits = orders
        .iter()
        .map(OrderInitialized::from)
        .collect::<Vec<_>>();

    Ok(SubmitOrderList::new(
        trader_id,
        client_id,
        strategy_id,
        order_list,
        order_inits,
        None,
        None,
        None,
        UUID4::new(),
        ts_init,
        None,
    ))
}

fn selected_entry_orders(
    entry: &SelectedOptionsEntry,
    order_list_id: &str,
    quantity: u64,
    trader_id: TraderId,
    strategy_id: StrategyId,
) -> anyhow::Result<Vec<OrderAny>> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }

    let mut factory = option_order_factory(trader_id, strategy_id);
    Ok(match entry {
        SelectedOptionsEntry::Credit(entry) => vec![
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "short"),
                &entry.candidate.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.short.bid,
                false,
            )?,
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "long"),
                &entry.candidate.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.long.ask,
                false,
            )?,
        ],
        SelectedOptionsEntry::IronCondor(entry) => vec![
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "short-put"),
                &entry.candidate.put.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.put.short.bid,
                false,
            )?,
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "long-put"),
                &entry.candidate.put.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.put.long.ask,
                false,
            )?,
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "short-call"),
                &entry.candidate.call.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.call.short.bid,
                false,
            )?,
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "long-call"),
                &entry.candidate.call.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.call.long.ask,
                false,
            )?,
        ],
        SelectedOptionsEntry::Debit(entry) => vec![
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "long"),
                &entry.candidate.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.long.ask,
                false,
            )?,
            limit_option_order(
                &mut factory,
                labeled_client_order_id(order_list_id, "short"),
                &entry.candidate.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.short.bid,
                false,
            )?,
        ],
        SelectedOptionsEntry::NakedOption(entry) => vec![limit_option_order(
            &mut factory,
            ClientOrderId::from(order_list_id),
            &entry.candidate.short.symbol,
            OrderSide::Sell,
            quantity,
            entry.candidate.short.bid,
            false,
        )?],
    })
}

fn close_entry_orders(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    trader_id: TraderId,
    strategy_id: StrategyId,
) -> anyhow::Result<Vec<OrderAny>> {
    let mut factory = option_order_factory(trader_id, strategy_id);
    let mut orders = vec![limit_option_order(
        &mut factory,
        if entry.is_naked_option() {
            ClientOrderId::from(order_list_id)
        } else {
            labeled_client_order_id(order_list_id, "short-close")
        },
        &entry.short_symbol,
        OrderSide::Buy,
        entry.quantity,
        quote.short_ask,
        true,
    )?];

    if !entry.long_symbol.is_empty() {
        orders.push(limit_option_order(
            &mut factory,
            labeled_client_order_id(order_list_id, "long-close"),
            &entry.long_symbol,
            OrderSide::Sell,
            entry.quantity,
            quote.long_bid,
            true,
        )?);
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
        orders.push(limit_option_order(
            &mut factory,
            labeled_client_order_id(order_list_id, "short-call-close"),
            short_call_symbol,
            OrderSide::Buy,
            entry.quantity,
            short_call_ask,
            true,
        )?);
        orders.push(limit_option_order(
            &mut factory,
            labeled_client_order_id(order_list_id, "long-call-close"),
            long_call_symbol,
            OrderSide::Sell,
            entry.quantity,
            long_call_bid,
            true,
        )?);
    }

    Ok(orders)
}

fn limit_option_order(
    factory: &mut OrderFactory,
    client_order_id: ClientOrderId,
    symbol: &str,
    side: OrderSide,
    quantity: u64,
    limit_price: f64,
    reduce_only: bool,
) -> anyhow::Result<OrderAny> {
    if quantity == 0 {
        anyhow::bail!("order {client_order_id} quantity must be positive");
    }
    if limit_price <= 0.0 {
        anyhow::bail!("order {client_order_id} limit price must be positive");
    }

    Ok(factory.limit(
        alpaca_instrument_id(symbol)?,
        side,
        Quantity::new(quantity as f64, 0),
        Price::new(limit_price, 2),
        Some(TimeInForce::Day),
        None,
        None,
        Some(reduce_only),
        Some(false),
        None,
        None,
        None,
        None,
        None,
        None,
        Some(client_order_id),
    ))
}

fn option_order_factory(trader_id: TraderId, strategy_id: StrategyId) -> OrderFactory {
    let clock: Rc<RefCell<dyn nautilus_common::clock::Clock>> =
        Rc::new(RefCell::new(LiveClock::default()));
    OrderFactory::new(trader_id, strategy_id, None, None, clock, false, false)
}

fn labeled_client_order_id(order_list_id: &str, label: &str) -> ClientOrderId {
    let client_order_id = format!("{order_list_id}-{label}");
    ClientOrderId::from(client_order_id.as_str())
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
        let mut orders =
            selected_entry_orders(entry, client_order_id, quantity, trader_id, strategy_id)?;
        let order = single_order(&mut orders, "open")?;
        Ok(submit_order_from_order(order, trader_id, client_id))
    }

    pub(crate) fn build_close_order(
        entry: &StrategyStateEntry,
        quote: &CloseQuote,
        client_order_id: &str,
        trader_id: TraderId,
        client_id: Option<ClientId>,
        strategy_id: StrategyId,
    ) -> anyhow::Result<SubmitOrder> {
        let mut orders = close_entry_orders(entry, quote, client_order_id, trader_id, strategy_id)?;
        let order = single_order(&mut orders, "close")?;
        Ok(submit_order_from_order(order, trader_id, client_id))
    }

    fn single_order<'a>(
        orders: &'a mut Vec<OrderAny>,
        operation: &str,
    ) -> anyhow::Result<&'a OrderAny> {
        if orders.len() != 1 {
            anyhow::bail!(
                "{operation} helper expected one order, found {}",
                orders.len()
            );
        }
        Ok(&orders[0])
    }
}
