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

//! Alpaca REST response and request models.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Alpaca option contract type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlpacaOptionType {
    /// Call option contract.
    Call,
    /// Put option contract.
    Put,
}

impl AlpacaOptionType {
    /// Returns the Alpaca API query value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Put => "put",
        }
    }
}

/// Request parameters for listing Alpaca option contracts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListOptionContractsRequest {
    /// One or more underlying symbols.
    pub underlying_symbols: Vec<String>,
    /// Optional contract type filter.
    pub option_type: Option<AlpacaOptionType>,
    /// Contract status filter.
    pub status: String,
    /// Exact expiration date filter.
    pub expiration_date: Option<String>,
    /// Minimum expiration date filter.
    pub expiration_date_gte: Option<String>,
    /// Maximum expiration date filter.
    pub expiration_date_lte: Option<String>,
    /// Page size.
    pub limit: usize,
    /// Optional page token.
    pub page_token: Option<String>,
    /// If deliverables should be included in the response.
    pub show_deliverables: bool,
}

impl ListOptionContractsRequest {
    /// Creates a request for active contracts on the given underlyings.
    #[must_use]
    pub fn active(underlying_symbols: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            underlying_symbols: underlying_symbols.into_iter().map(Into::into).collect(),
            option_type: None,
            status: "active".to_string(),
            expiration_date: None,
            expiration_date_gte: None,
            expiration_date_lte: None,
            limit: 1_000,
            page_token: None,
            show_deliverables: false,
        }
    }

    /// Returns a copy of this request with the page token replaced.
    #[must_use]
    pub fn with_page_token(&self, page_token: Option<String>) -> Self {
        Self {
            page_token,
            ..self.clone()
        }
    }

    pub(crate) fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if !self.underlying_symbols.is_empty() {
            pairs.push(("underlying_symbols", self.underlying_symbols.join(",")));
        }
        if let Some(option_type) = self.option_type {
            pairs.push(("type", option_type.as_str().to_string()));
        }
        if !self.status.is_empty() {
            pairs.push(("status", self.status.clone()));
        }
        if let Some(value) = &self.expiration_date {
            pairs.push(("expiration_date", value.clone()));
        }
        if let Some(value) = &self.expiration_date_gte {
            pairs.push(("expiration_date_gte", value.clone()));
        }
        if let Some(value) = &self.expiration_date_lte {
            pairs.push(("expiration_date_lte", value.clone()));
        }
        pairs.push(("limit", self.limit.to_string()));
        if let Some(value) = &self.page_token {
            pairs.push(("page_token", value.clone()));
        }
        if self.show_deliverables {
            pairs.push(("show_deliverables", "true".to_string()));
        }
        pairs
    }
}

/// Request parameters for option snapshots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptionSnapshotsRequest {
    /// Option contract symbols.
    pub symbols: Vec<String>,
    /// Option data feed.
    pub feed: Option<String>,
    /// Optional timestamp filter.
    pub updated_since: Option<String>,
    /// Page size.
    pub limit: usize,
    /// Optional page token.
    pub page_token: Option<String>,
}

impl OptionSnapshotsRequest {
    /// Creates a snapshot request for the given symbols.
    #[must_use]
    pub fn for_symbols(symbols: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            symbols: symbols.into_iter().map(Into::into).collect(),
            feed: None,
            updated_since: None,
            limit: 1_000,
            page_token: None,
        }
    }

    /// Returns a copy of this request with the symbols and page token replaced.
    #[must_use]
    pub fn with_symbols_and_page(
        &self,
        symbols: impl IntoIterator<Item = impl Into<String>>,
        page_token: Option<String>,
    ) -> Self {
        Self {
            symbols: symbols.into_iter().map(Into::into).collect(),
            page_token,
            ..self.clone()
        }
    }

    pub(crate) fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if !self.symbols.is_empty() {
            pairs.push(("symbols", self.symbols.join(",")));
        }
        if let Some(value) = &self.feed {
            pairs.push(("feed", value.clone()));
        }
        if let Some(value) = &self.updated_since {
            pairs.push(("updated_since", value.clone()));
        }
        pairs.push(("limit", self.limit.to_string()));
        if let Some(value) = &self.page_token {
            pairs.push(("page_token", value.clone()));
        }
        pairs
    }
}

/// Response from Alpaca's option snapshots endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OptionSnapshotsResponse {
    /// Snapshots keyed by Alpaca option symbol.
    #[serde(default)]
    pub snapshots: BTreeMap<String, AlpacaOptionSnapshot>,
    /// Next page token, if more records are available.
    pub next_page_token: Option<String>,
    /// Older response variants can include `page_token`.
    pub page_token: Option<String>,
}

impl OptionSnapshotsResponse {
    /// Returns the next token from any supported Alpaca response field.
    #[must_use]
    pub fn next_token(&self) -> Option<String> {
        self.next_page_token
            .clone()
            .or_else(|| self.page_token.clone())
            .filter(|value| !value.trim().is_empty())
    }
}

/// Response from Alpaca's option contracts endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OptionContractsResponse {
    /// Option contracts returned for the requested page.
    #[serde(default)]
    pub option_contracts: Vec<AlpacaOptionContract>,
    /// Next page token, if more records are available.
    pub next_page_token: Option<String>,
    /// Older response variants can include `page_token`.
    pub page_token: Option<String>,
}

impl OptionContractsResponse {
    /// Returns the next token from any supported Alpaca response field.
    #[must_use]
    pub fn next_token(&self) -> Option<String> {
        self.next_page_token
            .clone()
            .or_else(|| self.page_token.clone())
            .filter(|value| !value.trim().is_empty())
    }
}

/// Alpaca option contract model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaOptionContract {
    /// Alpaca contract ID.
    pub id: String,
    /// Alpaca option symbol.
    pub symbol: String,
    /// Human-readable contract name.
    pub name: Option<String>,
    /// Contract status.
    pub status: Option<String>,
    /// If the contract is tradable.
    pub tradable: Option<bool>,
    /// Expiration date in YYYY-MM-DD format.
    pub expiration_date: String,
    /// Option root symbol.
    pub root_symbol: Option<String>,
    /// Underlying symbol.
    pub underlying_symbol: String,
    /// Underlying asset ID.
    pub underlying_asset_id: Option<String>,
    /// Option contract type.
    #[serde(rename = "type")]
    pub option_type: String,
    /// Exercise style.
    pub style: Option<String>,
    /// Strike price, as returned by Alpaca.
    pub strike_price: String,
    /// Contract multiplier/size.
    pub size: Option<String>,
    /// Open interest, when available.
    pub open_interest: Option<String>,
    /// Open interest date, when available.
    pub open_interest_date: Option<String>,
    /// Previous close price, when available.
    pub close_price: Option<String>,
    /// Previous close price date, when available.
    pub close_price_date: Option<String>,
    /// Penny program indicator.
    pub ppind: Option<bool>,
}

/// Alpaca option snapshot model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaOptionSnapshot {
    /// Latest quote.
    #[serde(default, alias = "latestQuote", alias = "latest_quote")]
    pub latest_quote: Option<AlpacaOptionQuote>,
    /// Latest trade.
    #[serde(default, alias = "latestTrade", alias = "latest_trade")]
    pub latest_trade: Option<AlpacaOptionTrade>,
    /// Latest minute bar.
    #[serde(default, alias = "minuteBar", alias = "minute_bar")]
    pub minute_bar: Option<AlpacaOptionBar>,
    /// Latest daily bar.
    #[serde(default, alias = "dailyBar", alias = "daily_bar")]
    pub daily_bar: Option<AlpacaOptionBar>,
    /// Previous daily bar.
    #[serde(default, alias = "prevDailyBar", alias = "prev_daily_bar")]
    pub prev_daily_bar: Option<AlpacaOptionBar>,
    /// Option Greeks.
    #[serde(default)]
    pub greeks: Option<AlpacaOptionGreeks>,
    /// Implied volatility.
    #[serde(
        default,
        alias = "impliedVolatility",
        alias = "implied_volatility",
        alias = "iv",
        deserialize_with = "deserialize_optional_f64"
    )]
    pub implied_volatility: Option<f64>,
}

impl AlpacaOptionSnapshot {
    /// Returns `true` when the snapshot contains a positive, crossed-safe quote.
    #[must_use]
    pub fn has_valid_quote(&self) -> bool {
        self.latest_quote
            .as_ref()
            .is_some_and(AlpacaOptionQuote::is_valid)
    }

    /// Returns `true` when any Greek or implied volatility value is present.
    #[must_use]
    pub fn has_greek_inputs(&self) -> bool {
        self.implied_volatility.is_some()
            || self
                .greeks
                .as_ref()
                .is_some_and(AlpacaOptionGreeks::has_any)
    }
}

/// Alpaca option quote model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaOptionQuote {
    /// Ask price.
    #[serde(default, alias = "ap", deserialize_with = "deserialize_optional_f64")]
    pub ask_price: Option<f64>,
    /// Ask size.
    #[serde(default, alias = "as", deserialize_with = "deserialize_optional_u64")]
    pub ask_size: Option<u64>,
    /// Bid price.
    #[serde(default, alias = "bp", deserialize_with = "deserialize_optional_f64")]
    pub bid_price: Option<f64>,
    /// Bid size.
    #[serde(default, alias = "bs", deserialize_with = "deserialize_optional_u64")]
    pub bid_size: Option<u64>,
    /// Timestamp.
    #[serde(default, alias = "t")]
    pub timestamp: Option<String>,
}

impl AlpacaOptionQuote {
    /// Returns `true` when the quote has positive bid/ask prices and ask is not below bid.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match (self.bid_price, self.ask_price) {
            (Some(bid), Some(ask)) => bid > 0.0 && ask > 0.0 && ask >= bid,
            _ => false,
        }
    }

    /// Returns the quote midpoint, if the quote is valid.
    #[must_use]
    pub fn midpoint(&self) -> Option<f64> {
        if self.is_valid() {
            Some((self.bid_price? + self.ask_price?) / 2.0)
        } else {
            None
        }
    }
}

/// Alpaca option trade model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaOptionTrade {
    /// Price.
    #[serde(default, alias = "p", deserialize_with = "deserialize_optional_f64")]
    pub price: Option<f64>,
    /// Size.
    #[serde(default, alias = "s", deserialize_with = "deserialize_optional_u64")]
    pub size: Option<u64>,
    /// Timestamp.
    #[serde(default, alias = "t")]
    pub timestamp: Option<String>,
}

/// Alpaca option bar model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaOptionBar {
    /// Open price.
    #[serde(default, alias = "o", deserialize_with = "deserialize_optional_f64")]
    pub open: Option<f64>,
    /// High price.
    #[serde(default, alias = "h", deserialize_with = "deserialize_optional_f64")]
    pub high: Option<f64>,
    /// Low price.
    #[serde(default, alias = "l", deserialize_with = "deserialize_optional_f64")]
    pub low: Option<f64>,
    /// Close price.
    #[serde(default, alias = "c", deserialize_with = "deserialize_optional_f64")]
    pub close: Option<f64>,
    /// Volume.
    #[serde(default, alias = "v", deserialize_with = "deserialize_optional_u64")]
    pub volume: Option<u64>,
    /// Timestamp.
    #[serde(default, alias = "t")]
    pub timestamp: Option<String>,
}

/// Alpaca option Greeks model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaOptionGreeks {
    /// Delta.
    #[serde(default, alias = "d", deserialize_with = "deserialize_optional_f64")]
    pub delta: Option<f64>,
    /// Gamma.
    #[serde(default, alias = "g", deserialize_with = "deserialize_optional_f64")]
    pub gamma: Option<f64>,
    /// Rho.
    #[serde(default, alias = "r", deserialize_with = "deserialize_optional_f64")]
    pub rho: Option<f64>,
    /// Theta.
    #[serde(default, alias = "t", deserialize_with = "deserialize_optional_f64")]
    pub theta: Option<f64>,
    /// Vega.
    #[serde(default, alias = "v", deserialize_with = "deserialize_optional_f64")]
    pub vega: Option<f64>,
}

impl AlpacaOptionGreeks {
    /// Returns `true` when at least one Greek is present.
    #[must_use]
    pub fn has_any(&self) -> bool {
        self.delta.is_some()
            || self.gamma.is_some()
            || self.rho.is_some()
            || self.theta.is_some()
            || self.vega.is_some()
    }
}

fn deserialize_optional_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        Value::Number(number) => number.as_f64(),
        Value::String(value) => value.trim().parse::<f64>().ok(),
        _ => None,
    }))
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        Value::Number(number) => number.as_u64(),
        Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }))
}
