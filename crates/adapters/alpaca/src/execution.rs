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

//! Execution admission and report mapping helpers for Alpaca option-spread strategies.

#[cfg(feature = "live")]
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
};
use std::{collections::BTreeSet, str::FromStr};

#[cfg(feature = "live")]
use nautilus_core::{
    MUTEX_POISONED,
    datetime::unix_nanos_to_iso8601,
    time::{AtomicTime, get_atomic_clock_realtime},
};
use nautilus_core::{UUID4, UnixNanos};
#[cfg(feature = "live")]
use nautilus_model::{
    accounts::AccountAny,
    enums::{LiquiditySide, OmsType, PositionSideSpecified},
    identifiers::{ClientId, PositionId, TradeId, Venue},
    instruments::{Instrument, InstrumentAny},
    orders::{Order, OrderAny},
    reports::{ExecutionMassStatus, FillReport, PositionStatusReport},
    types::{AccountBalance, Currency, MarginBalance, Money},
};
use nautilus_model::{
    enums::{ContingencyType, OrderSide, OrderStatus, OrderType, TimeInForce, TrailingOffsetType},
    identifiers::{AccountId, ClientOrderId, InstrumentId, VenueOrderId},
    reports::OrderStatusReport,
    types::{Price, Quantity},
};
#[cfg(feature = "live")]
use {
    async_trait::async_trait,
    nautilus_common::{
        clients::ExecutionClient,
        live::{get_runtime, runner::get_exec_event_sender},
        messages::execution::{
            CancelOrder, GenerateFillReports, GenerateOrderStatusReport,
            GenerateOrderStatusReports, GeneratePositionStatusReports, ModifyOrder, QueryAccount,
            QueryOrder, SubmitOrder, SubmitOrderList,
        },
    },
    nautilus_live::{ExecutionClientCore, ExecutionEventEmitter},
    rust_decimal::Decimal,
    serde_json::json,
    tokio::task::JoinHandle,
};

#[cfg(feature = "live")]
use crate::http::client::AlpacaHttpClient;
use crate::{
    common::consts::ALPACA_VENUE,
    http::{
        error::{Error, Result},
        models::{AlpacaAccount, AlpacaOrder, AlpacaPosition},
    },
    parse::parse_alpaca_option_symbol,
};
#[cfg(feature = "live")]
use crate::{
    config::AlpacaExecClientConfig,
    http::models::{AlpacaActivity, ListActivitiesRequest, ListOrdersRequest, ReplaceOrderRequest},
    orders::{
        AlpacaPositionIntent, EquityOrderPayload, MlegOrderLeg, MlegOrderPayload,
        SimpleOrderPayload,
    },
    orders::{NetPremiumKind, TradeIntent, signed_net_limit_price},
    runtime::emit_operator_event,
    websocket::{
        client::AlpacaTradeUpdatesWebSocketClient,
        messages::{AlpacaTradeUpdate, AlpacaTradeUpdateLeg, AlpacaWsMessage},
    },
};

#[cfg(feature = "live")]
const DEFAULT_RECONCILIATION_LOOKBACK_MINS: u64 = 60;

/// Admission result for a candidate option spread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionDecision {
    /// If the candidate can be submitted.
    pub allowed: bool,
    /// Human-readable rejection reasons.
    pub reasons: Vec<String>,
}

impl AdmissionDecision {
    /// Returns an allowed admission result.
    #[must_use]
    pub fn allow() -> Self {
        Self {
            allowed: true,
            reasons: Vec::new(),
        }
    }

    /// Returns a rejected admission result.
    #[must_use]
    pub fn reject(reasons: Vec<String>) -> Self {
        Self {
            allowed: false,
            reasons,
        }
    }
}

/// Checks account, position, and open-order state before opening a put credit spread.
#[must_use]
pub fn check_put_credit_entry_admission(
    account: &AlpacaAccount,
    positions: &[AlpacaPosition],
    open_orders: &[AlpacaOrder],
    short_put_symbol: &str,
    long_put_symbol: &str,
) -> AdmissionDecision {
    check_option_spread_entry_admission(
        account,
        positions,
        open_orders,
        &[short_put_symbol, long_put_symbol],
    )
}

/// Checks account, position, and open-order state before opening option exposure.
#[must_use]
pub fn check_option_spread_entry_admission(
    account: &AlpacaAccount,
    positions: &[AlpacaPosition],
    open_orders: &[AlpacaOrder],
    candidate_symbols: &[&str],
) -> AdmissionDecision {
    let mut reasons = account_entry_admission_reasons(account);

    let candidate_underlyings = candidate_symbols
        .iter()
        .map(|symbol| option_underlying_symbol(symbol))
        .collect::<BTreeSet<_>>();
    let candidate_underlying = candidate_underlyings
        .iter()
        .next()
        .cloned()
        .unwrap_or_default();
    if candidate_symbols.is_empty()
        || candidate_underlying.is_empty()
        || candidate_underlyings.len() != 1
    {
        reasons.push("candidate option symbols must resolve to one underlying".to_string());
    }

    let candidate_symbols = candidate_symbols
        .iter()
        .map(|symbol| symbol.trim().to_string())
        .collect::<BTreeSet<_>>();

    for position in positions {
        let Some(symbol) = position.symbol.as_deref() else {
            continue;
        };
        if !has_nonzero_quantity(position.qty.as_deref()) {
            continue;
        }
        let position_underlying = option_underlying_symbol(symbol);
        if candidate_symbols.contains(symbol) {
            reasons.push(format!("existing open position on candidate leg {symbol}"));
        } else if !candidate_underlying.is_empty() && position_underlying == candidate_underlying {
            reasons.push(format!(
                "existing open option position on underlying {candidate_underlying}: {symbol}",
            ));
        }
    }

    for order in open_orders.iter().filter(|order| order.is_working()) {
        for symbol in order.symbols() {
            let order_underlying = option_underlying_symbol(&symbol);
            if candidate_symbols.contains(&symbol) {
                reasons.push(format!(
                    "working order already references candidate leg {symbol}"
                ));
            } else if !candidate_underlying.is_empty() && order_underlying == candidate_underlying {
                reasons.push(format!(
                    "working order already references underlying {candidate_underlying}: {symbol}",
                ));
            }
        }
    }

    if reasons.is_empty() {
        AdmissionDecision::allow()
    } else {
        reasons.sort();
        reasons.dedup();
        AdmissionDecision::reject(reasons)
    }
}

/// Returns broker account-level reasons that block new Alpaca option entries.
#[must_use]
pub fn account_entry_admission_reasons(account: &AlpacaAccount) -> Vec<String> {
    let mut reasons = Vec::new();
    check_account(account, &mut reasons);
    reasons.sort();
    reasons.dedup();
    reasons
}

/// Extracts the OCC-style underlying root from an Alpaca option contract symbol.
#[must_use]
pub fn option_underlying_symbol(symbol: &str) -> String {
    if let Ok(parts) = parse_alpaca_option_symbol(symbol) {
        return parts.underlying_symbol;
    }

    let mut root = String::new();
    for character in symbol.trim().chars() {
        if character.is_ascii_digit() {
            break;
        }
        root.push(character);
    }
    root
}

fn check_account(account: &AlpacaAccount, reasons: &mut Vec<String>) {
    if account.status.as_deref() != Some("ACTIVE") {
        reasons.push(format!(
            "account status is {}",
            account.status.as_deref().unwrap_or("unknown"),
        ));
    }
    if account.trading_blocked.unwrap_or(false) {
        reasons.push("account trading_blocked is true".to_string());
    }
    if account.account_blocked.unwrap_or(false) {
        reasons.push("account_blocked is true".to_string());
    }
    if account.trade_suspended_by_user.unwrap_or(false) {
        reasons.push("trade_suspended_by_user is true".to_string());
    }
}

fn has_nonzero_quantity(quantity: Option<&str>) -> bool {
    quantity
        .and_then(|value| value.parse::<f64>().ok())
        .is_some_and(|value| value != 0.0)
}

/// Converts an Alpaca order and nested legs into Nautilus order status reports.
///
/// Alpaca multi-leg parent orders do not carry a single tradable instrument. When the response
/// includes nested legs, this returns one report per leg. If no legs are present, it returns one
/// report for the order itself when a symbol is available.
///
/// # Errors
///
/// Returns an error when required order fields are absent or cannot be converted into Nautilus
/// identifiers, enums, or numeric types.
pub fn order_status_reports_from_alpaca(
    order: &AlpacaOrder,
    account_id: impl AsRef<str>,
    ts_init: UnixNanos,
) -> Result<Vec<OrderStatusReport>> {
    if let Some(legs) = &order.legs
        && !legs.is_empty()
    {
        return legs
            .iter()
            .map(|leg| {
                order_status_report_from_alpaca_leg(order, leg, account_id.as_ref(), ts_init)
            })
            .collect();
    }

    Ok(vec![order_status_report_from_alpaca_leg(
        order,
        order,
        account_id.as_ref(),
        ts_init,
    )?])
}

/// Converts an Alpaca trade-update payload into Nautilus order status reports.
///
/// When Alpaca sends multi-leg fill details outside `order.legs`, this function builds a minimal
/// nested order view from the trade update legs before using the standard order mapper.
///
/// # Errors
///
/// Returns an error when required order fields are absent or cannot be converted into Nautilus
/// identifiers, enums, or numeric types.
#[cfg(feature = "live")]
pub fn order_status_reports_from_trade_update(
    update: &AlpacaTradeUpdate,
    account_id: impl AsRef<str>,
    ts_init: UnixNanos,
) -> Result<Vec<OrderStatusReport>> {
    let order = order_with_trade_update_legs(update);
    order_status_reports_from_alpaca(&order, account_id, ts_init)
}

/// Converts an Alpaca fill or partial-fill trade update into Nautilus fill reports.
///
/// Multi-leg updates produce one fill report per filled leg. Non-fill events return an empty
/// vector.
///
/// # Errors
///
/// Returns an error when a fill update lacks required execution ID, symbol, order ID, side, price,
/// quantity, or timestamp fields.
#[cfg(feature = "live")]
pub fn fill_reports_from_trade_update(
    update: &AlpacaTradeUpdate,
    account_id: impl AsRef<str>,
    ts_init: UnixNanos,
) -> Result<Vec<FillReport>> {
    if !update.is_fill_event() {
        return Ok(Vec::new());
    }

    if let Some(legs) = update.legs.as_deref()
        && !legs.is_empty()
    {
        return legs
            .iter()
            .map(|leg| fill_report_from_trade_update_leg(update, leg, account_id.as_ref(), ts_init))
            .collect();
    }

    fill_report_from_trade_update_parent(update, account_id.as_ref(), ts_init)
        .map(|report| vec![report])
}

/// Converts Alpaca account activities into Nautilus fill reports.
///
/// Non-trade lifecycle activities such as assignment, exercise, and expiry are intentionally
/// skipped here; they are handled through position reconciliation until Nautilus has a dedicated
/// broker activity report for them.
#[cfg(feature = "live")]
pub fn fill_reports_from_alpaca_activities(
    activities: &[AlpacaActivity],
    account_id: impl AsRef<str>,
    ts_init: UnixNanos,
) -> Result<Vec<FillReport>> {
    activities
        .iter()
        .filter(|activity| activity_is_fill_reportable(activity))
        .map(|activity| fill_report_from_alpaca_activity(activity, account_id.as_ref(), ts_init))
        .collect()
}

/// Converts Alpaca positions into Nautilus position status reports.
///
/// Alpaca only returns open positions from the positions endpoint, so this mapper reports the
/// broker's current open inventory. Flat positions are reconstructed by the live reconciliation
/// manager when cached positions have no matching broker report.
#[cfg(feature = "live")]
pub fn position_status_reports_from_alpaca_positions(
    positions: &[AlpacaPosition],
    account_id: impl AsRef<str>,
    ts_init: UnixNanos,
) -> Result<Vec<PositionStatusReport>> {
    positions
        .iter()
        .map(|position| position_status_report_from_alpaca(position, account_id.as_ref(), ts_init))
        .collect()
}

#[cfg(feature = "live")]
fn order_with_trade_update_legs(update: &AlpacaTradeUpdate) -> AlpacaOrder {
    let mut order = update.order.clone();
    if order.legs.as_ref().is_some_and(|legs| !legs.is_empty()) {
        return order;
    }

    let Some(legs) = update.legs.as_deref().filter(|legs| !legs.is_empty()) else {
        return order;
    };

    order.legs = Some(
        legs.iter()
            .map(|leg| AlpacaOrder {
                id: leg.order_id.clone().or_else(|| order.id.clone()),
                client_order_id: order.client_order_id.clone(),
                created_at: order.created_at.clone(),
                updated_at: leg
                    .timestamp
                    .clone()
                    .or_else(|| update.timestamp.clone())
                    .or_else(|| order.updated_at.clone()),
                submitted_at: order.submitted_at.clone(),
                filled_at: leg.timestamp.clone().or_else(|| update.timestamp.clone()),
                expired_at: order.expired_at.clone(),
                canceled_at: order.canceled_at.clone(),
                failed_at: order.failed_at.clone(),
                asset_id: order.asset_id.clone(),
                symbol: leg.symbol.clone().or_else(|| order.symbol.clone()),
                asset_class: order.asset_class.clone(),
                qty: leg.qty.clone().or_else(|| order.qty.clone()),
                filled_qty: leg.qty.clone().or_else(|| order.filled_qty.clone()),
                filled_avg_price: leg.price.clone().or_else(|| order.filled_avg_price.clone()),
                order_type: order.order_type.clone(),
                side: leg.side.clone().or_else(|| order.side.clone()),
                time_in_force: order.time_in_force.clone(),
                limit_price: leg.price.clone().or_else(|| order.limit_price.clone()),
                status: order
                    .status
                    .clone()
                    .or_else(|| Some(status_from_trade_update_event(&update.event).to_string())),
                order_class: order.order_class.clone(),
                legs: None,
            })
            .collect(),
    );
    order
}

#[cfg(feature = "live")]
fn fill_report_from_trade_update_parent(
    update: &AlpacaTradeUpdate,
    account_id: &str,
    ts_init: UnixNanos,
) -> Result<FillReport> {
    let order = &update.order;
    let execution_id = update
        .execution_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca fill update missing execution_id".to_string()))?;
    let venue_order_id = order
        .id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca fill update missing order id".to_string()))?;
    let symbol = order
        .symbol
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca fill update missing symbol".to_string()))?;
    let ts_event = update
        .timestamp
        .as_deref()
        .map(unix_nanos_from_rfc3339)
        .transpose()?
        .ok_or_else(|| Error::Parse("Alpaca fill update missing timestamp".to_string()))?;

    Ok(FillReport::new(
        AccountId::from(account_id),
        instrument_id_from_alpaca_symbol(symbol)?,
        VenueOrderId::from(venue_order_id),
        TradeId::new(execution_id),
        order_side_from_alpaca(order.side.as_deref())?,
        quantity_from_optional_str(update.qty.as_deref(), "fill qty")?,
        price_from_required_str(update.price.as_deref(), "fill price")?,
        Money::zero(Currency::USD()),
        LiquiditySide::NoLiquiditySide,
        order
            .client_order_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(ClientOrderId::from),
        None,
        ts_event,
        ts_init,
        Some(UUID4::new()),
    ))
}

#[cfg(feature = "live")]
fn fill_report_from_trade_update_leg(
    update: &AlpacaTradeUpdate,
    leg: &AlpacaTradeUpdateLeg,
    account_id: &str,
    ts_init: UnixNanos,
) -> Result<FillReport> {
    let matching_leg = matching_order_leg(&update.order, leg);
    let execution_id = leg
        .execution_id
        .as_deref()
        .or(update.execution_id.as_deref())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca leg fill update missing execution_id".to_string()))?;
    let venue_order_id = leg
        .order_id
        .as_deref()
        .or_else(|| matching_leg.and_then(|order| order.id.as_deref()))
        .or(update.order.id.as_deref())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca leg fill update missing order id".to_string()))?;
    let symbol = leg
        .symbol
        .as_deref()
        .or_else(|| matching_leg.and_then(|order| order.symbol.as_deref()))
        .or(update.order.symbol.as_deref())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca leg fill update missing symbol".to_string()))?;
    let side = leg
        .side
        .as_deref()
        .or_else(|| matching_leg.and_then(|order| order.side.as_deref()))
        .or(update.order.side.as_deref());
    let ts_event = leg
        .timestamp
        .as_deref()
        .or(update.timestamp.as_deref())
        .map(unix_nanos_from_rfc3339)
        .transpose()?
        .ok_or_else(|| Error::Parse("Alpaca leg fill update missing timestamp".to_string()))?;

    Ok(FillReport::new(
        AccountId::from(account_id),
        instrument_id_from_alpaca_symbol(symbol)?,
        VenueOrderId::from(venue_order_id),
        TradeId::new(execution_id),
        order_side_from_alpaca(side)?,
        quantity_from_optional_str(leg.qty.as_deref(), "leg fill qty")?,
        price_from_required_str(leg.price.as_deref(), "leg fill price")?,
        Money::zero(Currency::USD()),
        LiquiditySide::NoLiquiditySide,
        matching_leg
            .and_then(|order| order.client_order_id.as_deref())
            .or(update.order.client_order_id.as_deref())
            .filter(|value| !value.trim().is_empty())
            .map(ClientOrderId::from),
        None,
        ts_event,
        ts_init,
        Some(UUID4::new()),
    ))
}

#[cfg(feature = "live")]
fn activity_is_fill_reportable(activity: &AlpacaActivity) -> bool {
    matches!(
        activity.activity_type.as_deref().map(normalize).as_deref(),
        Some("fill" | "optrd")
    )
}

#[cfg(feature = "live")]
fn fill_report_from_alpaca_activity(
    activity: &AlpacaActivity,
    account_id: &str,
    ts_init: UnixNanos,
) -> Result<FillReport> {
    let trade_id = activity
        .id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca fill activity missing id".to_string()))?;
    let venue_order_id = activity
        .order_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca fill activity missing order_id".to_string()))?;
    let symbol = activity
        .symbol
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca fill activity missing symbol".to_string()))?;
    let ts_event = activity
        .transaction_time
        .as_deref()
        .or(activity.date.as_deref())
        .map(unix_nanos_from_activity_timestamp)
        .transpose()?
        .ok_or_else(|| Error::Parse("Alpaca fill activity missing timestamp".to_string()))?;

    Ok(FillReport::new(
        AccountId::from(account_id),
        instrument_id_from_alpaca_symbol(symbol)?,
        VenueOrderId::from(venue_order_id),
        trade_id_from_alpaca_activity_id(trade_id)?,
        order_side_from_alpaca(activity.side.as_deref())?,
        quantity_from_optional_str(activity.qty.as_deref(), "activity qty")?,
        price_from_required_str(activity.price.as_deref(), "activity price")?,
        Money::zero(Currency::USD()),
        LiquiditySide::NoLiquiditySide,
        None,
        None,
        ts_event,
        ts_init,
        Some(UUID4::new()),
    ))
}

#[cfg(feature = "live")]
fn trade_id_from_alpaca_activity_id(value: &str) -> Result<TradeId> {
    let value = value.trim();
    if value.len() <= 36 {
        return TradeId::new_checked(value)
            .map_err(|e| Error::Parse(format!("invalid Alpaca trade id {value}: {e}")));
    }

    if let Some((_, suffix)) = value.rsplit_once("::")
        && suffix.len() <= 36
    {
        return TradeId::new_checked(suffix)
            .map_err(|e| Error::Parse(format!("invalid Alpaca trade id {value}: {e}")));
    }

    Err(Error::Parse(format!(
        "Alpaca activity id is too long for Nautilus TradeId: {value}",
    )))
}

#[cfg(feature = "live")]
fn position_status_report_from_alpaca(
    position: &AlpacaPosition,
    account_id: &str,
    ts_init: UnixNanos,
) -> Result<PositionStatusReport> {
    let symbol = position
        .symbol
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca position missing symbol".to_string()))?;
    let qty = decimal_from_required_str(position.qty.as_deref(), "position qty")?;
    let position_side = position_side_from_alpaca(position.side.as_deref(), qty)?;
    let quantity = quantity_from_decimal_abs(qty, position.qty.as_deref().unwrap_or("0"))?;
    let avg_px_open = position
        .avg_entry_price
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| Decimal::from_str(value))
        .transpose()
        .map_err(|e| Error::Parse(format!("invalid Alpaca avg_entry_price: {e}")))?;

    Ok(PositionStatusReport::new(
        AccountId::from(account_id),
        instrument_id_from_alpaca_symbol(symbol)?,
        position_side,
        quantity,
        ts_init,
        ts_init,
        Some(UUID4::new()),
        position
            .asset_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(PositionId::from),
        avg_px_open,
    ))
}

#[cfg(feature = "live")]
fn position_side_from_alpaca(side: Option<&str>, qty: Decimal) -> Result<PositionSideSpecified> {
    match side.map(normalize).as_deref() {
        Some("long") => Ok(PositionSideSpecified::Long),
        Some("short") => Ok(PositionSideSpecified::Short),
        Some(value) => Err(Error::Parse(format!(
            "unsupported Alpaca position side: {value}",
        ))),
        None if qty > Decimal::ZERO => Ok(PositionSideSpecified::Long),
        None if qty < Decimal::ZERO => Ok(PositionSideSpecified::Short),
        None => Ok(PositionSideSpecified::Flat),
    }
}

#[cfg(feature = "live")]
fn matching_order_leg<'a>(
    order: &'a AlpacaOrder,
    update_leg: &AlpacaTradeUpdateLeg,
) -> Option<&'a AlpacaOrder> {
    order.legs.as_deref()?.iter().find(|order_leg| {
        update_leg
            .order_id
            .as_deref()
            .zip(order_leg.id.as_deref())
            .is_some_and(|(left, right)| left == right)
            || update_leg
                .symbol
                .as_deref()
                .zip(order_leg.symbol.as_deref())
                .is_some_and(|(left, right)| left == right)
    })
}

#[cfg(feature = "live")]
fn status_from_trade_update_event(event: &str) -> &'static str {
    match event {
        "partial_fill" => "partially_filled",
        "fill" => "filled",
        "canceled" => "canceled",
        "expired" => "expired",
        "rejected" => "rejected",
        "pending_cancel" => "pending_cancel",
        "new" => "new",
        _ => "new",
    }
}

fn order_status_report_from_alpaca_leg(
    parent: &AlpacaOrder,
    leg: &AlpacaOrder,
    account_id: &str,
    ts_init: UnixNanos,
) -> Result<OrderStatusReport> {
    let venue_order_id = leg
        .id
        .as_deref()
        .or(parent.id.as_deref())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse("Alpaca order missing id".to_string()))?;
    let symbol = leg
        .symbol
        .as_deref()
        .or(parent.symbol.as_deref())
        .ok_or_else(|| Error::Parse("Alpaca order missing symbol".to_string()))?;
    let status = leg.status.as_deref().or(parent.status.as_deref());
    let order_type = leg.order_type.as_deref().or(parent.order_type.as_deref());
    let time_in_force = leg
        .time_in_force
        .as_deref()
        .or(parent.time_in_force.as_deref());
    let quantity = leg.qty.as_deref().or(parent.qty.as_deref());
    let filled_qty = leg.filled_qty.as_deref().or(parent.filled_qty.as_deref());
    let order_side = leg.side.as_deref().or(parent.side.as_deref());

    let mut report = OrderStatusReport::new(
        AccountId::from(account_id),
        instrument_id_from_alpaca_symbol(symbol)?,
        leg.client_order_id
            .as_deref()
            .or(parent.client_order_id.as_deref())
            .filter(|value| !value.trim().is_empty())
            .map(ClientOrderId::from),
        VenueOrderId::from(venue_order_id),
        order_side_from_alpaca(order_side)?,
        order_type_from_alpaca(order_type)?,
        time_in_force_from_alpaca(time_in_force)?,
        order_status_from_alpaca(status)?,
        quantity_from_optional_str(quantity, "qty")?,
        quantity_from_optional_str(filled_qty, "filled_qty")?,
        ts_from_order(parent, leg, "accepted")?,
        ts_from_order(parent, leg, "last")?,
        ts_init,
        Some(UUID4::new()),
    );
    report.contingency_type = ContingencyType::NoContingency;
    report.trailing_offset_type = TrailingOffsetType::NoTrailingOffset;
    report.price = leg
        .limit_price
        .as_deref()
        .or(parent.limit_price.as_deref())
        .map(price_from_str)
        .transpose()?;
    Ok(report)
}

/// Converts an Alpaca order status string into a Nautilus [`OrderStatus`].
///
/// # Errors
///
/// Returns an error for missing or unsupported statuses.
pub fn order_status_from_alpaca(status: Option<&str>) -> Result<OrderStatus> {
    match status.map(normalize).as_deref() {
        Some("accepted" | "accepted_for_bidding" | "new" | "pending_new") => {
            Ok(OrderStatus::Accepted)
        }
        Some("partially_filled") => Ok(OrderStatus::PartiallyFilled),
        Some("filled") => Ok(OrderStatus::Filled),
        Some("pending_cancel") => Ok(OrderStatus::PendingCancel),
        Some("pending_replace" | "replaced") => Ok(OrderStatus::PendingUpdate),
        Some("canceled") => Ok(OrderStatus::Canceled),
        Some("expired" | "done_for_day") => Ok(OrderStatus::Expired),
        Some("rejected" | "stopped" | "suspended" | "calculated") => Ok(OrderStatus::Rejected),
        Some(value) => Err(Error::Parse(format!(
            "unsupported Alpaca order status: {value}",
        ))),
        None => Err(Error::Parse("Alpaca order missing status".to_string())),
    }
}

fn order_side_from_alpaca(side: Option<&str>) -> Result<OrderSide> {
    match side.map(normalize).as_deref() {
        Some("buy" | "buy_to_cover" | "buy_to_open" | "buy_to_close") => Ok(OrderSide::Buy),
        Some("sell" | "sell_short" | "sell_to_open" | "sell_to_close") => Ok(OrderSide::Sell),
        Some(value) => Err(Error::Parse(format!(
            "unsupported Alpaca order side: {value}"
        ))),
        None => Err(Error::Parse("Alpaca order missing side".to_string())),
    }
}

fn order_type_from_alpaca(order_type: Option<&str>) -> Result<OrderType> {
    match order_type.map(normalize).as_deref() {
        Some("market") => Ok(OrderType::Market),
        Some("limit") => Ok(OrderType::Limit),
        Some("stop") => Ok(OrderType::StopMarket),
        Some("stop_limit") => Ok(OrderType::StopLimit),
        Some("trailing_stop") => Ok(OrderType::TrailingStopMarket),
        Some(value) => Err(Error::Parse(format!(
            "unsupported Alpaca order type: {value}"
        ))),
        None => Err(Error::Parse("Alpaca order missing type".to_string())),
    }
}

fn time_in_force_from_alpaca(time_in_force: Option<&str>) -> Result<TimeInForce> {
    match time_in_force.map(normalize).as_deref() {
        Some("day") => Ok(TimeInForce::Day),
        Some("gtc") => Ok(TimeInForce::Gtc),
        Some("ioc") => Ok(TimeInForce::Ioc),
        Some("fok") => Ok(TimeInForce::Fok),
        Some("opg") => Ok(TimeInForce::AtTheOpen),
        Some("cls") => Ok(TimeInForce::AtTheClose),
        Some(value) => Err(Error::Parse(format!(
            "unsupported Alpaca time_in_force: {value}",
        ))),
        None => Err(Error::Parse(
            "Alpaca order missing time_in_force".to_string(),
        )),
    }
}

fn instrument_id_from_alpaca_symbol(symbol: &str) -> Result<InstrumentId> {
    InstrumentId::from_str(&format!("{}.{}", symbol.trim(), ALPACA_VENUE))
        .map_err(|e| Error::Parse(format!("invalid Alpaca instrument id for {symbol}: {e}")))
}

fn quantity_from_optional_str(value: Option<&str>, field: &str) -> Result<Quantity> {
    value
        .ok_or_else(|| Error::Parse(format!("Alpaca order missing {field}")))
        .and_then(quantity_from_str)
}

fn quantity_from_str(value: &str) -> Result<Quantity> {
    value
        .parse::<f64>()
        .map(|parsed| Quantity::new(parsed, decimal_precision(value)))
        .map_err(|e| Error::Parse(format!("invalid Alpaca quantity {value}: {e}")))
}

#[cfg(feature = "live")]
fn quantity_from_decimal_abs(value: Decimal, raw_value: &str) -> Result<Quantity> {
    value
        .abs()
        .to_string()
        .parse::<f64>()
        .map(|parsed| Quantity::new(parsed, decimal_precision(raw_value)))
        .map_err(|e| Error::Parse(format!("invalid Alpaca quantity {raw_value}: {e}")))
}

fn price_from_str(value: &str) -> Result<Price> {
    value
        .parse::<f64>()
        .map(|parsed| Price::new(parsed, decimal_precision(value)))
        .map_err(|e| Error::Parse(format!("invalid Alpaca price {value}: {e}")))
}

#[cfg(feature = "live")]
fn price_from_required_str(value: Option<&str>, field: &str) -> Result<Price> {
    value
        .ok_or_else(|| Error::Parse(format!("Alpaca order missing {field}")))
        .and_then(price_from_str)
}

fn ts_from_order(parent: &AlpacaOrder, leg: &AlpacaOrder, kind: &str) -> Result<UnixNanos> {
    let timestamp = match kind {
        "accepted" => leg
            .submitted_at
            .as_deref()
            .or(leg.created_at.as_deref())
            .or(parent.submitted_at.as_deref())
            .or(parent.created_at.as_deref()),
        "last" => leg
            .updated_at
            .as_deref()
            .or(leg.filled_at.as_deref())
            .or(leg.canceled_at.as_deref())
            .or(leg.expired_at.as_deref())
            .or(leg.failed_at.as_deref())
            .or(leg.submitted_at.as_deref())
            .or(leg.created_at.as_deref())
            .or(parent.updated_at.as_deref())
            .or(parent.filled_at.as_deref())
            .or(parent.canceled_at.as_deref())
            .or(parent.expired_at.as_deref())
            .or(parent.failed_at.as_deref())
            .or(parent.submitted_at.as_deref())
            .or(parent.created_at.as_deref()),
        _ => None,
    };
    timestamp
        .map(unix_nanos_from_rfc3339)
        .transpose()?
        .ok_or_else(|| Error::Parse(format!("Alpaca order missing {kind} timestamp")))
}

fn unix_nanos_from_rfc3339(value: &str) -> Result<UnixNanos> {
    let timestamp =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .map_err(|e| Error::Parse(format!("invalid Alpaca timestamp {value}: {e}")))?;
    let nanos = timestamp.unix_timestamp_nanos();
    if nanos < 0 {
        return Err(Error::Parse(format!(
            "Alpaca timestamp predates Unix epoch: {value}",
        )));
    }
    Ok(UnixNanos::from(nanos as u64))
}

#[cfg(feature = "live")]
fn unix_nanos_from_activity_timestamp(value: &str) -> Result<UnixNanos> {
    if value.contains('T') {
        return unix_nanos_from_rfc3339(value);
    }

    let format = time::format_description::parse_borrowed::<3>("[year]-[month]-[day]")
        .map_err(|e| Error::Parse(format!("invalid Alpaca date format: {e}")))?;
    let date = time::Date::parse(value, &format)
        .map_err(|e| Error::Parse(format!("invalid Alpaca date {value}: {e}")))?;
    let timestamp = date
        .with_time(time::Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp_nanos();
    if timestamp < 0 {
        return Err(Error::Parse(format!(
            "Alpaca date predates Unix epoch: {value}",
        )));
    }
    Ok(UnixNanos::from(timestamp as u64))
}

#[cfg(feature = "live")]
fn decimal_from_required_str(value: Option<&str>, field: &str) -> Result<Decimal> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Parse(format!("Alpaca {field} missing")))
        .and_then(|value| {
            Decimal::from_str(value)
                .map_err(|e| Error::Parse(format!("invalid Alpaca {field} {value}: {e}")))
        })
}

fn decimal_precision(value: &str) -> u8 {
    value
        .trim_start_matches('-')
        .split_once('.')
        .map(|(_, decimals)| decimals.trim_end_matches('0').len().min(9) as u8)
        .unwrap_or(0)
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Local context for a submitted Alpaca multi-leg order.
#[cfg(feature = "live")]
#[derive(Clone, Debug, Default)]
struct MlegOrderContext {
    leg_client_ids_by_order_id: BTreeMap<String, ClientOrderId>,
    leg_client_ids_by_symbol: BTreeMap<String, ClientOrderId>,
    leg_sides_by_symbol: BTreeMap<String, String>,
}

#[cfg(feature = "live")]
type MlegOrderContextMap = BTreeMap<String, MlegOrderContext>;

/// Live execution client for Alpaca Trading.
#[cfg(feature = "live")]
#[derive(Debug)]
pub struct AlpacaExecutionClient {
    core: ExecutionClientCore,
    clock: &'static AtomicTime,
    config: AlpacaExecClientConfig,
    emitter: ExecutionEventEmitter,
    http_client: AlpacaHttpClient,
    ws_user: Option<AlpacaTradeUpdatesWebSocketClient>,
    ws_stream_handle: Option<JoinHandle<()>>,
    pending_tasks: Mutex<Vec<JoinHandle<()>>>,
    mleg_contexts: Arc<Mutex<MlegOrderContextMap>>,
    seen_trade_update_keys: Arc<Mutex<BTreeSet<String>>>,
    seen_activity_trade_ids: Arc<Mutex<BTreeSet<String>>>,
}

#[cfg(feature = "live")]
impl AlpacaExecutionClient {
    /// Creates a new [`AlpacaExecutionClient`].
    ///
    /// # Errors
    ///
    /// Returns an error if credentials cannot be resolved or the REST client cannot be built.
    pub fn new(core: ExecutionClientCore, config: AlpacaExecClientConfig) -> anyhow::Result<Self> {
        let credential = crate::common::credentials::AlpacaCredential::resolve(
            config.api_key.clone(),
            config.api_secret.clone(),
        )
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Alpaca credentials not available; set APCA_API_KEY_ID and APCA_API_SECRET_KEY or pass them in the config"
            )
        })?;
        let http_client = AlpacaHttpClient::from_exec_config(&config)
            .map_err(|e| anyhow::anyhow!("failed to create Alpaca HTTP client: {e}"))?;
        let ws_user = config.use_trade_updates_stream.then(|| {
            AlpacaTradeUpdatesWebSocketClient::new(
                config.resolved_trade_updates_ws_url(),
                credential,
            )
        });
        let clock = get_atomic_clock_realtime();
        let emitter = ExecutionEventEmitter::new(
            clock,
            core.trader_id,
            core.account_id,
            core.account_type,
            None,
        );

        Ok(Self {
            core,
            clock,
            config,
            emitter,
            http_client,
            ws_user,
            ws_stream_handle: None,
            pending_tasks: Mutex::new(Vec::new()),
            mleg_contexts: Arc::new(Mutex::new(MlegOrderContextMap::new())),
            seen_trade_update_keys: Arc::new(Mutex::new(BTreeSet::new())),
            seen_activity_trade_ids: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    fn spawn_task<F>(&self, description: &'static str, fut: F)
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let handle = get_runtime().spawn(async move {
            if let Err(e) = fut.await {
                log::warn!("{description} failed: {e:?}");
            }
        });

        let mut tasks = self.pending_tasks.lock().expect(MUTEX_POISONED);
        tasks.retain(|handle| !handle.is_finished());
        tasks.push(handle);
    }

    fn abort_pending_tasks(&self) {
        let mut tasks = self.pending_tasks.lock().expect(MUTEX_POISONED);
        for handle in tasks.drain(..) {
            handle.abort();
        }
    }

    async fn request_account_balances(&self) -> anyhow::Result<Vec<AccountBalance>> {
        let account = self
            .http_client
            .account()
            .await
            .map_err(|e| anyhow::anyhow!("failed to request Alpaca account: {e}"))?;
        account_balances_from_alpaca(&account)
    }

    fn order_from_cache_or_init(
        &self,
        client_order_id: ClientOrderId,
        order_init: nautilus_model::events::OrderInitialized,
    ) -> anyhow::Result<OrderAny> {
        self.core
            .cache()
            .order(&client_order_id)
            .map(|order| order.cloned())
            .map_or_else(|| Ok(OrderAny::try_from(order_init)?), Ok)
    }

    fn orders_from_submit_order_list(
        &self,
        cmd: &SubmitOrderList,
    ) -> anyhow::Result<Vec<OrderAny>> {
        cmd.order_list
            .client_order_ids
            .iter()
            .zip(cmd.order_inits.iter())
            .map(|(client_order_id, order_init)| {
                self.order_from_cache_or_init(*client_order_id, order_init.clone())
            })
            .collect()
    }
}

#[cfg(feature = "live")]
#[async_trait(?Send)]
impl ExecutionClient for AlpacaExecutionClient {
    fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    fn client_id(&self) -> ClientId {
        self.core.client_id
    }

    fn account_id(&self) -> AccountId {
        self.core.account_id
    }

    fn venue(&self) -> Venue {
        Venue::new(ALPACA_VENUE)
    }

    fn oms_type(&self) -> OmsType {
        self.core.oms_type
    }

    fn get_account(&self) -> Option<AccountAny> {
        self.core
            .cache()
            .account(&self.core.account_id)
            .map(|account| account.cloned())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.core.is_started() {
            return Ok(());
        }

        self.emitter.set_sender(get_exec_event_sender());
        self.core.set_started();
        log::info!(
            "Started: client_id={}, account_id={}, account_type={:?}, environment={:?}",
            self.core.client_id,
            self.core.account_id,
            self.core.account_type,
            self.config.environment,
        );
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if self.core.is_stopped() {
            return Ok(());
        }

        self.core.set_stopped();
        self.core.set_disconnected();

        if let Some(handle) = self.ws_stream_handle.take() {
            handle.abort();
        }
        self.abort_pending_tasks();
        log::info!("Stopped: client_id={}", self.core.client_id);
        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.core.is_connected() {
            return Ok(());
        }

        if let Some(ws_user) = &mut self.ws_user {
            if ws_user.is_active() || ws_user.is_reconnecting() {
                ws_user.disconnect().await;
            }

            ws_user.connect().await?;

            if let Some(mut rx) = ws_user.take_out_rx() {
                let emitter = self.emitter.clone();
                let http_client = self.http_client.clone();
                let account_id = self.core.account_id;
                let clock = self.clock;
                let client_order_id_prefix = self.config.client_order_id_prefix.clone();
                let external_order_filtering = self.config.external_order_filtering;
                let mleg_contexts = Arc::clone(&self.mleg_contexts);
                let seen_trade_update_keys = Arc::clone(&self.seen_trade_update_keys);
                let seen_activity_trade_ids = Arc::clone(&self.seen_activity_trade_ids);

                let handle = get_runtime().spawn(async move {
                    while let Some(message) = rx.recv().await {
                        match message {
                            AlpacaWsMessage::Authorization(auth) => {
                                log::info!(
                                    "Alpaca trade updates authorization status: {}",
                                    auth.status.as_deref().unwrap_or("unknown"),
                                );
                            }
                            AlpacaWsMessage::Listening(listening) => {
                                log::info!(
                                    "Alpaca trade updates listening on {:?}",
                                    listening.streams,
                                );
                            }
                            AlpacaWsMessage::TradeUpdate(update) => {
                                let mut update = *update;
                                apply_mleg_context_to_trade_update(&mut update, &mleg_contexts);
                                if external_order_filtering
                                    && !trade_update_matches_prefix(
                                        &update,
                                        &client_order_id_prefix,
                                    )
                                {
                                    continue;
                                }
                                if !mark_trade_update_seen(&update, &seen_trade_update_keys) {
                                    log::debug!(
                                        "Skipping duplicate Alpaca trade update: {}",
                                        trade_update_dedupe_key(&update),
                                    );
                                    continue;
                                }
                                emit_trade_update_reports(update, account_id, &emitter, clock);
                            }
                            AlpacaWsMessage::Disconnected { reason } => {
                                log::warn!("Alpaca trade updates WebSocket disconnected: {reason}");
                                emit_operator_event(
                                    "websocket_disconnect",
                                    json!({
                                        "stream": "trade_updates",
                                        "reason": reason,
                                    }),
                                );
                            }
                            AlpacaWsMessage::Reconnected => {
                                log::info!("Alpaca trade updates WebSocket reconnected");
                                emit_operator_event(
                                    "websocket_reconnect",
                                    json!({
                                        "stream": "trade_updates",
                                    }),
                                );
                                match emit_reconciliation_snapshot(
                                    &http_client,
                                    account_id,
                                    &emitter,
                                    clock,
                                    Some(60),
                                    Some(&seen_activity_trade_ids),
                                )
                                .await
                                {
                                    Ok(()) => emit_operator_event(
                                        "reconciliation_snapshot",
                                        json!({
                                            "source": "websocket_reconnect",
                                            "lookback_mins": 60,
                                        }),
                                    ),
                                    Err(e) => {
                                        log::warn!(
                                            "Failed to repair Alpaca state after reconnect: {e}"
                                        );
                                        emit_operator_event(
                                            "reconciliation_error",
                                            json!({
                                                "source": "websocket_reconnect",
                                                "error": e.to_string(),
                                            }),
                                        );
                                    }
                                }
                            }
                            AlpacaWsMessage::Error(err) => {
                                log::warn!("Alpaca trade updates WebSocket error: {err}");
                            }
                        }
                    }
                });
                self.ws_stream_handle = Some(handle);
            }
        }

        let balances = self.request_account_balances().await?;
        let ts = self.clock.get_time_ns();
        self.emitter
            .emit_account_state(balances, Vec::new(), true, ts);

        if let Some(poll_secs) = self
            .config
            .reconciliation_poll_secs
            .filter(|secs| *secs > 0)
        {
            let http_client = self.http_client.clone();
            let account_id = self.core.account_id;
            let emitter = self.emitter.clone();
            let clock = self.clock;
            let seen_activity_trade_ids = Arc::clone(&self.seen_activity_trade_ids);
            self.spawn_task("alpaca_reconciliation_poll", async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
                    if let Err(e) = emit_reconciliation_snapshot(
                        &http_client,
                        account_id,
                        &emitter,
                        clock,
                        Some(poll_secs / 60 + 2),
                        Some(&seen_activity_trade_ids),
                    )
                    .await
                    {
                        log::warn!("Alpaca periodic reconciliation repair failed: {e}");
                    }
                }
            });
        }

        self.core.set_connected();
        log::info!("Connected: client_id={}", self.core.client_id);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if self.core.is_disconnected() {
            return Ok(());
        }

        self.abort_pending_tasks();
        if let Some(ws_user) = &mut self.ws_user {
            ws_user.disconnect().await;
        }
        if let Some(handle) = self.ws_stream_handle.take() {
            handle.abort();
        }
        self.core.set_disconnected();
        log::info!("Disconnected: client_id={}", self.core.client_id);
        Ok(())
    }

    fn generate_account_state(
        &self,
        balances: Vec<AccountBalance>,
        margins: Vec<MarginBalance>,
        reported: bool,
        ts_event: UnixNanos,
    ) -> anyhow::Result<()> {
        self.emitter
            .emit_account_state(balances, margins, reported, ts_event);
        Ok(())
    }

    fn query_account(&self, _cmd: QueryAccount) -> anyhow::Result<()> {
        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        let clock = self.clock;

        self.spawn_task("query_account", async move {
            let account = http_client.account().await?;
            let balances = account_balances_from_alpaca(&account)?;
            let ts = clock.get_time_ns();
            emitter.emit_account_state(balances, Vec::new(), true, ts);
            Ok(())
        });
        Ok(())
    }

    fn query_order(&self, cmd: QueryOrder) -> anyhow::Result<()> {
        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        let account_id = self.core.account_id;
        let venue_order_id = cmd.venue_order_id;
        let client_order_id = cmd.client_order_id;
        let ts_init = cmd.ts_init;

        self.spawn_task("query_order", async move {
            let order = match venue_order_id {
                Some(venue_order_id) => {
                    http_client.order_by_id(venue_order_id.as_str(), true).await
                }
                None => {
                    http_client
                        .order_by_client_order_id(client_order_id.as_str(), true)
                        .await
                }
            }?;
            let reports = order_status_reports_from_alpaca(&order, account_id.as_str(), ts_init)?;
            for report in reports {
                emitter.send_order_status_report(report);
            }
            Ok(())
        });
        Ok(())
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let order = self.order_from_cache_or_init(cmd.client_order_id, cmd.order_init)?;
        if order.is_closed() {
            log::warn!("Cannot submit closed order {}", order.client_order_id());
            return Ok(());
        }

        let cache = self.core.cache();
        let instrument = cache.instrument(&order.instrument_id()).ok_or_else(|| {
            anyhow::anyhow!(
                "Alpaca simple order {} missing cached instrument {}",
                order.client_order_id(),
                order.instrument_id()
            )
        })?;
        let payload = build_simple_payload_from_order(&order, instrument)?;
        self.emitter.emit_order_submitted(&order);

        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        let clock = self.clock;
        self.spawn_task("submit_simple_order", async move {
            let result = match &payload {
                AlpacaSimplePayload::Equity(payload) => {
                    http_client.submit_equity_order(payload).await
                }
                AlpacaSimplePayload::Option(payload) => {
                    http_client.submit_simple_order(payload).await
                }
            };
            match result {
                Ok(submitted) => {
                    let ts_event = clock.get_time_ns();
                    match submitted.id.as_deref() {
                        Some(venue_order_id) => {
                            emitter.emit_order_accepted(
                                &order,
                                VenueOrderId::from(venue_order_id),
                                ts_event,
                            );
                        }
                        None => {
                            emitter.emit_order_rejected(
                                &order,
                                "submit-order-error: Alpaca accepted simple order but returned no venue order id",
                                ts_event,
                                false,
                            );
                        }
                    }
                }
                Err(e) => {
                    let ts_event = clock.get_time_ns();
                    emitter.emit_order_rejected(
                        &order,
                        &format!("submit-order-rejected: {e}"),
                        ts_event,
                        false,
                    );
                }
            }
            Ok(())
        });
        Ok(())
    }

    fn submit_order_list(&self, cmd: SubmitOrderList) -> anyhow::Result<()> {
        let orders = self.orders_from_submit_order_list(&cmd)?;
        for order in &orders {
            if order.is_closed() {
                log::warn!("Cannot submit closed order {}", order.client_order_id());
                return Ok(());
            }
        }

        let payload = build_mleg_payload_from_order_list(&cmd, &orders)?;

        for order in &orders {
            self.emitter.emit_order_submitted(order);
        }

        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        let mleg_contexts = Arc::clone(&self.mleg_contexts);
        let clock = self.clock;
        let order_list_id = cmd.order_list.id.to_string();
        register_pending_mleg_order_context(&mleg_contexts, &order_list_id, &orders);

        self.spawn_task("submit_mleg_order", async move {
            match http_client.submit_mleg_order(&payload).await {
                Ok(submitted) => {
                    let nested = match submitted.id.as_deref() {
                        Some(order_id)
                            if submitted
                                .legs
                                .as_ref()
                                .is_none_or(|legs| legs.is_empty()) =>
                        {
                            http_client
                                .order_by_id(order_id, true)
                                .await
                                .unwrap_or_else(|e| {
                                    log::warn!(
                                        "Submitted Alpaca MLeg order {order_id}, but nested lookup failed: {e}"
                                    );
                                    submitted.clone()
                                })
                        }
                        _ => submitted.clone(),
                    };
                    register_mleg_order_context(&mleg_contexts, &order_list_id, &orders, &nested);

                    let ts_event = clock.get_time_ns();
                    for order in &orders {
                        match venue_order_id_for_submitted_leg(order, &nested) {
                            Some(venue_order_id) => {
                                emitter.emit_order_accepted(order, venue_order_id, ts_event);
                            }
                            None => {
                                emitter.emit_order_rejected(
                                    order,
                                    "submit-order-error: Alpaca accepted MLeg order but returned no venue order id for this leg",
                                    ts_event,
                                    false,
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    let ts_event = clock.get_time_ns();
                    let reason = format!("submit-order-rejected: {e}");
                    for order in &orders {
                        emitter.emit_order_rejected(order, &reason, ts_event, false);
                    }
                }
            }

            Ok(())
        });

        Ok(())
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        let ts_event = self.clock.get_time_ns();
        let Some(venue_order_id) = cmd.venue_order_id else {
            self.emitter.emit_order_cancel_rejected_event(
                cmd.strategy_id,
                cmd.instrument_id,
                cmd.client_order_id,
                None,
                "cancel-order requires venue_order_id",
                ts_event,
            );
            return Ok(());
        };

        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        let strategy_id = cmd.strategy_id;
        let instrument_id = cmd.instrument_id;
        let client_order_id = cmd.client_order_id;

        self.spawn_task("cancel_order", async move {
            if let Err(e) = http_client.cancel_order(venue_order_id.as_str()).await {
                emitter.emit_order_cancel_rejected_event(
                    strategy_id,
                    instrument_id,
                    client_order_id,
                    Some(venue_order_id),
                    &format!("cancel-order rejected: {e}"),
                    ts_event,
                );
            }
            Ok(())
        });
        Ok(())
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        let ts_event = self.clock.get_time_ns();
        let Some(venue_order_id) = cmd.venue_order_id else {
            self.emitter.emit_order_modify_rejected_event(
                cmd.strategy_id,
                cmd.instrument_id,
                cmd.client_order_id,
                None,
                "modify-order requires venue_order_id",
                ts_event,
            );
            return Ok(());
        };

        let replace_request = match replace_order_request_from_modify_order(&cmd) {
            Ok(request) => request,
            Err(e) => {
                self.emitter.emit_order_modify_rejected_event(
                    cmd.strategy_id,
                    cmd.instrument_id,
                    cmd.client_order_id,
                    Some(venue_order_id),
                    &format!("modify-order rejected: {e}"),
                    ts_event,
                );
                return Ok(());
            }
        };

        let http_client = self.http_client.clone();
        let emitter = self.emitter.clone();
        let account_id = self.core.account_id;
        let strategy_id = cmd.strategy_id;
        let instrument_id = cmd.instrument_id;
        let client_order_id = cmd.client_order_id;
        let clock = self.clock;

        self.spawn_task("replace_order", async move {
            match http_client
                .replace_order(venue_order_id.as_str(), &replace_request)
                .await
            {
                Ok(replaced) => {
                    let ts_init = clock.get_time_ns();
                    let reports =
                        order_status_reports_from_alpaca(&replaced, account_id.as_str(), ts_init)?;
                    for report in reports {
                        emitter.send_order_status_report(report);
                    }
                }
                Err(e) => {
                    let ts_event = clock.get_time_ns();
                    emitter.emit_order_modify_rejected_event(
                        strategy_id,
                        instrument_id,
                        client_order_id,
                        Some(venue_order_id),
                        &format!("modify-order rejected: {e}"),
                        ts_event,
                    );
                }
            }

            Ok(())
        });

        Ok(())
    }

    async fn generate_order_status_report(
        &self,
        cmd: &GenerateOrderStatusReport,
    ) -> anyhow::Result<Option<OrderStatusReport>> {
        let order = if let Some(venue_order_id) = cmd.venue_order_id {
            self.http_client
                .order_by_id(venue_order_id.as_str(), true)
                .await?
        } else if let Some(client_order_id) = cmd.client_order_id {
            self.http_client
                .order_by_client_order_id(client_order_id.as_str(), true)
                .await?
        } else {
            return Ok(None);
        };

        let mut reports =
            order_status_reports_from_alpaca(&order, self.core.account_id.as_str(), cmd.ts_init)?;
        if let Some(instrument_id) = cmd.instrument_id {
            reports.retain(|report| report.instrument_id == instrument_id);
        }
        Ok(reports.into_iter().next())
    }

    async fn generate_order_status_reports(
        &self,
        cmd: &GenerateOrderStatusReports,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let mut request = crate::http::models::ListOrdersRequest {
            status: if cmd.open_only { "open" } else { "all" }.to_string(),
            nested: true,
            limit: 500,
            ..Default::default()
        };
        if let Some(instrument_id) = cmd.instrument_id {
            request.symbols = vec![instrument_id.symbol.as_str().to_string()];
        }
        if let Some(start) = cmd.start {
            request.after = Some(unix_nanos_to_rfc3339(start));
        }
        if let Some(end) = cmd.end {
            request.until = Some(unix_nanos_to_rfc3339(end));
        }

        let orders = self.http_client.orders(&request).await?;
        let mut reports = Vec::new();
        for order in orders {
            reports.extend(order_status_reports_from_alpaca(
                &order,
                self.core.account_id.as_str(),
                cmd.ts_init,
            )?);
        }
        Ok(reports)
    }

    async fn generate_fill_reports(
        &self,
        cmd: GenerateFillReports,
    ) -> anyhow::Result<Vec<FillReport>> {
        let mut request = ListActivitiesRequest::option_reconciliation();
        request.direction = Some("asc".to_string());
        if let Some(start) = cmd.start {
            request.after = Some(unix_nanos_to_rfc3339(start));
        }
        if let Some(end) = cmd.end {
            request.until = Some(unix_nanos_to_rfc3339(end));
        }

        let activities = self.http_client.account_activities_all(&request).await?;
        let mut reports = fill_reports_from_alpaca_activities(
            &activities,
            self.core.account_id.as_str(),
            cmd.ts_init,
        )?;

        if let Some(instrument_id) = cmd.instrument_id {
            reports.retain(|report| report.instrument_id == instrument_id);
        }
        if let Some(venue_order_id) = cmd.venue_order_id {
            reports.retain(|report| report.venue_order_id == venue_order_id);
        }
        if let Some(start) = cmd.start {
            reports.retain(|report| report.ts_event >= start);
        }
        if let Some(end) = cmd.end {
            reports.retain(|report| report.ts_event <= end);
        }

        Ok(reports)
    }

    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let positions = self.http_client.positions().await?;
        let mut reports = position_status_reports_from_alpaca_positions(
            &positions,
            self.core.account_id.as_str(),
            cmd.ts_init,
        )?;

        if let Some(instrument_id) = cmd.instrument_id {
            reports.retain(|report| report.instrument_id == instrument_id);
        }
        if let Some(start) = cmd.start {
            reports.retain(|report| report.ts_last >= start);
        }
        if let Some(end) = cmd.end {
            reports.retain(|report| report.ts_last <= end);
        }

        Ok(reports)
    }

    async fn generate_mass_status(
        &self,
        lookback_mins: Option<u64>,
    ) -> anyhow::Result<Option<ExecutionMassStatus>> {
        let ts_now = self.clock.get_time_ns();
        let start = reconciliation_start(ts_now, lookback_mins);
        let fill_cmd = GenerateFillReports::new(
            UUID4::new(),
            ts_now,
            None,
            None,
            Some(start),
            None,
            None,
            None,
        );
        let position_cmd =
            GeneratePositionStatusReports::new(UUID4::new(), ts_now, None, None, None, None, None);

        let (order_reports, fill_reports, position_reports) = tokio::try_join!(
            reconciliation_order_status_reports(
                &self.http_client,
                self.core.account_id.as_str(),
                ts_now,
                start,
            ),
            self.generate_fill_reports(fill_cmd),
            self.generate_position_status_reports(&position_cmd),
        )?;

        let mut mass_status = ExecutionMassStatus::new(
            self.core.client_id,
            self.core.account_id,
            Venue::new(ALPACA_VENUE),
            ts_now,
            None,
        );
        mass_status.add_order_reports(order_reports);
        mass_status.add_fill_reports(fill_reports);
        mass_status.add_position_reports(position_reports);
        Ok(Some(mass_status))
    }
}

#[cfg(feature = "live")]
fn reconciliation_start(ts_now: UnixNanos, lookback_mins: Option<u64>) -> UnixNanos {
    let lookback_mins = lookback_mins.unwrap_or(DEFAULT_RECONCILIATION_LOOKBACK_MINS);
    UnixNanos::from(
        ts_now.as_u64().saturating_sub(
            lookback_mins
                .saturating_mul(60)
                .saturating_mul(1_000_000_000),
        ),
    )
}

#[cfg(feature = "live")]
async fn reconciliation_order_status_reports(
    http_client: &AlpacaHttpClient,
    account_id: &str,
    ts_init: UnixNanos,
    start: UnixNanos,
) -> anyhow::Result<Vec<OrderStatusReport>> {
    let recent_request = ListOrdersRequest {
        status: "all".to_string(),
        nested: true,
        limit: 500,
        after: Some(unix_nanos_to_rfc3339(start)),
        ..Default::default()
    };
    let open_request = ListOrdersRequest {
        status: "open".to_string(),
        nested: true,
        limit: 500,
        ..Default::default()
    };

    let (recent_orders, open_orders) = tokio::try_join!(
        http_client.orders(&recent_request),
        http_client.orders(&open_request),
    )?;

    let mut reports_by_order = BTreeMap::new();
    for order in recent_orders.iter().chain(open_orders.iter()) {
        for report in order_status_reports_from_alpaca(order, account_id, ts_init)? {
            reports_by_order.insert(report.venue_order_id, report);
        }
    }

    Ok(reports_by_order.into_values().collect())
}

#[cfg(feature = "live")]
async fn emit_reconciliation_snapshot(
    http_client: &AlpacaHttpClient,
    account_id: AccountId,
    emitter: &ExecutionEventEmitter,
    clock: &'static AtomicTime,
    lookback_mins: Option<u64>,
    seen_activity_trade_ids: Option<&Arc<Mutex<BTreeSet<String>>>>,
) -> anyhow::Result<()> {
    let ts_now = clock.get_time_ns();
    let start = reconciliation_start(ts_now, lookback_mins);

    let account = http_client.account().await?;
    emitter.emit_account_state(
        account_balances_from_alpaca(&account)?,
        Vec::new(),
        true,
        ts_now,
    );

    for report in
        reconciliation_order_status_reports(http_client, account_id.as_str(), ts_now, start).await?
    {
        emitter.send_order_status_report(report);
    }

    let mut activity_request = ListActivitiesRequest::option_reconciliation();
    activity_request.direction = Some("asc".to_string());
    activity_request.after = Some(unix_nanos_to_rfc3339(start));
    let activities = http_client
        .account_activities_all(&activity_request)
        .await?;
    for report in fill_reports_from_alpaca_activities(&activities, account_id.as_str(), ts_now)? {
        if let Some(seen_activity_trade_ids) = seen_activity_trade_ids
            && !mark_fill_report_seen(&report, seen_activity_trade_ids)
        {
            continue;
        }
        emitter.send_fill_report(report);
    }

    let positions = http_client.positions().await?;
    for report in
        position_status_reports_from_alpaca_positions(&positions, account_id.as_str(), ts_now)?
    {
        emitter.send_position_report(report);
    }

    Ok(())
}

#[cfg(feature = "live")]
fn emit_trade_update_reports(
    update: AlpacaTradeUpdate,
    account_id: AccountId,
    emitter: &ExecutionEventEmitter,
    clock: &'static AtomicTime,
) {
    let ts_init = clock.get_time_ns();
    let order_reports =
        match order_status_reports_from_trade_update(&update, account_id.as_str(), ts_init) {
            Ok(reports) => reports,
            Err(e) => {
                log::warn!("Failed to convert Alpaca trade update into order report: {e}");
                return;
            }
        };
    let fill_reports = match fill_reports_from_trade_update(&update, account_id.as_str(), ts_init) {
        Ok(reports) => reports,
        Err(e) => {
            log::warn!("Failed to convert Alpaca trade update into fill report: {e}");
            Vec::new()
        }
    };

    if fill_reports.is_empty() {
        for report in order_reports {
            emitter.send_order_status_report(report);
        }
        return;
    }

    let mut fills_by_order: BTreeMap<VenueOrderId, Vec<FillReport>> = BTreeMap::new();
    for fill in fill_reports {
        fills_by_order
            .entry(fill.venue_order_id)
            .or_default()
            .push(fill);
    }

    for report in order_reports {
        if let Some(fills) = fills_by_order.remove(&report.venue_order_id) {
            emitter.send_order_with_fills(report, fills);
        } else {
            emitter.send_order_status_report(report);
        }
    }

    for fills in fills_by_order.into_values() {
        for fill in fills {
            emitter.send_fill_report(fill);
        }
    }
}

#[cfg(feature = "live")]
#[derive(Clone, Debug)]
enum AlpacaSimplePayload {
    Equity(EquityOrderPayload),
    Option(SimpleOrderPayload),
}

#[cfg(feature = "live")]
fn build_simple_payload_from_order(
    order: &OrderAny,
    instrument: &InstrumentAny,
) -> anyhow::Result<AlpacaSimplePayload> {
    match instrument {
        InstrumentAny::Equity(_) => build_equity_payload_from_order(order),
        InstrumentAny::OptionContract(_) => build_option_payload_from_order(order),
        instrument => anyhow::bail!(
            "Alpaca simple order {} uses unsupported instrument class {:?}",
            order.client_order_id(),
            instrument.instrument_class(),
        ),
    }
}

#[cfg(feature = "live")]
fn build_option_payload_from_order(order: &OrderAny) -> anyhow::Result<AlpacaSimplePayload> {
    validate_simple_option_order(order)?;
    let quantity = positive_integer_quantity(order.quantity(), order.client_order_id())?;
    let price = order
        .price()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Alpaca simple order {} missing limit price",
                order.client_order_id()
            )
        })?
        .as_f64();
    if price <= 0.0 {
        anyhow::bail!(
            "Alpaca simple order {} price must be positive, was {price}",
            order.client_order_id()
        );
    }
    let position_intent = alpaca_position_intent(order.order_side(), order.is_reduce_only())?;
    SimpleOrderPayload::new_option_limit(
        order.instrument_id().symbol.as_str(),
        quantity,
        position_intent,
        price,
    )
    .and_then(|payload| payload.with_client_order_id(order.client_order_id().to_string()))
    .map(AlpacaSimplePayload::Option)
    .map_err(|e| anyhow::anyhow!("invalid Alpaca simple payload: {e}"))
}

#[cfg(feature = "live")]
fn build_equity_payload_from_order(order: &OrderAny) -> anyhow::Result<AlpacaSimplePayload> {
    validate_simple_equity_order(order)?;
    let quantity = positive_integer_quantity(order.quantity(), order.client_order_id())?;
    let price = order
        .price()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Alpaca equity order {} missing limit price",
                order.client_order_id()
            )
        })?
        .as_f64();
    if price <= 0.0 {
        anyhow::bail!(
            "Alpaca equity order {} price must be positive, was {price}",
            order.client_order_id()
        );
    }
    let side = alpaca_order_side(order.order_side())?;
    EquityOrderPayload::new_limit(order.instrument_id().symbol.as_str(), quantity, side, price)
        .and_then(|payload| payload.with_client_order_id(order.client_order_id().to_string()))
        .map(AlpacaSimplePayload::Equity)
        .map_err(|e| anyhow::anyhow!("invalid Alpaca simple payload: {e}"))
}

#[cfg(feature = "live")]
fn build_mleg_payload_from_order_list(
    cmd: &SubmitOrderList,
    orders: &[OrderAny],
) -> anyhow::Result<MlegOrderPayload> {
    if orders.len() < 2 {
        anyhow::bail!("Alpaca MLeg submit requires at least two leg orders");
    }
    if orders.len() > 4 {
        anyhow::bail!("Alpaca MLeg submit supports at most four leg orders");
    }

    let quantities = orders
        .iter()
        .map(|order| positive_integer_quantity(order.quantity(), order.client_order_id()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let strategy_qty = quantities
        .iter()
        .copied()
        .reduce(gcd_u64)
        .ok_or_else(|| anyhow::anyhow!("Alpaca MLeg submit requires leg quantities"))?;
    if strategy_qty == 0 {
        anyhow::bail!("Alpaca MLeg strategy quantity must be positive");
    }

    let trade_intent = mleg_trade_intent(orders)?;
    let mut net_credit = 0.0_f64;
    let mut legs = Vec::with_capacity(orders.len());

    for (order, leg_qty) in orders.iter().zip(quantities) {
        validate_mleg_leg_order(order)?;
        let ratio_qty = leg_qty / strategy_qty;
        let price = order
            .price()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Alpaca MLeg leg {} missing limit price",
                    order.client_order_id()
                )
            })?
            .as_f64();
        if price <= 0.0 {
            anyhow::bail!(
                "Alpaca MLeg leg {} price must be positive, was {price}",
                order.client_order_id()
            );
        }

        let side_multiplier = match order.order_side() {
            OrderSide::Sell => 1.0,
            OrderSide::Buy => -1.0,
            OrderSide::NoOrderSide => {
                anyhow::bail!(
                    "Alpaca MLeg leg {} missing order side",
                    order.client_order_id()
                )
            }
        };
        net_credit += side_multiplier * price * ratio_qty as f64;

        let position_intent = alpaca_position_intent(order.order_side(), order.is_reduce_only())?;
        legs.push(MlegOrderLeg::new(
            order.instrument_id().symbol.as_str(),
            position_intent.side(),
            position_intent,
            ratio_qty.to_string(),
        ));
    }

    if net_credit == 0.0 {
        anyhow::bail!("Alpaca MLeg signed net limit price must be non-zero");
    }
    let premium_kind = match trade_intent {
        TradeIntent::Open if net_credit > 0.0 => NetPremiumKind::Credit,
        TradeIntent::Open => NetPremiumKind::Debit,
        TradeIntent::Close if net_credit < 0.0 => NetPremiumKind::Credit,
        TradeIntent::Close => NetPremiumKind::Debit,
    };
    let signed_limit_price = signed_net_limit_price(net_credit.abs(), premium_kind, trade_intent);

    MlegOrderPayload::new_limit(strategy_qty, signed_limit_price, legs)
        .and_then(|payload| payload.with_client_order_id(cmd.order_list.id.to_string()))
        .map_err(|e| anyhow::anyhow!("invalid Alpaca MLeg payload: {e}"))
}

#[cfg(feature = "live")]
fn replace_order_request_from_modify_order(
    cmd: &ModifyOrder,
) -> anyhow::Result<ReplaceOrderRequest> {
    if cmd.trigger_price.is_some() {
        anyhow::bail!("Alpaca MLeg replace does not support trigger_price");
    }

    let limit_price = cmd
        .params
        .as_ref()
        .and_then(|params| params.get_str("alpaca_limit_price"))
        .map(ToString::to_string)
        .or_else(|| {
            cmd.price
                .map(|price| price.as_decimal().normalize().to_string())
        });
    let qty = cmd
        .params
        .as_ref()
        .and_then(|params| params.get_str("alpaca_qty"))
        .map(ToString::to_string)
        .or_else(|| {
            cmd.quantity
                .map(|qty| qty.as_decimal().normalize().to_string())
        });

    if limit_price.is_none() && qty.is_none() {
        anyhow::bail!("Alpaca replace requires price, quantity, alpaca_limit_price, or alpaca_qty",);
    }

    Ok(ReplaceOrderRequest {
        qty,
        limit_price,
        ..Default::default()
    })
}

#[cfg(feature = "live")]
fn validate_simple_option_order(order: &OrderAny) -> anyhow::Result<()> {
    if order.instrument_id().venue != Venue::new(ALPACA_VENUE) {
        anyhow::bail!(
            "Alpaca simple order {} has non-Alpaca instrument {}",
            order.client_order_id(),
            order.instrument_id()
        );
    }
    if order.order_type() != OrderType::Limit {
        anyhow::bail!(
            "Alpaca simple order {} must be a limit order, was {:?}",
            order.client_order_id(),
            order.order_type()
        );
    }
    if order.time_in_force() != TimeInForce::Day {
        anyhow::bail!(
            "Alpaca simple order {} must use DAY time in force, was {:?}",
            order.client_order_id(),
            order.time_in_force()
        );
    }
    if order.is_quote_quantity() {
        anyhow::bail!(
            "Alpaca simple order {} cannot use quote quantity",
            order.client_order_id()
        );
    }
    if matches!(order.order_side(), OrderSide::NoOrderSide) {
        anyhow::bail!(
            "Alpaca simple order {} missing order side",
            order.client_order_id()
        );
    }
    Ok(())
}

#[cfg(feature = "live")]
fn validate_simple_equity_order(order: &OrderAny) -> anyhow::Result<()> {
    if order.instrument_id().venue != Venue::new(ALPACA_VENUE) {
        anyhow::bail!(
            "Alpaca equity order {} has non-Alpaca instrument {}",
            order.client_order_id(),
            order.instrument_id()
        );
    }
    if order.order_type() != OrderType::Limit {
        anyhow::bail!(
            "Alpaca equity order {} must be a limit order, was {:?}",
            order.client_order_id(),
            order.order_type()
        );
    }
    if order.time_in_force() != TimeInForce::Day {
        anyhow::bail!(
            "Alpaca equity order {} must use DAY time in force, was {:?}",
            order.client_order_id(),
            order.time_in_force()
        );
    }
    if order.is_quote_quantity() {
        anyhow::bail!(
            "Alpaca equity order {} cannot use quote quantity",
            order.client_order_id()
        );
    }
    if matches!(order.order_side(), OrderSide::NoOrderSide) {
        anyhow::bail!(
            "Alpaca equity order {} missing order side",
            order.client_order_id()
        );
    }
    Ok(())
}

#[cfg(feature = "live")]
fn validate_mleg_leg_order(order: &OrderAny) -> anyhow::Result<()> {
    if order.instrument_id().venue != Venue::new(ALPACA_VENUE) {
        anyhow::bail!(
            "Alpaca MLeg leg {} has non-Alpaca instrument {}",
            order.client_order_id(),
            order.instrument_id()
        );
    }
    if order.order_type() != OrderType::Limit {
        anyhow::bail!(
            "Alpaca MLeg leg {} must be a limit order, was {:?}",
            order.client_order_id(),
            order.order_type()
        );
    }
    if order.time_in_force() != TimeInForce::Day {
        anyhow::bail!(
            "Alpaca MLeg leg {} must use DAY time in force, was {:?}",
            order.client_order_id(),
            order.time_in_force()
        );
    }
    if order.is_quote_quantity() {
        anyhow::bail!(
            "Alpaca MLeg leg {} cannot use quote quantity",
            order.client_order_id()
        );
    }
    Ok(())
}

#[cfg(feature = "live")]
fn mleg_trade_intent(orders: &[OrderAny]) -> anyhow::Result<TradeIntent> {
    let all_reduce_only = orders.iter().all(|order| order.is_reduce_only());
    let any_reduce_only = orders.iter().any(|order| order.is_reduce_only());
    match (all_reduce_only, any_reduce_only) {
        (true, true) => Ok(TradeIntent::Close),
        (false, false) => Ok(TradeIntent::Open),
        (false, true) => {
            anyhow::bail!("Alpaca MLeg orders must be all opening or all closing legs")
        }
        (true, false) => unreachable!("all_reduce_only implies any_reduce_only"),
    }
}

#[cfg(feature = "live")]
fn positive_integer_quantity(
    quantity: Quantity,
    client_order_id: ClientOrderId,
) -> anyhow::Result<u64> {
    let normalized = quantity.as_decimal().normalize();
    if normalized.scale() != 0 {
        anyhow::bail!(
            "Alpaca MLeg leg {client_order_id} quantity must be an integer contract count, was {quantity}"
        );
    }
    let parsed = normalized
        .to_string()
        .parse::<u64>()
        .map_err(|e| anyhow::anyhow!("invalid Alpaca MLeg quantity {quantity}: {e}"))?;
    if parsed == 0 {
        anyhow::bail!("Alpaca MLeg leg {client_order_id} quantity must be positive");
    }
    Ok(parsed)
}

#[cfg(feature = "live")]
fn alpaca_position_intent(
    side: OrderSide,
    reduce_only: bool,
) -> anyhow::Result<AlpacaPositionIntent> {
    match (side, reduce_only) {
        (OrderSide::Buy, false) => Ok(AlpacaPositionIntent::BuyToOpen),
        (OrderSide::Sell, false) => Ok(AlpacaPositionIntent::SellToOpen),
        (OrderSide::Buy, true) => Ok(AlpacaPositionIntent::BuyToClose),
        (OrderSide::Sell, true) => Ok(AlpacaPositionIntent::SellToClose),
        (OrderSide::NoOrderSide, _) => anyhow::bail!("Alpaca MLeg leg missing order side"),
    }
}

#[cfg(feature = "live")]
fn alpaca_order_side(side: OrderSide) -> anyhow::Result<crate::orders::AlpacaOrderSide> {
    match side {
        OrderSide::Buy => Ok(crate::orders::AlpacaOrderSide::Buy),
        OrderSide::Sell => Ok(crate::orders::AlpacaOrderSide::Sell),
        OrderSide::NoOrderSide => anyhow::bail!("Alpaca equity order missing order side"),
    }
}

#[cfg(feature = "live")]
const fn alpaca_side_str(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "buy",
        OrderSide::Sell => "sell",
        OrderSide::NoOrderSide => "",
    }
}

#[cfg(feature = "live")]
const fn gcd_u64(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(feature = "live")]
fn register_mleg_order_context(
    contexts: &Arc<Mutex<MlegOrderContextMap>>,
    order_list_id: &str,
    orders: &[OrderAny],
    submitted: &AlpacaOrder,
) {
    let mut context = mleg_order_context_from_orders(orders);
    if let Some(legs) = submitted.legs.as_deref() {
        for leg in legs {
            let Some(symbol) = leg.symbol.as_deref() else {
                continue;
            };
            let Some(client_order_id) = context.leg_client_ids_by_symbol.get(symbol).copied()
            else {
                continue;
            };
            if let Some(order_id) = leg.id.as_deref().filter(|value| !value.trim().is_empty()) {
                context
                    .leg_client_ids_by_order_id
                    .insert(order_id.to_string(), client_order_id);
            }
        }
    }

    let mut map = contexts.lock().expect(MUTEX_POISONED);
    map.insert(order_list_id.to_string(), context.clone());
    if let Some(parent_id) = submitted
        .id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        map.insert(parent_id.to_string(), context.clone());
    }
    if let Some(parent_client_id) = submitted
        .client_order_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        map.insert(parent_client_id.to_string(), context);
    }
}

#[cfg(feature = "live")]
fn register_pending_mleg_order_context(
    contexts: &Arc<Mutex<MlegOrderContextMap>>,
    order_list_id: &str,
    orders: &[OrderAny],
) {
    let context = mleg_order_context_from_orders(orders);
    contexts
        .lock()
        .expect(MUTEX_POISONED)
        .insert(order_list_id.to_string(), context);
}

#[cfg(feature = "live")]
fn mleg_order_context_from_orders(orders: &[OrderAny]) -> MlegOrderContext {
    let mut context = MlegOrderContext::default();
    for order in orders {
        let symbol = order.instrument_id().symbol.as_str().to_string();
        context
            .leg_client_ids_by_symbol
            .insert(symbol.clone(), order.client_order_id());
        context
            .leg_sides_by_symbol
            .insert(symbol, alpaca_side_str(order.order_side()).to_string());
    }
    context
}

#[cfg(feature = "live")]
fn apply_mleg_context_to_trade_update(
    update: &mut AlpacaTradeUpdate,
    contexts: &Arc<Mutex<MlegOrderContextMap>>,
) {
    let Some(context) = mleg_context_for_trade_update(update, contexts) else {
        return;
    };

    if let Some(legs) = update.order.legs.as_mut() {
        for leg in legs {
            if leg.client_order_id.is_none() {
                leg.client_order_id = client_order_id_for_leg_context(
                    &context,
                    leg.id.as_deref(),
                    leg.symbol.as_deref(),
                )
                .map(|client_order_id| client_order_id.to_string());
            }
        }
    }

    if update
        .order
        .legs
        .as_ref()
        .is_none_or(|legs| legs.is_empty())
        && let Some(update_legs) = update.legs.as_deref()
    {
        update.order.legs = Some(
            update_legs
                .iter()
                .map(|leg| AlpacaOrder {
                    id: leg.order_id.clone().or_else(|| update.order.id.clone()),
                    client_order_id: client_order_id_for_leg_context(
                        &context,
                        leg.order_id.as_deref(),
                        leg.symbol.as_deref(),
                    )
                    .map(|client_order_id| client_order_id.to_string())
                    .or_else(|| update.order.client_order_id.clone()),
                    created_at: update.order.created_at.clone(),
                    updated_at: leg
                        .timestamp
                        .clone()
                        .or_else(|| update.timestamp.clone())
                        .or_else(|| update.order.updated_at.clone()),
                    submitted_at: update.order.submitted_at.clone(),
                    filled_at: leg.timestamp.clone().or_else(|| update.timestamp.clone()),
                    expired_at: update.order.expired_at.clone(),
                    canceled_at: update.order.canceled_at.clone(),
                    failed_at: update.order.failed_at.clone(),
                    asset_id: update.order.asset_id.clone(),
                    symbol: leg.symbol.clone().or_else(|| update.order.symbol.clone()),
                    asset_class: update.order.asset_class.clone(),
                    qty: leg.qty.clone().or_else(|| update.order.qty.clone()),
                    filled_qty: leg.qty.clone().or_else(|| update.order.filled_qty.clone()),
                    filled_avg_price: leg
                        .price
                        .clone()
                        .or_else(|| update.order.filled_avg_price.clone()),
                    order_type: update.order.order_type.clone(),
                    side: leg.side.clone().or_else(|| {
                        leg.symbol
                            .as_deref()
                            .and_then(|symbol| context.leg_sides_by_symbol.get(symbol).cloned())
                    }),
                    time_in_force: update.order.time_in_force.clone(),
                    limit_price: leg
                        .price
                        .clone()
                        .or_else(|| update.order.limit_price.clone()),
                    status: update.order.status.clone().or_else(|| {
                        Some(status_from_trade_update_event(&update.event).to_string())
                    }),
                    order_class: update.order.order_class.clone(),
                    legs: None,
                })
                .collect(),
        );
    }
}

#[cfg(feature = "live")]
fn mleg_context_for_trade_update(
    update: &AlpacaTradeUpdate,
    contexts: &Arc<Mutex<MlegOrderContextMap>>,
) -> Option<MlegOrderContext> {
    let map = contexts.lock().expect(MUTEX_POISONED);
    update
        .order
        .id
        .as_deref()
        .and_then(|key| map.get(key).cloned())
        .or_else(|| {
            update
                .order
                .client_order_id
                .as_deref()
                .and_then(|key| map.get(key).cloned())
        })
}

#[cfg(feature = "live")]
fn client_order_id_for_leg_context(
    context: &MlegOrderContext,
    venue_order_id: Option<&str>,
    symbol: Option<&str>,
) -> Option<ClientOrderId> {
    venue_order_id
        .and_then(|order_id| context.leg_client_ids_by_order_id.get(order_id).copied())
        .or_else(|| symbol.and_then(|symbol| context.leg_client_ids_by_symbol.get(symbol).copied()))
}

#[cfg(feature = "live")]
fn venue_order_id_for_submitted_leg(
    order: &OrderAny,
    submitted: &AlpacaOrder,
) -> Option<VenueOrderId> {
    submitted
        .legs
        .as_deref()
        .and_then(|legs| {
            legs.iter()
                .find(|leg| leg.symbol.as_deref() == Some(order.instrument_id().symbol.as_str()))
        })
        .and_then(|leg| leg.id.as_deref())
        .or(submitted.id.as_deref())
        .filter(|value| !value.trim().is_empty())
        .map(VenueOrderId::from)
}

#[cfg(feature = "live")]
fn trade_update_matches_prefix(update: &AlpacaTradeUpdate, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }

    update
        .order
        .client_order_id
        .as_deref()
        .is_some_and(|client_order_id| client_order_id.starts_with(prefix))
        || update.order.legs.as_deref().is_some_and(|legs| {
            legs.iter().any(|leg| {
                leg.client_order_id
                    .as_deref()
                    .is_some_and(|client_order_id| client_order_id.starts_with(prefix))
            })
        })
}

#[cfg(feature = "live")]
fn mark_trade_update_seen(
    update: &AlpacaTradeUpdate,
    seen_keys: &Arc<Mutex<BTreeSet<String>>>,
) -> bool {
    let key = trade_update_dedupe_key(update);
    let mut seen = seen_keys.lock().expect(MUTEX_POISONED);
    seen.insert(key)
}

#[cfg(feature = "live")]
fn mark_fill_report_seen(
    report: &FillReport,
    seen_trade_ids: &Arc<Mutex<BTreeSet<String>>>,
) -> bool {
    let mut seen = seen_trade_ids.lock().expect(MUTEX_POISONED);
    seen.insert(report.trade_id.as_str().to_string())
}

#[cfg(feature = "live")]
fn trade_update_dedupe_key(update: &AlpacaTradeUpdate) -> String {
    let mut parts = vec![
        update.event.clone(),
        update.order.id.clone().unwrap_or_default(),
        update.order.client_order_id.clone().unwrap_or_default(),
        update.timestamp.clone().unwrap_or_default(),
        update.execution_id.clone().unwrap_or_default(),
        update.price.clone().unwrap_or_default(),
        update.qty.clone().unwrap_or_default(),
    ];

    if let Some(legs) = update.legs.as_deref() {
        for leg in legs {
            parts.push(leg.order_id.clone().unwrap_or_default());
            parts.push(leg.execution_id.clone().unwrap_or_default());
            parts.push(leg.symbol.clone().unwrap_or_default());
            parts.push(leg.timestamp.clone().unwrap_or_default());
            parts.push(leg.price.clone().unwrap_or_default());
            parts.push(leg.qty.clone().unwrap_or_default());
        }
    }

    parts.join("|")
}

#[cfg(feature = "live")]
fn account_balances_from_alpaca(account: &AlpacaAccount) -> anyhow::Result<Vec<AccountBalance>> {
    let currency = account
        .currency
        .as_deref()
        .unwrap_or("USD")
        .parse::<Currency>()?;
    let total = decimal_from_account_field(
        account
            .equity
            .as_deref()
            .or(account.portfolio_value.as_deref())
            .or(account.cash.as_deref()),
        "equity/portfolio_value/cash",
    )?;
    let free = decimal_from_account_field(
        account
            .options_buying_power
            .as_deref()
            .or(account.buying_power.as_deref())
            .or(account.cash.as_deref())
            .or(Some("0")),
        "options_buying_power/buying_power/cash",
    )?;
    let balance = AccountBalance::from_total_and_free(total, free, currency)?;
    Ok(vec![balance])
}

#[cfg(feature = "live")]
fn decimal_from_account_field(value: Option<&str>, field: &str) -> anyhow::Result<Decimal> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Alpaca account missing {field}"))
        .and_then(|value| {
            Decimal::from_str(value)
                .map_err(|e| anyhow::anyhow!("invalid Alpaca account {field} {value}: {e}"))
        })
}

#[cfg(feature = "live")]
fn unix_nanos_to_rfc3339(ts: UnixNanos) -> String {
    unix_nanos_to_iso8601(ts)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "live")]
    use nautilus_core::Params;
    use nautilus_model::enums::OrderStatus;
    #[cfg(feature = "live")]
    use nautilus_model::{
        events::OrderInitialized,
        identifiers::{OrderListId, StrategyId, TraderId},
        instruments::{
            InstrumentAny,
            stubs::{equity_aapl, option_contract_appl},
        },
        orders::OrderList,
    };
    #[cfg(feature = "live")]
    use serde_json::json;

    use super::*;

    #[test]
    fn order_status_from_alpaca_maps_lifecycle_states() {
        assert_eq!(
            order_status_from_alpaca(Some("accepted")).unwrap(),
            OrderStatus::Accepted,
        );
        assert_eq!(
            order_status_from_alpaca(Some("partially_filled")).unwrap(),
            OrderStatus::PartiallyFilled,
        );
        assert_eq!(
            order_status_from_alpaca(Some("filled")).unwrap(),
            OrderStatus::Filled,
        );
        assert_eq!(
            order_status_from_alpaca(Some("pending_cancel")).unwrap(),
            OrderStatus::PendingCancel,
        );
        assert_eq!(
            order_status_from_alpaca(Some("pending_replace")).unwrap(),
            OrderStatus::PendingUpdate,
        );
        assert_eq!(
            order_status_from_alpaca(Some("canceled")).unwrap(),
            OrderStatus::Canceled,
        );
        assert_eq!(
            order_status_from_alpaca(Some("expired")).unwrap(),
            OrderStatus::Expired,
        );
        assert_eq!(
            order_status_from_alpaca(Some("rejected")).unwrap(),
            OrderStatus::Rejected,
        );
    }

    #[test]
    fn order_status_reports_from_alpaca_prefers_nested_leg_fields() {
        let mut parent = empty_order();
        parent.id = Some("parent-order".to_string());
        parent.client_order_id = Some("parent-client".to_string());
        parent.created_at = Some("2026-05-01T13:30:00Z".to_string());
        parent.updated_at = Some("2026-05-01T13:30:01Z".to_string());
        parent.order_type = Some("limit".to_string());
        parent.time_in_force = Some("day".to_string());
        parent.status = Some("new".to_string());
        parent.limit_price = Some("0.50".to_string());
        parent.legs = Some(vec![
            leg_order(
                "leg-short",
                "SPY260508P00500000",
                "sell",
                "filled",
                "1",
                "1",
                "2026-05-01T13:30:02Z",
                "2026-05-01T13:31:00Z",
            ),
            leg_order(
                "leg-long",
                "SPY260508P00495000",
                "buy",
                "new",
                "1",
                "0",
                "2026-05-01T13:30:03Z",
                "2026-05-01T13:30:04Z",
            ),
        ]);

        let reports =
            order_status_reports_from_alpaca(&parent, "ALPACA-001", UnixNanos::from(1)).unwrap();

        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].venue_order_id, VenueOrderId::from("leg-short"));
        assert_eq!(reports[0].order_status, OrderStatus::Filled);
        assert_eq!(
            reports[0].instrument_id,
            InstrumentId::from_str("SPY260508P00500000.ALPACA").unwrap(),
        );
        assert_eq!(reports[1].venue_order_id, VenueOrderId::from("leg-long"));
        assert_eq!(reports[1].order_status, OrderStatus::Accepted);
    }

    #[cfg(feature = "live")]
    #[test]
    fn fill_reports_from_trade_update_uses_per_leg_execution_payload() {
        let mut parent = empty_order();
        parent.id = Some("parent-order".to_string());
        parent.client_order_id = Some("nautilus-1".to_string());
        parent.created_at = Some("2026-05-01T13:30:00Z".to_string());
        parent.updated_at = Some("2026-05-01T13:31:00Z".to_string());
        parent.order_type = Some("limit".to_string());
        parent.time_in_force = Some("day".to_string());
        parent.status = Some("filled".to_string());
        parent.limit_price = Some("0.50".to_string());
        parent.legs = Some(vec![
            leg_order(
                "leg-short",
                "SPY260508P00500000",
                "sell",
                "filled",
                "1",
                "1",
                "2026-05-01T13:30:02Z",
                "2026-05-01T13:31:00Z",
            ),
            leg_order(
                "leg-long",
                "SPY260508P00495000",
                "buy",
                "filled",
                "1",
                "1",
                "2026-05-01T13:30:03Z",
                "2026-05-01T13:31:00Z",
            ),
        ]);

        let update = AlpacaTradeUpdate {
            event: "fill".to_string(),
            order: parent,
            execution_id: Some("parent-exec".to_string()),
            price: None,
            qty: None,
            position_qty: None,
            timestamp: Some("2026-05-01T13:31:00Z".to_string()),
            legs: Some(vec![
                AlpacaTradeUpdateLeg {
                    execution_id: Some("short-exec".to_string()),
                    price: Some("0.25".to_string()),
                    qty: Some("1".to_string()),
                    position_qty: None,
                    order_id: Some("leg-short".to_string()),
                    symbol: Some("SPY260508P00500000".to_string()),
                    timestamp: Some("2026-05-01T13:31:00Z".to_string()),
                    side: None,
                },
                AlpacaTradeUpdateLeg {
                    execution_id: Some("long-exec".to_string()),
                    price: Some("0.10".to_string()),
                    qty: Some("1".to_string()),
                    position_qty: None,
                    order_id: Some("leg-long".to_string()),
                    symbol: Some("SPY260508P00495000".to_string()),
                    timestamp: Some("2026-05-01T13:31:00Z".to_string()),
                    side: None,
                },
            ]),
        };

        let reports =
            fill_reports_from_trade_update(&update, "ALPACA-001", UnixNanos::from(1)).unwrap();

        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].venue_order_id, VenueOrderId::from("leg-short"));
        assert_eq!(reports[0].trade_id, TradeId::new("short-exec"));
        assert_eq!(reports[0].order_side, OrderSide::Sell);
        assert_eq!(
            reports[0].instrument_id,
            InstrumentId::from_str("SPY260508P00500000.ALPACA").unwrap(),
        );
        assert_eq!(reports[1].venue_order_id, VenueOrderId::from("leg-long"));
        assert_eq!(reports[1].order_side, OrderSide::Buy);
    }

    #[cfg(feature = "live")]
    #[test]
    fn fill_reports_from_alpaca_activities_maps_trade_activities() {
        let activities = vec![
            AlpacaActivity {
                activity_type: Some("FILL".to_string()),
                id: Some("20260501133100000::8efc7b9a-8b2b-4000-9955-d36e7db0df74".to_string()),
                cum_qty: Some("1".to_string()),
                leaves_qty: Some("0".to_string()),
                price: Some("0.72".to_string()),
                qty: Some("1".to_string()),
                side: Some("sell".to_string()),
                symbol: Some("SPY260508P00500000".to_string()),
                transaction_time: Some("2026-05-01T13:31:00Z".to_string()),
                order_id: Some("leg-short".to_string()),
                activity_subtype: None,
                date: None,
                net_amount: None,
                cusip: None,
                per_share_amount: None,
            },
            AlpacaActivity {
                activity_type: Some("OPASN".to_string()),
                id: Some("assignment-1".to_string()),
                cum_qty: None,
                leaves_qty: None,
                price: None,
                qty: Some("1".to_string()),
                side: None,
                symbol: Some("SPY260508P00500000".to_string()),
                transaction_time: None,
                order_id: None,
                activity_subtype: None,
                date: Some("2026-05-02".to_string()),
                net_amount: None,
                cusip: None,
                per_share_amount: None,
            },
        ];

        let reports =
            fill_reports_from_alpaca_activities(&activities, "ALPACA-001", UnixNanos::from(1))
                .unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].venue_order_id, VenueOrderId::from("leg-short"));
        assert_eq!(
            reports[0].trade_id,
            TradeId::new("8efc7b9a-8b2b-4000-9955-d36e7db0df74")
        );
        assert_eq!(reports[0].order_side, OrderSide::Sell);
        assert_eq!(
            reports[0].instrument_id,
            InstrumentId::from_str("SPY260508P00500000.ALPACA").unwrap(),
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn position_status_reports_from_alpaca_positions_maps_short_option_position() {
        let positions = vec![AlpacaPosition {
            asset_id: Some("asset-1".to_string()),
            symbol: Some("SPY260508P00500000".to_string()),
            exchange: Some("OPRA".to_string()),
            asset_class: Some("us_option".to_string()),
            qty: Some("2".to_string()),
            side: Some("short".to_string()),
            market_value: None,
            cost_basis: None,
            current_price: None,
            unrealized_pl: None,
            unrealized_plpc: None,
            avg_entry_price: Some("0.72".to_string()),
        }];

        let reports = position_status_reports_from_alpaca_positions(
            &positions,
            "ALPACA-001",
            UnixNanos::from(10),
        )
        .unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].position_side, PositionSideSpecified::Short);
        assert_eq!(
            reports[0].instrument_id,
            InstrumentId::from_str("SPY260508P00500000.ALPACA").unwrap(),
        );
        assert_eq!(
            reports[0].venue_position_id,
            Some(PositionId::from("asset-1"))
        );
        assert_eq!(reports[0].signed_decimal_qty, Decimal::from(-2));
        assert_eq!(
            reports[0].avg_px_open,
            Some(Decimal::from_str("0.72").unwrap())
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_simple_payload_from_order_uses_position_intent() {
        let order = mleg_limit_order("O-1", "SPY260508P00500000", OrderSide::Sell, 0.75, false);

        let instrument = InstrumentAny::OptionContract(option_contract_appl());

        let payload = build_simple_payload_from_order(&order, &instrument).unwrap();
        let AlpacaSimplePayload::Option(payload) = payload else {
            panic!("expected option simple payload");
        };

        assert_eq!(payload.symbol, "SPY260508P00500000");
        assert_eq!(payload.side, crate::orders::AlpacaOrderSide::Sell);
        assert_eq!(payload.position_intent, AlpacaPositionIntent::SellToOpen);
        assert_eq!(payload.limit_price.as_deref(), Some("0.75"));
        assert_eq!(payload.client_order_id.as_deref(), Some("O-1"));
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_simple_payload_from_order_routes_equity_instruments() {
        let order = mleg_limit_order("O-1", "AAPL", OrderSide::Buy, 212.34, false);
        let instrument = InstrumentAny::Equity(equity_aapl());

        let payload = build_simple_payload_from_order(&order, &instrument).unwrap();
        let AlpacaSimplePayload::Equity(payload) = payload else {
            panic!("expected equity simple payload");
        };

        assert_eq!(payload.symbol, "AAPL");
        assert_eq!(payload.side, crate::orders::AlpacaOrderSide::Buy);
        assert_eq!(payload.limit_price.as_deref(), Some("212.34"));
        assert_eq!(payload.client_order_id.as_deref(), Some("O-1"));
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_simple_payload_from_order_routes_equity_sell_orders() {
        let order = mleg_limit_order("O-2", "AAPL", OrderSide::Sell, 211.11, true);
        let instrument = InstrumentAny::Equity(equity_aapl());

        let payload = build_simple_payload_from_order(&order, &instrument).unwrap();
        let AlpacaSimplePayload::Equity(payload) = payload else {
            panic!("expected equity simple payload");
        };

        assert_eq!(payload.symbol, "AAPL");
        assert_eq!(payload.side, crate::orders::AlpacaOrderSide::Sell);
        assert_eq!(payload.limit_price.as_deref(), Some("211.11"));
        assert_eq!(payload.client_order_id.as_deref(), Some("O-2"));
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_mleg_payload_from_order_list_uses_signed_credit_open_price() {
        let orders = vec![
            mleg_limit_order("O-1", "SPY260508P00500000", OrderSide::Sell, 0.75, false),
            mleg_limit_order("O-2", "SPY260508P00495000", OrderSide::Buy, 0.25, false),
        ];
        let cmd = submit_order_list_for_orders("OL-1", &orders);

        let payload = build_mleg_payload_from_order_list(&cmd, &orders).unwrap();

        assert_eq!(payload.client_order_id.as_deref(), Some("OL-1"));
        assert_eq!(payload.qty, "1");
        assert_eq!(payload.limit_price, "-0.50");
        assert_eq!(payload.legs.len(), 2);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::SellToOpen
        );
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::BuyToOpen
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_mleg_payload_from_order_list_uses_signed_credit_close_price() {
        let orders = vec![
            mleg_limit_order("O-1", "SPY260508P00500000", OrderSide::Buy, 0.75, true),
            mleg_limit_order("O-2", "SPY260508P00495000", OrderSide::Sell, 0.25, true),
        ];
        let cmd = submit_order_list_for_orders("OL-CLOSE-1", &orders);

        let payload = build_mleg_payload_from_order_list(&cmd, &orders).unwrap();

        assert_eq!(payload.client_order_id.as_deref(), Some("OL-CLOSE-1"));
        assert_eq!(payload.qty, "1");
        assert_eq!(payload.limit_price, "0.50");
        assert_eq!(payload.legs.len(), 2);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::BuyToClose
        );
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::SellToClose
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_mleg_payload_from_order_list_uses_signed_debit_close_price() {
        let orders = vec![
            mleg_limit_order("O-1", "SPY260508P00500000", OrderSide::Sell, 1.55, true),
            mleg_limit_order("O-2", "SPY260508P00495000", OrderSide::Buy, 0.40, true),
        ];
        let cmd = submit_order_list_for_orders("OL-DEBIT-CLOSE-1", &orders);

        let payload = build_mleg_payload_from_order_list(&cmd, &orders).unwrap();

        assert_eq!(payload.client_order_id.as_deref(), Some("OL-DEBIT-CLOSE-1"));
        assert_eq!(payload.qty, "1");
        assert_eq!(payload.limit_price, "-1.15");
        assert_eq!(payload.legs.len(), 2);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::SellToClose
        );
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::BuyToClose
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_mleg_payload_from_order_list_supports_four_leg_iron_condor_open() {
        let orders = vec![
            mleg_limit_order("O-1", "SPY260508P00500000", OrderSide::Sell, 0.55, false),
            mleg_limit_order("O-2", "SPY260508P00495000", OrderSide::Buy, 0.20, false),
            mleg_limit_order("O-3", "SPY260508C00520000", OrderSide::Sell, 0.60, false),
            mleg_limit_order("O-4", "SPY260508C00525000", OrderSide::Buy, 0.25, false),
        ];
        let cmd = submit_order_list_for_orders("OL-IC-1", &orders);

        let payload = build_mleg_payload_from_order_list(&cmd, &orders).unwrap();

        assert_eq!(payload.client_order_id.as_deref(), Some("OL-IC-1"));
        assert_eq!(payload.qty, "1");
        assert_eq!(payload.limit_price, "-0.70");
        assert_eq!(payload.legs.len(), 4);
        assert_eq!(
            payload
                .legs
                .iter()
                .map(|leg| leg.position_intent)
                .collect::<Vec<_>>(),
            vec![
                AlpacaPositionIntent::SellToOpen,
                AlpacaPositionIntent::BuyToOpen,
                AlpacaPositionIntent::SellToOpen,
                AlpacaPositionIntent::BuyToOpen,
            ]
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn build_mleg_payload_from_order_list_supports_four_leg_iron_condor_close() {
        let orders = vec![
            mleg_limit_order("O-1", "SPY260508P00500000", OrderSide::Buy, 1.20, true),
            mleg_limit_order("O-2", "SPY260508P00495000", OrderSide::Sell, 0.30, true),
            mleg_limit_order("O-3", "SPY260508C00520000", OrderSide::Buy, 1.40, true),
            mleg_limit_order("O-4", "SPY260508C00525000", OrderSide::Sell, 0.40, true),
        ];
        let cmd = submit_order_list_for_orders("OL-IC-CLOSE-1", &orders);

        let payload = build_mleg_payload_from_order_list(&cmd, &orders).unwrap();

        assert_eq!(payload.client_order_id.as_deref(), Some("OL-IC-CLOSE-1"));
        assert_eq!(payload.qty, "1");
        assert_eq!(payload.limit_price, "1.90");
        assert_eq!(payload.legs.len(), 4);
        assert_eq!(
            payload
                .legs
                .iter()
                .map(|leg| leg.position_intent)
                .collect::<Vec<_>>(),
            vec![
                AlpacaPositionIntent::BuyToClose,
                AlpacaPositionIntent::SellToClose,
                AlpacaPositionIntent::BuyToClose,
                AlpacaPositionIntent::SellToClose,
            ]
        );
    }

    #[cfg(feature = "live")]
    #[test]
    fn replace_order_request_from_modify_order_prefers_alpaca_net_limit_param() {
        let mut params = Params::new();
        params.insert("alpaca_limit_price".to_string(), json!("-0.45"));

        let cmd = ModifyOrder::new(
            TraderId::from("TRADER-001"),
            None,
            StrategyId::from("S-001"),
            InstrumentId::from_str("SPY260508P00500000.ALPACA").unwrap(),
            ClientOrderId::from("O-1"),
            Some(VenueOrderId::from("parent-order")),
            Some(Quantity::new(2.0, 0)),
            Some(Price::new(0.70, 2)),
            None,
            UUID4::new(),
            UnixNanos::from(1),
            Some(params),
            None,
        );

        let request = replace_order_request_from_modify_order(&cmd).unwrap();

        assert_eq!(request.limit_price.as_deref(), Some("-0.45"));
        assert_eq!(request.qty.as_deref(), Some("2"));
        assert!(request.stop_price.is_none());
    }

    #[cfg(feature = "live")]
    #[test]
    fn replace_order_request_from_modify_order_rejects_trigger_price() {
        let cmd = ModifyOrder::new(
            TraderId::from("TRADER-001"),
            None,
            StrategyId::from("S-001"),
            InstrumentId::from_str("SPY260508P00500000.ALPACA").unwrap(),
            ClientOrderId::from("O-1"),
            Some(VenueOrderId::from("parent-order")),
            None,
            Some(Price::new(0.70, 2)),
            Some(Price::new(0.50, 2)),
            UUID4::new(),
            UnixNanos::from(1),
            None,
            None,
        );

        assert!(replace_order_request_from_modify_order(&cmd).is_err());
    }

    #[cfg(feature = "live")]
    #[test]
    fn mleg_context_restores_leg_client_ids_on_trade_update() {
        let orders = vec![
            mleg_limit_order("O-1", "SPY260508P00500000", OrderSide::Sell, 0.75, false),
            mleg_limit_order("O-2", "SPY260508P00495000", OrderSide::Buy, 0.25, false),
        ];
        let mut submitted = empty_order();
        submitted.id = Some("parent-order".to_string());
        submitted.client_order_id = Some("OL-1".to_string());
        submitted.created_at = Some("2026-05-01T13:30:00Z".to_string());
        submitted.updated_at = Some("2026-05-01T13:30:01Z".to_string());
        submitted.order_type = Some("limit".to_string());
        submitted.time_in_force = Some("day".to_string());
        submitted.status = Some("new".to_string());
        submitted.limit_price = Some("-0.50".to_string());
        submitted.legs = Some(vec![
            leg_order(
                "leg-short",
                "SPY260508P00500000",
                "sell",
                "new",
                "1",
                "0",
                "2026-05-01T13:30:00Z",
                "2026-05-01T13:30:01Z",
            ),
            leg_order(
                "leg-long",
                "SPY260508P00495000",
                "buy",
                "new",
                "1",
                "0",
                "2026-05-01T13:30:00Z",
                "2026-05-01T13:30:01Z",
            ),
        ]);

        let contexts = Arc::new(Mutex::new(MlegOrderContextMap::new()));
        register_mleg_order_context(&contexts, "OL-1", &orders, &submitted);

        let mut update = AlpacaTradeUpdate {
            event: "fill".to_string(),
            order: {
                let mut order = empty_order();
                order.id = Some("parent-order".to_string());
                order.client_order_id = Some("OL-1".to_string());
                order.created_at = Some("2026-05-01T13:30:00Z".to_string());
                order.updated_at = Some("2026-05-01T13:31:00Z".to_string());
                order.order_type = Some("limit".to_string());
                order.time_in_force = Some("day".to_string());
                order.status = Some("filled".to_string());
                order.limit_price = Some("-0.50".to_string());
                order
            },
            execution_id: None,
            price: None,
            qty: None,
            position_qty: None,
            timestamp: Some("2026-05-01T13:31:00Z".to_string()),
            legs: Some(vec![
                AlpacaTradeUpdateLeg {
                    execution_id: Some("short-exec".to_string()),
                    price: Some("0.75".to_string()),
                    qty: Some("1".to_string()),
                    position_qty: None,
                    order_id: Some("leg-short".to_string()),
                    symbol: Some("SPY260508P00500000".to_string()),
                    timestamp: Some("2026-05-01T13:31:00Z".to_string()),
                    side: None,
                },
                AlpacaTradeUpdateLeg {
                    execution_id: Some("long-exec".to_string()),
                    price: Some("0.25".to_string()),
                    qty: Some("1".to_string()),
                    position_qty: None,
                    order_id: Some("leg-long".to_string()),
                    symbol: Some("SPY260508P00495000".to_string()),
                    timestamp: Some("2026-05-01T13:31:00Z".to_string()),
                    side: None,
                },
            ]),
        };

        apply_mleg_context_to_trade_update(&mut update, &contexts);
        let reports =
            order_status_reports_from_trade_update(&update, "ALPACA-001", UnixNanos::from(1))
                .unwrap();

        assert_eq!(reports[0].client_order_id, Some(ClientOrderId::from("O-1")));
        assert_eq!(reports[0].order_side, OrderSide::Sell);
        assert_eq!(reports[1].client_order_id, Some(ClientOrderId::from("O-2")));
        assert_eq!(reports[1].order_side, OrderSide::Buy);
    }

    #[cfg(feature = "live")]
    #[test]
    fn mark_trade_update_seen_filters_duplicate_updates() {
        let update = AlpacaTradeUpdate {
            event: "fill".to_string(),
            order: {
                let mut order = empty_order();
                order.id = Some("parent-order".to_string());
                order.client_order_id = Some("OL-1".to_string());
                order
            },
            execution_id: Some("exec-1".to_string()),
            price: Some("0.50".to_string()),
            qty: Some("1".to_string()),
            position_qty: None,
            timestamp: Some("2026-05-01T13:31:00Z".to_string()),
            legs: None,
        };
        let seen = Arc::new(Mutex::new(BTreeSet::new()));

        assert!(mark_trade_update_seen(&update, &seen));
        assert!(!mark_trade_update_seen(&update, &seen));
    }

    fn leg_order(
        id: &str,
        symbol: &str,
        side: &str,
        status: &str,
        qty: &str,
        filled_qty: &str,
        submitted_at: &str,
        updated_at: &str,
    ) -> AlpacaOrder {
        let mut order = empty_order();
        order.id = Some(id.to_string());
        order.symbol = Some(symbol.to_string());
        order.qty = Some(qty.to_string());
        order.filled_qty = Some(filled_qty.to_string());
        order.order_type = Some("limit".to_string());
        order.side = Some(side.to_string());
        order.time_in_force = Some("day".to_string());
        order.limit_price = Some("0.25".to_string());
        order.status = Some(status.to_string());
        order.submitted_at = Some(submitted_at.to_string());
        order.updated_at = Some(updated_at.to_string());
        order
    }

    fn empty_order() -> AlpacaOrder {
        AlpacaOrder {
            id: None,
            client_order_id: None,
            created_at: None,
            updated_at: None,
            submitted_at: None,
            filled_at: None,
            expired_at: None,
            canceled_at: None,
            failed_at: None,
            asset_id: None,
            symbol: None,
            asset_class: None,
            qty: None,
            filled_qty: None,
            filled_avg_price: None,
            order_type: None,
            side: None,
            time_in_force: None,
            limit_price: None,
            status: None,
            order_class: None,
            legs: None,
        }
    }

    #[cfg(feature = "live")]
    fn mleg_limit_order(
        client_order_id: &str,
        symbol: &str,
        side: OrderSide,
        price: f64,
        reduce_only: bool,
    ) -> OrderAny {
        let init = OrderInitialized::new(
            TraderId::from("TRADER-001"),
            StrategyId::from("S-001"),
            InstrumentId::from_str(&format!("{symbol}.ALPACA")).unwrap(),
            ClientOrderId::from(client_order_id),
            side,
            OrderType::Limit,
            Quantity::new(1.0, 0),
            TimeInForce::Day,
            false,
            reduce_only,
            false,
            false,
            UUID4::new(),
            UnixNanos::from(1),
            UnixNanos::from(1),
            Some(Price::new(price, 2)),
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
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        OrderAny::try_from(init).unwrap()
    }

    #[cfg(feature = "live")]
    fn submit_order_list_for_orders(order_list_id: &str, orders: &[OrderAny]) -> SubmitOrderList {
        let trader_id = TraderId::from("TRADER-001");
        let strategy_id = StrategyId::from("S-001");
        let order_list_id = OrderListId::from(order_list_id);
        let order_list = OrderList::new(
            order_list_id,
            orders[0].instrument_id(),
            strategy_id,
            orders.iter().map(Order::client_order_id).collect(),
            UnixNanos::from(1),
        );
        SubmitOrderList::new(
            trader_id,
            None,
            strategy_id,
            order_list,
            orders
                .iter()
                .map(|order| order.init_event().clone())
                .collect(),
            None,
            None,
            None,
            UUID4::new(),
            UnixNanos::from(1),
            None,
        )
    }
}
