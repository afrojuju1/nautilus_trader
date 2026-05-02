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

//! Dry-run put credit spread scanner using Alpaca option contracts and snapshots.

use std::{collections::HashMap, env, str::FromStr};

use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaOptionContract, AlpacaOptionSnapshot, AlpacaOptionType, OptionSnapshotsRequest,
        },
    },
    providers::AlpacaOptionContractProvider,
};
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug)]
struct StrategyConfig {
    min_dte: i64,
    max_dte: i64,
    short_delta_min: f64,
    short_delta_max: f64,
    widths: Vec<f64>,
    min_open_interest: u64,
    max_leg_spread_pct: f64,
    min_return_on_risk: f64,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            min_dte: env_parse("ALPACA_DRY_RUN_MIN_DTE", 5),
            max_dte: env_parse("ALPACA_DRY_RUN_MAX_DTE", 10),
            short_delta_min: env_parse("ALPACA_DRY_RUN_SHORT_DELTA_MIN", 0.18),
            short_delta_max: env_parse("ALPACA_DRY_RUN_SHORT_DELTA_MAX", 0.28),
            widths: env_widths("ALPACA_DRY_RUN_WIDTHS", &[2.0, 3.0, 5.0]),
            min_open_interest: env_parse("ALPACA_DRY_RUN_MIN_OPEN_INTEREST", 200),
            max_leg_spread_pct: env_parse("ALPACA_DRY_RUN_MAX_LEG_SPREAD_PCT", 0.15),
            min_return_on_risk: env_parse("ALPACA_DRY_RUN_MIN_RETURN_ON_RISK", 0.13),
        }
    }
}

#[derive(Clone, Debug)]
struct ScoredContract {
    symbol: String,
    expiration_date: String,
    strike: f64,
    bid: f64,
    ask: f64,
    delta_abs: f64,
    spread_pct: f64,
    implied_volatility: Option<f64>,
}

#[derive(Clone, Debug)]
struct SpreadCandidate {
    short: ScoredContract,
    long: ScoredContract,
    width: f64,
    credit: f64,
    max_loss: f64,
    return_on_risk: f64,
    score: f64,
}

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

    let strategy = StrategyConfig::default();
    let today = OffsetDateTime::now_utc().date();
    let min_expiration = (today + Duration::days(strategy.min_dte)).to_string();
    let max_expiration = (today + Duration::days(strategy.max_dte)).to_string();

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&data_config)?;
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
        let symbols = contracts
            .iter()
            .map(|contract| contract.symbol.clone())
            .collect::<Vec<_>>();
        let mut snapshots_request = OptionSnapshotsRequest::for_symbols(symbols);
        snapshots_request.feed = Some(data_config.option_feed.as_str().to_string());
        let snapshots = client.option_snapshots(&snapshots_request).await?.snapshots;

        let scored = score_contracts(&contracts, &snapshots, &strategy);
        let candidates = build_candidates(&scored, &strategy);

        if let Some(best) = candidates.first() {
            println!(
                "{underlying}: contracts={}, snapshots={}, scoreable={}, candidates={}, top short={} long={} exp={} width={:.2} credit={:.2} max_loss={:.2} ror={:.1}% delta={:.3} iv={} score={:.1}",
                contracts.len(),
                snapshots.len(),
                scored.len(),
                candidates.len(),
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
                "{underlying}: contracts={}, snapshots={}, scoreable={}, candidates=0",
                contracts.len(),
                snapshots.len(),
                scored.len(),
            );
        }
    }

    Ok(())
}

fn score_contracts(
    contracts: &[AlpacaOptionContract],
    snapshots: &std::collections::BTreeMap<String, AlpacaOptionSnapshot>,
    config: &StrategyConfig,
) -> Vec<ScoredContract> {
    contracts
        .iter()
        .filter_map(|contract| {
            let open_interest = contract
                .open_interest
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())?;
            if open_interest < config.min_open_interest {
                return None;
            }

            let snapshot = snapshots.get(&contract.symbol)?;
            let quote = snapshot.latest_quote.as_ref()?;
            let bid = quote.bid_price?;
            let ask = quote.ask_price?;
            let midpoint = quote.midpoint()?;
            let spread_pct = (ask - bid) / midpoint;
            if spread_pct > config.max_leg_spread_pct {
                return None;
            }

            let delta_abs = snapshot.greeks.as_ref()?.delta?.abs();
            if delta_abs < config.short_delta_min || delta_abs > config.short_delta_max {
                return None;
            }

            Some(ScoredContract {
                symbol: contract.symbol.clone(),
                expiration_date: contract.expiration_date.clone(),
                strike: contract.strike_price.parse::<f64>().ok()?,
                bid,
                ask,
                delta_abs,
                spread_pct,
                implied_volatility: snapshot.implied_volatility,
            })
        })
        .collect()
}

fn build_candidates(contracts: &[ScoredContract], config: &StrategyConfig) -> Vec<SpreadCandidate> {
    let by_expiration_strike = contracts
        .iter()
        .map(|contract| {
            (
                (
                    contract.expiration_date.clone(),
                    strike_key(contract.strike),
                ),
                contract,
            )
        })
        .collect::<HashMap<_, _>>();

    let mut candidates = Vec::new();
    for short in contracts {
        for width in &config.widths {
            let long_strike = short.strike - width;
            let Some(long) = by_expiration_strike
                .get(&(short.expiration_date.clone(), strike_key(long_strike)))
                .copied()
            else {
                continue;
            };

            let credit = short.bid - long.ask;
            if credit <= 0.0 {
                continue;
            }
            let max_loss = width - credit;
            if max_loss <= 0.0 {
                continue;
            }
            let return_on_risk = credit / max_loss;
            if return_on_risk < config.min_return_on_risk {
                continue;
            }

            let delta_midpoint = (config.short_delta_min + config.short_delta_max) / 2.0;
            let delta_half_range = (config.short_delta_max - config.short_delta_min) / 2.0;
            let delta_score =
                (1.0 - ((short.delta_abs - delta_midpoint).abs() / delta_half_range)).max(0.0);
            let ror_score = (return_on_risk / config.min_return_on_risk).min(2.0) / 2.0;
            let credit_score = (credit / width).min(0.5) / 0.5;
            let spread_penalty =
                ((short.spread_pct + long.spread_pct) / (2.0 * config.max_leg_spread_pct)).min(1.0);
            let score =
                delta_score * 35.0 + ror_score * 30.0 + credit_score * 25.0 - spread_penalty * 10.0;

            candidates.push(SpreadCandidate {
                short: short.clone(),
                long: long.clone(),
                width: *width,
                credit,
                max_loss,
                return_on_risk,
                score,
            });
        }
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

fn strike_key(strike: f64) -> i64 {
    (strike * 1_000.0).round() as i64
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
