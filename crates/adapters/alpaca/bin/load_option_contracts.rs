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

//! Smoke utility for loading Alpaca option contracts.

use std::env;

use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{client::AlpacaHttpClient, models::AlpacaOptionType},
    providers::AlpacaOptionContractProvider,
};
use time::{Duration, OffsetDateTime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let underlyings: Vec<String> = env::args().skip(1).collect();
    let underlyings = if underlyings.is_empty() {
        vec!["SPY".to_string(), "QQQ".to_string()]
    } else {
        underlyings
    };

    let today = OffsetDateTime::now_utc().date();
    let min_expiration =
        env::var("ALPACA_CONTRACTS_MIN_EXPIRATION").unwrap_or_else(|_| today.to_string());
    let max_expiration = env::var("ALPACA_CONTRACTS_MAX_EXPIRATION")
        .unwrap_or_else(|_| (today + Duration::days(14)).to_string());

    let mut config = AlpacaDataClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&config)?;
    let provider = AlpacaOptionContractProvider::new(client);

    for underlying in underlyings {
        let puts = provider
            .load_active_contracts(
                underlying.clone(),
                min_expiration.clone(),
                max_expiration.clone(),
                Some(AlpacaOptionType::Put),
            )
            .await?;
        println!(
            "{underlying}: {} active put contracts expiring {min_expiration}..{max_expiration}",
            puts.len()
        );
    }

    Ok(())
}
