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

//! JSON bridge for submitting a Nautilus `SubmitOrderList` through the Alpaca execution client.
//!
//! The bridge reads a spreads runtime handoff JSON document from stdin and writes one JSON result to
//! stdout. It does not cancel accepted orders unless `ALPACA_ORDER_LIST_BRIDGE_CANCEL_AFTER_ACCEPT`
//! is explicitly enabled for smoke testing.

use std::{cell::RefCell, env, io::Read, rc::Rc, str::FromStr, time::Duration};

use nautilus_alpaca::{
    AlpacaExecutionClient,
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::AlpacaExecClientConfig,
    http::{client::AlpacaHttpClient, error::Error, models::AlpacaOrder},
    submit::{MlegSubmitLeg, MlegSubmitOrderListRequest, build_mleg_submit_order_list},
};
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{ExecutionEvent, execution::SubmitOrderList},
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
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, timeout},
};

const DEFAULT_EVENT_TIMEOUT_SECS: u64 = 20;
const DEFAULT_LOOKUP_ATTEMPTS: u64 = 5;
const DEFAULT_LOOKUP_POLL_SECS: u64 = 1;

#[derive(Debug, Deserialize)]
struct BridgeHandoff {
    order_list_id: String,
    #[serde(default)]
    strategy_family: Option<String>,
    legs: Vec<BridgeLeg>,
}

#[derive(Debug, Deserialize)]
struct BridgeLeg {
    client_order_id: String,
    symbol: String,
    #[serde(default)]
    instrument_id: Option<String>,
    side: String,
    #[serde(default)]
    position_intent: Option<String>,
    quantity: u64,
    limit_price: f64,
}

#[derive(Debug, Serialize)]
struct BridgeResult {
    status: String,
    order_list_id: String,
    parent_order_id: Option<String>,
    parent_client_order_id: Option<String>,
    order_snapshot: Option<AlpacaOrder>,
    events: Vec<BridgeEvent>,
    cleanup: Option<CleanupResult>,
}

#[derive(Debug, Serialize)]
struct BridgeEvent {
    event_type: String,
    client_order_id: String,
    instrument_id: String,
    venue_order_id: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct CleanupResult {
    attempted: bool,
    canceled: bool,
    parent_order_id: Option<String>,
    status: Option<String>,
    message: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let handoff = read_handoff()?;
    let exec_config = exec_config_from_env();

    let (tx, mut rx) = mpsc::unbounded_channel();
    replace_exec_event_sender(tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let trader_id = TraderId::from("TRADER-001");
    let client_id = ClientId::from(ALPACA_CLIENT_ID);
    let account_id = AccountId::from("ALPACA-001");
    let strategy_id = strategy_id_from_handoff(&handoff);
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

    let order_list_id = handoff.order_list_id.clone();
    let cmd = build_submit_order_list(&handoff, trader_id, Some(client_id), strategy_id)?;
    client.submit_order_list(cmd)?;

    let events = collect_execution_events(&mut rx, handoff.legs.len()).await;
    let http_client = AlpacaHttpClient::from_exec_config(&exec_config)?;
    let order_snapshot = lookup_parent_order(&http_client, &order_list_id).await?;
    let cleanup = cleanup_if_requested(
        &http_client,
        order_snapshot.as_ref(),
        accepted_count(&events) > 0,
    )
    .await?;

    client.disconnect().await?;
    client.stop()?;

    let parent_order_id = order_snapshot.as_ref().and_then(|order| order.id.clone());
    let parent_client_order_id = order_snapshot
        .as_ref()
        .and_then(|order| order.client_order_id.clone());
    let result = BridgeResult {
        status: bridge_status(&events, handoff.legs.len(), order_snapshot.as_ref()),
        order_list_id,
        parent_order_id,
        parent_client_order_id,
        order_snapshot,
        events,
        cleanup,
    };
    println!("{}", serde_json::to_string(&result)?);

    Ok(())
}

fn read_handoff() -> anyhow::Result<BridgeHandoff> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    if input.trim().is_empty() {
        anyhow::bail!("expected handoff JSON on stdin");
    }
    Ok(serde_json::from_str(&input)?)
}

fn strategy_id_from_handoff(handoff: &BridgeHandoff) -> StrategyId {
    let suffix = handoff
        .strategy_family
        .as_deref()
        .unwrap_or("SPREADS")
        .trim()
        .to_ascii_uppercase()
        .replace([' ', '_'], "-");
    StrategyId::from(format!("SPREADS-{suffix}").as_str())
}

fn build_submit_order_list(
    handoff: &BridgeHandoff,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    if handoff.legs.len() < 2 {
        anyhow::bail!("Nautilus bridge requires at least two legs");
    }
    if handoff.legs.len() > 4 {
        anyhow::bail!("Nautilus bridge supports at most four legs");
    }

    let mut legs = Vec::with_capacity(handoff.legs.len());
    for leg in &handoff.legs {
        if leg.quantity == 0 {
            anyhow::bail!("leg {} quantity must be positive", leg.client_order_id);
        }
        if leg.limit_price <= 0.0 {
            anyhow::bail!("leg {} limit price must be positive", leg.client_order_id);
        }
        legs.push(MlegSubmitLeg {
            client_order_id: ClientOrderId::from(leg.client_order_id.as_str()),
            instrument_id: instrument_id(leg)?,
            order_side: order_side(leg)?,
            quantity: Quantity::new(leg.quantity as f64, 0),
            limit_price: Price::new(leg.limit_price, 2),
            reduce_only: is_reduce_only(leg),
        });
    }

    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id: OrderListId::from(handoff.order_list_id.as_str()),
        legs,
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

fn instrument_id(leg: &BridgeLeg) -> anyhow::Result<InstrumentId> {
    let value = leg
        .instrument_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}.{}", leg.symbol, ALPACA_VENUE));
    Ok(InstrumentId::from_str(&value)?)
}

fn order_side(leg: &BridgeLeg) -> anyhow::Result<OrderSide> {
    match leg.side.trim().to_ascii_lowercase().as_str() {
        "buy" => Ok(OrderSide::Buy),
        "sell" => Ok(OrderSide::Sell),
        other => anyhow::bail!("leg {} has unsupported side {other}", leg.client_order_id),
    }
}

fn is_reduce_only(leg: &BridgeLeg) -> bool {
    leg.position_intent
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase().ends_with("_to_close"))
        .unwrap_or(false)
}

async fn collect_execution_events(
    rx: &mut mpsc::UnboundedReceiver<ExecutionEvent>,
    leg_count: usize,
) -> Vec<BridgeEvent> {
    let mut events = Vec::new();
    let deadline = Instant::now()
        + Duration::from_secs(env_parse(
            "ALPACA_ORDER_LIST_BRIDGE_TIMEOUT_SECS",
            DEFAULT_EVENT_TIMEOUT_SECS,
        ));

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(Some(event)) = timeout(remaining, rx.recv()).await else {
            break;
        };

        if let ExecutionEvent::Order(order_event) = event {
            events.push(bridge_event(&order_event));
            if terminal_event_count(&events) >= leg_count {
                break;
            }
        }
    }

    events
}

fn bridge_event(order_event: &OrderEventAny) -> BridgeEvent {
    let event_type = format!("{:?}", order_event.event_type()).to_ascii_lowercase();
    let event = order_event.clone().into_boxed();
    BridgeEvent {
        event_type,
        client_order_id: event.client_order_id().to_string(),
        instrument_id: event.instrument_id().to_string(),
        venue_order_id: event.venue_order_id().map(|value| value.to_string()),
        reason: event.reason().map(|value| value.to_string()),
    }
}

fn terminal_event_count(events: &[BridgeEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.event_type.as_str(), "accepted" | "rejected"))
        .count()
}

fn accepted_count(events: &[BridgeEvent]) -> usize {
    events
        .iter()
        .filter(|event| event.event_type == "accepted")
        .count()
}

async fn lookup_parent_order(
    client: &AlpacaHttpClient,
    order_list_id: &str,
) -> anyhow::Result<Option<AlpacaOrder>> {
    let attempts = env_parse(
        "ALPACA_ORDER_LIST_BRIDGE_LOOKUP_ATTEMPTS",
        DEFAULT_LOOKUP_ATTEMPTS,
    );
    let poll_secs = env_parse(
        "ALPACA_ORDER_LIST_BRIDGE_LOOKUP_POLL_SECS",
        DEFAULT_LOOKUP_POLL_SECS,
    );
    for attempt in 1..=attempts {
        match client.order_by_client_order_id(order_list_id, true).await {
            Ok(order) => return Ok(Some(order)),
            Err(Error::HttpStatus { status, .. }) if status == 404 && attempt < attempts => {
                sleep(Duration::from_secs(poll_secs)).await;
            }
            Err(Error::HttpStatus { status, .. }) if status == 404 => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

async fn cleanup_if_requested(
    client: &AlpacaHttpClient,
    order_snapshot: Option<&AlpacaOrder>,
    accepted: bool,
) -> anyhow::Result<Option<CleanupResult>> {
    if !env_bool("ALPACA_ORDER_LIST_BRIDGE_CANCEL_AFTER_ACCEPT", false) {
        return Ok(None);
    }
    if !accepted {
        return Ok(Some(CleanupResult {
            attempted: false,
            canceled: false,
            parent_order_id: None,
            status: None,
            message: Some("no accepted order events observed".to_string()),
        }));
    }

    let Some(order) = order_snapshot else {
        return Ok(Some(CleanupResult {
            attempted: false,
            canceled: false,
            parent_order_id: None,
            status: None,
            message: Some("accepted order had no parent snapshot".to_string()),
        }));
    };
    if order.is_terminal() {
        return Ok(Some(CleanupResult {
            attempted: false,
            canceled: false,
            parent_order_id: order.id.clone(),
            status: order.status.clone(),
            message: Some("parent order already terminal".to_string()),
        }));
    }
    let Some(order_id) = order.id.as_deref() else {
        return Ok(Some(CleanupResult {
            attempted: false,
            canceled: false,
            parent_order_id: None,
            status: order.status.clone(),
            message: Some("accepted parent order had no id".to_string()),
        }));
    };

    client.cancel_order(order_id).await?;
    let mut latest = order.clone();
    let attempts = env_parse("ALPACA_ORDER_LIST_BRIDGE_CANCEL_POLL_ATTEMPTS", 3_u64);
    let poll_secs = env_parse("ALPACA_ORDER_LIST_BRIDGE_CANCEL_POLL_SECS", 1_u64);
    for _ in 0..attempts {
        sleep(Duration::from_secs(poll_secs)).await;
        latest = client.order_by_id(order_id, true).await?;
        if latest.is_terminal() {
            break;
        }
    }

    Ok(Some(CleanupResult {
        attempted: true,
        canceled: latest.status.as_deref() == Some("canceled"),
        parent_order_id: latest.id,
        status: latest.status,
        message: None,
    }))
}

fn bridge_status(
    events: &[BridgeEvent],
    leg_count: usize,
    order_snapshot: Option<&AlpacaOrder>,
) -> String {
    if events.iter().any(|event| event.event_type == "rejected") {
        return "rejected".to_string();
    }
    if let Some(status) = order_snapshot.and_then(|order| order.status.as_deref()) {
        return status.to_string();
    }
    if accepted_count(events) >= leg_count {
        return "accepted".to_string();
    }
    "timeout".to_string()
}

fn exec_config_from_env() -> AlpacaExecClientConfig {
    let mut config = AlpacaExecClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.trade_updates_ws_url = env::var("ALPACA_TRADE_UPDATES_WS_URL").ok();
    config.use_trade_updates_stream = env_bool("ALPACA_ORDER_LIST_BRIDGE_USE_WS", false);
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
