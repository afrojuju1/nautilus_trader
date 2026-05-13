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

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
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

/// Request parameters for historical option bars.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptionBarsRequest {
    /// Option contract symbols.
    pub symbols: Vec<String>,
    /// Bar timeframe, for example `1Min`.
    pub timeframe: String,
    /// Inclusive start timestamp in RFC3339 format.
    pub start: String,
    /// Exclusive end timestamp in RFC3339 format.
    pub end: Option<String>,
    /// Option data feed.
    pub feed: Option<String>,
    /// Page size.
    pub limit: usize,
    /// Sort order.
    pub sort: Option<String>,
    /// Optional page token.
    pub page_token: Option<String>,
}

impl OptionBarsRequest {
    /// Creates a historical bars request for the given symbols.
    #[must_use]
    pub fn for_symbols(
        symbols: impl IntoIterator<Item = impl Into<String>>,
        timeframe: impl Into<String>,
        start: impl Into<String>,
    ) -> Self {
        Self {
            symbols: symbols.into_iter().map(Into::into).collect(),
            timeframe: timeframe.into(),
            start: start.into(),
            end: None,
            feed: None,
            limit: 10_000,
            sort: Some("asc".to_string()),
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
        pairs.push(("timeframe", self.timeframe.clone()));
        pairs.push(("start", self.start.clone()));
        if let Some(value) = &self.end {
            pairs.push(("end", value.clone()));
        }
        if let Some(value) = &self.feed {
            pairs.push(("feed", value.clone()));
        }
        pairs.push(("limit", self.limit.to_string()));
        if let Some(value) = &self.sort {
            pairs.push(("sort", value.clone()));
        }
        if let Some(value) = &self.page_token {
            pairs.push(("page_token", value.clone()));
        }
        pairs
    }
}

/// Request parameters for stock snapshots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StockSnapshotsRequest {
    /// Stock symbols.
    pub symbols: Vec<String>,
    /// Stock data feed.
    pub feed: Option<String>,
}

impl StockSnapshotsRequest {
    /// Creates a snapshot request for the given symbols.
    #[must_use]
    pub fn for_symbols(symbols: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            symbols: symbols.into_iter().map(Into::into).collect(),
            feed: None,
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
        pairs
    }
}

/// Request parameters for historical stock bars.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StockBarsRequest {
    /// Stock symbols.
    pub symbols: Vec<String>,
    /// Bar timeframe, for example `1Min`.
    pub timeframe: String,
    /// Inclusive start timestamp in RFC3339 format.
    pub start: String,
    /// Exclusive end timestamp in RFC3339 format.
    pub end: Option<String>,
    /// Stock data feed.
    pub feed: Option<String>,
    /// Page size.
    pub limit: usize,
    /// Sort order.
    pub sort: Option<String>,
    /// Optional page token.
    pub page_token: Option<String>,
}

impl StockBarsRequest {
    /// Creates a historical bars request for the given symbols.
    #[must_use]
    pub fn for_symbols(
        symbols: impl IntoIterator<Item = impl Into<String>>,
        timeframe: impl Into<String>,
        start: impl Into<String>,
    ) -> Self {
        Self {
            symbols: symbols.into_iter().map(Into::into).collect(),
            timeframe: timeframe.into(),
            start: start.into(),
            end: None,
            feed: None,
            limit: 10_000,
            sort: Some("asc".to_string()),
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
        pairs.push(("timeframe", self.timeframe.clone()));
        pairs.push(("start", self.start.clone()));
        if let Some(value) = &self.end {
            pairs.push(("end", value.clone()));
        }
        if let Some(value) = &self.feed {
            pairs.push(("feed", value.clone()));
        }
        pairs.push(("limit", self.limit.to_string()));
        if let Some(value) = &self.sort {
            pairs.push(("sort", value.clone()));
        }
        if let Some(value) = &self.page_token {
            pairs.push(("page_token", value.clone()));
        }
        pairs
    }
}

/// Request parameters for listing Alpaca orders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListOrdersRequest {
    /// Order status filter: `open`, `closed`, or `all`.
    pub status: String,
    /// Maximum number of orders to return.
    pub limit: usize,
    /// Include orders submitted after this timestamp.
    pub after: Option<String>,
    /// Include orders submitted until this timestamp.
    pub until: Option<String>,
    /// Sort direction: `asc` or `desc`.
    pub direction: Option<String>,
    /// Roll up multi-leg orders under the parent order's `legs` field.
    pub nested: bool,
    /// Optional symbol filter.
    pub symbols: Vec<String>,
    /// Optional side filter.
    pub side: Option<String>,
    /// Optional asset class filter.
    pub asset_class: Vec<String>,
    /// Return orders submitted before this order ID.
    pub before_order_id: Option<String>,
    /// Return orders submitted after this order ID.
    pub after_order_id: Option<String>,
}

impl Default for ListOrdersRequest {
    fn default() -> Self {
        Self {
            status: "open".to_string(),
            limit: 50,
            after: None,
            until: None,
            direction: None,
            nested: false,
            symbols: Vec::new(),
            side: None,
            asset_class: Vec::new(),
            before_order_id: None,
            after_order_id: None,
        }
    }
}

impl ListOrdersRequest {
    /// Creates a request for open orders with multi-leg orders nested.
    #[must_use]
    pub fn open_nested() -> Self {
        Self {
            nested: true,
            ..Self::default()
        }
    }

    pub(crate) fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if !self.status.is_empty() {
            pairs.push(("status", self.status.clone()));
        }
        pairs.push(("limit", self.limit.to_string()));
        if let Some(value) = &self.after {
            pairs.push(("after", value.clone()));
        }
        if let Some(value) = &self.until {
            pairs.push(("until", value.clone()));
        }
        if let Some(value) = &self.direction {
            pairs.push(("direction", value.clone()));
        }
        if self.nested {
            pairs.push(("nested", "true".to_string()));
        }
        if !self.symbols.is_empty() {
            pairs.push(("symbols", self.symbols.join(",")));
        }
        if let Some(value) = &self.side {
            pairs.push(("side", value.clone()));
        }
        if !self.asset_class.is_empty() {
            pairs.push(("asset_class", self.asset_class.join(",")));
        }
        if let Some(value) = &self.before_order_id {
            pairs.push(("before_order_id", value.clone()));
        }
        if let Some(value) = &self.after_order_id {
            pairs.push(("after_order_id", value.clone()));
        }
        pairs
    }
}

/// Request parameters for listing Alpaca account activities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListActivitiesRequest {
    /// Activity type filters, such as `FILL`, `OPASN`, `OPEXP`, or `OPEXC`.
    pub activity_types: Vec<String>,
    /// Activity category filter: `trade_activity` or `non_trade_activity`.
    pub category: Option<String>,
    /// Activity creation date filter.
    pub date: Option<String>,
    /// Include activities before this timestamp.
    pub until: Option<String>,
    /// Include activities after this timestamp.
    pub after: Option<String>,
    /// Sort direction: `asc` or `desc`.
    pub direction: Option<String>,
    /// Page size.
    pub page_size: usize,
    /// Optional page token.
    pub page_token: Option<String>,
}

impl Default for ListActivitiesRequest {
    fn default() -> Self {
        Self {
            activity_types: Vec::new(),
            category: None,
            date: None,
            until: None,
            after: None,
            direction: Some("desc".to_string()),
            page_size: 100,
            page_token: None,
        }
    }
}

impl ListActivitiesRequest {
    /// Creates a request for recent option reconciliation activities.
    #[must_use]
    pub fn option_reconciliation() -> Self {
        Self {
            activity_types: vec![
                "FILL".to_string(),
                "OPASN".to_string(),
                "OPEXP".to_string(),
                "OPEXC".to_string(),
                "OPTRD".to_string(),
            ],
            ..Self::default()
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
        if !self.activity_types.is_empty() {
            pairs.push(("activity_types", self.activity_types.join(",")));
        }
        if let Some(value) = &self.category {
            pairs.push(("category", value.clone()));
        }
        if let Some(value) = &self.date {
            pairs.push(("date", value.clone()));
        }
        if let Some(value) = &self.until {
            pairs.push(("until", value.clone()));
        }
        if let Some(value) = &self.after {
            pairs.push(("after", value.clone()));
        }
        if let Some(value) = &self.direction {
            pairs.push(("direction", value.clone()));
        }
        pairs.push(("page_size", self.page_size.to_string()));
        if let Some(value) = &self.page_token {
            pairs.push(("page_token", value.clone()));
        }
        pairs
    }
}

/// Request body for replacing an existing Alpaca order.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReplaceOrderRequest {
    /// New order quantity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qty: Option<String>,
    /// New time-in-force.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<String>,
    /// New limit price. For MLeg orders, positive is debit and negative is credit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<String>,
    /// New stop price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_price: Option<String>,
    /// New trailing stop value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trail: Option<String>,
    /// Client order ID for the replacement order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
}

/// Alpaca account model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaAccount {
    /// Alpaca account ID.
    pub id: Option<String>,
    /// Alpaca account number.
    pub account_number: Option<String>,
    /// Account status.
    pub status: Option<String>,
    /// Account currency.
    pub currency: Option<String>,
    /// Cash balance.
    pub cash: Option<String>,
    /// Portfolio value.
    pub portfolio_value: Option<String>,
    /// Equity value.
    pub equity: Option<String>,
    /// Buying power.
    pub buying_power: Option<String>,
    /// Regulation T buying power.
    pub regt_buying_power: Option<String>,
    /// Day-trading buying power.
    pub daytrading_buying_power: Option<String>,
    /// Options buying power.
    pub options_buying_power: Option<String>,
    /// Pattern day trader flag.
    pub pattern_day_trader: Option<bool>,
    /// Trading blocked flag.
    pub trading_blocked: Option<bool>,
    /// Transfer blocked flag.
    pub transfers_blocked: Option<bool>,
    /// Account blocked flag.
    pub account_blocked: Option<bool>,
    /// User-suspended trading flag.
    pub trade_suspended_by_user: Option<bool>,
    /// Account multiplier.
    pub multiplier: Option<String>,
}

/// Alpaca open position model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaPosition {
    /// Alpaca asset ID.
    pub asset_id: Option<String>,
    /// Asset symbol.
    pub symbol: Option<String>,
    /// Exchange.
    pub exchange: Option<String>,
    /// Asset class.
    pub asset_class: Option<String>,
    /// Position quantity.
    pub qty: Option<String>,
    /// Position side.
    pub side: Option<String>,
    /// Market value.
    pub market_value: Option<String>,
    /// Cost basis.
    pub cost_basis: Option<String>,
    /// Current price.
    pub current_price: Option<String>,
    /// Unrealized profit/loss.
    pub unrealized_pl: Option<String>,
    /// Unrealized profit/loss percentage.
    pub unrealized_plpc: Option<String>,
    /// Average entry price.
    pub avg_entry_price: Option<String>,
}

/// Alpaca order model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaOrder {
    /// Alpaca order ID.
    pub id: Option<String>,
    /// Client order ID.
    pub client_order_id: Option<String>,
    /// Created timestamp.
    pub created_at: Option<String>,
    /// Updated timestamp.
    pub updated_at: Option<String>,
    /// Submitted timestamp.
    pub submitted_at: Option<String>,
    /// Filled timestamp.
    pub filled_at: Option<String>,
    /// Expired timestamp.
    pub expired_at: Option<String>,
    /// Canceled timestamp.
    pub canceled_at: Option<String>,
    /// Failed timestamp.
    pub failed_at: Option<String>,
    /// Asset ID.
    pub asset_id: Option<String>,
    /// Asset symbol.
    pub symbol: Option<String>,
    /// Asset class.
    pub asset_class: Option<String>,
    /// Ordered quantity.
    pub qty: Option<String>,
    /// Filled quantity.
    pub filled_qty: Option<String>,
    /// Average filled price.
    pub filled_avg_price: Option<String>,
    /// Order type.
    #[serde(rename = "type")]
    pub order_type: Option<String>,
    /// Order side.
    pub side: Option<String>,
    /// Time in force.
    pub time_in_force: Option<String>,
    /// Limit price.
    pub limit_price: Option<String>,
    /// Order status.
    pub status: Option<String>,
    /// Advanced order class.
    pub order_class: Option<String>,
    /// Nested child orders or multi-leg order legs.
    #[serde(default)]
    pub legs: Option<Vec<AlpacaOrder>>,
}

impl AlpacaOrder {
    /// Returns `true` if Alpaca has no further expected lifecycle updates for this order.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("filled" | "canceled" | "expired" | "rejected")
        )
    }

    /// Returns `true` if this order can still block duplicate strategy admission.
    #[must_use]
    pub fn is_working(&self) -> bool {
        !self.is_terminal()
    }

    /// Returns all symbols on this order, including nested multi-leg symbols.
    #[must_use]
    pub fn symbols(&self) -> Vec<String> {
        let mut symbols = Vec::new();
        self.push_symbols(&mut symbols);
        symbols.sort();
        symbols.dedup();
        symbols
    }

    fn push_symbols(&self, symbols: &mut Vec<String>) {
        if let Some(symbol) = self
            .symbol
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            symbols.push(symbol.clone());
        }
        if let Some(legs) = &self.legs {
            for leg in legs {
                leg.push_symbols(symbols);
            }
        }
    }
}

/// Alpaca account activity model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaActivity {
    /// Activity type.
    pub activity_type: Option<String>,
    /// Activity ID.
    pub id: Option<String>,
    /// Cumulative filled quantity.
    pub cum_qty: Option<String>,
    /// Remaining quantity.
    pub leaves_qty: Option<String>,
    /// Execution price.
    pub price: Option<String>,
    /// Activity quantity.
    pub qty: Option<String>,
    /// Activity side.
    pub side: Option<String>,
    /// Activity symbol.
    pub symbol: Option<String>,
    /// Trade transaction timestamp.
    pub transaction_time: Option<String>,
    /// Related broker order ID.
    pub order_id: Option<String>,
    /// Trade activity subtype.
    #[serde(rename = "type")]
    pub activity_subtype: Option<String>,
    /// Non-trade activity date.
    pub date: Option<String>,
    /// Net cash amount.
    pub net_amount: Option<String>,
    /// Security CUSIP.
    pub cusip: Option<String>,
    /// Per-share amount.
    pub per_share_amount: Option<String>,
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

/// Response from Alpaca's historical option bars endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OptionBarsResponse {
    /// Bars grouped by option symbol.
    #[serde(default)]
    pub bars: BTreeMap<String, Vec<AlpacaOptionBar>>,
    /// Next page token, if more records are available.
    pub next_page_token: Option<String>,
}

impl OptionBarsResponse {
    /// Returns the next token from Alpaca's response.
    #[must_use]
    pub fn next_token(&self) -> Option<String> {
        self.next_page_token
            .clone()
            .filter(|value| !value.trim().is_empty())
    }
}

/// Response from Alpaca's stock snapshots endpoint.
#[derive(Clone, Debug, Serialize)]
pub struct StockSnapshotsResponse {
    /// Snapshots keyed by stock symbol.
    pub snapshots: BTreeMap<String, AlpacaStockSnapshot>,
}

impl<'de> Deserialize<'de> for StockSnapshotsResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let source = value.get("snapshots").cloned().unwrap_or(value);
        let snapshots = BTreeMap::<String, AlpacaStockSnapshot>::deserialize(source)
            .map_err(D::Error::custom)?;
        Ok(Self { snapshots })
    }
}

/// Response from Alpaca's historical stock bars endpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StockBarsResponse {
    /// Bars grouped by stock symbol.
    #[serde(default)]
    pub bars: BTreeMap<String, Vec<AlpacaStockBar>>,
    /// Next page token, if more records are available.
    pub next_page_token: Option<String>,
}

impl StockBarsResponse {
    /// Returns the next token from Alpaca's response.
    #[must_use]
    pub fn next_token(&self) -> Option<String> {
        self.next_page_token
            .clone()
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

/// Alpaca stock snapshot model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaStockSnapshot {
    /// Latest quote.
    #[serde(default, alias = "latestQuote", alias = "latest_quote")]
    pub latest_quote: Option<AlpacaStockQuote>,
    /// Latest trade.
    #[serde(default, alias = "latestTrade", alias = "latest_trade")]
    pub latest_trade: Option<AlpacaStockTrade>,
    /// Latest minute bar.
    #[serde(default, alias = "minuteBar", alias = "minute_bar")]
    pub minute_bar: Option<AlpacaStockBar>,
    /// Latest daily bar.
    #[serde(default, alias = "dailyBar", alias = "daily_bar")]
    pub daily_bar: Option<AlpacaStockBar>,
    /// Previous daily bar.
    #[serde(default, alias = "prevDailyBar", alias = "prev_daily_bar")]
    pub prev_daily_bar: Option<AlpacaStockBar>,
}

impl AlpacaStockSnapshot {
    /// Returns the best available spot proxy for scanner metrics.
    #[must_use]
    pub fn latest_price(&self) -> Option<f64> {
        self.latest_quote
            .as_ref()
            .and_then(AlpacaStockQuote::midpoint)
            .or_else(|| self.latest_trade.as_ref().and_then(|trade| trade.price))
            .or_else(|| self.minute_bar.as_ref().and_then(|bar| bar.close))
            .or_else(|| self.daily_bar.as_ref().and_then(|bar| bar.close))
            .or_else(|| self.prev_daily_bar.as_ref().and_then(|bar| bar.close))
            .filter(|price| *price > 0.0)
    }
}

/// Alpaca stock quote model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaStockQuote {
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

impl AlpacaStockQuote {
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

/// Alpaca stock trade model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaStockTrade {
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

/// Alpaca stock bar model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlpacaStockBar {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn list_activities_request_replaces_page_token() {
        let request = ListActivitiesRequest::option_reconciliation();
        let paged = request.with_page_token(Some("page-1".to_string()));

        assert_eq!(paged.page_token.as_deref(), Some("page-1"));
        assert!(paged.activity_types.contains(&"FILL".to_string()));
        assert!(paged.activity_types.contains(&"OPASN".to_string()));
        assert!(paged.activity_types.contains(&"OPEXP".to_string()));
        assert!(paged.activity_types.contains(&"OPEXC".to_string()));
        assert!(paged.activity_types.contains(&"OPTRD".to_string()));
    }

    #[test]
    fn replace_order_request_skips_empty_fields() {
        let request = ReplaceOrderRequest {
            limit_price: Some("-0.50".to_string()),
            client_order_id: Some("replace-1".to_string()),
            ..Default::default()
        };

        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(
            value,
            json!({
                "limit_price": "-0.50",
                "client_order_id": "replace-1",
            }),
        );
    }
}
