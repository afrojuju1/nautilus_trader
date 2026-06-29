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

//! Diagnostic command for loading Alpaca option snapshots for recently loaded contracts.

use std::env;

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaOptionType, OptionSnapshotsRequest},
    },
    operator,
    providers::AlpacaOptionContractProvider,
};
use time::{Duration, OffsetDateTime};

pub(crate) async fn run() -> anyhow::Result<()> {
    let underlyings = operator::args();
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
    let contract_limit = env::var("ALPACA_SNAPSHOT_CONTRACT_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);

    let mut config = AlpacaDataClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&config)?;
    let provider = AlpacaOptionContractProvider::new(client.clone());

    for underlying in underlyings {
        let contracts = provider
            .load_active_contracts(
                underlying.clone(),
                min_expiration.clone(),
                max_expiration.clone(),
                Some(AlpacaOptionType::Put),
            )
            .await?;
        let symbols: Vec<String> = contracts
            .into_iter()
            .take(contract_limit)
            .map(|contract| contract.symbol)
            .collect();
        let mut request = OptionSnapshotsRequest::for_symbols(symbols.iter().cloned());
        request.feed = Some(config.option_feed.as_str().to_string());

        let snapshots = client.option_snapshots(&request).await?.snapshots;
        let quote_count = snapshots
            .values()
            .filter(|snapshot| snapshot.has_valid_quote())
            .count();
        let greek_count = snapshots
            .values()
            .filter(|snapshot| snapshot.has_greek_inputs())
            .count();
        let delta_count = snapshots
            .values()
            .filter(|snapshot| {
                snapshot
                    .greeks
                    .as_ref()
                    .and_then(|greeks| greeks.delta)
                    .is_some()
            })
            .count();
        let iv_count = snapshots
            .values()
            .filter(|snapshot| snapshot.implied_volatility.is_some())
            .count();

        println!(
            "{underlying}: requested {} snapshots from {} contracts; snapshots={}, quotes={}, greeks_or_iv={}, delta={}, iv={}",
            symbols.len(),
            contract_limit,
            snapshots.len(),
            quote_count,
            greek_count,
            delta_count,
            iv_count,
        );
    }

    Ok(())
}
