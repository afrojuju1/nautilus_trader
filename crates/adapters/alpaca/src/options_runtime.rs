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

use std::{collections::BTreeMap, env, path::PathBuf, sync::Arc};

use chrono::NaiveTime;
use chrono_tz::Tz;
use serde_json::{Map, Value, json};

#[cfg(feature = "live")]
use crate::storage::{self, StorageRepository};
use crate::{
    candidate_engine::{
        CreditSpreadKind, DebitSpreadKind, DebitSpreadScannerConfig, IronCondorScannerConfig,
        NakedOptionCapitalContext, NakedOptionKind, NakedOptionScannerConfig,
        PutCreditScannerConfig, annualized_premium_yield,
    },
    config::AlpacaDataClientConfig,
    fleet::ResolvedFleetConfig,
    http::client::AlpacaHttpClient,
    management::CreditSpreadManagementConfig,
    runtime::{
        StrategyState, credit_spread_strategy_name, debit_spread_strategy_name,
        emit_operator_event, naked_option_strategy_name,
    },
    strategy::{
        scan_call_credit_underlying, scan_call_debit_underlying, scan_iron_condor_underlying,
        scan_naked_option_underlying_with_capital, scan_put_credit_underlying,
        scan_put_debit_underlying,
    },
};

pub use crate::{
    candidate_payloads::{
        candidate_alert_identity_key, candidate_alert_key, credit_candidate_ledger_payload,
        debit_candidate_ledger_payload, insert_string_field, insert_value_field,
        iron_condor_candidate_ledger_payload, naked_candidate_ledger_payload,
    },
    options_entry::{
        OptionEntryDescriptor, SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry,
        SelectedNakedOptionEntry, SelectedOptionsEntry,
    },
};

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
pub(crate) use config::{
    active_sector_count, active_underlying_count, fleet_active_underlying_count,
    fleet_sector_limit_state,
};
use config::{build_options_engine_config, load_runtime_config_file_from_env};

const HIGH_SCORE_CANDIDATE_ALERT: &str = "high_score_candidate";
const CANDIDATE_ALERT_NAKED_MIN_SCORE: f64 = 95.0;
const CANDIDATE_ALERT_NAKED_ONE_TO_THREE_DTE_MIN_SCORE: f64 = 100.0;
const CANDIDATE_ALERT_IRON_CONDOR_MIN_SCORE: f64 = 80.0;
const CANDIDATE_ALERT_CREDIT_MIN_SCORE: f64 = 80.0;
const CANDIDATE_ALERT_DEBIT_MIN_SCORE: f64 = 80.0;

/// Runtime config for the options account-engine slice.
#[derive(Debug)]
pub struct OptionsEngineConfig {
    /// Underlyings to scan.
    pub underlyings: Vec<String>,
    /// Enabled spread kinds.
    pub spread_kinds: Vec<CreditSpreadKind>,
    /// Whether the iron-condor strategy is enabled.
    pub iron_condor_enabled: bool,
    /// Enabled long-premium debit spread kinds.
    pub debit_kinds: Vec<DebitSpreadKind>,
    /// Enabled naked short option kinds.
    pub naked_kinds: Vec<NakedOptionKind>,
    /// Credit spread kinds which scan but never submit.
    pub dry_run_spread_kinds: Vec<CreditSpreadKind>,
    /// Whether iron-condor candidates scan but never submit.
    pub iron_condor_dry_run: bool,
    /// Debit spread kinds which scan but never submit.
    pub dry_run_debit_kinds: Vec<DebitSpreadKind>,
    /// Naked option kinds which scan but never submit.
    pub dry_run_naked_kinds: Vec<NakedOptionKind>,
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
    /// Underlying to sector/correlation-group mapping.
    pub sectors: BTreeMap<String, String>,
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
    /// Stale close timeout.
    pub stale_close_secs: u64,
    /// Whether close order submission is enabled.
    pub close_enabled: bool,
    /// Whether non-forced close submissions are limited to regular options hours.
    pub close_regular_hours_only: bool,
    /// Close window start.
    pub close_start: NaiveTime,
    /// Close window end.
    pub close_end: NaiveTime,
    /// Additional debit allowed on submitted close limits.
    pub close_price_cushion: f64,
    /// Maximum accepted close submissions per entry. Zero means unlimited.
    pub max_close_attempts: u32,
    /// Minimum delay after a close submission before another close may be submitted.
    pub close_reprice_cooldown_secs: u64,
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
    /// Strategy-state and ledger persistence repository.
    pub storage_repository: Option<Arc<StorageRepository>>,
    /// Postgres database URL used for persistence when storage is enabled.
    pub storage_database_url: Option<String>,
    /// Postgres schema for persistence tables.
    pub storage_schema: String,
    /// Optional account ID override for persisted records.
    pub storage_account_id: Option<String>,
}

impl OptionsEngineConfig {
    /// Builds config from TOML config, short environment overrides, and optional positional
    /// underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when config, strategy names, times, or timezone values are invalid.
    pub fn from_env() -> anyhow::Result<Self> {
        crate::runtime_env::load_options_env_file()?;
        let mut config = build_options_engine_config(
            load_runtime_config_file_from_env()?,
            env::args().skip(1).collect::<Vec<_>>(),
        )?;
        config.storage_database_url = None;
        config.storage_repository = None;
        config.storage_schema = storage::STORAGE_SCHEMA_DEFAULT.to_string();
        config.storage_account_id = None;
        Ok(config)
    }

    /// Builds config without reading positional CLI underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when config, strategy names, times, or timezone values are invalid.
    pub fn from_runtime_env() -> anyhow::Result<Self> {
        crate::runtime_env::load_options_env_file()?;
        let mut config =
            build_options_engine_config(load_runtime_config_file_from_env()?, Vec::new())?;
        config.storage_database_url = None;
        config.storage_repository = None;
        config.storage_schema = storage::STORAGE_SCHEMA_DEFAULT.to_string();
        config.storage_account_id = None;
        Ok(config)
    }

    /// Builds config from TOML config, CLI underlyings, and required Postgres persistence.
    ///
    /// # Errors
    ///
    /// Returns an error when config or storage initialization fails.
    pub async fn from_env_with_storage() -> anyhow::Result<Self> {
        let mut config = Self::from_env()?;
        config.connect_storage_from_env().await?;
        Ok(config)
    }

    /// Builds config without positional CLI underlyings and required Postgres persistence.
    ///
    /// # Errors
    ///
    /// Returns an error when config or storage initialization fails.
    pub async fn from_runtime_env_with_storage() -> anyhow::Result<Self> {
        let mut config = Self::from_runtime_env()?;
        config.connect_storage_from_env().await?;
        Ok(config)
    }

    async fn connect_storage_from_env(&mut self) -> anyhow::Result<()> {
        let database_url = env::var("ALPACA_STORAGE_DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("ALPACA_STORAGE_DATABASE_URL is required"))?;
        let schema = env::var("ALPACA_STORAGE_SCHEMA")
            .unwrap_or_else(|_| storage::STORAGE_SCHEMA_DEFAULT.to_string());
        let repository = Arc::new(
            storage::StorageRepository::connect_with_schema(&database_url, &schema).await?,
        );
        self.storage_database_url = Some(database_url);
        self.storage_schema = schema;
        self.storage_repository = Some(repository);
        if self.storage_account_id.is_none() {
            self.storage_account_id = env::var("ALPACA_STORAGE_ACCOUNT_ID")
                .ok()
                .or_else(|| env::var("NAUTILUS_ALPACA_ACCOUNT").ok());
        }
        Ok(())
    }

    /// Returns the effective storage account identifier for persistence operations.
    #[must_use]
    pub fn storage_account_id(&self) -> &str {
        self.storage_account_id
            .as_deref()
            .or(self.fleet_account_id.as_deref())
            .unwrap_or(storage::STORAGE_ACCOUNT_ID_DEFAULT)
    }

    /// Loads state from Postgres when configured, bootstrapping from the local JSON state file when
    /// the account has no stored DB row.
    pub async fn load_strategy_state(&self) -> anyhow::Result<StrategyState> {
        if let Some(storage) = &self.storage_repository {
            crate::runtime::load_strategy_state_with_storage(
                &self.state_path,
                storage,
                self.storage_account_id(),
            )
            .await
        } else {
            crate::runtime::load_strategy_state(&self.state_path)
        }
    }

    /// Saves state to Postgres when configured, otherwise to the local JSON state file.
    pub async fn save_strategy_state(&self, state: &StrategyState) -> anyhow::Result<()> {
        if let Some(storage) = &self.storage_repository {
            crate::runtime::save_strategy_state_with_storage(
                &self.state_path,
                storage,
                state,
                self.storage_account_id(),
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
        let Some(storage) = &self.storage_repository else {
            emit_operator_event(
                "candidate_ledger_error",
                json!({
                    "reason": "storage_not_connected",
                    "record_type": record_type,
                    "trade_date": trade_date,
                }),
            );
            return;
        };
        if let Err(error) = storage::append_candidate_ledger_record(
            storage,
            self.storage_account_id(),
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
    pub fn enabled_strategy_names(&self) -> Vec<&'static str> {
        let mut names = self
            .spread_kinds
            .iter()
            .map(|kind| credit_spread_strategy_name(*kind))
            .collect::<Vec<_>>();
        if self.iron_condor_enabled {
            names.push("iron_condor");
        }
        names.extend(
            self.debit_kinds
                .iter()
                .map(|kind| debit_spread_strategy_name(*kind)),
        );
        names.extend(
            self.naked_kinds
                .iter()
                .map(|kind| naked_option_strategy_name(*kind)),
        );
        names
    }

    /// Returns strategy names which are configured for dry-run selection only.
    #[must_use]
    pub fn dry_run_strategy_names(&self) -> Vec<&'static str> {
        let mut names = self
            .dry_run_spread_kinds
            .iter()
            .map(|kind| credit_spread_strategy_name(*kind))
            .collect::<Vec<_>>();
        if self.iron_condor_dry_run {
            names.push("iron_condor");
        }
        names.extend(
            self.dry_run_debit_kinds
                .iter()
                .map(|kind| debit_spread_strategy_name(*kind)),
        );
        names.extend(
            self.dry_run_naked_kinds
                .iter()
                .map(|kind| naked_option_strategy_name(*kind)),
        );
        names
    }

    /// Returns whether a selected credit-spread kind may submit live orders.
    #[must_use]
    pub fn credit_submit_enabled(&self, kind: CreditSpreadKind) -> bool {
        self.submit_enabled && !self.dry_run_spread_kinds.contains(&kind)
    }

    /// Returns whether a selected iron condor may submit live orders.
    #[must_use]
    pub fn iron_condor_submit_enabled(&self) -> bool {
        self.submit_enabled && !self.iron_condor_dry_run
    }

    /// Returns whether a selected debit-spread kind may submit live orders.
    #[must_use]
    pub fn debit_submit_enabled(&self, kind: DebitSpreadKind) -> bool {
        self.submit_enabled && !self.dry_run_debit_kinds.contains(&kind)
    }

    /// Returns whether a selected naked-option kind may submit live orders.
    #[must_use]
    pub fn naked_submit_enabled(&self, kind: NakedOptionKind) -> bool {
        self.submit_enabled && !self.dry_run_naked_kinds.contains(&kind)
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

/// Result of one configured options family scan for one underlying.
#[derive(Clone, Debug)]
pub struct OptionsScanReport {
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

/// Opportunity set produced by scanner discovery for one strategy iteration.
#[derive(Clone, Debug)]
pub struct OptionsOpportunitySet {
    /// Market trade date for the opportunity set.
    pub trade_date: String,
    /// Per-underlying, per-strategy scanner diagnostics.
    pub scans: Vec<OptionsScanReport>,
    /// Ranked candidate entries across all enabled strategy families.
    pub ranked_entries: Vec<SelectedOptionsEntry>,
}

impl OptionsOpportunitySet {
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

    pub fn consider_candidate(&mut self, candidate: SelectedOptionsEntry) {
        self.ranked_entries.push(candidate);
        self.ranked_entries
            .sort_by(|left, right| right.score().total_cmp(&left.score()));
    }

    /// Returns the ranked entry candidates across all enabled strategy families.
    #[must_use]
    pub fn ranked_entries(&self) -> &[SelectedOptionsEntry] {
        &self.ranked_entries
    }

    /// Returns the highest-scoring selected entry candidate, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&SelectedOptionsEntry> {
        self.ranked_entries.first()
    }

    /// Consumes the opportunity set and returns the selected entry candidate.
    #[must_use]
    pub fn into_selected_entry(self) -> Option<SelectedOptionsEntry> {
        self.ranked_entries.into_iter().next()
    }
}

/// Discovers ranked option opportunities for one strategy iteration.
///
/// # Errors
///
/// Returns an error when Alpaca account, position, order, contract, or snapshot requests fail.
pub async fn scan_options_opportunities(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &OptionsEngineConfig,
    trade_date: &str,
) -> anyhow::Result<OptionsOpportunitySet> {
    let account = client.account().await?;
    let options_buying_power = account_options_buying_power(&account);
    let mut opportunities = OptionsOpportunitySet::new(trade_date);

    for underlying in &config.underlyings {
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
            let strategy_name = credit_spread_strategy_name(*kind);
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
            opportunities.push_scan(OptionsScanReport::new(
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
                    credit_spread_strategy_name(*kind),
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
                        "strategy": credit_spread_strategy_name(*kind),
                        "result": "no_candidate",
                        "reason": reason,
                        "contracts": result.contract_count,
                        "snapshots": result.snapshot_count,
                        "scoreable": result.scoreable_count,
                        "rejections": &result.rejection_counts,
                    }),
                );
                continue;
            };

            println!(
                "{underlying}: candidate strategy={} short={} long={} credit={:.2} ror={:.1}% score={:.1}",
                credit_spread_strategy_name(*kind),
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
                    "strategy": credit_spread_strategy_name(*kind),
                    "result": "candidate",
                    "short_symbol": &best.short.symbol,
                    "long_symbol": &best.long.symbol,
                    "credit": best.credit,
                    "return_on_risk": best.return_on_risk,
                    "score": best.score,
                    "rejections": &result.rejection_counts,
                }),
            );

            opportunities.consider_candidate(SelectedOptionsEntry::Credit(SelectedEntry {
                underlying: underlying.clone(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }

        if config.iron_condor_enabled {
            let result = scan_iron_condor_underlying(
                client,
                data_config,
                &config.iron_condor_scanner,
                underlying,
            )
            .await?;
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
                }),
            )
            .await;
            record_iron_condor_candidate_ledger(config, trade_date, underlying, &result.candidates)
                .await;
            opportunities.push_scan(OptionsScanReport::new(
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
                }),
            );

            opportunities.consider_candidate(SelectedOptionsEntry::IronCondor(
                SelectedIronCondorEntry {
                    underlying: underlying.clone(),
                    candidate: best.clone(),
                },
            ));
        }

        for kind in &config.debit_kinds {
            let result = match kind {
                DebitSpreadKind::Call => {
                    scan_call_debit_underlying(
                        client,
                        data_config,
                        &config.debit_scanner,
                        underlying,
                    )
                    .await?
                }
                DebitSpreadKind::Put => {
                    scan_put_debit_underlying(
                        client,
                        data_config,
                        &config.debit_scanner,
                        underlying,
                    )
                    .await?
                }
            };
            let strategy_name = debit_spread_strategy_name(*kind);
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
            opportunities.push_scan(OptionsScanReport::new(
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
                    debit_spread_strategy_name(*kind),
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
                        "strategy": debit_spread_strategy_name(*kind),
                        "result": "no_candidate",
                        "reason": reason,
                        "contracts": result.contract_count,
                        "snapshots": result.snapshot_count,
                        "scoreable": result.scoreable_count,
                        "rejections": &result.rejection_counts,
                    }),
                );
                continue;
            };

            println!(
                "{underlying}: candidate strategy={} long={} short={} debit={:.2} rtr={:.1}% score={:.1}",
                debit_spread_strategy_name(*kind),
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
                    "strategy": debit_spread_strategy_name(*kind),
                    "result": "candidate",
                    "long_symbol": &best.long.symbol,
                    "short_symbol": &best.short.symbol,
                    "debit": best.debit,
                    "reward_to_risk": best.reward_to_risk,
                    "score": best.score,
                    "rejections": &result.rejection_counts,
                }),
            );

            opportunities.consider_candidate(SelectedOptionsEntry::Debit(SelectedDebitEntry {
                underlying: underlying.clone(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }

        for kind in &config.naked_kinds {
            let result = scan_naked_option_underlying_with_capital(
                client,
                data_config,
                config.naked_scanner_for(*kind),
                underlying,
                *kind,
                Some(NakedOptionCapitalContext {
                    options_buying_power,
                    quantity: config.quantity,
                }),
            )
            .await?;
            let strategy_name = naked_option_strategy_name(*kind);
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
            opportunities.push_scan(OptionsScanReport::new(
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
                    naked_option_strategy_name(*kind),
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
                        "strategy": naked_option_strategy_name(*kind),
                        "result": "no_candidate",
                        "reason": reason,
                        "contracts": result.contract_count,
                        "snapshots": result.snapshot_count,
                        "scoreable": result.scoreable_count,
                        "rejections": &result.rejection_counts,
                    }),
                );
                continue;
            };

            let metrics = best.short.metrics.as_ref();
            if let Some(metrics) = metrics {
                println!(
                    "{underlying}: candidate strategy={} short={} credit={:.2} delta={:.2} pop={:.1}% touch={:.1}% be_dist={:.1}% em_cov={:.2} bpr=${:.0} bp_use={} rbp={:.3}% score={:.1}",
                    naked_option_strategy_name(*kind),
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
                    naked_option_strategy_name(*kind),
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
                    "strategy": naked_option_strategy_name(*kind),
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
                }),
            );

            opportunities.consider_candidate(SelectedOptionsEntry::NakedOption(
                SelectedNakedOptionEntry {
                    underlying: underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                },
            ));
        }
    }

    Ok(opportunities)
}
