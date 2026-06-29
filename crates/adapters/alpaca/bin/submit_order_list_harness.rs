// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

//! Smoke utility for submitting a real Nautilus `SubmitOrderList` through the Alpaca execution client.

use std::{cell::RefCell, env, process, rc::Rc, str::FromStr, time::Duration};

use nautilus_alpaca::{
    AlpacaExecutionClient,
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{AlpacaOrder, ReplaceOrderRequest},
    },
    strategy::scan_put_credit_underlying,
};
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{ExecutionEvent, execution::SubmitOrderList},
};
use nautilus_core::{UUID4, time::get_atomic_clock_realtime};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType, OrderSide, OrderType, TimeInForce},
    events::OrderInitialized,
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId, Venue,
    },
    orders::OrderList,
    types::{Price, Quantity},
};
use nautilus_trading::options::candidates::PutCreditScannerConfig;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, timeout},
};

const DEFAULT_EVENT_TIMEOUT_SECS: u64 = 20;

#[derive(Debug)]
struct HarnessOrderSpec {
    short_symbol: String,
    long_symbol: String,
    short_limit_price: f64,
    long_limit_price: f64,
    quantity: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        usage();
        process::exit(2);
    }

    let mut data_config = data_config_from_env();
    let exec_config = exec_config_from_env();
    data_config.trading_base_url = exec_config.trading_base_url.clone();

    let spec = order_spec_from_args(&args, &data_config).await?;
    println!(
        "submit_order_list_harness: short={} short_price={:.2} long={} long_price={:.2} qty={}",
        spec.short_symbol,
        spec.short_limit_price,
        spec.long_symbol,
        spec.long_limit_price,
        spec.quantity,
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    replace_exec_event_sender(tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let trader_id = TraderId::from("TRADER-001");
    let client_id = ClientId::from(ALPACA_CLIENT_ID);
    let account_id = AccountId::from("ALPACA-001");
    let strategy_id = StrategyId::from("ALPACA-MLEG-HARNESS");
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

    let order_list_id = format!("nautilus-mleg-{}", UUID4::new());
    let cmd = build_submit_order_list(
        &spec,
        trader_id,
        Some(client_id),
        strategy_id,
        &order_list_id,
    )?;
    println!("submit_order_list: order_list_id={order_list_id}");
    client.submit_order_list(cmd)?;

    let terminal_events = collect_execution_events(&mut rx).await;
    let replacement_parent_order_id = match replace_parent_if_requested(
        &exec_config,
        &order_list_id,
        &spec,
        terminal_events.accepted > 0,
    )
    .await
    {
        Ok(order_id) => order_id,
        Err(error) => {
            println!("replace: failed error={error}");
            None
        }
    };
    cleanup_if_accepted(
        &exec_config,
        &order_list_id,
        replacement_parent_order_id.as_deref(),
        terminal_events.accepted > 0,
    )
    .await?;

    client.disconnect().await?;
    client.stop()?;

    if terminal_events.accepted == 0 && terminal_events.rejected == 0 {
        anyhow::bail!("no accepted or rejected order events observed before timeout");
    }

    Ok(())
}

async fn order_spec_from_args(
    args: &[String],
    data_config: &AlpacaDataClientConfig,
) -> anyhow::Result<HarnessOrderSpec> {
    if args.first().is_some_and(|value| value == "--scan") {
        let underlyings = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("missing --scan underlyings"))?;
        let quantity = args
            .get(2)
            .map(|value| value.parse::<u64>())
            .transpose()?
            .unwrap_or(1);
        return scan_order_spec(data_config, underlyings, quantity).await;
    }

    if args.len() < 4 {
        usage();
        process::exit(2);
    }

    Ok(HarnessOrderSpec {
        short_symbol: args[0].clone(),
        long_symbol: args[1].clone(),
        short_limit_price: args[2].parse::<f64>()?,
        long_limit_price: args[3].parse::<f64>()?,
        quantity: args
            .get(4)
            .map(|value| value.parse::<u64>())
            .transpose()?
            .unwrap_or(1),
    })
}

async fn scan_order_spec(
    data_config: &AlpacaDataClientConfig,
    underlyings: &str,
    quantity: u64,
) -> anyhow::Result<HarnessOrderSpec> {
    let client = AlpacaHttpClient::from_data_config(data_config)?;
    let scanner_config = PutCreditScannerConfig::default();
    for underlying in underlyings
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let result =
            scan_put_credit_underlying(&client, data_config, &scanner_config, underlying).await?;
        let Some(best) = result.candidates.first() else {
            println!(
                "scan: underlying={} contracts={} snapshots={} scoreable={} candidates=0",
                result.underlying,
                result.contract_count,
                result.snapshot_count,
                result.scoreable_count,
            );
            continue;
        };

        println!(
            "scan_selected: underlying={} short={} bid={:.2} long={} ask={:.2} credit={:.2} score={:.1}",
            result.underlying,
            best.short.symbol,
            best.short.bid,
            best.long.symbol,
            best.long.ask,
            best.credit,
            best.score,
        );
        return Ok(HarnessOrderSpec {
            short_symbol: best.short.symbol.clone(),
            long_symbol: best.long.symbol.clone(),
            short_limit_price: best.short.bid,
            long_limit_price: best.long.ask,
            quantity,
        });
    }

    anyhow::bail!("no scan candidate found for {underlyings}");
}

fn build_submit_order_list(
    spec: &HarnessOrderSpec,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
    order_list_id: &str,
) -> anyhow::Result<SubmitOrderList> {
    if spec.quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }
    let order_list_id = OrderListId::from(order_list_id);
    let short_client_id = ClientOrderId::from(format!("{order_list_id}-short").as_str());
    let long_client_id = ClientOrderId::from(format!("{order_list_id}-long").as_str());
    let quantity = Quantity::new(spec.quantity as f64, 0);
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: vec![
            MlegSubmitLeg {
                client_order_id: short_client_id,
                instrument_id: alpaca_instrument_id(&spec.short_symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(spec.short_limit_price, 2),
                reduce_only: false,
            },
            MlegSubmitLeg {
                client_order_id: long_client_id,
                instrument_id: alpaca_instrument_id(&spec.long_symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(spec.long_limit_price, 2),
                reduce_only: false,
            },
        ],
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

#[derive(Clone, Copy, Debug)]
struct MlegSubmitLeg {
    client_order_id: ClientOrderId,
    instrument_id: InstrumentId,
    order_side: OrderSide,
    quantity: Quantity,
    limit_price: Price,
    reduce_only: bool,
}

#[derive(Clone, Debug)]
struct MlegSubmitOrderListRequest {
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
    order_list_id: OrderListId,
    legs: Vec<MlegSubmitLeg>,
    ts_init: nautilus_core::UnixNanos,
}

fn build_mleg_submit_order_list(
    request: MlegSubmitOrderListRequest,
) -> anyhow::Result<SubmitOrderList> {
    if request.legs.len() < 2 {
        anyhow::bail!("multi-leg SubmitOrderList requires at least two legs");
    }
    if request.legs.len() > 4 {
        anyhow::bail!("multi-leg SubmitOrderList supports at most four legs");
    }

    let client_order_ids = request
        .legs
        .iter()
        .map(|leg| leg.client_order_id)
        .collect::<Vec<_>>();
    let first_instrument_id = request.legs[0].instrument_id;
    let order_inits = request
        .legs
        .iter()
        .map(|leg| {
            if !leg.quantity.is_positive() {
                anyhow::bail!("leg {} quantity must be positive", leg.client_order_id);
            }
            if !leg.limit_price.is_positive() {
                anyhow::bail!("leg {} limit price must be positive", leg.client_order_id);
            }

            let linked_order_ids = client_order_ids
                .iter()
                .copied()
                .filter(|candidate| *candidate != leg.client_order_id)
                .collect::<Vec<_>>();
            Ok(order_init(
                request.trader_id,
                request.strategy_id,
                *leg,
                request.order_list_id,
                linked_order_ids,
                request.ts_init,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let order_list = OrderList::new(
        request.order_list_id,
        first_instrument_id,
        request.strategy_id,
        client_order_ids,
        request.ts_init,
    );

    Ok(SubmitOrderList::new(
        request.trader_id,
        request.client_id,
        request.strategy_id,
        order_list,
        order_inits,
        None,
        None,
        None,
        UUID4::new(),
        request.ts_init,
        None,
    ))
}

fn order_init(
    trader_id: TraderId,
    strategy_id: StrategyId,
    leg: MlegSubmitLeg,
    order_list_id: OrderListId,
    linked_order_ids: Vec<ClientOrderId>,
    ts: nautilus_core::UnixNanos,
) -> OrderInitialized {
    OrderInitialized::new(
        trader_id,
        strategy_id,
        leg.instrument_id,
        leg.client_order_id,
        leg.order_side,
        OrderType::Limit,
        leg.quantity,
        TimeInForce::Day,
        false,
        leg.reduce_only,
        false,
        false,
        UUID4::new(),
        ts,
        ts,
        Some(leg.limit_price),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(order_list_id),
        Some(linked_order_ids),
        None,
        None,
        None,
        None,
        None,
    )
}

fn alpaca_instrument_id(symbol: &str) -> anyhow::Result<InstrumentId> {
    Ok(InstrumentId::from_str(&format!("{symbol}.{ALPACA_VENUE}"))?)
}

#[derive(Default)]
struct TerminalEventCounts {
    accepted: usize,
    rejected: usize,
}

async fn collect_execution_events(
    rx: &mut mpsc::UnboundedReceiver<ExecutionEvent>,
) -> TerminalEventCounts {
    let mut counts = TerminalEventCounts::default();
    let deadline = Instant::now()
        + Duration::from_secs(env_parse(
            "ALPACA_ORDER_LIST_HARNESS_TIMEOUT_SECS",
            DEFAULT_EVENT_TIMEOUT_SECS,
        ));

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(Some(event)) = timeout(remaining, rx.recv()).await else {
            break;
        };

        match event {
            ExecutionEvent::Order(order_event) => {
                print_order_event(&order_event);
                match order_event {
                    nautilus_model::events::OrderEventAny::Accepted(_) => counts.accepted += 1,
                    nautilus_model::events::OrderEventAny::Rejected(_) => counts.rejected += 1,
                    _ => {}
                }
            }
            ExecutionEvent::Account(account_state) => {
                println!(
                    "execution_event: account account_id={} balances={}",
                    account_state.account_id,
                    account_state.balances.len(),
                );
            }
            other => println!("execution_event: {other:?}"),
        }

        if counts.accepted + counts.rejected >= 2 {
            break;
        }
    }

    counts
}

fn print_order_event(order_event: &nautilus_model::events::OrderEventAny) {
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

async fn cleanup_if_accepted(
    config: &AlpacaExecClientConfig,
    order_list_id: &str,
    parent_order_id: Option<&str>,
    accepted: bool,
) -> anyhow::Result<()> {
    if !accepted || !env_bool("ALPACA_ORDER_LIST_HARNESS_CANCEL_OPEN", true) {
        return Ok(());
    }

    let client = AlpacaHttpClient::from_exec_config(config)?;
    if let Some(order_id) = parent_order_id.filter(|value| !value.trim().is_empty()) {
        return cleanup_order_lookup(&client, client.order_by_id(order_id, true).await).await;
    }

    cleanup_order_lookup(
        &client,
        client.order_by_client_order_id(order_list_id, true).await,
    )
    .await
}

async fn cleanup_order_lookup(
    client: &AlpacaHttpClient,
    result: std::result::Result<AlpacaOrder, Error>,
) -> anyhow::Result<()> {
    match result {
        Ok(order) => cleanup_parent_order(client, &order).await?,
        Err(Error::HttpStatus { status, body, .. }) => {
            println!("cleanup: parent_lookup_failed status={status} body={body}");
        }
        Err(error) => return Err(error.into()),
    }

    Ok(())
}

async fn cleanup_parent_order(
    client: &AlpacaHttpClient,
    order: &AlpacaOrder,
) -> anyhow::Result<()> {
    if order.is_terminal() {
        println!(
            "cleanup: parent_order_id={} status={} terminal=true",
            order.id.as_deref().unwrap_or("unknown"),
            order.status.as_deref().unwrap_or("unknown"),
        );
        return Ok(());
    }

    let Some(order_id) = order.id.as_deref() else {
        println!("cleanup: accepted parent order had no id");
        return Ok(());
    };
    client.cancel_order(order_id).await?;
    println!("cleanup: cancel_requested parent_order_id={order_id}");
    poll_canceled_parent(client, order_id).await
}

async fn replace_parent_if_requested(
    config: &AlpacaExecClientConfig,
    order_list_id: &str,
    spec: &HarnessOrderSpec,
    accepted: bool,
) -> anyhow::Result<Option<String>> {
    if !accepted || !env_bool("ALPACA_ORDER_LIST_HARNESS_REPLACE_OPEN", false) {
        return Ok(None);
    }

    let client = AlpacaHttpClient::from_exec_config(config)?;
    match client.order_by_client_order_id(order_list_id, true).await {
        Ok(order) if order.is_terminal() => {
            println!(
                "replace: skipped parent_order_id={} status={} terminal=true",
                order.id.as_deref().unwrap_or("unknown"),
                order.status.as_deref().unwrap_or("unknown"),
            );
            Ok(None)
        }
        Ok(order) => {
            let Some(order_id) = order.id.as_deref() else {
                println!("replace: accepted parent order had no id");
                return Ok(None);
            };
            let current_credit = spec.short_limit_price - spec.long_limit_price;
            let replacement_credit = env::var("ALPACA_ORDER_LIST_HARNESS_REPLACE_CREDIT")
                .ok()
                .map(|value| value.parse::<f64>())
                .transpose()?
                .unwrap_or_else(|| (current_credit - 0.01).max(0.01));
            let requested_limit_price = format!("-{replacement_credit:.2}");
            let replaced = client
                .replace_order(
                    order_id,
                    &ReplaceOrderRequest {
                        limit_price: Some(requested_limit_price.clone()),
                        ..Default::default()
                    },
                )
                .await?;
            println!(
                "replace: parent_order_id={} requested_limit_price={} status={} limit_price={}",
                replaced.id.as_deref().unwrap_or(order_id),
                requested_limit_price,
                replaced.status.as_deref().unwrap_or("unknown"),
                replaced.limit_price.as_deref().unwrap_or("unknown"),
            );
            Ok(replaced.id.or_else(|| Some(order_id.to_string())))
        }
        Err(Error::HttpStatus { status, body, .. }) => {
            println!("replace: parent_lookup_failed status={status} body={body}");
            Ok(None)
        }
        Err(error) => return Err(error.into()),
    }
}

async fn poll_canceled_parent(client: &AlpacaHttpClient, order_id: &str) -> anyhow::Result<()> {
    let attempts = env_parse("ALPACA_ORDER_LIST_HARNESS_CANCEL_POLL_ATTEMPTS", 3_u64);
    let poll_secs = env_parse("ALPACA_ORDER_LIST_HARNESS_CANCEL_POLL_SECS", 1_u64);
    for attempt in 1..=attempts {
        sleep(Duration::from_secs(poll_secs)).await;
        let order = client.order_by_id(order_id, true).await?;
        println!(
            "cleanup_poll_{attempt}: parent_order_id={} status={} terminal={}",
            order.id.as_deref().unwrap_or("unknown"),
            order.status.as_deref().unwrap_or("unknown"),
            order.is_terminal(),
        );
        if order.is_terminal() {
            break;
        }
    }
    Ok(())
}

fn data_config_from_env() -> AlpacaDataClientConfig {
    let mut config = AlpacaDataClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    config
}

fn exec_config_from_env() -> AlpacaExecClientConfig {
    let mut config = AlpacaExecClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.trade_updates_ws_url = env::var("ALPACA_TRADE_UPDATES_WS_URL").ok();
    config.use_trade_updates_stream = env_bool("ALPACA_ORDER_LIST_HARNESS_USE_WS", false);
    config.external_order_filtering = false;
    config
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
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

fn usage() {
    eprintln!(
        "usage:\n  alpaca-submit-order-list-harness --scan <UNDERLYING[,UNDERLYING...]> [QTY]\n  alpaca-submit-order-list-harness <SHORT_PUT_SYMBOL> <LONG_PUT_SYMBOL> <SHORT_LIMIT_PRICE> <LONG_LIMIT_PRICE> [QTY]"
    );
}
