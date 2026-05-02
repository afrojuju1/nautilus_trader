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
            qty: quantity.to_string(),
            order_type: "limit".to_string(),
            limit_price: format!("{signed_limit_price:.2}"),
            time_in_force: "day".to_string(),
            legs,
        };
        payload.validate()?;
        Ok(payload)
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
