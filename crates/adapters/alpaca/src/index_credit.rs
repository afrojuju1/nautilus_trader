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

use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::NaiveTime;
use chrono_tz::Tz;
use serde::Deserialize;

use crate::{
    config::AlpacaDataClientConfig,
    execution::check_option_spread_entry_admission,
    http::{client::AlpacaHttpClient, models::ListOrdersRequest},
    management::CreditSpreadManagementConfig,
    runtime::{StrategyState, credit_spread_strategy_name},
    strategy::{
        CreditSpreadKind, IronCondorCandidate, IronCondorScannerConfig, PutCreditScannerConfig,
        SpreadCandidate, scan_call_credit_underlying, scan_iron_condor_underlying,
        scan_put_credit_underlying,
    },
};

/// Runtime config for the index credit account-engine slice.
#[derive(Debug)]
pub struct IndexCreditConfig {
    /// Underlyings to scan.
    pub underlyings: Vec<String>,
    /// Enabled spread kinds.
    pub spread_kinds: Vec<CreditSpreadKind>,
    /// Whether the index iron-condor strategy is enabled.
    pub iron_condor_enabled: bool,
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
    /// Iron-condor scanner config.
    pub iron_condor_scanner: IronCondorScannerConfig,
}

impl IndexCreditConfig {
    /// Builds config from TOML config, short environment overrides, and optional positional
    /// underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when config, strategy names, times, or timezone values are invalid.
    pub fn from_env() -> anyhow::Result<Self> {
        build_index_credit_config(
            load_runtime_config_file_from_env()?,
            env::args().skip(1).collect::<Vec<_>>(),
        )
    }

    /// Builds config without reading positional CLI underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when config, strategy names, times, or timezone values are invalid.
    pub fn from_runtime_env() -> anyhow::Result<Self> {
        build_index_credit_config(load_runtime_config_file_from_env()?, Vec::new())
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

    /// Returns enabled strategy names for operator logs.
    #[must_use]
    pub fn enabled_strategy_names(&self) -> Vec<&'static str> {
        let mut names = self
            .spread_kinds
            .iter()
            .map(|kind| credit_spread_strategy_name(*kind))
            .collect::<Vec<_>>();
        if self.iron_condor_enabled {
            names.push("index_iron_condor_entry");
        }
        names
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

/// Selected index iron-condor candidate.
#[derive(Clone, Debug)]
pub struct SelectedIronCondorEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Scored iron-condor candidate.
    pub candidate: IronCondorCandidate,
}

/// Selected index strategy candidate.
#[derive(Clone, Debug)]
pub enum SelectedIndexEntry {
    /// Two-leg credit spread.
    Credit(SelectedEntry),
    /// Four-leg iron condor.
    IronCondor(SelectedIronCondorEntry),
}

impl SelectedIndexEntry {
    /// Returns the scanner score.
    #[must_use]
    pub fn score(&self) -> f64 {
        match self {
            Self::Credit(entry) => entry.candidate.score,
            Self::IronCondor(entry) => entry.candidate.score,
        }
    }
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
    Ok(
        select_index_strategy_entry(client, data_config, config, state, trade_date)
            .await?
            .and_then(|entry| match entry {
                SelectedIndexEntry::Credit(entry) => Some(entry),
                SelectedIndexEntry::IronCondor(_) => None,
            }),
    )
}

/// Selects the best allowed index strategy entry for one iteration.
///
/// # Errors
///
/// Returns an error when Alpaca account, position, order, contract, or snapshot requests fail.
pub async fn select_index_strategy_entry(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &IndexCreditConfig,
    state: &StrategyState,
    trade_date: &str,
) -> anyhow::Result<Option<SelectedIndexEntry>> {
    let account = client.account().await?;
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    let mut selected: Option<SelectedIndexEntry> = None;

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

            let admission = check_option_spread_entry_admission(
                &account,
                &positions,
                &open_orders,
                &[&best.short.symbol, &best.long.symbol],
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
                .is_none_or(|current| best.score > current.score())
            {
                selected = Some(SelectedIndexEntry::Credit(SelectedEntry {
                    underlying: underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                }));
            }
        }

        if config.iron_condor_enabled {
            let result = scan_iron_condor_underlying(
                client,
                data_config,
                &config.iron_condor_scanner,
                underlying,
            )
            .await?;
            let Some(best) = result.candidates.first() else {
                println!(
                    "{underlying}: no_candidate strategy=index_iron_condor_entry contracts={} snapshots={} scoreable={}",
                    result.contract_count, result.snapshot_count, result.scoreable_count,
                );
                continue;
            };

            let admission = check_option_spread_entry_admission(
                &account,
                &positions,
                &open_orders,
                &[
                    &best.put.short.symbol,
                    &best.put.long.symbol,
                    &best.call.short.symbol,
                    &best.call.long.symbol,
                ],
            );
            if !admission.allowed {
                println!(
                    "{underlying}: admission_rejected strategy=index_iron_condor_entry short_put={} long_put={} short_call={} long_call={} reasons={}",
                    best.put.short.symbol,
                    best.put.long.symbol,
                    best.call.short.symbol,
                    best.call.long.symbol,
                    admission.reasons.join(" | "),
                );
                continue;
            }

            println!(
                "{underlying}: candidate strategy=index_iron_condor_entry short_put={} long_put={} short_call={} long_call={} credit={:.2} ror={:.1}% score={:.1}",
                best.put.short.symbol,
                best.put.long.symbol,
                best.call.short.symbol,
                best.call.long.symbol,
                best.credit,
                best.return_on_risk * 100.0,
                best.score,
            );

            if selected
                .as_ref()
                .is_none_or(|current| best.score > current.score())
            {
                selected = Some(SelectedIndexEntry::IronCondor(SelectedIronCondorEntry {
                    underlying: underlying.clone(),
                    candidate: best.clone(),
                }));
            }
        }
    }

    Ok(selected)
}

#[derive(Clone, Debug)]
struct StrategyConfig {
    credit_kinds: Vec<CreditSpreadKind>,
    iron_condor_enabled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RuntimeConfigFile {
    runtime: RuntimeSection,
    index: IndexSection,
    scanner: ScannerSection,
    iron_condor: IronCondorSection,
    management: ManagementSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RuntimeSection {
    strategies: Vec<String>,
    max_iterations: Option<u64>,
    interval_secs: Option<u64>,
    submit: Option<bool>,
    manage: Option<bool>,
    close: Option<bool>,
    kill_switch: Option<bool>,
    force_flatten: Option<bool>,
    cancel_after_accept: Option<bool>,
    ignore_entry_window: Option<bool>,
    state_path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IndexSection {
    underlyings: Vec<String>,
    quantity: Option<u64>,
    entry_start: Option<String>,
    entry_end: Option<String>,
    entry_timezone: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ScannerSection {
    min_dte: Option<i64>,
    max_dte: Option<i64>,
    short_delta_min: Option<f64>,
    short_delta_max: Option<f64>,
    widths: Option<Vec<f64>>,
    min_open_interest: Option<u64>,
    max_leg_spread_pct: Option<f64>,
    min_return_on_risk: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IronCondorSection {
    min_return_on_risk: Option<f64>,
    wing_min_return_on_risk: Option<f64>,
    require_equal_widths: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ManagementSection {
    stale_entry_secs: Option<u64>,
    profit_target_close_fraction: Option<f64>,
    stop_loss_close_multiple: Option<f64>,
    max_hold_secs: Option<u64>,
    expiration_exit_days: Option<i64>,
}

fn load_runtime_config_file_from_env() -> anyhow::Result<RuntimeConfigFile> {
    if let Some(path) = env::var_os("ALPACA_CONFIG_PATH") {
        let path = PathBuf::from(path);
        return load_runtime_config_file(&path, true);
    }

    let path = default_config_path();
    if path.exists() {
        load_runtime_config_file(&path, false)
    } else {
        Ok(RuntimeConfigFile::default())
    }
}

fn load_runtime_config_file(path: &Path, explicit: bool) -> anyhow::Result<RuntimeConfigFile> {
    match fs::read_to_string(path) {
        Ok(raw) => parse_runtime_config(&raw),
        Err(error) if !explicit && error.kind() == std::io::ErrorKind::NotFound => {
            Ok(RuntimeConfigFile::default())
        }
        Err(error) => {
            anyhow::bail!(
                "failed to read Alpaca runtime config {}: {error}",
                path.display()
            )
        }
    }
}

fn parse_runtime_config(raw: &str) -> anyhow::Result<RuntimeConfigFile> {
    toml::from_str(raw).map_err(|error| anyhow::anyhow!("invalid Alpaca runtime config: {error}"))
}

fn build_index_credit_config(
    file: RuntimeConfigFile,
    cli_underlyings: Vec<String>,
) -> anyhow::Result<IndexCreditConfig> {
    let strategy_values = env::var("ALPACA_STRATEGIES")
        .ok()
        .map(|value| split_strings([value]))
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| {
            if file.runtime.strategies.is_empty() {
                vec!["put".to_string()]
            } else {
                file.runtime.strategies.clone()
            }
        });
    let strategy_config = strategy_config_from_values(strategy_values)?;
    let scanner = scanner_config_from_file(&file.scanner);
    Ok(IndexCreditConfig {
        underlyings: underlyings_from_sources(cli_underlyings, &file.index),
        spread_kinds: strategy_config.credit_kinds,
        iron_condor_enabled: strategy_config.iron_condor_enabled,
        max_iterations: env_parse("ALPACA_MAX_ITERATIONS")
            .or(file.runtime.max_iterations)
            .unwrap_or(1),
        interval_secs: env_parse("ALPACA_INTERVAL_SECS")
            .or(file.runtime.interval_secs)
            .unwrap_or(300),
        quantity: env_parse("ALPACA_QTY").or(file.index.quantity).unwrap_or(1),
        submit_enabled: env_bool("ALPACA_SUBMIT")
            .or(file.runtime.submit)
            .unwrap_or(false),
        manage_enabled: env_bool("ALPACA_MANAGE")
            .or(file.runtime.manage)
            .unwrap_or(false),
        kill_switch: env_bool("ALPACA_KILL_SWITCH")
            .or(file.runtime.kill_switch)
            .unwrap_or(false),
        force_flatten: env_bool("ALPACA_FORCE_FLATTEN")
            .or(file.runtime.force_flatten)
            .unwrap_or(false),
        cancel_after_accept: env_bool("ALPACA_CANCEL_AFTER_ACCEPT")
            .or(file.runtime.cancel_after_accept)
            .unwrap_or(false),
        stale_entry_secs: file.management.stale_entry_secs.unwrap_or(900),
        close_enabled: env_bool("ALPACA_CLOSE")
            .or(file.runtime.close)
            .unwrap_or(false),
        profit_target_close_fraction: file.management.profit_target_close_fraction.unwrap_or(0.50),
        stop_loss_close_multiple: file.management.stop_loss_close_multiple.unwrap_or(2.0),
        max_hold_secs: file.management.max_hold_secs.unwrap_or(0),
        expiration_exit_days: file.management.expiration_exit_days.unwrap_or(1),
        ignore_entry_window: env_bool("ALPACA_IGNORE_ENTRY_WINDOW")
            .or(file.runtime.ignore_entry_window)
            .unwrap_or(false),
        entry_start: parse_time_value(file.index.entry_start.as_deref(), "09:45")?,
        entry_end: parse_time_value(file.index.entry_end.as_deref(), "14:30")?,
        entry_timezone: file
            .index
            .entry_timezone
            .as_deref()
            .unwrap_or("America/New_York")
            .parse::<Tz>()?,
        state_path: env::var("ALPACA_STATE_PATH")
            .map(PathBuf::from)
            .ok()
            .or(file.runtime.state_path)
            .unwrap_or_else(default_state_path),
        iron_condor_scanner: iron_condor_scanner_config_from_file(&scanner, &file.iron_condor),
        scanner,
    })
}

fn strategy_config_from_values(values: Vec<String>) -> anyhow::Result<StrategyConfig> {
    let mut kinds = Vec::new();
    let mut iron_condor_enabled = false;
    for raw in values
        .into_iter()
        .flat_map(|value| split_strings([value]))
        .map(|value| value.to_ascii_lowercase())
    {
        match raw.as_str() {
            "put" | "put_credit" | "index_put_credit_entry" => {
                kinds.push(CreditSpreadKind::Put);
            }
            "call" | "call_credit" | "index_call_credit_entry" => {
                kinds.push(CreditSpreadKind::Call);
            }
            "both" => {
                kinds.push(CreditSpreadKind::Put);
                kinds.push(CreditSpreadKind::Call);
            }
            "all" => {
                kinds.push(CreditSpreadKind::Put);
                kinds.push(CreditSpreadKind::Call);
                iron_condor_enabled = true;
            }
            "iron_condor" | "condor" | "index_iron_condor_entry" => {
                iron_condor_enabled = true;
            }
            other => anyhow::bail!("unsupported Alpaca strategy value {other}"),
        }
    }
    if kinds.is_empty() && !iron_condor_enabled {
        kinds.push(CreditSpreadKind::Put);
    }
    kinds.sort_by_key(|kind| match kind {
        CreditSpreadKind::Put => 0,
        CreditSpreadKind::Call => 1,
    });
    kinds.dedup();
    Ok(StrategyConfig {
        credit_kinds: kinds,
        iron_condor_enabled,
    })
}

fn underlyings_from_sources(cli_underlyings: Vec<String>, config: &IndexSection) -> Vec<String> {
    let args = split_strings(cli_underlyings);
    if !args.is_empty() {
        return args;
    }
    let configured = split_strings(config.underlyings.clone());
    if !configured.is_empty() {
        return configured;
    }
    default_underlyings()
}

fn split_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
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

fn default_underlyings() -> Vec<String> {
    ["SPY", "QQQ", "IWM", "DIA", "GLD"]
        .into_iter()
        .map(ToString::to_string)
        .collect()
}

fn scanner_config_from_file(config: &ScannerSection) -> PutCreditScannerConfig {
    PutCreditScannerConfig {
        min_dte: config.min_dte.unwrap_or(5),
        max_dte: config.max_dte.unwrap_or(10),
        short_delta_min: config.short_delta_min.unwrap_or(0.18),
        short_delta_max: config.short_delta_max.unwrap_or(0.28),
        widths: config.widths.clone().unwrap_or_else(|| vec![2.0, 3.0, 5.0]),
        min_open_interest: config.min_open_interest.unwrap_or(200),
        max_leg_spread_pct: config.max_leg_spread_pct.unwrap_or(0.15),
        min_return_on_risk: config.min_return_on_risk.unwrap_or(0.13),
    }
}

fn iron_condor_scanner_config_from_file(
    scanner: &PutCreditScannerConfig,
    config: &IronCondorSection,
) -> IronCondorScannerConfig {
    let mut credit = scanner.clone();
    credit.min_return_on_risk = config.wing_min_return_on_risk.unwrap_or(0.10);
    IronCondorScannerConfig {
        credit,
        min_return_on_risk: config.min_return_on_risk.unwrap_or(0.18),
        require_equal_widths: config.require_equal_widths.unwrap_or(true),
    }
}

fn parse_time_value(value: Option<&str>, default: &str) -> anyhow::Result<NaiveTime> {
    Ok(NaiveTime::parse_from_str(
        value.unwrap_or(default),
        "%H:%M",
    )?)
}

fn default_config_path() -> PathBuf {
    if let Some(value) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(value)
            .join("nautilus-trader")
            .join("alpaca")
            .join("index-credit.toml");
    }
    if let Some(value) = env::var_os("HOME") {
        return PathBuf::from(value)
            .join(".config")
            .join("nautilus-trader")
            .join("alpaca")
            .join("index-credit.toml");
    }
    PathBuf::from("index-credit.toml")
}

fn default_state_path() -> PathBuf {
    if let Some(value) = env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(value)
            .join("nautilus_trader")
            .join("alpaca_index_credit_state.json");
    }
    if let Some(value) = env::var_os("HOME") {
        return PathBuf::from(value)
            .join(".local")
            .join("state")
            .join("nautilus_trader")
            .join("alpaca_index_credit_state.json");
    }
    PathBuf::from("alpaca_index_credit_state.json")
}

fn env_bool(name: &str) -> Option<bool> {
    env::var(name).ok().and_then(|value| {
        let value = value.trim();
        if matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON") {
            Some(true)
        } else if matches!(value, "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF") {
            Some(false)
        } else {
            None
        }
    })
}

fn env_parse<T>(name: &str) -> Option<T>
where
    T: FromStr,
{
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<T>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_strings_splits_args_and_csv() {
        assert_eq!(
            split_strings(["SPY, QQQ".to_string(), "IWM".to_string()]),
            vec!["SPY", "QQQ", "IWM"],
        );
    }

    #[test]
    fn parse_runtime_config_reads_strategy_scanner_and_management_sections() {
        let config = parse_runtime_config(
            r#"
[runtime]
strategies = ["put", "iron_condor"]
max_iterations = 0
submit = false
manage = true
close = true
kill_switch = true
state_path = "/tmp/alpaca-state.json"

[index]
underlyings = ["SPY", "QQQ"]
quantity = 2
entry_start = "09:45"
entry_end = "14:30"
entry_timezone = "America/New_York"

[scanner]
min_dte = 7
max_dte = 14
short_delta_min = 0.18
short_delta_max = 0.28
widths = [2.0, 5.0]
min_open_interest = 300
max_leg_spread_pct = 0.10
min_return_on_risk = 0.15

[iron_condor]
min_return_on_risk = 0.20
wing_min_return_on_risk = 0.11
require_equal_widths = true

[management]
stale_entry_secs = 600
profit_target_close_fraction = 0.45
stop_loss_close_multiple = 1.8
max_hold_secs = 3600
expiration_exit_days = 2
"#,
        )
        .unwrap();

        assert_eq!(config.runtime.strategies, vec!["put", "iron_condor"]);
        assert_eq!(config.index.underlyings, vec!["SPY", "QQQ"]);
        assert_eq!(config.scanner.widths, Some(vec![2.0, 5.0]));
        assert_eq!(config.iron_condor.min_return_on_risk, Some(0.20));
        assert_eq!(config.management.stale_entry_secs, Some(600));
    }

    #[test]
    fn strategy_config_accepts_combined_four_leg_strategy() {
        let config = strategy_config_from_values(vec!["both,iron_condor".to_string()]).unwrap();

        assert_eq!(
            config.credit_kinds,
            vec![CreditSpreadKind::Put, CreditSpreadKind::Call],
        );
        assert!(config.iron_condor_enabled);
    }
}
