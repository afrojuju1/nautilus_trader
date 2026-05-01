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

//! Default Alpaca API endpoints.

/// Alpaca paper trading REST base URL.
pub const PAPER_TRADING_BASE_URL: &str = "https://paper-api.alpaca.markets";

/// Alpaca live trading REST base URL.
pub const LIVE_TRADING_BASE_URL: &str = "https://api.alpaca.markets";

/// Alpaca market data REST base URL for stocks and options.
pub const DATA_BASE_URL: &str = "https://data.alpaca.markets";

/// Alpaca stock market data WebSocket URL.
pub const STOCK_WS_URL: &str = "wss://stream.data.alpaca.markets/v2";

/// Alpaca option market data WebSocket URL.
pub const OPTION_WS_URL: &str = "wss://stream.data.alpaca.markets/v1beta1";

/// Alpaca account trade update WebSocket URL for paper trading.
pub const PAPER_TRADE_UPDATES_WS_URL: &str = "wss://paper-api.alpaca.markets/stream";

/// Alpaca account trade update WebSocket URL for live trading.
pub const LIVE_TRADE_UPDATES_WS_URL: &str = "wss://api.alpaca.markets/stream";
