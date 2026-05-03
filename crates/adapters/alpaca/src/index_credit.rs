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

//! Index credit strategy configuration and candidate selection.

use std::{env, path::PathBuf, str::FromStr};

use chrono::NaiveTime;
use chrono_tz::Tz;

use crate::{
    config::AlpacaDataClientConfig,
    execution::check_put_credit_entry_admission,
    http::{client::AlpacaHttpClient, models::ListOrdersRequest},
    management::CreditSpreadManagementConfig,
    runtime::{StrategyState, credit_spread_strategy_name},
    strategy::{
        CreditSpreadKind, PutCreditScannerConfig, SpreadCandidate, scan_call_credit_underlying,
        scan_put_credit_underlying,
    },
};

/// Environment-driven config for the index credit account-engine slice.
#[derive(Debug)]
pub struct IndexCreditConfig {
    /// Underlyings to scan.
    pub underlyings: Vec<String>,
    /// Enabled spread kinds.
    pub spread_kinds: Vec<CreditSpreadKind>,
    /// Maximum loop iterations. Zero means run continuously.
    pub max_iterations: u64,
    /// Delay between iterations.
    pub interval_secs: u64,
    /// Strategy quantity.
    pub quantity: u64,
    /// Whether entry submission is enabled.
    pub submit_enabled: bool,
    /// Whether management actions are enabled.
    pub manage_enabled: bool,
    /// Whether new entries are blocked.
    pub kill_switch: bool,
    /// Whether every active entry should be flattened.
    pub force_flatten: bool,
    /// Whether accepted smoke orders should be canceled.
    pub cancel_after_accept: bool,
    /// Stale entry timeout.
    pub stale_entry_secs: u64,
    /// Whether close order submission is enabled.
    pub close_enabled: bool,
    /// Profit-target close fraction.
    pub profit_target_close_fraction: f64,
    /// Stop-loss close multiple.
    pub stop_loss_close_multiple: f64,
    /// Maximum hold time. Zero disables.
    pub max_hold_secs: u64,
    /// Expiration-risk exit days. Negative disables.
    pub expiration_exit_days: i64,
    /// Whether the entry window gate should be ignored.
    pub ignore_entry_window: bool,
    /// Entry window start.
    pub entry_start: NaiveTime,
    /// Entry window end.
    pub entry_end: NaiveTime,
    /// Entry window timezone.
    pub entry_timezone: Tz,
    /// Local strategy state path.
    pub state_path: PathBuf,
    /// Credit scanner config.
    pub scanner: PutCreditScannerConfig,
}

impl IndexCreditConfig {
    /// Builds config from environment variables and optional positional underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when strategy names, times, or timezone values are invalid.
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            underlyings: underlyings_from_env(),
            spread_kinds: spread_kinds_from_env()?,
            max_iterations: env_parse("ALPACA_INDEX_PUT_CREDIT_MAX_ITERATIONS", 1_u64),
            interval_secs: env_parse("ALPACA_INDEX_PUT_CREDIT_INTERVAL_SECS", 300_u64),
            quantity: env_parse("ALPACA_INDEX_PUT_CREDIT_QTY", 1_u64),
            submit_enabled: env_bool("ALPACA_INDEX_PUT_CREDIT_SUBMIT", false),
            manage_enabled: env_bool("ALPACA_INDEX_CREDIT_MANAGE", false),
            kill_switch: env_bool("ALPACA_INDEX_CREDIT_KILL_SWITCH", false),
            force_flatten: env_bool("ALPACA_INDEX_CREDIT_FORCE_FLATTEN", false),
            cancel_after_accept: env_bool("ALPACA_INDEX_PUT_CREDIT_CANCEL_AFTER_ACCEPT", false),
            stale_entry_secs: env_parse("ALPACA_INDEX_CREDIT_STALE_ENTRY_SECS", 900_u64),
            close_enabled: env_bool("ALPACA_INDEX_CREDIT_CLOSE", false),
            profit_target_close_fraction: env_parse(
                "ALPACA_INDEX_CREDIT_PROFIT_TARGET_CLOSE_FRACTION",
                0.50_f64,
            ),
            stop_loss_close_multiple: env_parse(
                "ALPACA_INDEX_CREDIT_STOP_LOSS_CLOSE_MULTIPLE",
                2.0_f64,
            ),
            max_hold_secs: env_parse("ALPACA_INDEX_CREDIT_MAX_HOLD_SECS", 0_u64),
            expiration_exit_days: env_parse("ALPACA_INDEX_CREDIT_EXPIRATION_EXIT_DAYS", 1_i64),
            ignore_entry_window: env_bool("ALPACA_INDEX_PUT_CREDIT_IGNORE_WINDOW", false),
            entry_start: parse_time_env("ALPACA_INDEX_PUT_CREDIT_ENTRY_START", "09:45")?,
            entry_end: parse_time_env("ALPACA_INDEX_PUT_CREDIT_ENTRY_END", "14:30")?,
            entry_timezone: env::var("ALPACA_INDEX_PUT_CREDIT_ENTRY_TZ")
                .unwrap_or_else(|_| "America/New_York".to_string())
                .parse::<Tz>()?,
            state_path: env::var("ALPACA_INDEX_PUT_CREDIT_STATE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_state_path()),
            scanner: scanner_config_from_env(),
        })
    }

    /// Returns pure management thresholds for this runtime config.
    #[must_use]
    pub fn management_config(&self) -> CreditSpreadManagementConfig {
        CreditSpreadManagementConfig {
            force_flatten: self.force_flatten,
            profit_target_close_fraction: self.profit_target_close_fraction,
            stop_loss_close_multiple: self.stop_loss_close_multiple,
            max_hold_secs: self.max_hold_secs,
            expiration_exit_days: self.expiration_exit_days,
        }
    }
}

/// Selected strategy entry candidate.
#[derive(Clone, Debug)]
pub struct SelectedEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Credit-spread kind.
    pub kind: CreditSpreadKind,
    /// Scored spread candidate.
    pub candidate: SpreadCandidate,
}

/// Selects the best allowed index credit entry for one iteration.
///
/// # Errors
///
/// Returns an error when Alpaca account, position, order, contract, or snapshot requests fail.
pub async fn select_index_credit_entry(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &IndexCreditConfig,
    state: &StrategyState,
    trade_date: &str,
) -> anyhow::Result<Option<SelectedEntry>> {
    let account = client.account().await?;
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    let mut selected: Option<SelectedEntry> = None;

    for underlying in &config.underlyings {
        if state.has_submitted_underlying(trade_date, underlying) {
            println!("{underlying}: admission_rejected reason=daily_duplicate_state");
            continue;
        }

        for kind in &config.spread_kinds {
            let result = match kind {
                CreditSpreadKind::Put => {
                    scan_put_credit_underlying(client, data_config, &config.scanner, underlying)
                        .await?
                }
                CreditSpreadKind::Call => {
                    scan_call_credit_underlying(client, data_config, &config.scanner, underlying)
                        .await?
                }
            };
            let Some(best) = result.candidates.first() else {
                println!(
                    "{underlying}: no_candidate strategy={} contracts={} snapshots={} scoreable={}",
                    credit_spread_strategy_name(*kind),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
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
            if !admission.allowed {
                println!(
                    "{underlying}: admission_rejected strategy={} short={} long={} reasons={}",
                    credit_spread_strategy_name(*kind),
                    best.short.symbol,
                    best.long.symbol,
                    admission.reasons.join(" | "),
                );
                continue;
            }

            println!(
                "{underlying}: candidate strategy={} short={} long={} credit={:.2} ror={:.1}% score={:.1}",
                credit_spread_strategy_name(*kind),
                best.short.symbol,
                best.long.symbol,
                best.credit,
                best.return_on_risk * 100.0,
                best.score,
            );

            if selected
                .as_ref()
                .is_none_or(|current| best.score > current.candidate.score)
            {
                selected = Some(SelectedEntry {
                    underlying: underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                });
            }
        }
    }

    Ok(selected)
}

fn spread_kinds_from_env() -> anyhow::Result<Vec<CreditSpreadKind>> {
    let value = env::var("ALPACA_INDEX_CREDIT_STRATEGIES")
        .unwrap_or_else(|_| "put".to_string())
        .to_ascii_lowercase();
    let mut kinds = Vec::new();
    for raw in value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        match raw {
            "put" | "put_credit" | "index_put_credit_entry" => {
                kinds.push(CreditSpreadKind::Put);
            }
            "call" | "call_credit" | "index_call_credit_entry" => {
                kinds.push(CreditSpreadKind::Call);
            }
            "both" | "all" => {
                kinds.push(CreditSpreadKind::Put);
                kinds.push(CreditSpreadKind::Call);
            }
            other => anyhow::bail!("unsupported ALPACA_INDEX_CREDIT_STRATEGIES value {other}"),
        }
    }
    if kinds.is_empty() {
        kinds.push(CreditSpreadKind::Put);
    }
    kinds.sort_by_key(|kind| match kind {
        CreditSpreadKind::Put => 0,
        CreditSpreadKind::Call => 1,
    });
    kinds.dedup();
    Ok(kinds)
}

fn underlyings_from_env() -> Vec<String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if !args.is_empty() {
        return split_underlyings(args);
    }
    env::var("ALPACA_INDEX_PUT_CREDIT_UNDERLYINGS")
        .ok()
        .map(|value| split_underlyings([value]))
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| {
            ["SPY", "QQQ", "IWM", "DIA", "GLD"]
                .into_iter()
                .map(ToString::to_string)
                .collect()
        })
}

fn split_underlyings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn scanner_config_from_env() -> PutCreditScannerConfig {
    PutCreditScannerConfig {
        min_dte: env_parse("ALPACA_PUT_CREDIT_MIN_DTE", 5_i64),
        max_dte: env_parse("ALPACA_PUT_CREDIT_MAX_DTE", 10_i64),
        short_delta_min: env_parse("ALPACA_PUT_CREDIT_SHORT_DELTA_MIN", 0.18_f64),
        short_delta_max: env_parse("ALPACA_PUT_CREDIT_SHORT_DELTA_MAX", 0.28_f64),
        widths: env::var("ALPACA_PUT_CREDIT_WIDTHS")
            .ok()
            .and_then(|value| parse_csv_f64(&value))
            .unwrap_or_else(|| vec![2.0, 3.0, 5.0]),
        min_open_interest: env_parse("ALPACA_PUT_CREDIT_MIN_OPEN_INTEREST", 200_u64),
        max_leg_spread_pct: env_parse("ALPACA_PUT_CREDIT_MAX_LEG_SPREAD_PCT", 0.15_f64),
        min_return_on_risk: env_parse("ALPACA_PUT_CREDIT_MIN_RETURN_ON_RISK", 0.13_f64),
    }
}

fn parse_csv_f64(value: &str) -> Option<Vec<f64>> {
    let values = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!values.is_empty()).then_some(values)
}

fn parse_time_env(name: &str, default: &str) -> anyhow::Result<NaiveTime> {
    Ok(NaiveTime::parse_from_str(
        &env::var(name).unwrap_or_else(|_| default.to_string()),
        "%H:%M",
    )?)
}

fn default_state_path() -> PathBuf {
    if let Some(value) = env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(value)
            .join("nautilus_trader")
            .join("alpaca_index_put_credit_entry_state.json");
    }
    if let Some(value) = env::var_os("HOME") {
        return PathBuf::from(value)
            .join(".local")
            .join("state")
            .join("nautilus_trader")
            .join("alpaca_index_put_credit_entry_state.json");
    }
    PathBuf::from("alpaca_index_put_credit_entry_state.json")
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_underlyings_splits_args_and_csv() {
        assert_eq!(
            split_underlyings(["SPY, QQQ".to_string(), "IWM".to_string()]),
            vec!["SPY", "QQQ", "IWM"],
        );
    }

    #[test]
    fn parse_csv_f64_rejects_empty_or_invalid_values() {
        assert_eq!(parse_csv_f64("2,3,5"), Some(vec![2.0, 3.0, 5.0]));
        assert_eq!(parse_csv_f64(""), None);
        assert_eq!(parse_csv_f64("2,bad"), None);
    }
}
