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

//! Dry-run put-credit scanner using Alpaca option contracts and snapshots.
//!
//! Production entries are owned by `alpaca-option-chain-scan-live-node`.

use std::{env, str::FromStr};

use nautilus_alpaca::{
    candidate_engine::PutCreditScannerConfig, config::AlpacaDataClientConfig,
    http::client::AlpacaHttpClient, strategy::scan_put_credit_underlying,
};

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

    let strategy = strategy_config_from_env();

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&data_config)?;

    for underlying in underlyings {
        let result =
            scan_put_credit_underlying(&client, &data_config, &strategy, underlying).await?;

        if let Some(best) = result.candidates.first() {
            println!(
                "{}: contracts={}, snapshots={}, scoreable={}, candidates={}, top short={} long={} exp={} width={:.2} credit={:.2} max_loss={:.2} ror={:.1}% delta={:.3} iv={} score={:.1}",
                result.underlying,
                result.contract_count,
                result.snapshot_count,
                result.scoreable_count,
                result.candidates.len(),
                best.short.symbol,
                best.long.symbol,
                best.short.expiration_date,
                best.width,
                best.credit,
                best.max_loss,
                best.return_on_risk * 100.0,
                best.short.delta_abs,
                best.short
                    .implied_volatility
                    .map(|value| format!("{value:.3}"))
                    .unwrap_or_else(|| "none".to_string()),
                best.score,
            );
        } else {
            println!(
                "{}: contracts={}, snapshots={}, scoreable={}, candidates=0",
                result.underlying,
                result.contract_count,
                result.snapshot_count,
                result.scoreable_count,
            );
        }
    }

    Ok(())
}

fn strategy_config_from_env() -> PutCreditScannerConfig {
    PutCreditScannerConfig {
        min_dte: env_parse("ALPACA_DRY_RUN_MIN_DTE", 5),
        max_dte: env_parse("ALPACA_DRY_RUN_MAX_DTE", 10),
        short_delta_min: env_parse("ALPACA_DRY_RUN_SHORT_DELTA_MIN", 0.18),
        short_delta_max: env_parse("ALPACA_DRY_RUN_SHORT_DELTA_MAX", 0.28),
        widths: env_widths("ALPACA_DRY_RUN_WIDTHS", &[2.0, 3.0, 5.0]),
        min_open_interest: env_parse("ALPACA_DRY_RUN_MIN_OPEN_INTEREST", 200),
        max_leg_spread_pct: env_parse("ALPACA_DRY_RUN_MAX_LEG_SPREAD_PCT", 0.15),
        min_return_on_risk: env_parse("ALPACA_DRY_RUN_MIN_RETURN_ON_RISK", 0.13),
        min_credit_to_width: env_parse("ALPACA_DRY_RUN_MIN_CREDIT_TO_WIDTH", 0.08),
    }
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

fn env_widths(name: &str, default: &[f64]) -> Vec<f64> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter_map(|part| part.trim().parse::<f64>().ok())
                .filter(|value| *value > 0.0)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| default.to_vec())
}
