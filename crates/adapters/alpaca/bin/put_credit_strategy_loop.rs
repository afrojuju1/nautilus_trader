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

//! Legacy timer-style put-credit strategy loop using the shared Alpaca scanner.
//!
//! The production account engine is `alpaca-index-credit-engine`.

use std::{env, str::FromStr, time::Duration};

use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    execution::check_put_credit_entry_admission,
    http::{client::AlpacaHttpClient, models::ListOrdersRequest},
    strategy::{PutCreditScannerConfig, scan_put_credit_underlying},
};
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let underlyings: Vec<String> = env::args().skip(1).collect();
    let underlyings = if underlyings.is_empty() {
        vec![
            "SPY".to_string(),
            "QQQ".to_string(),
            "IWM".to_string(),
            "DIA".to_string(),
            "GLD".to_string(),
        ]
    } else {
        underlyings
    };
    let max_iterations = env_parse("ALPACA_STRATEGY_MAX_ITERATIONS", 1_u64);
    let interval_secs = env_parse("ALPACA_STRATEGY_INTERVAL_SECS", 300_u64);

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&data_config)?;
    let scanner_config = PutCreditScannerConfig::default();

    for iteration in 1..=max_iterations {
        println!("strategy_iteration={iteration}");
        let account = client.account().await?;
        let positions = client.positions().await?;
        let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;

        for underlying in &underlyings {
            let result =
                scan_put_credit_underlying(&client, &data_config, &scanner_config, underlying)
                    .await?;
            let Some(best) = result.candidates.first() else {
                println!(
                    "{underlying}: no_candidate contracts={} snapshots={} scoreable={}",
                    result.contract_count, result.snapshot_count, result.scoreable_count,
                );
                continue;
            };

            let admission = check_put_credit_entry_admission(
                &account,
                &positions,
                &open_orders,
                &best.short.symbol,
                &best.long.symbol,
            );
            if admission.allowed {
                println!(
                    "{underlying}: entry_ready short={} long={} credit={:.2} ror={:.1}% score={:.1}",
                    best.short.symbol,
                    best.long.symbol,
                    best.credit,
                    best.return_on_risk * 100.0,
                    best.score,
                );
            } else {
                println!(
                    "{underlying}: admission_rejected short={} long={} reasons={}",
                    best.short.symbol,
                    best.long.symbol,
                    admission.reasons.join(" | "),
                );
            }
        }

        if iteration < max_iterations {
            sleep(Duration::from_secs(interval_secs)).await;
        }
    }

    Ok(())
}

fn env_parse<T>(name: &str, default: T) -> T
where
    T: FromStr,
{
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default)
}
