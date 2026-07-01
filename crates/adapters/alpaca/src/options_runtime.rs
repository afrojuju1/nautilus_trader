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

//! Alpaca options runtime configuration and candidate selection.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use chrono::NaiveTime;
use chrono_tz::Tz;
use nautilus_infrastructure::sql::operational::{self, OperationalRepository};
use nautilus_trading::options::{
    candidates::{
        CreditSpreadKind, DebitSpreadKind, DebitSpreadScannerConfig, IronCondorScannerConfig,
        NakedOptionCapitalContext, NakedOptionKind, NakedOptionScannerConfig,
        PutCreditScannerConfig, annualized_premium_yield,
    },
    entries::{
        credit_spread_strategy_name, debit_spread_strategy_name, naked_option_strategy_name,
    },
};
use serde_json::{Map, Value, json};

use crate::{
    config::AlpacaDataClientConfig,
    earnings::EarningsEvent,
    fleet::ResolvedFleetConfig,
    http::client::AlpacaHttpClient,
    options_lifecycle::OptionLifecycleRiskConfig,
    options_management::CreditSpreadManagementConfig,
    runtime::{StrategyState, emit_operator_event},
    strategy::{
        scan_call_credit_underlying, scan_call_debit_underlying, scan_iron_condor_underlying,
        scan_naked_option_underlying_with_capital, scan_put_credit_underlying,
        scan_put_debit_underlying,
    },
};

pub use crate::candidate_payloads::{
    candidate_alert_identity_key, candidate_alert_key, credit_candidate_ledger_payload,
    debit_candidate_ledger_payload, insert_string_field, insert_value_field,
    iron_condor_candidate_ledger_payload, naked_candidate_ledger_payload,
};
pub use nautilus_trading::options::entries::{
    OptionEntryDescriptor, SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry,
    SelectedNakedOptionEntry, SelectedOptionsEntry,
};

#[derive(Clone, Copy)]
enum OperationalStoreConnectionMode {
    ApplyMigrations,
    ReadOnly,
}

mod candidate_ledger;
mod config;

use candidate_ledger::{
    record_credit_candidate_ledger, record_debit_candidate_ledger,
    record_iron_condor_candidate_ledger, record_naked_candidate_ledger,
    record_scanner_ledger_result,
};
use config::{
    account_options_buying_power, format_optional_pct, format_rejection_counts, no_candidate_reason,
};
pub(crate) use config::{active_sector_count, active_underlying_count};
use config::{build_options_runtime_config, load_runtime_config_file_from_env};

const HIGH_SCORE_CANDIDATE_ALERT: &str = "high_score_candidate";
const CANDIDATE_ALERT_NAKED_MIN_SCORE: f64 = 95.0;
const CANDIDATE_ALERT_NAKED_ONE_TO_THREE_DTE_MIN_SCORE: f64 = 100.0;
const CANDIDATE_ALERT_IRON_CONDOR_MIN_SCORE: f64 = 80.0;
const CANDIDATE_ALERT_CREDIT_MIN_SCORE: f64 = 80.0;
const CANDIDATE_ALERT_DEBIT_MIN_SCORE: f64 = 80.0;

/// Runtime config for the Alpaca options slice.
#[derive(Debug)]
pub struct AlpacaOptionsRuntimeConfig {
    /// Underlyings to scan.
    pub underlyings: Vec<String>,
    /// Resolved strategy profiles that define scan composition.
    pub strategy_profiles: Vec<AlpacaOptionsStrategyProfile>,
    /// Maximum active strategy entries. `None` means unlimited.
    pub max_active_entries: Option<usize>,
    /// Maximum accepted strategy submissions for one trade date. `None` means unlimited.
    pub max_daily_submits: Option<usize>,
    /// Maximum working broker orders before new entries are blocked. `None` means unlimited.
    pub max_open_orders: Option<usize>,
    /// Maximum active entries for one underlying. `None` means unlimited.
    pub max_active_entries_per_underlying: Option<usize>,
    /// Maximum active entries for one configured sector/correlation group. `None` means unlimited.
    pub max_active_entries_per_sector: Option<usize>,
    /// Maximum risk-capital estimate for one selected entry in USD. `None` means unlimited.
    pub max_single_entry_risk_capital_usd: Option<f64>,
    /// Maximum active plus selected portfolio risk-capital estimate in USD. `None` means unlimited.
    pub max_portfolio_risk_capital_usd: Option<f64>,
    /// Whether configured risk-capital limits block entries when risk cannot be estimated.
    pub block_unestimated_risk_capital: bool,
    /// Approved earnings events used by the event-shock admission guard.
    pub event_shock_earnings_events: Vec<EarningsEvent>,
    /// Calendar days before an earnings report to block new entries.
    pub event_shock_block_days_before_earnings: i64,
    /// Calendar days after an earnings report to block new entries.
    pub event_shock_block_days_after_earnings: i64,
    /// Underlying to sector/correlation-group mapping.
    pub sectors: BTreeMap<String, String>,
    /// Maximum loop iterations. Zero means run continuously.
    pub max_iterations: u64,
    /// Delay between iterations.
    pub interval_secs: u64,
    /// Strategy quantity.
    pub quantity: u64,
    /// Whether broker orders which open new risk are enabled.
    pub open_orders_enabled: bool,
    /// Whether every active entry should be flattened.
    pub force_flatten: bool,
    /// Whether accepted smoke orders should be canceled.
    pub cancel_after_accept: bool,
    /// Stale entry timeout.
    pub stale_entry_secs: u64,
    /// Stale close timeout.
    pub stale_close_secs: u64,
    /// Whether broker orders which close or reduce risk are enabled.
    pub close_orders_enabled: bool,
    /// Whether non-forced close submissions are limited to regular options hours.
    pub close_regular_hours_only: bool,
    /// Close window start.
    pub close_start: NaiveTime,
    /// Close window end.
    pub close_end: NaiveTime,
    /// Additional debit allowed on submitted close limits.
    pub close_price_cushion: f64,
    /// Additional close cushion added per accepted close attempt.
    pub close_reprice_step: f64,
    /// Maximum total close cushion after repricing steps.
    pub max_close_price_cushion: f64,
    /// Maximum accepted close submissions per entry. Zero means unlimited.
    pub max_close_attempts: u32,
    /// Minimum delay after a close submission before another close may be submitted.
    pub close_reprice_cooldown_secs: u64,
    /// Number of high-rank candidate entries to keep quote-subscribed for active risk checks.
    pub active_risk_candidate_quote_limit: usize,
    /// Maximum accepted active-risk quote age. Zero disables freshness blocks.
    pub active_risk_quote_stale_secs: u64,
    /// Profit-target close fraction.
    pub profit_target_close_fraction: f64,
    /// Stop-loss close multiple.
    pub stop_loss_close_multiple: f64,
    /// Maximum hold time. Zero disables.
    pub max_hold_secs: u64,
    /// Expiration-risk exit days. Negative disables.
    pub expiration_exit_days: i64,
    /// Lifecycle account-activity poll interval. Zero disables background polling.
    pub lifecycle_poll_secs: u64,
    /// Lifecycle account-activity lookback.
    pub lifecycle_activity_lookback_hours: u64,
    /// Recent assignment/exercise block window.
    pub lifecycle_activity_block_hours: u64,
    /// Calendar DTE threshold that blocks new entries. Negative disables.
    pub expiration_entry_block_days: i64,
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
    /// Whether scanner evidence should be written to the candidate ledger.
    pub candidate_ledger_enabled: bool,
    /// Maximum ranked candidates to write per scanner result. `0` means all candidates.
    pub candidate_ledger_max_candidates: usize,
    /// Credit scanner config.
    pub scanner: PutCreditScannerConfig,
    /// Iron-condor scanner config.
    pub iron_condor_scanner: IronCondorScannerConfig,
    /// Debit-spread scanner config.
    pub debit_scanner: DebitSpreadScannerConfig,
    /// Naked-option scanner config.
    pub naked_scanner: NakedOptionScannerConfig,
    /// 1-3 DTE naked-option scanner config.
    pub naked_1_3dte_scanner: NakedOptionScannerConfig,
    /// Loaded fleet registry, if configured.
    pub fleet: Option<ResolvedFleetConfig>,
    /// Current fleet account ID, if this runtime matched one.
    pub fleet_account_id: Option<String>,
    /// Fleet policy blocks applied to this runtime.
    pub fleet_policy_blocks: Vec<String>,
    /// Strategy-state and ledger operational repository.
    pub operational_repository: Option<Arc<OperationalRepository>>,
    /// Postgres database URL used for operational persistence.
    pub operational_database_url: Option<String>,
    /// Postgres schema for operational persistence tables.
    pub operational_schema: String,
    /// Optional account ID override for persisted records.
    pub operational_account_id: Option<String>,
}

/// Runtime strategy profile mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlpacaOptionsStrategyMode {
    /// Candidate can submit broker orders when account-level order gates allow it.
    Live,
    /// Candidate scans and records evidence but cannot submit opening broker orders.
    DryRun,
}

impl AlpacaOptionsStrategyMode {
    /// Returns the stable config/status label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::DryRun => "dry_run",
        }
    }
}

impl FromStr for AlpacaOptionsStrategyMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "live" | "enabled" | "submit" => Ok(Self::Live),
            "dry_run" | "dry-run" | "watchlist" | "paper_watch" => Ok(Self::DryRun),
            other => Err(format!("unsupported Alpaca strategy mode {other}")),
        }
    }
}

/// Stable strategy family for one Alpaca options profile.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AlpacaOptionsStrategyFamily {
    /// Put credit spread.
    PutCredit,
    /// Call credit spread.
    CallCredit,
    /// Iron condor.
    IronCondor,
    /// Put debit spread.
    PutDebit,
    /// Call debit spread.
    CallDebit,
    /// Naked short put.
    NakedPut,
    /// Naked short call.
    NakedCall,
    /// 1-3 DTE naked short put.
    NakedPutOneToThreeDte,
    /// 1-3 DTE naked short call.
    NakedCallOneToThreeDte,
}

impl AlpacaOptionsStrategyFamily {
    /// Returns the stable strategy label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PutCredit => "put_credit",
            Self::CallCredit => "call_credit",
            Self::IronCondor => "iron_condor",
            Self::PutDebit => "put_debit",
            Self::CallDebit => "call_debit",
            Self::NakedPut => "naked_put",
            Self::NakedCall => "naked_call",
            Self::NakedPutOneToThreeDte => "naked_put_1_3dte",
            Self::NakedCallOneToThreeDte => "naked_call_1_3dte",
        }
    }
}

impl FromStr for AlpacaOptionsStrategyFamily {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "put" | "put_credit" => Ok(Self::PutCredit),
            "call" | "call_credit" => Ok(Self::CallCredit),
            "iron_condor" | "condor" => Ok(Self::IronCondor),
            "put_debit" | "earnings_put_debit_entry" => Ok(Self::PutDebit),
            "call_debit" | "earnings_call_debit_entry" => Ok(Self::CallDebit),
            "naked_put" | "short_put" => Ok(Self::NakedPut),
            "naked_call" | "short_call" => Ok(Self::NakedCall),
            "naked_put_1_3dte" | "short_put_1_3dte" => Ok(Self::NakedPutOneToThreeDte),
            "naked_call_1_3dte" | "short_call_1_3dte" => Ok(Self::NakedCallOneToThreeDte),
            other => Err(format!("unsupported Alpaca strategy family {other}")),
        }
    }
}

/// Resolved scanner config for one Alpaca options strategy profile.
#[derive(Clone, Debug, PartialEq)]
pub enum AlpacaOptionsStrategyScannerConfig {
    /// Credit-spread scanner config.
    Credit(PutCreditScannerConfig),
    /// Iron-condor scanner config.
    IronCondor(IronCondorScannerConfig),
    /// Debit-spread scanner config.
    Debit(DebitSpreadScannerConfig),
    /// Naked-option scanner config.
    Naked(NakedOptionScannerConfig),
}

/// Profile-local risk overrides read from config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlpacaOptionsStrategyRiskOverrides {
    /// Maximum active entries for this profile. Enforced by future profile-owned admission state.
    pub max_active_entries: Option<usize>,
    /// Maximum accepted submissions for this profile and trade date.
    pub max_daily_submits: Option<usize>,
    /// Maximum active entries for one profile underlying.
    pub max_active_entries_per_underlying: Option<usize>,
    /// Maximum selected-entry risk capital in USD.
    pub max_single_entry_risk_capital_usd: Option<f64>,
}

/// Resolved strategy profile used by scanners and operator status.
#[derive(Clone, Debug, PartialEq)]
pub struct AlpacaOptionsStrategyProfile {
    /// Stable profile identifier.
    pub id: String,
    /// Strategy family scanned by this profile.
    pub family: AlpacaOptionsStrategyFamily,
    /// Opening-order mode for this profile.
    pub mode: AlpacaOptionsStrategyMode,
    /// Underlyings scanned by this profile.
    pub underlyings: Vec<String>,
    /// Contract quantity for this profile.
    pub quantity: u64,
    /// Resolved scanner config for this profile.
    pub scanner: AlpacaOptionsStrategyScannerConfig,
    /// Profile-local risk overrides from config.
    pub risk: AlpacaOptionsStrategyRiskOverrides,
}

impl AlpacaOptionsStrategyProfile {
    /// Returns the stable strategy label.
    #[must_use]
    pub const fn strategy_name(&self) -> &'static str {
        self.family.as_str()
    }

    /// Returns whether this profile is a dry-run/watchlist profile.
    #[must_use]
    pub const fn is_dry_run(&self) -> bool {
        matches!(self.mode, AlpacaOptionsStrategyMode::DryRun)
    }

    /// Returns whether this profile scans the supplied underlying.
    #[must_use]
    pub fn scans_underlying(&self, underlying: &str) -> bool {
        self.underlyings
            .iter()
            .any(|value| value.eq_ignore_ascii_case(underlying))
    }

    /// Returns a compact config-check/status summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let risk = self.risk.summary();
        if risk.is_empty() {
            format!(
                "{}:{}:{}:{}:qty={}",
                self.id,
                self.family.as_str(),
                self.mode.as_str(),
                self.underlyings.join("|"),
                self.quantity,
            )
        } else {
            format!(
                "{}:{}:{}:{}:qty={}:risk={}",
                self.id,
                self.family.as_str(),
                self.mode.as_str(),
                self.underlyings.join("|"),
                self.quantity,
                risk,
            )
        }
    }
}

impl AlpacaOptionsStrategyRiskOverrides {
    /// Returns whether this profile carries any local risk override.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.max_active_entries.is_none()
            && self.max_daily_submits.is_none()
            && self.max_active_entries_per_underlying.is_none()
            && self.max_single_entry_risk_capital_usd.is_none()
    }

    /// Returns profile-local risk overrides as compact operator JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut fields = Map::new();
        if let Some(value) = self.max_active_entries {
            fields.insert("max_active_entries".to_string(), json!(value));
        }
        if let Some(value) = self.max_daily_submits {
            fields.insert("max_daily_submits".to_string(), json!(value));
        }
        if let Some(value) = self.max_active_entries_per_underlying {
            fields.insert(
                "max_active_entries_per_underlying".to_string(),
                json!(value),
            );
        }
        if let Some(value) = self.max_single_entry_risk_capital_usd {
            fields.insert(
                "max_single_entry_risk_capital_usd".to_string(),
                json!(value),
            );
        }
        Value::Object(fields)
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(value) = self.max_active_entries {
            parts.push(format!("max_active_entries={value}"));
        }
        if let Some(value) = self.max_daily_submits {
            parts.push(format!("max_daily_submits={value}"));
        }
        if let Some(value) = self.max_active_entries_per_underlying {
            parts.push(format!("max_active_entries_per_underlying={value}"));
        }
        if let Some(value) = self.max_single_entry_risk_capital_usd {
            parts.push(format!("max_single_entry_risk_capital_usd={value:.2}"));
        }
        parts.join("|")
    }
}

/// Profile context carried with Alpaca scanner-selected candidates.
#[derive(Clone, Debug, PartialEq)]
pub struct AlpacaOptionsCandidateProfile {
    /// Stable profile identifier.
    pub id: String,
    /// Strategy family scanned by this profile.
    pub family: AlpacaOptionsStrategyFamily,
    /// Opening-order mode for this profile.
    pub mode: AlpacaOptionsStrategyMode,
    /// Contract quantity configured for this profile.
    pub quantity: u64,
    /// Profile-local risk overrides.
    pub risk: AlpacaOptionsStrategyRiskOverrides,
}

impl AlpacaOptionsCandidateProfile {
    /// Builds candidate profile metadata from a resolved strategy profile.
    #[must_use]
    pub fn from_strategy_profile(profile: &AlpacaOptionsStrategyProfile) -> Self {
        Self {
            id: profile.id.clone(),
            family: profile.family,
            mode: profile.mode,
            quantity: profile.quantity,
            risk: profile.risk.clone(),
        }
    }

    /// Builds profile metadata for legacy diagnostic paths that have no resolved profile.
    #[must_use]
    pub fn synthetic(
        id: impl Into<String>,
        family: AlpacaOptionsStrategyFamily,
        quantity: u64,
    ) -> Self {
        Self {
            id: id.into(),
            family,
            mode: AlpacaOptionsStrategyMode::Live,
            quantity: quantity.max(1),
            risk: AlpacaOptionsStrategyRiskOverrides::default(),
        }
    }

    /// Inserts profile fields into an operator JSON payload.
    pub fn insert_json_fields(&self, payload: &mut Value) {
        let Value::Object(fields) = payload else {
            return;
        };
        fields.insert("profile_id".to_string(), json!(self.id));
        fields.insert("profile_family".to_string(), json!(self.family.as_str()));
        fields.insert("profile_mode".to_string(), json!(self.mode.as_str()));
        fields.insert("profile_quantity".to_string(), json!(self.quantity));
        if !self.risk.is_empty() {
            fields.insert("profile_risk".to_string(), self.risk.to_json_value());
        }
    }

    /// Returns profile fields as compact operator JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut payload = json!({});
        self.insert_json_fields(&mut payload);
        payload
    }
}

/// Alpaca-owned wrapper for a selected option entry and its originating profile.
#[derive(Clone, Debug)]
pub struct ProfiledOptionsEntry {
    /// Profile that produced this candidate.
    pub profile: AlpacaOptionsCandidateProfile,
    /// Source-neutral selected option entry.
    pub entry: SelectedOptionsEntry,
}

impl ProfiledOptionsEntry {
    /// Creates a profiled selected entry.
    #[must_use]
    pub fn new(profile: AlpacaOptionsCandidateProfile, entry: SelectedOptionsEntry) -> Self {
        Self { profile, entry }
    }

    /// Returns the source-neutral selected entry.
    #[must_use]
    pub fn selected_entry(&self) -> &SelectedOptionsEntry {
        &self.entry
    }

    /// Consumes the wrapper and returns the source-neutral selected entry.
    #[must_use]
    pub fn into_selected_entry(self) -> SelectedOptionsEntry {
        self.entry
    }

    /// Returns shared selected-entry metadata.
    #[must_use]
    pub fn descriptor(&self) -> OptionEntryDescriptor {
        self.entry.descriptor()
    }

    /// Returns the scanner score.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.entry.score()
    }

    /// Returns the underlying symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        self.entry.underlying()
    }

    /// Returns the stable strategy family name.
    #[must_use]
    pub fn strategy_name(&self) -> &'static str {
        self.entry.strategy_name()
    }

    /// Returns the candidate option symbols.
    #[must_use]
    pub fn option_symbols(&self) -> Vec<&str> {
        self.entry.option_symbols()
    }

    /// Returns whether the selected entry is a single-leg naked option.
    #[must_use]
    pub const fn is_naked_option(&self) -> bool {
        self.entry.is_naked_option()
    }

    /// Returns the entry premium kind.
    #[must_use]
    pub fn entry_premium_kind(&self) -> &'static str {
        self.entry.entry_premium_kind()
    }

    /// Returns the entry premium per spread or option.
    #[must_use]
    pub fn entry_premium(&self) -> f64 {
        self.entry.entry_premium()
    }

    /// Inserts the originating profile fields into an operator JSON payload.
    pub fn insert_profile_json_fields(&self, payload: &mut Value) {
        self.profile.insert_json_fields(payload);
    }
}

impl AlpacaOptionsRuntimeConfig {
    /// Builds config from TOML config, short environment overrides, and optional positional
    /// underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when config, strategy names, times, or timezone values are invalid.
    pub fn from_env() -> anyhow::Result<Self> {
        crate::runtime_env::load_options_env_file()?;
        let mut config = build_options_runtime_config(
            load_runtime_config_file_from_env()?,
            env::args().skip(1).collect::<Vec<_>>(),
        )?;
        config.operational_database_url = None;
        config.operational_repository = None;
        config.operational_schema = operational::OPERATIONAL_SCHEMA_DEFAULT.to_string();
        config.operational_account_id = None;
        Ok(config)
    }

    /// Builds the default runtime config for unit tests.
    #[cfg(test)]
    pub(crate) fn from_runtime_config_for_tests() -> Self {
        build_options_runtime_config(config::RuntimeConfigFile::default(), Vec::new())
            .expect("default Alpaca options runtime config should parse")
    }

    /// Builds config without reading positional CLI underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when config, strategy names, times, or timezone values are invalid.
    pub fn from_runtime_env() -> anyhow::Result<Self> {
        crate::runtime_env::load_options_env_file()?;
        let mut config =
            build_options_runtime_config(load_runtime_config_file_from_env()?, Vec::new())?;
        config.operational_database_url = None;
        config.operational_repository = None;
        config.operational_schema = operational::OPERATIONAL_SCHEMA_DEFAULT.to_string();
        config.operational_account_id = None;
        Ok(config)
    }

    /// Builds config from TOML config, CLI underlyings, and required Postgres persistence.
    ///
    /// # Errors
    ///
    /// Returns an error when config or operational-store initialization fails.
    pub async fn from_env_with_operational_store() -> anyhow::Result<Self> {
        let mut config = Self::from_env()?;
        config.connect_operational_store_from_env().await?;
        Ok(config)
    }

    /// Builds config without positional CLI underlyings and required Postgres persistence.
    ///
    /// # Errors
    ///
    /// Returns an error when config or operational-store initialization fails.
    pub async fn from_runtime_env_with_operational_store() -> anyhow::Result<Self> {
        let mut config = Self::from_runtime_env()?;
        config.connect_operational_store_from_env().await?;
        Ok(config)
    }

    /// Builds config without positional CLI underlyings and read-only Postgres persistence.
    ///
    /// # Errors
    ///
    /// Returns an error when config or operational-store initialization fails.
    pub async fn from_runtime_env_with_read_only_operational_store() -> anyhow::Result<Self> {
        let mut config = Self::from_runtime_env()?;
        config
            .connect_operational_store_from_env_with_mode(OperationalStoreConnectionMode::ReadOnly)
            .await?;
        Ok(config)
    }

    async fn connect_operational_store_from_env(&mut self) -> anyhow::Result<()> {
        self.connect_operational_store_from_env_with_mode(
            OperationalStoreConnectionMode::ApplyMigrations,
        )
        .await
    }

    async fn connect_operational_store_from_env_with_mode(
        &mut self,
        mode: OperationalStoreConnectionMode,
    ) -> anyhow::Result<()> {
        let database_url = env::var("NAUTILUS_OPERATIONAL_DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("NAUTILUS_OPERATIONAL_DATABASE_URL is required"))?;
        let schema = env::var("NAUTILUS_OPERATIONAL_SCHEMA")
            .unwrap_or_else(|_| operational::OPERATIONAL_SCHEMA_DEFAULT.to_string());
        let repository = match mode {
            OperationalStoreConnectionMode::ApplyMigrations => {
                OperationalRepository::connect_with_schema(&database_url, &schema).await?
            }
            OperationalStoreConnectionMode::ReadOnly => {
                OperationalRepository::connect_read_only_with_schema(&database_url, &schema).await?
            }
        };
        let repository = Arc::new(repository);
        self.operational_database_url = Some(database_url);
        self.operational_schema = schema;
        self.operational_repository = Some(repository);
        if self.operational_account_id.is_none() {
            self.operational_account_id = non_empty_env("NAUTILUS_OPERATIONAL_ACCOUNT_ID")
                .or_else(|| non_empty_env("NAUTILUS_ALPACA_ACCOUNT"));
        }
        Ok(())
    }

    /// Returns the effective operational account identifier for persistence operations.
    #[must_use]
    pub fn operational_account_id(&self) -> &str {
        self.operational_account_id
            .as_deref()
            .or(self.fleet_account_id.as_deref())
            .unwrap_or(operational::OPERATIONAL_ACCOUNT_ID_DEFAULT)
    }

    /// Loads state from Postgres when configured, bootstrapping from the local JSON state file when
    /// the account has no stored DB row.
    pub async fn load_strategy_state(&self) -> anyhow::Result<StrategyState> {
        if let Some(repository) = &self.operational_repository {
            crate::runtime::load_strategy_state_with_storage(
                &self.state_path,
                repository,
                self.operational_account_id(),
            )
            .await
        } else {
            crate::runtime::load_strategy_state(&self.state_path)
        }
    }

    /// Saves state to Postgres when configured, otherwise to the local JSON state file.
    pub async fn save_strategy_state(&self, state: &StrategyState) -> anyhow::Result<()> {
        if let Some(repository) = &self.operational_repository {
            crate::runtime::save_strategy_state_with_storage(
                &self.state_path,
                repository,
                state,
                self.operational_account_id(),
            )
            .await
        } else {
            crate::runtime::save_strategy_state_atomic(&self.state_path, state)
        }
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

    /// Returns lifecycle-risk settings for the live strategy runtime.
    #[must_use]
    pub const fn lifecycle_risk_config(&self) -> OptionLifecycleRiskConfig {
        OptionLifecycleRiskConfig {
            poll_secs: self.lifecycle_poll_secs,
            activity_lookback_hours: self.lifecycle_activity_lookback_hours,
            activity_block_hours: self.lifecycle_activity_block_hours,
            expiration_entry_block_days: self.expiration_entry_block_days,
        }
    }

    /// Appends one analytical record to the candidate ledger when enabled.
    pub async fn record_candidate_ledger(
        &self,
        trade_date: &str,
        record_type: &str,
        payload: Value,
    ) {
        if !self.candidate_ledger_enabled {
            return;
        }
        let Some(repository) = &self.operational_repository else {
            emit_operator_event(
                "candidate_ledger_error",
                json!({
                    "reason": "operational_store_not_connected",
                    "record_type": record_type,
                    "trade_date": trade_date,
                }),
            );
            return;
        };
        if let Err(error) = operational::append_candidate_ledger_record(
            repository,
            self.operational_account_id(),
            trade_date,
            record_type,
            payload,
        )
        .await
        {
            emit_operator_event(
                "candidate_ledger_error",
                json!({
                    "reason": "append_failed",
                    "record_type": record_type,
                    "trade_date": trade_date,
                    "error": error.to_string(),
                }),
            );
        }
    }

    /// Appends one typed candidate alert event to the candidate ledger when enabled.
    pub async fn record_candidate_alert_ledger(
        &self,
        trade_date: &str,
        alert_type: &str,
        severity: &str,
        alert_key: String,
        payload: Value,
    ) {
        let mut record = match payload {
            Value::Object(fields) => fields,
            value => {
                let mut fields = Map::new();
                fields.insert("payload".to_string(), value);
                fields
            }
        };
        record.insert(
            "alert_type".to_string(),
            Value::String(alert_type.to_string()),
        );
        record.insert("severity".to_string(), Value::String(severity.to_string()));
        record.insert("alert_key".to_string(), Value::String(alert_key));
        self.record_candidate_ledger(trade_date, "candidate_alert", Value::Object(record))
            .await;
    }

    fn candidate_ledger_candidate_limit(&self, candidate_count: usize) -> usize {
        if self.candidate_ledger_max_candidates == 0 {
            candidate_count
        } else {
            candidate_count.min(self.candidate_ledger_max_candidates)
        }
    }

    /// Returns enabled strategy names for operator logs.
    #[must_use]
    pub fn enabled_strategy_family_names(&self) -> Vec<&'static str> {
        unique_strategy_names(
            self.strategy_profiles
                .iter()
                .map(AlpacaOptionsStrategyProfile::strategy_name),
        )
    }

    /// Returns compact resolved strategy profile summaries.
    #[must_use]
    pub fn strategy_profile_summaries(&self) -> Vec<String> {
        self.strategy_profiles
            .iter()
            .map(AlpacaOptionsStrategyProfile::summary)
            .collect()
    }

    /// Returns the scanner config for a naked-option strategy profile.
    #[must_use]
    pub fn naked_scanner_for(&self, kind: NakedOptionKind) -> &NakedOptionScannerConfig {
        if kind.is_one_to_three_dte() {
            &self.naked_1_3dte_scanner
        } else {
            &self.naked_scanner
        }
    }

    /// Returns the configured sector/correlation group for an underlying.
    #[must_use]
    pub fn sector_for(&self, underlying: &str) -> Option<&str> {
        self.sectors
            .get(&underlying.to_ascii_uppercase())
            .map(String::as_str)
    }
}

fn unique_strategy_names(names: impl IntoIterator<Item = &'static str>) -> Vec<&'static str> {
    let mut seen = BTreeSet::new();
    names
        .into_iter()
        .filter(|name| seen.insert(*name))
        .collect()
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// Result of one configured options family scan for one underlying.
#[derive(Clone, Debug)]
pub struct OptionsScanReport {
    /// Profile that produced this scan report.
    pub profile: AlpacaOptionsCandidateProfile,
    /// Underlying symbol.
    pub underlying: String,
    /// Stable strategy name.
    pub strategy: &'static str,
    /// Whether the scan produced at least one ranked candidate.
    pub outcome: OptionsScanOutcome,
    /// Number of ranked candidates produced by the scanner.
    pub candidate_count: usize,
    /// Stable no-candidate reason when no candidate was produced.
    pub reason: Option<String>,
    /// Number of contracts loaded.
    pub contract_count: usize,
    /// Number of snapshots loaded.
    pub snapshot_count: usize,
    /// Number of scoreable contracts.
    pub scoreable_count: usize,
    /// Counts of scanner rejection reasons.
    pub rejection_counts: BTreeMap<String, usize>,
}

impl OptionsScanReport {
    pub fn new(
        profile: AlpacaOptionsCandidateProfile,
        underlying: &str,
        strategy: &'static str,
        candidate_count: usize,
        contract_count: usize,
        snapshot_count: usize,
        scoreable_count: usize,
        rejection_counts: BTreeMap<String, usize>,
    ) -> Self {
        let outcome = if candidate_count == 0 {
            OptionsScanOutcome::NoCandidate
        } else {
            OptionsScanOutcome::Candidate
        };
        let reason = (outcome == OptionsScanOutcome::NoCandidate).then(|| {
            no_candidate_reason(contract_count, snapshot_count, scoreable_count).to_string()
        });
        Self {
            profile,
            underlying: underlying.to_string(),
            strategy,
            outcome,
            candidate_count,
            reason,
            contract_count,
            snapshot_count,
            scoreable_count,
            rejection_counts,
        }
    }
}

/// Outcome for one scanner pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionsScanOutcome {
    /// At least one candidate passed the scanner thresholds.
    Candidate,
    /// No candidate passed the scanner thresholds.
    NoCandidate,
}

/// Candidate set produced by scanner discovery for one strategy iteration.
#[derive(Clone, Debug)]
pub struct OptionsCandidateSet {
    /// Market trade date for the candidate set.
    pub trade_date: String,
    /// Per-underlying, per-strategy scanner diagnostics.
    pub scans: Vec<OptionsScanReport>,
    /// Ranked candidate entries across all enabled strategy profiles.
    pub ranked_entries: Vec<ProfiledOptionsEntry>,
}

impl OptionsCandidateSet {
    pub fn new(trade_date: &str) -> Self {
        Self {
            trade_date: trade_date.to_string(),
            scans: Vec::new(),
            ranked_entries: Vec::new(),
        }
    }

    pub fn push_scan(&mut self, report: OptionsScanReport) {
        self.scans.push(report);
    }

    pub fn consider_candidate(&mut self, candidate: ProfiledOptionsEntry) {
        self.ranked_entries.push(candidate);
        self.ranked_entries
            .sort_by(|left, right| right.score().total_cmp(&left.score()));
    }

    /// Returns the ranked entry candidates across all enabled strategy profiles.
    #[must_use]
    pub fn ranked_entries(&self) -> &[ProfiledOptionsEntry] {
        &self.ranked_entries
    }

    /// Returns the highest-scoring selected entry candidate, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&ProfiledOptionsEntry> {
        self.ranked_entries.first()
    }

    /// Consumes the candidate set and returns the selected entry candidate.
    #[must_use]
    pub fn into_selected_entry(self) -> Option<ProfiledOptionsEntry> {
        self.ranked_entries.into_iter().next()
    }
}

/// Discovers ranked option candidates for one strategy iteration.
///
/// # Errors
///
/// Returns an error when Alpaca account, position, order, contract, or snapshot requests fail.
pub async fn scan_options_candidates(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &AlpacaOptionsRuntimeConfig,
    trade_date: &str,
) -> anyhow::Result<OptionsCandidateSet> {
    let account = client.account().await?;
    let options_buying_power = account_options_buying_power(&account);
    let mut candidates = OptionsCandidateSet::new(trade_date);

    for profile in &config.strategy_profiles {
        let profile_context = AlpacaOptionsCandidateProfile::from_strategy_profile(profile);
        for underlying in &profile.underlyings {
            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Credit(scanner)) =
                (credit_kind_from_family(profile.family), &profile.scanner)
            {
                let result = match kind {
                    CreditSpreadKind::Put => {
                        scan_put_credit_underlying(client, data_config, scanner, underlying).await?
                    }
                    CreditSpreadKind::Call => {
                        scan_call_credit_underlying(client, data_config, scanner, underlying)
                            .await?
                    }
                };
                let strategy_name = credit_spread_strategy_name(kind);
                let scanner_reason = result.candidates.is_empty().then(|| {
                    no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    )
                });
                record_scanner_ledger_result(
                config,
                trade_date,
                json!({
                    "underlying": underlying,
                    "strategy": strategy_name,
                    "result": if result.candidates.is_empty() { "no_candidate" } else { "candidate" },
                    "reason": scanner_reason,
                    "contracts": result.contract_count,
                    "snapshots": result.snapshot_count,
                    "scoreable": result.scoreable_count,
                    "rejections": &result.rejection_counts,
                    "profile_id": &profile.id,
                    "profile_mode": profile.mode.as_str(),
                }),
            )
            .await;
                record_credit_candidate_ledger(
                    config,
                    trade_date,
                    underlying,
                    strategy_name,
                    &result.candidates,
                )
                .await;
                candidates.push_scan(OptionsScanReport::new(
                    profile_context.clone(),
                    underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                let Some(best) = result.candidates.first() else {
                    let reason = no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    );
                    println!(
                        "{underlying}: no_candidate strategy={} reason={} contracts={} snapshots={} scoreable={} rejections={}",
                        credit_spread_strategy_name(kind),
                        reason,
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                        format_rejection_counts(&result.rejection_counts),
                    );
                    emit_operator_event(
                        "scanner_diagnostic",
                        json!({
                            "underlying": underlying,
                            "strategy": credit_spread_strategy_name(kind),
                            "result": "no_candidate",
                            "reason": reason,
                            "contracts": result.contract_count,
                            "snapshots": result.snapshot_count,
                            "scoreable": result.scoreable_count,
                            "rejections": &result.rejection_counts,
                            "profile_id": &profile.id,
                            "profile_mode": profile.mode.as_str(),
                        }),
                    );
                    continue;
                };

                println!(
                    "{underlying}: candidate strategy={} short={} long={} credit={:.2} ror={:.1}% score={:.1}",
                    credit_spread_strategy_name(kind),
                    best.short.symbol,
                    best.long.symbol,
                    best.credit,
                    best.return_on_risk * 100.0,
                    best.score,
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": credit_spread_strategy_name(kind),
                        "result": "candidate",
                        "short_symbol": &best.short.symbol,
                        "long_symbol": &best.long.symbol,
                        "credit": best.credit,
                        "return_on_risk": best.return_on_risk,
                        "score": best.score,
                        "rejections": &result.rejection_counts,
                        "profile_id": &profile.id,
                        "profile_mode": profile.mode.as_str(),
                    }),
                );

                candidates.consider_candidate(ProfiledOptionsEntry::new(
                    profile_context.clone(),
                    SelectedOptionsEntry::Credit(SelectedEntry {
                        underlying: underlying.clone(),
                        kind,
                        candidate: best.clone(),
                    }),
                ));
            }

            if matches!(profile.family, AlpacaOptionsStrategyFamily::IronCondor) {
                let AlpacaOptionsStrategyScannerConfig::IronCondor(scanner) = &profile.scanner
                else {
                    continue;
                };
                let result =
                    scan_iron_condor_underlying(client, data_config, scanner, underlying).await?;
                let scanner_reason = result.candidates.is_empty().then(|| {
                    no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    )
                });
                record_scanner_ledger_result(
                config,
                trade_date,
                json!({
                    "underlying": underlying,
                    "strategy": "iron_condor",
                    "result": if result.candidates.is_empty() { "no_candidate" } else { "candidate" },
                    "reason": scanner_reason,
                    "contracts": result.contract_count,
                    "snapshots": result.snapshot_count,
                    "scoreable": result.scoreable_count,
                    "rejections": &result.rejection_counts,
                    "profile_id": &profile.id,
                    "profile_mode": profile.mode.as_str(),
                }),
            )
            .await;
                record_iron_condor_candidate_ledger(
                    config,
                    trade_date,
                    underlying,
                    &result.candidates,
                )
                .await;
                candidates.push_scan(OptionsScanReport::new(
                    profile_context.clone(),
                    underlying,
                    "iron_condor",
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                let Some(best) = result.candidates.first() else {
                    let reason = no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    );
                    println!(
                        "{underlying}: no_candidate strategy=iron_condor reason={} contracts={} snapshots={} scoreable={} rejections={}",
                        reason,
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                        format_rejection_counts(&result.rejection_counts),
                    );
                    emit_operator_event(
                        "scanner_diagnostic",
                        json!({
                            "underlying": underlying,
                            "strategy": "iron_condor",
                            "result": "no_candidate",
                            "reason": reason,
                            "contracts": result.contract_count,
                            "snapshots": result.snapshot_count,
                            "scoreable": result.scoreable_count,
                            "rejections": &result.rejection_counts,
                            "profile_id": &profile.id,
                            "profile_mode": profile.mode.as_str(),
                        }),
                    );
                    continue;
                };

                println!(
                    "{underlying}: candidate strategy=iron_condor short_put={} long_put={} short_call={} long_call={} credit={:.2} ror={:.1}% score={:.1}",
                    best.put.short.symbol,
                    best.put.long.symbol,
                    best.call.short.symbol,
                    best.call.long.symbol,
                    best.credit,
                    best.return_on_risk * 100.0,
                    best.score,
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": "iron_condor",
                        "result": "candidate",
                        "short_put_symbol": &best.put.short.symbol,
                        "long_put_symbol": &best.put.long.symbol,
                        "short_call_symbol": &best.call.short.symbol,
                        "long_call_symbol": &best.call.long.symbol,
                        "credit": best.credit,
                        "return_on_risk": best.return_on_risk,
                        "score": best.score,
                        "rejections": &result.rejection_counts,
                        "profile_id": &profile.id,
                        "profile_mode": profile.mode.as_str(),
                    }),
                );

                candidates.consider_candidate(ProfiledOptionsEntry::new(
                    profile_context.clone(),
                    SelectedOptionsEntry::IronCondor(SelectedIronCondorEntry {
                        underlying: underlying.clone(),
                        candidate: best.clone(),
                    }),
                ));
            }

            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Debit(scanner)) =
                (debit_kind_from_family(profile.family), &profile.scanner)
            {
                let result = match kind {
                    DebitSpreadKind::Call => {
                        scan_call_debit_underlying(client, data_config, scanner, underlying).await?
                    }
                    DebitSpreadKind::Put => {
                        scan_put_debit_underlying(client, data_config, scanner, underlying).await?
                    }
                };
                let strategy_name = debit_spread_strategy_name(kind);
                let scanner_reason = result.candidates.is_empty().then(|| {
                    no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    )
                });
                record_scanner_ledger_result(
                config,
                trade_date,
                json!({
                    "underlying": underlying,
                    "strategy": strategy_name,
                    "result": if result.candidates.is_empty() { "no_candidate" } else { "candidate" },
                    "reason": scanner_reason,
                    "contracts": result.contract_count,
                    "snapshots": result.snapshot_count,
                    "scoreable": result.scoreable_count,
                    "rejections": &result.rejection_counts,
                    "profile_id": &profile.id,
                    "profile_mode": profile.mode.as_str(),
                }),
            )
            .await;
                record_debit_candidate_ledger(
                    config,
                    trade_date,
                    underlying,
                    strategy_name,
                    &result.candidates,
                )
                .await;
                candidates.push_scan(OptionsScanReport::new(
                    profile_context.clone(),
                    underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                let Some(best) = result.candidates.first() else {
                    let reason = no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    );
                    println!(
                        "{underlying}: no_candidate strategy={} reason={} contracts={} snapshots={} scoreable={} rejections={}",
                        debit_spread_strategy_name(kind),
                        reason,
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                        format_rejection_counts(&result.rejection_counts),
                    );
                    emit_operator_event(
                        "scanner_diagnostic",
                        json!({
                            "underlying": underlying,
                            "strategy": debit_spread_strategy_name(kind),
                            "result": "no_candidate",
                            "reason": reason,
                            "contracts": result.contract_count,
                            "snapshots": result.snapshot_count,
                            "scoreable": result.scoreable_count,
                            "rejections": &result.rejection_counts,
                            "profile_id": &profile.id,
                            "profile_mode": profile.mode.as_str(),
                        }),
                    );
                    continue;
                };

                println!(
                    "{underlying}: candidate strategy={} long={} short={} debit={:.2} rtr={:.1}% score={:.1}",
                    debit_spread_strategy_name(kind),
                    best.long.symbol,
                    best.short.symbol,
                    best.debit,
                    best.reward_to_risk * 100.0,
                    best.score,
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": debit_spread_strategy_name(kind),
                        "result": "candidate",
                        "long_symbol": &best.long.symbol,
                        "short_symbol": &best.short.symbol,
                        "debit": best.debit,
                        "reward_to_risk": best.reward_to_risk,
                        "score": best.score,
                        "rejections": &result.rejection_counts,
                        "profile_id": &profile.id,
                        "profile_mode": profile.mode.as_str(),
                    }),
                );

                candidates.consider_candidate(ProfiledOptionsEntry::new(
                    profile_context.clone(),
                    SelectedOptionsEntry::Debit(SelectedDebitEntry {
                        underlying: underlying.clone(),
                        kind,
                        candidate: best.clone(),
                    }),
                ));
            }

            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Naked(scanner)) =
                (naked_kind_from_family(profile.family), &profile.scanner)
            {
                let result = scan_naked_option_underlying_with_capital(
                    client,
                    data_config,
                    scanner,
                    underlying,
                    kind,
                    Some(NakedOptionCapitalContext {
                        options_buying_power,
                        quantity: profile.quantity,
                    }),
                )
                .await?;
                let strategy_name = naked_option_strategy_name(kind);
                let scanner_reason = result.candidates.is_empty().then(|| {
                    no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    )
                });
                record_scanner_ledger_result(
                config,
                trade_date,
                json!({
                    "underlying": underlying,
                    "strategy": strategy_name,
                    "result": if result.candidates.is_empty() { "no_candidate" } else { "candidate" },
                    "reason": scanner_reason,
                    "contracts": result.contract_count,
                    "snapshots": result.snapshot_count,
                    "scoreable": result.scoreable_count,
                    "rejections": &result.rejection_counts,
                    "profile_id": &profile.id,
                    "profile_mode": profile.mode.as_str(),
                }),
            )
            .await;
                record_naked_candidate_ledger(
                    config,
                    trade_date,
                    underlying,
                    strategy_name,
                    options_buying_power,
                    &result.candidates,
                )
                .await;
                candidates.push_scan(OptionsScanReport::new(
                    profile_context.clone(),
                    underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                let Some(best) = result.candidates.first() else {
                    let reason = no_candidate_reason(
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                    );
                    println!(
                        "{underlying}: no_candidate strategy={} reason={} contracts={} snapshots={} scoreable={} rejections={}",
                        naked_option_strategy_name(kind),
                        reason,
                        result.contract_count,
                        result.snapshot_count,
                        result.scoreable_count,
                        format_rejection_counts(&result.rejection_counts),
                    );
                    emit_operator_event(
                        "scanner_diagnostic",
                        json!({
                            "underlying": underlying,
                            "strategy": naked_option_strategy_name(kind),
                            "result": "no_candidate",
                            "reason": reason,
                            "contracts": result.contract_count,
                            "snapshots": result.snapshot_count,
                            "scoreable": result.scoreable_count,
                            "rejections": &result.rejection_counts,
                            "profile_id": &profile.id,
                            "profile_mode": profile.mode.as_str(),
                        }),
                    );
                    continue;
                };

                let metrics = best.short.metrics.as_ref();
                if let Some(metrics) = metrics {
                    println!(
                        "{underlying}: candidate strategy={} short={} credit={:.2} delta={:.2} pop={:.1}% touch={:.1}% be_dist={:.1}% em_cov={:.2} bpr=${:.0} bp_use={} rbp={:.3}% score={:.1}",
                        naked_option_strategy_name(kind),
                        best.short.symbol,
                        best.credit,
                        best.short.delta_abs,
                        metrics.breakeven_pop * 100.0,
                        metrics.probability_of_touch_est * 100.0,
                        metrics.distance_to_breakeven_pct * 100.0,
                        metrics.expected_move_coverage,
                        best.estimated_buying_power_requirement,
                        format_optional_pct(best.buying_power_usage_pct),
                        best.return_on_buying_power * 100.0,
                        best.score,
                    );
                } else {
                    println!(
                        "{underlying}: candidate strategy={} short={} credit={:.2} delta={:.2} bpr=${:.0} bp_use={} rbp={:.3}% score={:.1}",
                        naked_option_strategy_name(kind),
                        best.short.symbol,
                        best.credit,
                        best.short.delta_abs,
                        best.estimated_buying_power_requirement,
                        format_optional_pct(best.buying_power_usage_pct),
                        best.return_on_buying_power * 100.0,
                        best.score,
                    );
                }
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": naked_option_strategy_name(kind),
                        "result": "candidate",
                        "short_symbol": &best.short.symbol,
                        "credit": best.credit,
                        "delta_abs": best.short.delta_abs,
                        "dte": best.short.dte,
                        "strike": best.short.strike,
                        "spread_pct": best.short.spread_pct,
                        "bid_size": best.short.bid_size,
                        "ask_size": best.short.ask_size,
                        "volume": best.short.volume,
                        "open_interest": best.short.open_interest,
                        "implied_volatility": best.short.implied_volatility,
                        "account_options_buying_power": options_buying_power,
                        "capital_requirement_model": best.capital_requirement_model.as_str(),
                        "estimated_buying_power_requirement": best.estimated_buying_power_requirement,
                        "buying_power_usage_pct": best.buying_power_usage_pct,
                        "return_on_buying_power": best.return_on_buying_power,
                        "annualized_premium_yield": annualized_premium_yield(
                            best.credit,
                            best.short.strike,
                            best.short.dte,
                        ),
                        "underlying_price": metrics.map(|metrics| metrics.underlying_price),
                        "breakeven": metrics.map(|metrics| metrics.breakeven),
                        "strike_itm_probability": metrics.map(|metrics| metrics.strike_itm_probability),
                        "delta_pop_proxy": metrics.map(|metrics| metrics.delta_pop_proxy),
                        "breakeven_pop": metrics.map(|metrics| metrics.breakeven_pop),
                        "probability_of_touch_est": metrics.map(|metrics| metrics.probability_of_touch_est),
                        "expected_move": metrics.map(|metrics| metrics.expected_move),
                        "expected_move_pct": metrics.map(|metrics| metrics.expected_move_pct),
                        "distance_to_strike_pct": metrics.map(|metrics| metrics.distance_to_strike_pct),
                        "distance_to_breakeven_pct": metrics.map(|metrics| metrics.distance_to_breakeven_pct),
                        "expected_move_coverage": metrics.map(|metrics| metrics.expected_move_coverage),
                        "model_delta_abs": metrics.map(|metrics| metrics.model_delta_abs),
                        "model_gamma": metrics.map(|metrics| metrics.model_gamma),
                        "model_theta": metrics.map(|metrics| metrics.model_theta),
                        "model_vega": metrics.map(|metrics| metrics.model_vega),
                        "score": best.score,
                        "rejections": &result.rejection_counts,
                        "profile_id": &profile.id,
                        "profile_mode": profile.mode.as_str(),
                    }),
                );

                candidates.consider_candidate(ProfiledOptionsEntry::new(
                    profile_context.clone(),
                    SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
                        underlying: underlying.clone(),
                        kind,
                        candidate: best.clone(),
                    }),
                ));
            }
        }
    }

    Ok(candidates)
}

fn credit_kind_from_family(family: AlpacaOptionsStrategyFamily) -> Option<CreditSpreadKind> {
    match family {
        AlpacaOptionsStrategyFamily::PutCredit => Some(CreditSpreadKind::Put),
        AlpacaOptionsStrategyFamily::CallCredit => Some(CreditSpreadKind::Call),
        _ => None,
    }
}

fn debit_kind_from_family(family: AlpacaOptionsStrategyFamily) -> Option<DebitSpreadKind> {
    match family {
        AlpacaOptionsStrategyFamily::PutDebit => Some(DebitSpreadKind::Put),
        AlpacaOptionsStrategyFamily::CallDebit => Some(DebitSpreadKind::Call),
        _ => None,
    }
}

fn naked_kind_from_family(family: AlpacaOptionsStrategyFamily) -> Option<NakedOptionKind> {
    match family {
        AlpacaOptionsStrategyFamily::NakedPut => Some(NakedOptionKind::Put),
        AlpacaOptionsStrategyFamily::NakedCall => Some(NakedOptionKind::Call),
        AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte => {
            Some(NakedOptionKind::PutOneToThreeDte)
        }
        AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte => {
            Some(NakedOptionKind::CallOneToThreeDte)
        }
        _ => None,
    }
}
