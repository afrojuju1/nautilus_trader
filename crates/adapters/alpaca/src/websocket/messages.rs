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

//! Alpaca trade-updates WebSocket message models.

use serde::{Deserialize, Serialize};

use crate::http::models::AlpacaOrder;

/// Alpaca account WebSocket stream name for order lifecycle updates.
pub const TRADE_UPDATES_STREAM: &str = "trade_updates";

/// Authentication request sent to Alpaca's account WebSocket.
#[derive(Serialize)]
pub struct AlpacaAuthRequest<'a> {
    action: &'static str,
    key: &'a str,
    secret: &'a str,
}

impl<'a> AlpacaAuthRequest<'a> {
    /// Creates a new authentication request.
    #[must_use]
    pub const fn new(key: &'a str, secret: &'a str) -> Self {
        Self {
            action: "auth",
            key,
            secret,
        }
    }
}

impl std::fmt::Debug for AlpacaAuthRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlpacaAuthRequest")
            .field("action", &self.action)
            .field("key", &"***")
            .field("secret", &"***")
            .finish()
    }
}

/// Listen request sent after authentication.
#[derive(Debug, Serialize)]
pub struct AlpacaListenRequest<'a> {
    action: &'static str,
    data: AlpacaListenData<'a>,
}

impl<'a> AlpacaListenRequest<'a> {
    /// Creates a new listen request for the given streams.
    #[must_use]
    pub const fn new(streams: &'a [&'a str]) -> Self {
        Self {
            action: "listen",
            data: AlpacaListenData { streams },
        }
    }
}

#[derive(Debug, Serialize)]
struct AlpacaListenData<'a> {
    streams: &'a [&'a str],
}

/// Authentication status payload from Alpaca's account WebSocket.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaAuthorization {
    /// Alpaca action, normally `authenticate`.
    pub action: Option<String>,
    /// Authentication status, such as `authorized` or `unauthorized`.
    pub status: Option<String>,
}

/// Listening status payload from Alpaca's account WebSocket.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaListening {
    /// Current subscribed streams.
    #[serde(default)]
    pub streams: Vec<String>,
}

/// Single Alpaca trade update event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaTradeUpdate {
    /// Event type, such as `new`, `partial_fill`, `fill`, `canceled`, or `rejected`.
    pub event: String,
    /// Alpaca order payload, usually parent order for multi-leg orders.
    pub order: AlpacaOrder,
    /// Execution ID for single-leg fill events.
    pub execution_id: Option<String>,
    /// Fill price for single-leg fill events.
    pub price: Option<String>,
    /// Fill quantity for single-leg fill events.
    pub qty: Option<String>,
    /// Position quantity after this update.
    pub position_qty: Option<String>,
    /// Event timestamp.
    pub timestamp: Option<String>,
    /// Per-leg execution details for multi-leg fills.
    #[serde(default)]
    pub legs: Option<Vec<AlpacaTradeUpdateLeg>>,
}

impl AlpacaTradeUpdate {
    /// Returns true when the update represents one or more executions.
    #[must_use]
    pub fn is_fill_event(&self) -> bool {
        matches!(self.event.as_str(), "fill" | "partial_fill")
    }
}

/// Per-leg trade update payload for multi-leg fills.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaTradeUpdateLeg {
    /// Execution ID for this leg fill.
    pub execution_id: Option<String>,
    /// Fill price for this leg.
    pub price: Option<String>,
    /// Fill quantity for this leg.
    pub qty: Option<String>,
    /// Position quantity after this leg fill.
    pub position_qty: Option<String>,
    /// Alpaca order ID for this leg.
    pub order_id: Option<String>,
    /// Alpaca option or equity symbol for this leg.
    pub symbol: Option<String>,
    /// Event timestamp for this leg.
    pub timestamp: Option<String>,
    /// Optional side when Alpaca includes it directly on the leg update.
    pub side: Option<String>,
}

/// Parsed inbound Alpaca WebSocket message.
#[derive(Clone, Debug)]
pub enum AlpacaWsMessage {
    /// Authentication status.
    Authorization(AlpacaAuthorization),
    /// Listening status.
    Listening(AlpacaListening),
    /// Trade update event.
    TradeUpdate(Box<AlpacaTradeUpdate>),
    /// The network client reconnected.
    Reconnected,
    /// Alpaca or adapter error.
    Error(String),
}
