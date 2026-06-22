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

//! Constants for the Alpaca adapter.

/// Nautilus venue code used for Alpaca instruments and accounts.
pub const ALPACA_VENUE: &str = "ALPACA";

/// Default Nautilus client identifier for Alpaca data and execution clients.
pub const ALPACA_CLIENT_ID: &str = "ALPACA";

/// Primary environment variable used to load the Alpaca API key.
pub const ENV_APCA_API_KEY_ID: &str = "APCA_API_KEY_ID";

/// Fallback environment variable used by some existing Nautilus deployments.
pub const ENV_ALPACA_API_KEY: &str = "ALPACA_API_KEY";

/// Primary environment variable used to load the Alpaca API secret.
pub const ENV_APCA_API_SECRET_KEY: &str = "APCA_API_SECRET_KEY";

/// Fallback environment variable used by some existing Nautilus deployments.
pub const ENV_ALPACA_SECRET_KEY: &str = "ALPACA_SECRET_KEY";

/// Alternate environment variable for the Alpaca API secret.
pub const ENV_ALPACA_API_SECRET: &str = "ALPACA_API_SECRET";

/// Alpaca option symbol pattern used by the options contracts and market data APIs.
pub const OPTION_SYMBOL_FORMAT: &str = "O:<ROOT><YYMMDD><C|P><STRIKE>";

/// Instrument metadata key used to preserve Alpaca option contract open interest.
pub const ALPACA_OPEN_INTEREST_INFO_KEY: &str = "alpaca_open_interest";
