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

//! Error types for the Alpaca REST client.

/// Result type for Alpaca adapter operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Error type for Alpaca adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// API credentials were required but could not be resolved.
    #[error(
        "Alpaca API credentials are required; set APCA_API_KEY_ID/APCA_API_SECRET_KEY or ALPACA_API_KEY/ALPACA_SECRET_KEY"
    )]
    MissingCredentials,
    /// The HTTP client could not be built.
    #[error("failed to build Alpaca HTTP client: {0}")]
    ClientBuild(String),
    /// The Alpaca API returned a non-success status code.
    #[error("Alpaca request failed with HTTP {status} for {url}: {body}")]
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// Requested URL.
        url: String,
        /// Response body.
        body: String,
    },
    /// A request failed before receiving an Alpaca API response.
    #[error("Alpaca request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// A response body could not be decoded.
    #[error("failed to decode Alpaca response: {0}")]
    Decode(#[from] serde_json::Error),
    /// A URL could not be constructed.
    #[error("invalid Alpaca URL: {0}")]
    Url(#[from] url::ParseError),
}
