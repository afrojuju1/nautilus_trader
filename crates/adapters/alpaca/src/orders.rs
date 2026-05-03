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

use serde::Serialize;

use crate::http::error::{Error, Result};

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
