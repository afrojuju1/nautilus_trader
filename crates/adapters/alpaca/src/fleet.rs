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

//! Account fleet registry parsing and account-boundary policy helpers.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::runtime::{StrategyState, load_strategy_state};

/// Fleet registry config.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct FleetConfig {
    /// Fleet-level metadata and caps.
    pub fleet: FleetSection,
    /// Configured Alpaca accounts.
    pub accounts: Vec<AccountConfig>,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            fleet: FleetSection::default(),
            accounts: Vec::new(),
        }
    }
}

/// Fleet-level metadata and caps.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FleetSection {
    /// Operator status binary path.
    pub operator_bin: Option<PathBuf>,
    /// Fleet-wide kill switch for new entries.
    pub kill_switch: bool,
    /// Maximum active entries across enabled accounts.
    pub max_active_entries: Option<usize>,
    /// Maximum active entries for one underlying across enabled accounts.
    pub max_active_entries_per_underlying: Option<usize>,
    /// Maximum active entries for one sector/correlation group across enabled accounts.
    pub max_active_entries_per_sector: Option<usize>,
    /// Underlying to sector/correlation-group mapping.
    pub sectors: BTreeMap<String, String>,
}

/// One configured account in the Alpaca fleet.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AccountConfig {
    /// Account registry ID.
    pub id: String,
    /// Operator-defined role.
    pub role: String,
    /// Whether this account may be checked and started intentionally.
    pub enabled: bool,
    /// User systemd service name.
    pub service: String,
    /// Account env file.
    pub env_file: PathBuf,
    /// Strategy config file.
    pub config_file: Option<PathBuf>,
    /// Account log directory.
    pub log_dir: Option<PathBuf>,
    /// Account lock directory.
    pub lock_dir: Option<PathBuf>,
    /// Account strategy-state file.
    pub state_path: Option<PathBuf>,
    /// Strategy permissions.
    pub permissions: AccountPermissions,
    /// Account risk budget.
    pub risk_budget: RiskBudget,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            role: "unspecified".to_string(),
            enabled: true,
            service: String::new(),
            env_file: PathBuf::new(),
            config_file: None,
            log_dir: None,
            lock_dir: None,
            state_path: None,
            permissions: AccountPermissions::default(),
            risk_budget: RiskBudget::default(),
        }
    }
}

/// Account-level strategy permissions.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AccountPermissions {
    /// Defined-risk spread strategies.
    pub defined_risk: bool,
    /// Long-premium spread strategies.
    pub long_premium: bool,
    /// Undefined-risk short premium strategies.
    pub undefined_risk: bool,
    /// Naked call strategies.
    pub naked_calls: bool,
    /// Naked put strategies.
    pub naked_puts: bool,
}

/// Account risk budget metadata.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RiskBudget {
    /// Maximum active entries.
    pub max_active_entries: Option<usize>,
    /// Maximum account buying power fraction.
    pub max_buying_power_pct: Option<f64>,
    /// Maximum notional risk.
    pub max_notional_risk: Option<f64>,
    /// Maximum absolute delta.
    pub max_delta_abs: Option<f64>,
    /// Maximum absolute gamma.
    pub max_gamma_abs: Option<f64>,
    /// Maximum absolute vega.
    pub max_vega_abs: Option<f64>,
}

/// Fleet state counts used by options admission and operator status.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FleetExposure {
    /// Active entries across enabled accounts.
    pub active_entries: usize,
    /// Active entries by underlying across enabled accounts.
    pub active_entries_by_underlying: BTreeMap<String, usize>,
    /// Active entries by sector/correlation group across enabled accounts.
    pub active_entries_by_sector: BTreeMap<String, usize>,
}

/// Loaded fleet config with resolved account paths.
#[derive(Clone, Debug)]
pub struct ResolvedFleetConfig {
    /// Registry file path.
    pub path: PathBuf,
    /// Registry directory.
    pub registry_dir: PathBuf,
    /// Fleet config.
    pub config: FleetConfig,
}

impl ResolvedFleetConfig {
    /// Returns the resolved operator binary path, if configured.
    #[must_use]
    pub fn operator_bin(&self) -> Option<PathBuf> {
        self.config
            .fleet
            .operator_bin
            .as_deref()
            .map(|path| resolve_path(path, &self.registry_dir))
    }

    /// Returns all enabled accounts.
    pub fn enabled_accounts(&self) -> impl Iterator<Item = &AccountConfig> {
        self.config
            .accounts
            .iter()
            .filter(|account| account.enabled)
    }

    /// Resolves an account env file.
    #[must_use]
    pub fn env_file(&self, account: &AccountConfig) -> PathBuf {
        resolve_path(&account.env_file, &self.registry_dir)
    }

    /// Resolves an optional account config file.
    #[must_use]
    pub fn config_file(&self, account: &AccountConfig) -> Option<PathBuf> {
        account
            .config_file
            .as_deref()
            .map(|path| resolve_path(path, &self.registry_dir))
    }

    /// Resolves an optional account log directory.
    #[must_use]
    pub fn log_dir(&self, account: &AccountConfig) -> Option<PathBuf> {
        account
            .log_dir
            .as_deref()
            .map(|path| resolve_path(path, &self.registry_dir))
    }

    /// Resolves an optional account lock directory.
    #[must_use]
    pub fn lock_dir(&self, account: &AccountConfig) -> Option<PathBuf> {
        account
            .lock_dir
            .as_deref()
            .map(|path| resolve_path(path, &self.registry_dir))
    }

    /// Resolves an optional account strategy-state path.
    #[must_use]
    pub fn state_path(&self, account: &AccountConfig) -> Option<PathBuf> {
        account
            .state_path
            .as_deref()
            .map(|path| resolve_path(path, &self.registry_dir))
    }

    /// Returns the account matching the current process environment.
    #[must_use]
    pub fn current_account(&self) -> Option<&AccountConfig> {
        let account_id = env::var("NAUTILUS_ALPACA_ACCOUNT").ok();
        if let Some(account_id) = account_id.as_deref()
            && let Some(account) = self
                .config
                .accounts
                .iter()
                .find(|account| account.id == account_id)
        {
            return Some(account);
        }

        let service = env::var("NAUTILUS_ALPACA_SERVICE").ok();
        if let Some(service) = service.as_deref()
            && let Some(account) = self
                .config
                .accounts
                .iter()
                .find(|account| account.service == service)
        {
            return Some(account);
        }

        let config_file = env::var_os("ALPACA_CONFIG_PATH").map(PathBuf::from);
        if let Some(config_file) = config_file.as_deref()
            && let Some(account) = self
                .config
                .accounts
                .iter()
                .find(|account| self.config_file(account).as_deref() == Some(config_file))
        {
            return Some(account);
        }

        let env_file = env::var_os("NAUTILUS_ALPACA_ENV_FILE").map(PathBuf::from);
        env_file.as_deref().and_then(|env_file| {
            self.config
                .accounts
                .iter()
                .find(|account| self.env_file(account) == env_file)
        })
    }

    /// Computes fleet exposure from configured account strategy-state files.
    pub fn exposure(&self) -> FleetExposure {
        let mut exposure = FleetExposure::default();
        for account in self.enabled_accounts() {
            let Some(path) = self.state_path(account) else {
                continue;
            };
            let Ok(state) = load_strategy_state(&path) else {
                continue;
            };
            add_state_exposure(&mut exposure, &state, &self.config.fleet.sectors);
        }
        exposure
    }
}

/// Loads a fleet config from the default or env-configured path if it exists.
///
/// # Errors
///
/// Returns an error if an explicit fleet config path is missing/unreadable or invalid.
pub fn load_fleet_config_from_env() -> anyhow::Result<Option<ResolvedFleetConfig>> {
    let (path, explicit) = env::var_os("NAUTILUS_ALPACA_FLEET_CONFIG")
        .map(|path| (PathBuf::from(path), true))
        .unwrap_or_else(|| (default_registry_path(), false));
    if !path.exists() {
        if explicit {
            anyhow::bail!("missing Alpaca fleet config {}", path.display());
        }
        return Ok(None);
    }
    load_fleet_config(&path).map(Some)
}

/// Loads a fleet config from a path.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or validated.
pub fn load_fleet_config(path: &Path) -> anyhow::Result<ResolvedFleetConfig> {
    let raw = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!("failed to read fleet config {}: {error}", path.display())
    })?;
    let config: FleetConfig = toml::from_str(&raw)
        .map_err(|error| anyhow::anyhow!("invalid fleet config {}: {error}", path.display()))?;
    validate_config(&config)?;
    Ok(ResolvedFleetConfig {
        path: path.to_path_buf(),
        registry_dir: path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
        config,
    })
}

fn validate_config(config: &FleetConfig) -> anyhow::Result<()> {
    for account in &config.accounts {
        if account.id.trim().is_empty() {
            anyhow::bail!("fleet account id cannot be empty");
        }
        if account.service.trim().is_empty() {
            anyhow::bail!("fleet account {} service cannot be empty", account.id);
        }
        if account.env_file.as_os_str().is_empty() {
            anyhow::bail!("fleet account {} env_file cannot be empty", account.id);
        }
    }
    Ok(())
}

fn add_state_exposure(
    exposure: &mut FleetExposure,
    state: &StrategyState,
    sectors: &BTreeMap<String, String>,
) {
    for entry in state.entries.iter().filter(|entry| entry.is_active()) {
        exposure.active_entries += 1;
        let underlying = entry.underlying.to_ascii_uppercase();
        *exposure
            .active_entries_by_underlying
            .entry(underlying.clone())
            .or_insert(0) += 1;
        if let Some(sector) = sectors.get(&underlying) {
            *exposure
                .active_entries_by_sector
                .entry(sector.clone())
                .or_insert(0) += 1;
        }
    }
}

/// Resolves a path relative to a base directory.
#[must_use]
pub fn resolve_path(path: &Path, base_dir: &Path) -> PathBuf {
    let expanded = expand_home(path);
    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}

fn expand_home(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

/// Returns the default fleet registry path.
#[must_use]
pub fn default_registry_path() -> PathBuf {
    default_config_home()
        .join("nautilus-trader")
        .join("alpaca")
        .join("fleet.toml")
}

fn default_config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME").map_or_else(|| home_dir().join(".config"), PathBuf::from)
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_config_defaults_future_accounts_disabled() {
        let raw = r#"
            [fleet]
            kill_switch = true
            max_active_entries = 3

            [[accounts]]
            id = "paper-main"
            service = "alpaca-options.service"
            env_file = "/tmp/main.env"

            [accounts.permissions]
            defined_risk = true
        "#;
        let config: FleetConfig = toml::from_str(raw).unwrap();
        assert!(config.fleet.kill_switch);
        assert_eq!(config.fleet.max_active_entries, Some(3));
        assert!(config.accounts[0].enabled);
        assert!(config.accounts[0].permissions.defined_risk);
        assert!(!config.accounts[0].permissions.long_premium);
    }
}
