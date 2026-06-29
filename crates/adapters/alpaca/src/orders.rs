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

//! Alpaca order request builders and local validation.

#[cfg(feature = "live")]
use nautilus_model::{
    enums::{OrderSide, OrderType, TimeInForce},
    identifiers::Venue,
    orders::{Order, OrderAny},
    types::Quantity,
};
use serde::Serialize;

use crate::http::error::{Error, Result};
#[cfg(feature = "live")]
use crate::{common::consts::ALPACA_VENUE, parse::parse_alpaca_option_instrument_id};

/// Alpaca order side.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlpacaOrderSide {
    /// Buy side.
    Buy,
    /// Sell side.
    Sell,
}

/// Alpaca option position intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlpacaPositionIntent {
    /// Buy to open.
    BuyToOpen,
    /// Buy to close.
    BuyToClose,
    /// Sell to open.
    SellToOpen,
    /// Sell to close.
    SellToClose,
}

impl AlpacaPositionIntent {
    /// Returns the only valid side for this position intent.
    #[must_use]
    pub const fn side(self) -> AlpacaOrderSide {
        match self {
            Self::BuyToOpen | Self::BuyToClose => AlpacaOrderSide::Buy,
            Self::SellToOpen | Self::SellToClose => AlpacaOrderSide::Sell,
        }
    }
}

/// Strategy-level net premium classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetPremiumKind {
    /// Opening the strategy receives credit.
    Credit,
    /// Opening the strategy pays debit.
    Debit,
}

/// Strategy trade direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TradeIntent {
    /// Open a new strategy.
    Open,
    /// Close an existing strategy.
    Close,
}

/// One Alpaca multi-leg order leg.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MlegOrderLeg {
    /// Alpaca option contract symbol.
    pub symbol: String,
    /// Relative leg ratio in simplest form.
    pub ratio_qty: String,
    /// Leg side.
    pub side: AlpacaOrderSide,
    /// Position intent for the leg.
    pub position_intent: AlpacaPositionIntent,
}

impl MlegOrderLeg {
    /// Creates a new multi-leg order leg.
    #[must_use]
    pub fn new(
        symbol: impl Into<String>,
        side: AlpacaOrderSide,
        position_intent: AlpacaPositionIntent,
        ratio_qty: impl Into<String>,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            ratio_qty: ratio_qty.into(),
            side,
            position_intent,
        }
    }
}

/// Normalized multi-leg order semantics derived from Nautilus order-list legs.
#[cfg(feature = "live")]
#[derive(Clone, Debug, PartialEq)]
pub struct MlegOrderPlan {
    /// Number of strategy units to trade.
    pub strategy_quantity: u64,
    /// Alpaca signed net limit price for the strategy order.
    pub signed_limit_price: f64,
    /// Multi-leg components.
    pub legs: Vec<MlegOrderLeg>,
}

/// Alpaca simple single-leg order payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SimpleOrderPayload {
    /// Alpaca asset symbol.
    pub symbol: String,
    /// Number of contracts or shares.
    pub qty: String,
    /// Order side.
    pub side: AlpacaOrderSide,
    /// Alpaca order type.
    #[serde(rename = "type")]
    pub order_type: String,
    /// Time in force.
    pub time_in_force: String,
    /// Simple order class.
    pub order_class: String,
    /// Option position intent.
    pub position_intent: AlpacaPositionIntent,
    /// Optional parent client order ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    /// Limit price for limit orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<String>,
}

impl SimpleOrderPayload {
    /// Creates a simple option limit order payload and validates it locally.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload is malformed.
    pub fn new_option_limit(
        symbol: impl Into<String>,
        quantity: u64,
        position_intent: AlpacaPositionIntent,
        limit_price: f64,
    ) -> Result<Self> {
        let payload = Self {
            symbol: symbol.into(),
            qty: quantity.to_string(),
            side: position_intent.side(),
            order_type: "limit".to_string(),
            time_in_force: "day".to_string(),
            order_class: "simple".to_string(),
            position_intent,
            client_order_id: None,
            limit_price: Some(format!("{limit_price:.2}")),
        };
        payload.validate()?;
        Ok(payload)
    }

    /// Returns a copy of the payload with a client order ID set.
    ///
    /// # Errors
    ///
    /// Returns an error when the client order ID is empty or non-ASCII.
    pub fn with_client_order_id(mut self, client_order_id: impl Into<String>) -> Result<Self> {
        let client_order_id = client_order_id.into();
        if client_order_id.trim().is_empty() {
            return Err(validation("client_order_id must not be empty"));
        }
        if !client_order_id.is_ascii() {
            return Err(validation("client_order_id must be ASCII"));
        }
        self.client_order_id = Some(client_order_id);
        self.validate()?;
        Ok(self)
    }

    /// Validates this payload without submitting it.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload is malformed.
    pub fn validate(&self) -> Result<()> {
        if self.symbol.trim().is_empty() {
            return Err(validation("symbol must not be empty"));
        }
        parse_positive_u64(&self.qty, "qty")?;
        if self.side != self.position_intent.side() {
            return Err(validation(
                "side must match the side implied by position_intent",
            ));
        }
        if self.order_class != "simple" {
            return Err(validation("order_class must be simple"));
        }
        if self.order_type != "limit" {
            return Err(validation("only limit simple option orders are supported"));
        }
        if self.time_in_force != "day" {
            return Err(validation("options simple time_in_force must be day"));
        }
        let limit_price = self
            .limit_price
            .as_ref()
            .ok_or_else(|| validation("limit_price is required"))?
            .parse::<f64>()
            .map_err(|_| validation("limit_price must be numeric"))?;
        if limit_price <= 0.0 {
            return Err(validation("limit_price must be positive"));
        }
        if let Some(client_order_id) = &self.client_order_id {
            if client_order_id.trim().is_empty() {
                return Err(validation("client_order_id must not be empty"));
            }
            if !client_order_id.is_ascii() {
                return Err(validation("client_order_id must be ASCII"));
            }
        }
        Ok(())
    }
}

/// Alpaca simple equity order payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EquityOrderPayload {
    /// Alpaca equity or ETF symbol.
    pub symbol: String,
    /// Number of whole shares.
    pub qty: String,
    /// Order side.
    pub side: AlpacaOrderSide,
    /// Alpaca order type.
    #[serde(rename = "type")]
    pub order_type: String,
    /// Time in force.
    pub time_in_force: String,
    /// Optional client order ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    /// Limit price for limit orders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<String>,
}

impl EquityOrderPayload {
    /// Creates a simple equity limit order payload and validates it locally.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload is malformed.
    pub fn new_limit(
        symbol: impl Into<String>,
        quantity: u64,
        side: AlpacaOrderSide,
        limit_price: f64,
    ) -> Result<Self> {
        let payload = Self {
            symbol: symbol.into(),
            qty: quantity.to_string(),
            side,
            order_type: "limit".to_string(),
            time_in_force: "day".to_string(),
            client_order_id: None,
            limit_price: Some(format!("{limit_price:.2}")),
        };
        payload.validate()?;
        Ok(payload)
    }

    /// Returns a copy of the payload with a client order ID set.
    ///
    /// # Errors
    ///
    /// Returns an error when the client order ID is empty or non-ASCII.
    pub fn with_client_order_id(mut self, client_order_id: impl Into<String>) -> Result<Self> {
        let client_order_id = client_order_id.into();
        if client_order_id.trim().is_empty() {
            return Err(validation("client_order_id must not be empty"));
        }
        if !client_order_id.is_ascii() {
            return Err(validation("client_order_id must be ASCII"));
        }
        self.client_order_id = Some(client_order_id);
        self.validate()?;
        Ok(self)
    }

    /// Validates this payload without submitting it.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload is malformed.
    pub fn validate(&self) -> Result<()> {
        if self.symbol.trim().is_empty() {
            return Err(validation("symbol must not be empty"));
        }
        parse_positive_u64(&self.qty, "qty")?;
        if self.order_type != "limit" {
            return Err(validation("only limit equity orders are supported"));
        }
        if self.time_in_force != "day" {
            return Err(validation("equity simple time_in_force must be day"));
        }
        let limit_price = self
            .limit_price
            .as_ref()
            .ok_or_else(|| validation("limit_price is required"))?
            .parse::<f64>()
            .map_err(|_| validation("limit_price must be numeric"))?;
        if limit_price <= 0.0 {
            return Err(validation("limit_price must be positive"));
        }
        if let Some(client_order_id) = &self.client_order_id {
            if client_order_id.trim().is_empty() {
                return Err(validation("client_order_id must not be empty"));
            }
            if !client_order_id.is_ascii() {
                return Err(validation("client_order_id must be ASCII"));
            }
        }
        Ok(())
    }
}

/// Alpaca multi-leg order payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MlegOrderPayload {
    /// Alpaca advanced order class.
    pub order_class: String,
    /// Optional parent client order ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    /// Number of strategy units to trade.
    pub qty: String,
    /// Alpaca order type.
    #[serde(rename = "type")]
    pub order_type: String,
    /// Signed net limit price.
    pub limit_price: String,
    /// Time in force.
    pub time_in_force: String,
    /// Multi-leg components.
    pub legs: Vec<MlegOrderLeg>,
}

impl MlegOrderPayload {
    /// Creates a limit multi-leg order payload and validates it locally.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload does not match the Alpaca multi-leg order shape supported
    /// by this adapter slice.
    pub fn new_limit(
        quantity: u64,
        signed_limit_price: f64,
        legs: Vec<MlegOrderLeg>,
    ) -> Result<Self> {
        let payload = Self {
            order_class: "mleg".to_string(),
            client_order_id: None,
            qty: quantity.to_string(),
            order_type: "limit".to_string(),
            limit_price: format!("{signed_limit_price:.2}"),
            time_in_force: "day".to_string(),
            legs,
        };
        payload.validate()?;
        Ok(payload)
    }

    /// Returns a copy of the payload with the parent client order ID set.
    ///
    /// # Errors
    ///
    /// Returns an error when the client order ID is empty or non-ASCII.
    pub fn with_client_order_id(mut self, client_order_id: impl Into<String>) -> Result<Self> {
        let client_order_id = client_order_id.into();
        if client_order_id.trim().is_empty() {
            return Err(validation("client_order_id must not be empty"));
        }
        if !client_order_id.is_ascii() {
            return Err(validation("client_order_id must be ASCII"));
        }
        self.client_order_id = Some(client_order_id);
        self.validate()?;
        Ok(self)
    }

    /// Validates this payload without submitting it.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload does not match the Alpaca multi-leg order shape supported
    /// by this adapter slice.
    pub fn validate(&self) -> Result<()> {
        if self.order_class != "mleg" {
            return Err(validation("order_class must be mleg"));
        }
        if let Some(client_order_id) = &self.client_order_id {
            if client_order_id.trim().is_empty() {
                return Err(validation("client_order_id must not be empty"));
            }
            if !client_order_id.is_ascii() {
                return Err(validation("client_order_id must be ASCII"));
            }
        }
        if self.order_type != "limit" {
            return Err(validation(
                "only limit mleg orders are supported in this slice",
            ));
        }
        if self.time_in_force != "day" {
            return Err(validation("options mleg time_in_force must be day"));
        }
        parse_positive_u64(&self.qty, "qty")?;
        let limit_price = self
            .limit_price
            .parse::<f64>()
            .map_err(|_| validation("limit_price must be numeric"))?;
        if limit_price == 0.0 {
            return Err(validation("limit_price must be non-zero"));
        }
        if self.legs.len() < 2 {
            return Err(validation("mleg orders require at least two legs"));
        }
        if self.legs.len() > 4 {
            return Err(validation("mleg orders support at most four legs"));
        }

        let mut ratios = Vec::with_capacity(self.legs.len());
        for leg in &self.legs {
            if leg.symbol.trim().is_empty() {
                return Err(validation("leg symbol must not be empty"));
            }
            let ratio = parse_positive_u64(&leg.ratio_qty, "ratio_qty")?;
            ratios.push(ratio);
            if leg.position_intent.side() != leg.side {
                return Err(validation(
                    "leg side must match the side implied by position_intent",
                ));
            }
        }

        if ratios
            .into_iter()
            .reduce(greatest_common_divisor)
            .is_some_and(|value| value > 1)
        {
            return Err(validation("leg ratio_qty values must be in simplest form"));
        }

        Ok(())
    }
}

#[cfg(feature = "live")]
impl MlegOrderPlan {
    /// Converts this order plan into an Alpaca multi-leg payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan does not satisfy Alpaca's locally validated MLeg shape.
    pub fn into_payload(self) -> Result<MlegOrderPayload> {
        MlegOrderPayload::new_limit(self.strategy_quantity, self.signed_limit_price, self.legs)
    }
}

/// Builds normalized multi-leg order semantics from cached Nautilus order-list legs.
///
/// # Errors
///
/// Returns an error when the order list is not a valid Alpaca option MLeg order.
#[cfg(feature = "live")]
pub fn build_mleg_order_plan(orders: &[OrderAny]) -> anyhow::Result<MlegOrderPlan> {
    if orders.len() < 2 {
        anyhow::bail!("Alpaca MLeg submit requires at least two leg orders");
    }
    if orders.len() > 4 {
        anyhow::bail!("Alpaca MLeg submit supports at most four leg orders");
    }

    let quantities = orders
        .iter()
        .map(|order| {
            positive_integer_quantity(
                order.quantity(),
                format!("Alpaca MLeg leg {}", order.client_order_id()),
            )
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let strategy_quantity = quantities
        .iter()
        .copied()
        .reduce(greatest_common_divisor)
        .ok_or_else(|| anyhow::anyhow!("Alpaca MLeg submit requires leg quantities"))?;
    if strategy_quantity == 0 {
        anyhow::bail!("Alpaca MLeg strategy quantity must be positive");
    }

    let trade_intent = mleg_trade_intent(orders)?;
    let mut net_credit = 0.0_f64;
    let mut legs = Vec::with_capacity(orders.len());

    for (order, leg_qty) in orders.iter().zip(quantities) {
        validate_mleg_leg_order(order)?;
        let ratio_qty = leg_qty / strategy_quantity;
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

        let position_intent =
            position_intent_from_order_side(order.order_side(), order.is_reduce_only())?;
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

    Ok(MlegOrderPlan {
        strategy_quantity,
        signed_limit_price,
        legs,
    })
}

/// Returns the Alpaca position intent implied by a Nautilus order side and reduce-only flag.
///
/// # Errors
///
/// Returns an error when the order side is missing.
#[cfg(feature = "live")]
pub fn position_intent_from_order_side(
    side: OrderSide,
    reduce_only: bool,
) -> anyhow::Result<AlpacaPositionIntent> {
    match (side, reduce_only) {
        (OrderSide::Buy, false) => Ok(AlpacaPositionIntent::BuyToOpen),
        (OrderSide::Sell, false) => Ok(AlpacaPositionIntent::SellToOpen),
        (OrderSide::Buy, true) => Ok(AlpacaPositionIntent::BuyToClose),
        (OrderSide::Sell, true) => Ok(AlpacaPositionIntent::SellToClose),
        (OrderSide::NoOrderSide, _) => anyhow::bail!("Alpaca order missing order side"),
    }
}

/// Returns the Alpaca order side for a Nautilus order side.
///
/// # Errors
///
/// Returns an error when the order side is missing.
#[cfg(feature = "live")]
pub fn order_side_from_nautilus(side: OrderSide) -> anyhow::Result<AlpacaOrderSide> {
    match side {
        OrderSide::Buy => Ok(AlpacaOrderSide::Buy),
        OrderSide::Sell => Ok(AlpacaOrderSide::Sell),
        OrderSide::NoOrderSide => anyhow::bail!("Alpaca order missing order side"),
    }
}

/// Parses a positive integer order quantity.
///
/// # Errors
///
/// Returns an error when the quantity is fractional, zero, or cannot be represented as a `u64`.
#[cfg(feature = "live")]
pub fn positive_integer_quantity(
    quantity: Quantity,
    order_context: impl std::fmt::Display,
) -> anyhow::Result<u64> {
    let normalized = quantity.as_decimal().normalize();
    if normalized.scale() != 0 {
        anyhow::bail!("{order_context} quantity must be an integer contract count, was {quantity}");
    }
    let parsed = normalized
        .to_string()
        .parse::<u64>()
        .map_err(|e| anyhow::anyhow!("invalid Alpaca quantity {quantity}: {e}"))?;
    if parsed == 0 {
        anyhow::bail!("{order_context} quantity must be positive");
    }
    Ok(parsed)
}

/// Builds a paper-safe put credit spread opening payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or credit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_put_credit_spread_open_order(
    short_put_symbol: impl Into<String>,
    long_put_symbol: impl Into<String>,
    credit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    if quantity == 0 {
        return Err(validation("quantity must be positive"));
    }
    if credit_limit <= 0.0 {
        return Err(validation("credit limit must be positive"));
    }

    MlegOrderPayload::new_limit(
        quantity,
        signed_net_limit_price(credit_limit, NetPremiumKind::Credit, TradeIntent::Open),
        vec![
            MlegOrderLeg::new(
                short_put_symbol,
                AlpacaOrderSide::Sell,
                AlpacaPositionIntent::SellToOpen,
                "1",
            ),
            MlegOrderLeg::new(
                long_put_symbol,
                AlpacaOrderSide::Buy,
                AlpacaPositionIntent::BuyToOpen,
                "1",
            ),
        ],
    )
}

/// Builds a paper-safe put credit spread closing payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or debit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_put_credit_spread_close_order(
    short_put_symbol: impl Into<String>,
    long_put_symbol: impl Into<String>,
    debit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_credit_spread_order(
        short_put_symbol,
        long_put_symbol,
        debit_limit,
        quantity,
        TradeIntent::Close,
    )
}

/// Builds a paper-safe call credit spread opening payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or credit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_call_credit_spread_open_order(
    short_call_symbol: impl Into<String>,
    long_call_symbol: impl Into<String>,
    credit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_credit_spread_order(
        short_call_symbol,
        long_call_symbol,
        credit_limit,
        quantity,
        TradeIntent::Open,
    )
}

/// Builds a paper-safe call credit spread closing payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or debit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_call_credit_spread_close_order(
    short_call_symbol: impl Into<String>,
    long_call_symbol: impl Into<String>,
    debit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_credit_spread_order(
        short_call_symbol,
        long_call_symbol,
        debit_limit,
        quantity,
        TradeIntent::Close,
    )
}

/// Builds a paper-safe call debit spread opening payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or debit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_call_debit_spread_open_order(
    long_call_symbol: impl Into<String>,
    short_call_symbol: impl Into<String>,
    debit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_debit_spread_order(
        long_call_symbol,
        short_call_symbol,
        debit_limit,
        quantity,
        TradeIntent::Open,
    )
}

/// Builds a paper-safe call debit spread closing payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or credit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_call_debit_spread_close_order(
    long_call_symbol: impl Into<String>,
    short_call_symbol: impl Into<String>,
    credit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_debit_spread_order(
        long_call_symbol,
        short_call_symbol,
        credit_limit,
        quantity,
        TradeIntent::Close,
    )
}

/// Builds a paper-safe put debit spread opening payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or debit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_put_debit_spread_open_order(
    long_put_symbol: impl Into<String>,
    short_put_symbol: impl Into<String>,
    debit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_debit_spread_order(
        long_put_symbol,
        short_put_symbol,
        debit_limit,
        quantity,
        TradeIntent::Open,
    )
}

/// Builds a paper-safe put debit spread closing payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or credit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_put_debit_spread_close_order(
    long_put_symbol: impl Into<String>,
    short_put_symbol: impl Into<String>,
    credit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_debit_spread_order(
        long_put_symbol,
        short_put_symbol,
        credit_limit,
        quantity,
        TradeIntent::Close,
    )
}

/// Builds a paper-safe iron-condor opening payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or credit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_iron_condor_open_order(
    short_put_symbol: impl Into<String>,
    long_put_symbol: impl Into<String>,
    short_call_symbol: impl Into<String>,
    long_call_symbol: impl Into<String>,
    credit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_iron_condor_order(
        short_put_symbol,
        long_put_symbol,
        short_call_symbol,
        long_call_symbol,
        credit_limit,
        quantity,
        TradeIntent::Open,
    )
}

/// Builds a paper-safe iron-condor closing payload without submitting it.
///
/// # Errors
///
/// Returns an error when quantity or debit limit are invalid, or the generated payload fails local
/// validation.
pub fn build_iron_condor_close_order(
    short_put_symbol: impl Into<String>,
    long_put_symbol: impl Into<String>,
    short_call_symbol: impl Into<String>,
    long_call_symbol: impl Into<String>,
    debit_limit: f64,
    quantity: u64,
) -> Result<MlegOrderPayload> {
    build_iron_condor_order(
        short_put_symbol,
        long_put_symbol,
        short_call_symbol,
        long_call_symbol,
        debit_limit,
        quantity,
        TradeIntent::Close,
    )
}

fn build_credit_spread_order(
    short_symbol: impl Into<String>,
    long_symbol: impl Into<String>,
    limit: f64,
    quantity: u64,
    trade_intent: TradeIntent,
) -> Result<MlegOrderPayload> {
    if quantity == 0 {
        return Err(validation("quantity must be positive"));
    }
    if limit <= 0.0 {
        return Err(validation("limit must be positive"));
    }

    let (short_intent, long_intent) = match trade_intent {
        TradeIntent::Open => (
            AlpacaPositionIntent::SellToOpen,
            AlpacaPositionIntent::BuyToOpen,
        ),
        TradeIntent::Close => (
            AlpacaPositionIntent::BuyToClose,
            AlpacaPositionIntent::SellToClose,
        ),
    };

    MlegOrderPayload::new_limit(
        quantity,
        signed_net_limit_price(limit, NetPremiumKind::Credit, trade_intent),
        vec![
            MlegOrderLeg::new(short_symbol, short_intent.side(), short_intent, "1"),
            MlegOrderLeg::new(long_symbol, long_intent.side(), long_intent, "1"),
        ],
    )
}

fn build_iron_condor_order(
    short_put_symbol: impl Into<String>,
    long_put_symbol: impl Into<String>,
    short_call_symbol: impl Into<String>,
    long_call_symbol: impl Into<String>,
    limit: f64,
    quantity: u64,
    trade_intent: TradeIntent,
) -> Result<MlegOrderPayload> {
    if quantity == 0 {
        return Err(validation("quantity must be positive"));
    }
    if limit <= 0.0 {
        return Err(validation("limit must be positive"));
    }

    let (short_intent, long_intent) = match trade_intent {
        TradeIntent::Open => (
            AlpacaPositionIntent::SellToOpen,
            AlpacaPositionIntent::BuyToOpen,
        ),
        TradeIntent::Close => (
            AlpacaPositionIntent::BuyToClose,
            AlpacaPositionIntent::SellToClose,
        ),
    };

    MlegOrderPayload::new_limit(
        quantity,
        signed_net_limit_price(limit, NetPremiumKind::Credit, trade_intent),
        vec![
            MlegOrderLeg::new(short_put_symbol, short_intent.side(), short_intent, "1"),
            MlegOrderLeg::new(long_put_symbol, long_intent.side(), long_intent, "1"),
            MlegOrderLeg::new(short_call_symbol, short_intent.side(), short_intent, "1"),
            MlegOrderLeg::new(long_call_symbol, long_intent.side(), long_intent, "1"),
        ],
    )
}

fn build_debit_spread_order(
    long_symbol: impl Into<String>,
    short_symbol: impl Into<String>,
    limit: f64,
    quantity: u64,
    trade_intent: TradeIntent,
) -> Result<MlegOrderPayload> {
    if quantity == 0 {
        return Err(validation("quantity must be positive"));
    }
    if limit <= 0.0 {
        return Err(validation("limit must be positive"));
    }

    let (long_intent, short_intent) = match trade_intent {
        TradeIntent::Open => (
            AlpacaPositionIntent::BuyToOpen,
            AlpacaPositionIntent::SellToOpen,
        ),
        TradeIntent::Close => (
            AlpacaPositionIntent::SellToClose,
            AlpacaPositionIntent::BuyToClose,
        ),
    };

    MlegOrderPayload::new_limit(
        quantity,
        signed_net_limit_price(limit, NetPremiumKind::Debit, trade_intent),
        vec![
            MlegOrderLeg::new(long_symbol, long_intent.side(), long_intent, "1"),
            MlegOrderLeg::new(short_symbol, short_intent.side(), short_intent, "1"),
        ],
    )
}

/// Returns Alpaca's signed net limit price for a strategy order.
#[must_use]
pub fn signed_net_limit_price(
    limit_price: f64,
    premium_kind: NetPremiumKind,
    trade_intent: TradeIntent,
) -> f64 {
    let normalized_limit = limit_price.abs();
    match (premium_kind, trade_intent) {
        (NetPremiumKind::Credit, TradeIntent::Open)
        | (NetPremiumKind::Debit, TradeIntent::Close) => -normalized_limit,
        (NetPremiumKind::Debit, TradeIntent::Open)
        | (NetPremiumKind::Credit, TradeIntent::Close) => normalized_limit,
    }
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
    parse_alpaca_option_instrument_id(order.instrument_id()).map_err(|e| {
        anyhow::anyhow!(
            "Alpaca MLeg leg {} has invalid Alpaca option instrument {}: {e}",
            order.client_order_id(),
            order.instrument_id()
        )
    })?;
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

fn parse_positive_u64(value: &str, field: &str) -> Result<u64> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| validation(format!("{field} must be a positive integer")))?;
    if parsed == 0 {
        return Err(validation(format!("{field} must be positive")));
    }
    Ok(parsed)
}

fn greatest_common_divisor(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn validation(message: impl Into<String>) -> Error {
    Error::Validation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_option_open_payload_uses_sell_to_open() {
        let payload = SimpleOrderPayload::new_option_limit(
            "SPY260512P00708000",
            1,
            AlpacaPositionIntent::SellToOpen,
            0.55,
        )
        .unwrap()
        .with_client_order_id("naked-put-1")
        .unwrap();

        assert_eq!(payload.symbol, "SPY260512P00708000");
        assert_eq!(payload.side, AlpacaOrderSide::Sell);
        assert_eq!(payload.position_intent, AlpacaPositionIntent::SellToOpen);
        assert_eq!(payload.limit_price.as_deref(), Some("0.55"));
        assert_eq!(payload.order_class, "simple");
    }

    #[test]
    fn simple_option_close_payload_uses_buy_to_close() {
        let payload = SimpleOrderPayload::new_option_limit(
            "SPY260512P00708000",
            1,
            AlpacaPositionIntent::BuyToClose,
            0.20,
        )
        .unwrap();

        assert_eq!(payload.side, AlpacaOrderSide::Buy);
        assert_eq!(payload.position_intent, AlpacaPositionIntent::BuyToClose);
        assert_eq!(payload.limit_price.as_deref(), Some("0.20"));
    }

    #[test]
    fn equity_limit_payload_omits_option_position_intent() {
        let payload = EquityOrderPayload::new_limit("SPY", 12, AlpacaOrderSide::Buy, 510.25)
            .unwrap()
            .with_client_order_id("equity-buy-1")
            .unwrap();

        let value = serde_json::to_value(&payload).unwrap();

        assert_eq!(value["symbol"], "SPY");
        assert_eq!(value["qty"], "12");
        assert_eq!(value["side"], "buy");
        assert_eq!(value["type"], "limit");
        assert_eq!(value["time_in_force"], "day");
        assert_eq!(value["limit_price"], "510.25");
        assert!(value.get("position_intent").is_none());
        assert!(value.get("order_class").is_none());
    }

    #[test]
    fn credit_spread_close_payload_uses_buy_to_close_and_positive_debit() {
        let payload = build_put_credit_spread_close_order(
            "SPY260512P00708000",
            "SPY260512P00705000",
            0.25,
            1,
        )
        .unwrap();

        assert_eq!(payload.limit_price, "0.25");
        assert_eq!(payload.legs[0].side, AlpacaOrderSide::Buy);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::BuyToClose
        );
        assert_eq!(payload.legs[1].side, AlpacaOrderSide::Sell);
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::SellToClose
        );
    }

    #[test]
    fn call_credit_open_payload_uses_credit_signing() {
        let payload = build_call_credit_spread_open_order(
            "SPY260512C00710000",
            "SPY260512C00713000",
            0.45,
            1,
        )
        .unwrap();

        assert_eq!(payload.limit_price, "-0.45");
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::SellToOpen
        );
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::BuyToOpen
        );
    }

    #[test]
    fn call_debit_open_payload_uses_debit_signing() {
        let payload =
            build_call_debit_spread_open_order("SPY260512C00710000", "SPY260512C00713000", 1.25, 1)
                .unwrap();

        assert_eq!(payload.limit_price, "1.25");
        assert_eq!(payload.legs[0].side, AlpacaOrderSide::Buy);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::BuyToOpen
        );
        assert_eq!(payload.legs[1].side, AlpacaOrderSide::Sell);
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::SellToOpen
        );
    }

    #[test]
    fn put_debit_close_payload_uses_credit_signing() {
        let payload =
            build_put_debit_spread_close_order("SPY260512P00710000", "SPY260512P00705000", 1.55, 1)
                .unwrap();

        assert_eq!(payload.limit_price, "-1.55");
        assert_eq!(payload.legs[0].side, AlpacaOrderSide::Sell);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::SellToClose
        );
        assert_eq!(payload.legs[1].side, AlpacaOrderSide::Buy);
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::BuyToClose
        );
    }

    #[test]
    fn iron_condor_open_payload_uses_four_opening_legs_and_credit_signing() {
        let payload = build_iron_condor_open_order(
            "SPY260512P00708000",
            "SPY260512P00705000",
            "SPY260512C00712000",
            "SPY260512C00715000",
            0.95,
            1,
        )
        .unwrap();

        assert_eq!(payload.limit_price, "-0.95");
        assert_eq!(payload.legs.len(), 4);
        assert_eq!(payload.legs[0].side, AlpacaOrderSide::Sell);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::SellToOpen
        );
        assert_eq!(payload.legs[1].side, AlpacaOrderSide::Buy);
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::BuyToOpen
        );
        assert_eq!(payload.legs[2].side, AlpacaOrderSide::Sell);
        assert_eq!(
            payload.legs[2].position_intent,
            AlpacaPositionIntent::SellToOpen
        );
        assert_eq!(payload.legs[3].side, AlpacaOrderSide::Buy);
        assert_eq!(
            payload.legs[3].position_intent,
            AlpacaPositionIntent::BuyToOpen
        );
    }

    #[test]
    fn iron_condor_close_payload_uses_four_closing_legs_and_debit_signing() {
        let payload = build_iron_condor_close_order(
            "SPY260512P00708000",
            "SPY260512P00705000",
            "SPY260512C00712000",
            "SPY260512C00715000",
            0.35,
            1,
        )
        .unwrap();

        assert_eq!(payload.limit_price, "0.35");
        assert_eq!(payload.legs.len(), 4);
        assert_eq!(payload.legs[0].side, AlpacaOrderSide::Buy);
        assert_eq!(
            payload.legs[0].position_intent,
            AlpacaPositionIntent::BuyToClose
        );
        assert_eq!(payload.legs[1].side, AlpacaOrderSide::Sell);
        assert_eq!(
            payload.legs[1].position_intent,
            AlpacaPositionIntent::SellToClose
        );
        assert_eq!(payload.legs[2].side, AlpacaOrderSide::Buy);
        assert_eq!(
            payload.legs[2].position_intent,
            AlpacaPositionIntent::BuyToClose
        );
        assert_eq!(payload.legs[3].side, AlpacaOrderSide::Sell);
        assert_eq!(
            payload.legs[3].position_intent,
            AlpacaPositionIntent::SellToClose
        );
    }
}
