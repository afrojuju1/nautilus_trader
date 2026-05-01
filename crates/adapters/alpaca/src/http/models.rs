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

use serde::{Deserialize, Serialize};

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
