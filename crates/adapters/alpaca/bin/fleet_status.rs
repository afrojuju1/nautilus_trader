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

//! Fleet status command for account-scoped Alpaca runtimes.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::Utc;
use nautilus_alpaca::runtime_env::configure_account_command_env;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(default)]
struct FleetConfig {
    fleet: FleetSection,
    accounts: Vec<AccountConfig>,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            fleet: FleetSection::default(),
            accounts: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FleetSection {
    operator_bin: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
struct AccountConfig {
    id: String,
    role: String,
    enabled: bool,
    service: String,
    env_file: PathBuf,
    config_file: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    lock_dir: Option<PathBuf>,
    permissions: AccountPermissions,
    risk_budget: RiskBudget,
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
            permissions: AccountPermissions::default(),
            risk_budget: RiskBudget::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct AccountPermissions {
    defined_risk: bool,
    long_premium: bool,
    undefined_risk: bool,
    naked_calls: bool,
    naked_puts: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct RiskBudget {
    max_active_entries: Option<usize>,
    max_buying_power_pct: Option<f64>,
    max_notional_risk: Option<f64>,
    max_delta_abs: Option<f64>,
    max_gamma_abs: Option<f64>,
    max_vega_abs: Option<f64>,
}

#[derive(Debug, Serialize)]
struct FleetStatus {
    checked_at_utc: String,
    registry_path: String,
    operator_bin: String,
    summary: FleetSummary,
    accounts: Vec<AccountFleetStatus>,
}

#[derive(Debug, Default, Serialize)]
struct FleetSummary {
    configured: usize,
    enabled: usize,
    checked: usize,
    ok: usize,
    disabled: usize,
    missing_env: usize,
    operator_errors: usize,
    broken: usize,
    open_orders: usize,
    positions: usize,
    unmanaged_positions: usize,
    active_entries: usize,
}

#[derive(Debug, Serialize)]
struct AccountFleetStatus {
    id: String,
    role: String,
    enabled: bool,
    service: String,
    env_file: String,
    config_file: Option<String>,
    log_dir: Option<String>,
    lock_dir: Option<String>,
    permissions: AccountPermissions,
    risk_budget: RiskBudget,
    status: AccountStatusKind,
    engine_state: Option<String>,
    open_orders: Option<usize>,
    positions: Option<usize>,
    unmanaged_positions: Option<usize>,
    active_entries: Option<usize>,
    alerts: Option<usize>,
    operator_status: Option<Value>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AccountStatusKind {
    Ok,
    Disabled,
    MissingEnv,
    OperatorError,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    let registry_path = args.registry_path();
    let registry_dir = registry_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let config = load_fleet_config(&registry_path)?;
    let operator_bin = config
        .fleet
        .operator_bin
        .as_deref()
        .map(|path| resolve_path(path, &registry_dir))
        .unwrap_or_else(default_operator_bin);

    let mut accounts = Vec::new();
    for account in &config.accounts {
        if !account.enabled && !args.include_disabled {
            continue;
        }
        accounts.push(check_account(account, &operator_bin, &registry_dir));
    }

    let summary = summarize(&config, &accounts);
    let failed = summary.missing_env > 0 || summary.operator_errors > 0 || summary.broken > 0;
    let status = FleetStatus {
        checked_at_utc: Utc::now().to_rfc3339(),
        registry_path: registry_path.display().to_string(),
        operator_bin: operator_bin.display().to_string(),
        summary,
        accounts,
    };

    if args.json_output {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_human_status(&status);
    }

    if failed {
        std::process::exit(1);
    }
    Ok(())
}

#[derive(Debug)]
struct Args {
    json_output: bool,
    include_disabled: bool,
    registry: Option<PathBuf>,
}

impl Args {
    fn parse() -> anyhow::Result<Self> {
        let mut json_output = false;
        let mut include_disabled = false;
        let mut registry = None;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--json" => json_output = true,
                "--include-disabled" => include_disabled = true,
                "--registry" => {
                    let Some(path) = args.next() else {
                        anyhow::bail!("--registry requires a path");
                    };
                    registry = Some(PathBuf::from(path));
                }
                "--help" | "-h" => {
                    println!(
                        "usage: alpaca-fleet-status [--json] [--include-disabled] [--registry PATH]"
                    );
                    std::process::exit(0);
                }
                other => anyhow::bail!("unsupported argument {other}"),
            }
        }
        Ok(Self {
            json_output,
            include_disabled,
            registry,
        })
    }

    fn registry_path(&self) -> PathBuf {
        self.registry
            .clone()
            .or_else(|| env::var_os("NAUTILUS_ALPACA_FLEET_CONFIG").map(PathBuf::from))
            .unwrap_or_else(default_registry_path)
    }
}

fn load_fleet_config(path: &Path) -> anyhow::Result<FleetConfig> {
    let raw = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!("failed to read fleet config {}: {error}", path.display())
    })?;
    let config: FleetConfig = toml::from_str(&raw)
        .map_err(|error| anyhow::anyhow!("invalid fleet config {}: {error}", path.display()))?;
    validate_config(&config)?;
    Ok(config)
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

fn check_account(
    account: &AccountConfig,
    operator_bin: &Path,
    registry_dir: &Path,
) -> AccountFleetStatus {
    let env_file = resolve_path(&account.env_file, registry_dir);
    let config_file = account
        .config_file
        .as_deref()
        .map(|path| resolve_path(path, registry_dir));
    let log_dir = account
        .log_dir
        .as_deref()
        .map(|path| resolve_path(path, registry_dir));
    let lock_dir = account
        .lock_dir
        .as_deref()
        .map(|path| resolve_path(path, registry_dir));

    if !account.enabled {
        return account_status(
            account,
            &env_file,
            config_file.as_deref(),
            log_dir.as_deref(),
            lock_dir.as_deref(),
            AccountStatusKind::Disabled,
            None,
            Some("account disabled in fleet registry".to_string()),
        );
    }
    if !env_file.exists() {
        return account_status(
            account,
            &env_file,
            config_file.as_deref(),
            log_dir.as_deref(),
            lock_dir.as_deref(),
            AccountStatusKind::MissingEnv,
            None,
            Some(format!("missing env file {}", env_file.display())),
        );
    }

    let mut command = Command::new(operator_bin);
    if let Err(error) = configure_account_command_env(&mut command, &env_file) {
        return account_status(
            account,
            &env_file,
            config_file.as_deref(),
            log_dir.as_deref(),
            lock_dir.as_deref(),
            AccountStatusKind::OperatorError,
            None,
            Some(format!("failed to prepare account env: {error}")),
        );
    }
    command
        .arg("--json")
        .env("NAUTILUS_ALPACA_SERVICE", &account.service);
    if let Some(config_file) = &config_file {
        command.env("ALPACA_CONFIG_PATH", config_file);
    }
    if let Some(log_dir) = &log_dir {
        command.env("NAUTILUS_ALPACA_LOG_DIR", log_dir);
    }
    if let Some(lock_dir) = &lock_dir {
        command.env("NAUTILUS_ALPACA_LOCK_DIR", lock_dir);
    }

    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            return account_status(
                account,
                &env_file,
                config_file.as_deref(),
                log_dir.as_deref(),
                lock_dir.as_deref(),
                AccountStatusKind::OperatorError,
                None,
                Some(format!(
                    "failed to run operator {}: {error}",
                    operator_bin.display(),
                )),
            );
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match serde_json::from_str::<Value>(&stdout) {
        Ok(value) => {
            let engine_state = value
                .get("engine_state")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let status = if output.status.success() {
                AccountStatusKind::Ok
            } else {
                AccountStatusKind::OperatorError
            };
            account_status(
                account,
                &env_file,
                config_file.as_deref(),
                log_dir.as_deref(),
                lock_dir.as_deref(),
                status,
                Some(value),
                (!output.status.success()).then(|| {
                    format!(
                        "operator exited with status {}{}",
                        output.status,
                        optional_stderr(&stderr),
                    )
                }),
            )
            .with_engine_state(engine_state)
        }
        Err(error) => account_status(
            account,
            &env_file,
            config_file.as_deref(),
            log_dir.as_deref(),
            lock_dir.as_deref(),
            AccountStatusKind::OperatorError,
            None,
            Some(format!(
                "failed to parse operator JSON: {error}; stdout={}{}",
                stdout.trim(),
                optional_stderr(&stderr),
            )),
        ),
    }
}

fn account_status(
    account: &AccountConfig,
    env_file: &Path,
    config_file: Option<&Path>,
    log_dir: Option<&Path>,
    lock_dir: Option<&Path>,
    status: AccountStatusKind,
    operator_status: Option<Value>,
    error: Option<String>,
) -> AccountFleetStatus {
    let open_orders = nested_usize(operator_status.as_ref(), &["orders", "open"]);
    let positions = nested_usize(operator_status.as_ref(), &["positions", "total"]);
    let unmanaged_positions = nested_usize(operator_status.as_ref(), &["positions", "unmanaged"]);
    let active_entries = nested_usize(
        operator_status.as_ref(),
        &["strategy_state", "active_entries"],
    );
    let alerts = operator_status
        .as_ref()
        .and_then(|value| value.get("alerts"))
        .and_then(Value::as_array)
        .map(Vec::len);
    let engine_state = operator_status
        .as_ref()
        .and_then(|value| value.get("engine_state"))
        .and_then(Value::as_str)
        .map(ToString::to_string);

    AccountFleetStatus {
        id: account.id.clone(),
        role: account.role.clone(),
        enabled: account.enabled,
        service: account.service.clone(),
        env_file: env_file.display().to_string(),
        config_file: config_file.map(|path| path.display().to_string()),
        log_dir: log_dir.map(|path| path.display().to_string()),
        lock_dir: lock_dir.map(|path| path.display().to_string()),
        permissions: account.permissions.clone(),
        risk_budget: account.risk_budget.clone(),
        status,
        engine_state,
        open_orders,
        positions,
        unmanaged_positions,
        active_entries,
        alerts,
        operator_status,
        error,
    }
}

impl AccountFleetStatus {
    fn with_engine_state(mut self, engine_state: Option<String>) -> Self {
        self.engine_state = engine_state;
        self
    }
}

fn summarize(config: &FleetConfig, accounts: &[AccountFleetStatus]) -> FleetSummary {
    let mut summary = FleetSummary {
        configured: config.accounts.len(),
        enabled: config
            .accounts
            .iter()
            .filter(|account| account.enabled)
            .count(),
        checked: accounts.len(),
        ..FleetSummary::default()
    };
    for account in accounts {
        match account.status {
            AccountStatusKind::Ok => summary.ok += 1,
            AccountStatusKind::Disabled => summary.disabled += 1,
            AccountStatusKind::MissingEnv => summary.missing_env += 1,
            AccountStatusKind::OperatorError => summary.operator_errors += 1,
        }
        if account.engine_state.as_deref() == Some("broken") {
            summary.broken += 1;
        }
        summary.open_orders += account.open_orders.unwrap_or(0);
        summary.positions += account.positions.unwrap_or(0);
        summary.unmanaged_positions += account.unmanaged_positions.unwrap_or(0);
        summary.active_entries += account.active_entries.unwrap_or(0);
    }
    summary
}

fn print_human_status(status: &FleetStatus) {
    println!(
        "fleet: checked_at_utc={} registry={} operator={}",
        status.checked_at_utc, status.registry_path, status.operator_bin,
    );
    println!(
        "summary: configured={} enabled={} checked={} ok={} disabled={} missing_env={} operator_errors={} broken={} open_orders={} positions={} unmanaged_positions={} active_entries={}",
        status.summary.configured,
        status.summary.enabled,
        status.summary.checked,
        status.summary.ok,
        status.summary.disabled,
        status.summary.missing_env,
        status.summary.operator_errors,
        status.summary.broken,
        status.summary.open_orders,
        status.summary.positions,
        status.summary.unmanaged_positions,
        status.summary.active_entries,
    );
    for account in &status.accounts {
        println!(
            "account: id={} role={} enabled={} status={:?} engine={} service={} open_orders={} positions={} unmanaged={} active_entries={} alerts={}",
            account.id,
            account.role,
            account.enabled,
            account.status,
            account.engine_state.as_deref().unwrap_or("unknown"),
            account.service,
            format_optional_usize(account.open_orders),
            format_optional_usize(account.positions),
            format_optional_usize(account.unmanaged_positions),
            format_optional_usize(account.active_entries),
            format_optional_usize(account.alerts),
        );
        if let Some(error) = account.error.as_deref() {
            println!("account_error: id={} error={}", account.id, error);
        }
    }
}

fn nested_usize(value: Option<&Value>, path: &[&str]) -> Option<usize> {
    let mut current = value?;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
}

fn resolve_path(path: &Path, base_dir: &Path) -> PathBuf {
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

fn default_registry_path() -> PathBuf {
    default_config_home()
        .join("nautilus-trader")
        .join("alpaca")
        .join("fleet.toml")
}

fn default_operator_bin() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("alpaca-operator-status"))
        })
        .filter(|path| path.exists())
        .unwrap_or_else(|| home_dir().join(".local/bin/alpaca-operator-status"))
}

fn default_config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME").map_or_else(|| home_dir().join(".config"), PathBuf::from)
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn optional_stderr(stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        String::new()
    } else {
        format!("; stderr={stderr}")
    }
}

fn format_optional_usize(value: Option<usize>) -> String {
    value.map_or_else(|| "unknown".to_string(), |value| value.to_string())
}
