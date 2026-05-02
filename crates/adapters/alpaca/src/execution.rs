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

use std::{collections::BTreeSet, str::FromStr};

use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    enums::{ContingencyType, OrderSide, OrderStatus, OrderType, TimeInForce, TrailingOffsetType},
    identifiers::{AccountId, ClientOrderId, InstrumentId, VenueOrderId},
    reports::OrderStatusReport,
    types::{Price, Quantity},
};

use crate::{
    common::consts::ALPACA_VENUE,
    http::{
        error::{Error, Result},
        models::{AlpacaAccount, AlpacaOrder, AlpacaPosition},
    },
};

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
    let mut reasons = Vec::new();
    check_account(account, &mut reasons);

    let candidate_underlying = option_underlying_symbol(short_put_symbol);
    if candidate_underlying.is_empty()
        || candidate_underlying != option_underlying_symbol(long_put_symbol)
    {
        reasons.push("candidate legs must resolve to the same option underlying".to_string());
    }

    let candidate_symbols = BTreeSet::from([
        short_put_symbol.trim().to_string(),
        long_put_symbol.trim().to_string(),
    ]);

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

/// Extracts the OCC-style underlying root from an Alpaca option contract symbol.
#[must_use]
pub fn option_underlying_symbol(symbol: &str) -> String {
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
        Some("buy") => Ok(OrderSide::Buy),
        Some("sell") => Ok(OrderSide::Sell),
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

fn price_from_str(value: &str) -> Result<Price> {
    value
        .parse::<f64>()
        .map(|parsed| Price::new(parsed, decimal_precision(value)))
        .map_err(|e| Error::Parse(format!("invalid Alpaca price {value}: {e}")))
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

fn decimal_precision(value: &str) -> u8 {
    value
        .split_once('.')
        .map(|(_, decimals)| decimals.trim_end_matches('0').len().min(9) as u8)
        .unwrap_or(0)
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use nautilus_model::enums::OrderStatus;

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
            order_type: None,
            side: None,
            time_in_force: None,
            limit_price: None,
            status: None,
            order_class: None,
            legs: None,
        }
    }
}
