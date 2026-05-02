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

//! Configuration structures for the Alpaca adapter scaffold.

use crate::common::{
    credentials::AlpacaCredential,
    urls::{
        DATA_BASE_URL, LIVE_TRADE_UPDATES_WS_URL, LIVE_TRADING_BASE_URL,
        PAPER_TRADE_UPDATES_WS_URL, PAPER_TRADING_BASE_URL,
    },
};

/// Alpaca trading environment.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlpacaEnvironment {
    /// Paper trading account and order endpoints.
    #[default]
    Paper,
    /// Live trading account and order endpoints.
    Live,
}

impl AlpacaEnvironment {
    /// Returns the default trading REST base URL for the environment.
    #[must_use]
    pub const fn trading_base_url(self) -> &'static str {
        match self {
            Self::Paper => PAPER_TRADING_BASE_URL,
            Self::Live => LIVE_TRADING_BASE_URL,
        }
    }

    /// Returns the default account trade updates WebSocket URL for the environment.
    #[must_use]
    pub const fn trade_updates_ws_url(self) -> &'static str {
        match self {
            Self::Paper => PAPER_TRADE_UPDATES_WS_URL,
            Self::Live => LIVE_TRADE_UPDATES_WS_URL,
        }
    }
}

/// Alpaca stock market data feed selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlpacaStockFeed {
    /// IEX feed, available on free and paid plans.
    #[default]
    Iex,
    /// SIP feed, available on paid plans.
    Sip,
    /// Delayed SIP feed.
    DelayedSip,
}

impl AlpacaStockFeed {
    /// Returns the Alpaca API feed parameter value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Iex => "iex",
            Self::Sip => "sip",
            Self::DelayedSip => "delayed_sip",
        }
    }
}

/// Alpaca option market data feed selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlpacaOptionFeed {
    /// Indicative option feed.
    #[default]
    Indicative,
    /// OPRA option feed, available on subscribed accounts.
    Opra,
}

impl AlpacaOptionFeed {
    /// Returns the Alpaca API feed parameter value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Indicative => "indicative",
            Self::Opra => "opra",
        }
    }
}

/// Configuration for the planned Alpaca live data client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlpacaDataClientConfig {
    /// Optional API key. If omitted, the adapter reads `ALPACA_API_KEY`.
    pub api_key: Option<String>,
    /// Optional API secret. If omitted, the adapter reads `ALPACA_API_SECRET`.
    pub api_secret: Option<String>,
    /// Trading environment used when data requests need account context.
    pub environment: AlpacaEnvironment,
    /// Optional override for the market data REST base URL.
    pub data_base_url: Option<String>,
    /// Optional override for the trading REST base URL.
    pub trading_base_url: Option<String>,
    /// Stock market data feed.
    pub stock_feed: AlpacaStockFeed,
    /// Option market data feed.
    pub option_feed: AlpacaOptionFeed,
    /// Maximum number of option symbols to subscribe in one WebSocket request.
    pub max_option_subscriptions: usize,
    /// HTTP request timeout in seconds.
    pub request_timeout_secs: u64,
    /// Optional interval for polling option snapshot Greeks and IV.
    pub snapshot_greeks_poll_secs: Option<u64>,
}

impl Default for AlpacaDataClientConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            api_secret: None,
            environment: AlpacaEnvironment::default(),
            data_base_url: None,
            trading_base_url: None,
            stock_feed: AlpacaStockFeed::default(),
            option_feed: AlpacaOptionFeed::default(),
            max_option_subscriptions: 1_000,
            request_timeout_secs: 30,
            snapshot_greeks_poll_secs: None,
        }
    }
}

impl AlpacaDataClientConfig {
    /// Creates a configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the data REST base URL, considering overrides.
    #[must_use]
    pub fn resolved_data_base_url(&self) -> &str {
        self.data_base_url.as_deref().unwrap_or(DATA_BASE_URL)
    }

    /// Returns the trading REST base URL, considering overrides and environment.
    #[must_use]
    pub fn resolved_trading_base_url(&self) -> &str {
        self.trading_base_url
            .as_deref()
            .unwrap_or_else(|| self.environment.trading_base_url())
    }

    /// Returns `true` if credentials are configured directly or through environment variables.
    #[must_use]
    pub fn has_api_credentials(&self) -> bool {
        AlpacaCredential::resolve(self.api_key.clone(), self.api_secret.clone()).is_some()
    }
}

/// Configuration for the planned Alpaca live execution client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlpacaExecClientConfig {
    /// Optional API key. If omitted, the adapter reads `ALPACA_API_KEY`.
    pub api_key: Option<String>,
    /// Optional API secret. If omitted, the adapter reads `ALPACA_API_SECRET`.
    pub api_secret: Option<String>,
    /// Trading environment.
    pub environment: AlpacaEnvironment,
    /// Optional override for the trading REST base URL.
    pub trading_base_url: Option<String>,
    /// Optional override for the account trade updates WebSocket URL.
    pub trade_updates_ws_url: Option<String>,
    /// HTTP request timeout in seconds.
    pub request_timeout_secs: u64,
    /// Whether to consume Alpaca trade update events over WebSocket.
    pub use_trade_updates_stream: bool,
    /// Prefix for client order IDs generated by the adapter.
    pub client_order_id_prefix: String,
    /// Whether to ignore execution reports for client order IDs outside the configured prefix.
    pub external_order_filtering: bool,
    /// Optional interval for REST reconciliation repair polling.
    pub reconciliation_poll_secs: Option<u64>,
}

impl Default for AlpacaExecClientConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            api_secret: None,
            environment: AlpacaEnvironment::default(),
            trading_base_url: None,
            trade_updates_ws_url: None,
            request_timeout_secs: 30,
            use_trade_updates_stream: true,
            client_order_id_prefix: "nautilus".to_string(),
            external_order_filtering: false,
            reconciliation_poll_secs: Some(60),
        }
    }
}

impl AlpacaExecClientConfig {
    /// Creates a configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the trading REST base URL, considering overrides and environment.
    #[must_use]
    pub fn resolved_trading_base_url(&self) -> &str {
        self.trading_base_url
            .as_deref()
            .unwrap_or_else(|| self.environment.trading_base_url())
    }

    /// Returns the trade update WebSocket URL, considering overrides and environment.
    #[must_use]
    pub fn resolved_trade_updates_ws_url(&self) -> &str {
        self.trade_updates_ws_url
            .as_deref()
            .unwrap_or_else(|| self.environment.trade_updates_ws_url())
    }

    /// Returns `true` if credentials are configured directly or through environment variables.
    #[must_use]
    pub fn has_api_credentials(&self) -> bool {
        AlpacaCredential::resolve(self.api_key.clone(), self.api_secret.clone()).is_some()
    }
}
