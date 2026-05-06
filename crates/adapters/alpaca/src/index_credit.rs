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
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::NaiveTime;
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    candidate_ledger::{append_candidate_ledger_record, default_candidate_ledger_dir},
    config::AlpacaDataClientConfig,
    execution::check_option_spread_entry_admission,
    fleet::{ResolvedFleetConfig, load_fleet_config_from_env},
    http::{client::AlpacaHttpClient, models::ListOrdersRequest},
    management::CreditSpreadManagementConfig,
    runtime::{
        StrategyState, credit_spread_strategy_name, debit_spread_strategy_name,
        emit_operator_event, naked_option_strategy_name,
    },
    strategy::{
        CreditSpreadKind, DebitSpreadCandidate, DebitSpreadKind, DebitSpreadScannerConfig,
        IronCondorCandidate, IronCondorScannerConfig, NakedOptionCandidate,
        NakedOptionCapitalContext, NakedOptionKind, NakedOptionScannerConfig,
        OptionCandidateMetrics, PutCreditScannerConfig, ScoredContract, SpreadCandidate,
        annualized_premium_yield, scan_call_credit_underlying, scan_call_debit_underlying,
        scan_iron_condor_underlying, scan_naked_option_underlying_with_capital,
        scan_put_credit_underlying, scan_put_debit_underlying,
    },
};

const HIGH_SCORE_CANDIDATE_ALERT: &str = "high_score_candidate";
const CANDIDATE_ALERT_NAKED_MIN_SCORE: f64 = 95.0;
const CANDIDATE_ALERT_NAKED_ONE_TO_THREE_DTE_MIN_SCORE: f64 = 100.0;
const CANDIDATE_ALERT_IRON_CONDOR_MIN_SCORE: f64 = 80.0;
const CANDIDATE_ALERT_CREDIT_MIN_SCORE: f64 = 80.0;
const CANDIDATE_ALERT_DEBIT_MIN_SCORE: f64 = 80.0;

/// Runtime config for the index credit account-engine slice.
#[derive(Debug)]
pub struct IndexCreditConfig {
    /// Underlyings to scan.
    pub underlyings: Vec<String>,
    /// Enabled spread kinds.
    pub spread_kinds: Vec<CreditSpreadKind>,
    /// Whether the index iron-condor strategy is enabled.
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
    /// Directory for append-only candidate-ledger JSONL files.
    pub candidate_ledger_dir: PathBuf,
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
}

impl IndexCreditConfig {
    /// Builds config from TOML config, short environment overrides, and optional positional
    /// underlyings.
    ///
    /// # Errors
    ///
    /// Returns an error when config, strategy names, times, or timezone values are invalid.
    pub fn from_env() -> anyhow::Result<Self> {
        crate::runtime_env::load_index_credit_env_file()?;
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
        crate::runtime_env::load_index_credit_env_file()?;
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

    /// Appends one analytical record to the candidate ledger when enabled.
    pub fn record_candidate_ledger(&self, trade_date: &str, record_type: &str, payload: Value) {
        if !self.candidate_ledger_enabled {
            return;
        }
        if let Err(error) = append_candidate_ledger_record(
            &self.candidate_ledger_dir,
            trade_date,
            self.fleet_account_id.as_deref(),
            record_type,
            payload,
        ) {
            emit_operator_event(
                "candidate_ledger_error",
                json!({
                    "reason": "append_failed",
                    "record_type": record_type,
                    "trade_date": trade_date,
                    "ledger_dir": self.candidate_ledger_dir.display().to_string(),
                    "error": error.to_string(),
                }),
            );
        }
    }

    /// Appends one typed candidate alert event to the candidate ledger when enabled.
    pub fn record_candidate_alert_ledger(
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
        self.record_candidate_ledger(trade_date, "candidate_alert", Value::Object(record));
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
            names.push("index_iron_condor_entry");
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
            names.push("index_iron_condor_entry");
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

/// Selected long-premium debit-spread candidate.
#[derive(Clone, Debug)]
pub struct SelectedDebitEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Debit-spread kind.
    pub kind: DebitSpreadKind,
    /// Scored debit-spread candidate.
    pub candidate: DebitSpreadCandidate,
}

/// Selected naked short option candidate.
#[derive(Clone, Debug)]
pub struct SelectedNakedOptionEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Naked option kind.
    pub kind: NakedOptionKind,
    /// Scored naked-option candidate.
    pub candidate: NakedOptionCandidate,
}

/// Selected index strategy candidate.
#[derive(Clone, Debug)]
pub enum SelectedIndexEntry {
    /// Two-leg credit spread.
    Credit(SelectedEntry),
    /// Four-leg iron condor.
    IronCondor(SelectedIronCondorEntry),
    /// Two-leg debit spread.
    Debit(SelectedDebitEntry),
    /// Single-leg naked short option.
    NakedOption(SelectedNakedOptionEntry),
}

impl SelectedIndexEntry {
    /// Returns the scanner score.
    #[must_use]
    pub fn score(&self) -> f64 {
        match self {
            Self::Credit(entry) => entry.candidate.score,
            Self::IronCondor(entry) => entry.candidate.score,
            Self::Debit(entry) => entry.candidate.score,
            Self::NakedOption(entry) => entry.candidate.score,
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
                SelectedIndexEntry::Debit(_) => None,
                SelectedIndexEntry::NakedOption(_) => None,
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
    let options_buying_power = account_options_buying_power(&account);
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    let mut selected: Option<SelectedIndexEntry> = None;

    for underlying in &config.underlyings {
        if let Some(limit) = config.max_active_entries_per_underlying {
            let current = active_underlying_count(state, underlying);
            if current >= limit {
                println!(
                    "{underlying}: admission_rejected reason=risk_max_active_entries_per_underlying current={} limit={}",
                    current, limit,
                );
                record_scanner_ledger_result(
                    config,
                    trade_date,
                    json!({
                        "underlying": underlying,
                        "result": "admission_rejected",
                        "reason": "risk_max_active_entries_per_underlying",
                        "current": current,
                        "limit": limit,
                    }),
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "result": "admission_rejected",
                        "reason": "risk_max_active_entries_per_underlying",
                        "current": current,
                        "limit": limit,
                    }),
                );
                continue;
            }
        }
        if let Some(limit) = config.max_active_entries_per_sector
            && let Some(sector) = config.sector_for(underlying)
        {
            let current = active_sector_count(state, &config.sectors, sector);
            if current >= limit {
                println!(
                    "{underlying}: admission_rejected reason=risk_max_active_entries_per_sector sector={} current={} limit={}",
                    sector, current, limit,
                );
                record_scanner_ledger_result(
                    config,
                    trade_date,
                    json!({
                        "underlying": underlying,
                        "result": "admission_rejected",
                        "reason": "risk_max_active_entries_per_sector",
                        "sector": sector,
                        "current": current,
                        "limit": limit,
                    }),
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "result": "admission_rejected",
                        "reason": "risk_max_active_entries_per_sector",
                        "sector": sector,
                        "current": current,
                        "limit": limit,
                    }),
                );
                continue;
            }
        }
        if let Some(limit) = config
            .fleet
            .as_ref()
            .and_then(|fleet| fleet.config.fleet.max_active_entries_per_underlying)
        {
            let current = fleet_active_underlying_count(config, underlying);
            if current >= limit {
                println!(
                    "{underlying}: admission_rejected reason=fleet_max_active_entries_per_underlying current={} limit={}",
                    current, limit,
                );
                record_scanner_ledger_result(
                    config,
                    trade_date,
                    json!({
                        "underlying": underlying,
                        "result": "admission_rejected",
                        "reason": "fleet_max_active_entries_per_underlying",
                        "current": current,
                        "limit": limit,
                    }),
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "result": "admission_rejected",
                        "reason": "fleet_max_active_entries_per_underlying",
                        "current": current,
                        "limit": limit,
                    }),
                );
                continue;
            }
        }
        if let Some((sector, current, limit)) = fleet_sector_limit_state(config, underlying)
            && current >= limit
        {
            println!(
                "{underlying}: admission_rejected reason=fleet_max_active_entries_per_sector sector={} current={} limit={}",
                sector, current, limit,
            );
            record_scanner_ledger_result(
                config,
                trade_date,
                json!({
                    "underlying": underlying,
                    "result": "admission_rejected",
                    "reason": "fleet_max_active_entries_per_sector",
                    "sector": sector,
                    "current": current,
                    "limit": limit,
                }),
            );
            emit_operator_event(
                "scanner_diagnostic",
                json!({
                    "underlying": underlying,
                    "result": "admission_rejected",
                    "reason": "fleet_max_active_entries_per_sector",
                    "sector": sector,
                    "current": current,
                    "limit": limit,
                }),
            );
            continue;
        }

        if state.has_submitted_underlying_today(trade_date, underlying) {
            println!("{underlying}: admission_rejected reason=daily_duplicate_state");
            record_scanner_ledger_result(
                config,
                trade_date,
                json!({
                    "underlying": underlying,
                    "result": "admission_rejected",
                    "reason": "daily_duplicate_state",
                    "scope": "same_day_underlying_reentry",
                }),
            );
            emit_operator_event(
                "scanner_diagnostic",
                json!({
                    "underlying": underlying,
                    "result": "admission_rejected",
                    "reason": "daily_duplicate_state",
                    "scope": "same_day_underlying_reentry",
                }),
            );
            continue;
        }

        if fleet_has_active_underlying_elsewhere(config, underlying) {
            println!("{underlying}: admission_rejected reason=fleet_duplicate_underlying");
            record_scanner_ledger_result(
                config,
                trade_date,
                json!({
                    "underlying": underlying,
                    "result": "admission_rejected",
                    "reason": "fleet_duplicate_underlying",
                }),
            );
            emit_operator_event(
                "scanner_diagnostic",
                json!({
                    "underlying": underlying,
                    "result": "admission_rejected",
                    "reason": "fleet_duplicate_underlying",
                }),
            );
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
            );
            record_credit_candidate_ledger(
                config,
                trade_date,
                underlying,
                strategy_name,
                &result.candidates,
            );
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
                record_scanner_ledger_result(
                    config,
                    trade_date,
                    json!({
                        "underlying": underlying,
                        "strategy": credit_spread_strategy_name(*kind),
                        "result": "admission_rejected",
                        "short_symbol": &best.short.symbol,
                        "long_symbol": &best.long.symbol,
                        "reasons": &admission.reasons,
                    }),
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": credit_spread_strategy_name(*kind),
                        "result": "admission_rejected",
                        "short_symbol": &best.short.symbol,
                        "long_symbol": &best.long.symbol,
                        "reasons": admission.reasons,
                    }),
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
                    "strategy": "index_iron_condor_entry",
                    "result": if result.candidates.is_empty() { "no_candidate" } else { "candidate" },
                    "reason": scanner_reason,
                    "contracts": result.contract_count,
                    "snapshots": result.snapshot_count,
                    "scoreable": result.scoreable_count,
                    "rejections": &result.rejection_counts,
                }),
            );
            record_iron_condor_candidate_ledger(config, trade_date, underlying, &result.candidates);
            let Some(best) = result.candidates.first() else {
                let reason = no_candidate_reason(
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                );
                println!(
                    "{underlying}: no_candidate strategy=index_iron_condor_entry reason={} contracts={} snapshots={} scoreable={} rejections={}",
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
                        "strategy": "index_iron_condor_entry",
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
                record_scanner_ledger_result(
                    config,
                    trade_date,
                    json!({
                        "underlying": underlying,
                        "strategy": "index_iron_condor_entry",
                        "result": "admission_rejected",
                        "short_put_symbol": &best.put.short.symbol,
                        "long_put_symbol": &best.put.long.symbol,
                        "short_call_symbol": &best.call.short.symbol,
                        "long_call_symbol": &best.call.long.symbol,
                        "reasons": &admission.reasons,
                    }),
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": "index_iron_condor_entry",
                        "result": "admission_rejected",
                        "short_put_symbol": &best.put.short.symbol,
                        "long_put_symbol": &best.put.long.symbol,
                        "short_call_symbol": &best.call.short.symbol,
                        "long_call_symbol": &best.call.long.symbol,
                        "reasons": admission.reasons,
                    }),
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
            emit_operator_event(
                "scanner_diagnostic",
                json!({
                    "underlying": underlying,
                    "strategy": "index_iron_condor_entry",
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
            );
            record_debit_candidate_ledger(
                config,
                trade_date,
                underlying,
                strategy_name,
                &result.candidates,
            );
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

            let admission = check_option_spread_entry_admission(
                &account,
                &positions,
                &open_orders,
                &[&best.long.symbol, &best.short.symbol],
            );
            if !admission.allowed {
                println!(
                    "{underlying}: admission_rejected strategy={} long={} short={} reasons={}",
                    debit_spread_strategy_name(*kind),
                    best.long.symbol,
                    best.short.symbol,
                    admission.reasons.join(" | "),
                );
                record_scanner_ledger_result(
                    config,
                    trade_date,
                    json!({
                        "underlying": underlying,
                        "strategy": debit_spread_strategy_name(*kind),
                        "result": "admission_rejected",
                        "long_symbol": &best.long.symbol,
                        "short_symbol": &best.short.symbol,
                        "reasons": &admission.reasons,
                    }),
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": debit_spread_strategy_name(*kind),
                        "result": "admission_rejected",
                        "long_symbol": &best.long.symbol,
                        "short_symbol": &best.short.symbol,
                        "reasons": admission.reasons,
                    }),
                );
                continue;
            }

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

            if selected
                .as_ref()
                .is_none_or(|current| best.score > current.score())
            {
                selected = Some(SelectedIndexEntry::Debit(SelectedDebitEntry {
                    underlying: underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                }));
            }
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
            );
            record_naked_candidate_ledger(
                config,
                trade_date,
                underlying,
                strategy_name,
                options_buying_power,
                &result.candidates,
            );
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

            let admission = check_option_spread_entry_admission(
                &account,
                &positions,
                &open_orders,
                &[&best.short.symbol],
            );
            if !admission.allowed {
                println!(
                    "{underlying}: admission_rejected strategy={} short={} reasons={}",
                    naked_option_strategy_name(*kind),
                    best.short.symbol,
                    admission.reasons.join(" | "),
                );
                record_scanner_ledger_result(
                    config,
                    trade_date,
                    json!({
                        "underlying": underlying,
                        "strategy": naked_option_strategy_name(*kind),
                        "result": "admission_rejected",
                        "short_symbol": &best.short.symbol,
                        "reasons": &admission.reasons,
                    }),
                );
                emit_operator_event(
                    "scanner_diagnostic",
                    json!({
                        "underlying": underlying,
                        "strategy": naked_option_strategy_name(*kind),
                        "result": "admission_rejected",
                        "short_symbol": &best.short.symbol,
                        "reasons": admission.reasons,
                    }),
                );
                continue;
            }

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

            if selected
                .as_ref()
                .is_none_or(|current| best.score > current.score())
            {
                selected = Some(SelectedIndexEntry::NakedOption(SelectedNakedOptionEntry {
                    underlying: underlying.clone(),
                    kind: *kind,
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
    debit_kinds: Vec<DebitSpreadKind>,
    naked_kinds: Vec<NakedOptionKind>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RuntimeConfigFile {
    extends: Option<PathBuf>,
    runtime: RuntimeSection,
    index: IndexSection,
    scanner: ScannerSection,
    iron_condor: IronCondorSection,
    debit_scanner: DebitScannerSection,
    naked_scanner: NakedScannerSection,
    naked_1_3dte_scanner: NakedScannerSection,
    management: ManagementSection,
    risk: RiskSection,
}

impl RuntimeConfigFile {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            extends: None,
            runtime: self.runtime.merge_parent(parent.runtime),
            index: self.index.merge_parent(parent.index),
            scanner: self.scanner.merge_parent(parent.scanner),
            iron_condor: self.iron_condor.merge_parent(parent.iron_condor),
            debit_scanner: self.debit_scanner.merge_parent(parent.debit_scanner),
            naked_scanner: self.naked_scanner.merge_parent(parent.naked_scanner),
            naked_1_3dte_scanner: self
                .naked_1_3dte_scanner
                .merge_parent(parent.naked_1_3dte_scanner),
            management: self.management.merge_parent(parent.management),
            risk: self.risk.merge_parent(parent.risk),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RuntimeSection {
    strategies: Vec<String>,
    dry_run_strategies: Vec<String>,
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
    candidate_ledger_enabled: Option<bool>,
    candidate_ledger_dir: Option<PathBuf>,
    candidate_ledger_max_candidates: Option<usize>,
}

impl RuntimeSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            strategies: merge_vec(self.strategies, parent.strategies),
            dry_run_strategies: merge_vec(self.dry_run_strategies, parent.dry_run_strategies),
            max_iterations: self.max_iterations.or(parent.max_iterations),
            interval_secs: self.interval_secs.or(parent.interval_secs),
            submit: self.submit.or(parent.submit),
            manage: self.manage.or(parent.manage),
            close: self.close.or(parent.close),
            kill_switch: self.kill_switch.or(parent.kill_switch),
            force_flatten: self.force_flatten.or(parent.force_flatten),
            cancel_after_accept: self.cancel_after_accept.or(parent.cancel_after_accept),
            ignore_entry_window: self.ignore_entry_window.or(parent.ignore_entry_window),
            state_path: self.state_path.or(parent.state_path),
            candidate_ledger_enabled: self
                .candidate_ledger_enabled
                .or(parent.candidate_ledger_enabled),
            candidate_ledger_dir: self.candidate_ledger_dir.or(parent.candidate_ledger_dir),
            candidate_ledger_max_candidates: self
                .candidate_ledger_max_candidates
                .or(parent.candidate_ledger_max_candidates),
        }
    }
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

impl IndexSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            underlyings: merge_vec(self.underlyings, parent.underlyings),
            quantity: self.quantity.or(parent.quantity),
            entry_start: self.entry_start.or(parent.entry_start),
            entry_end: self.entry_end.or(parent.entry_end),
            entry_timezone: self.entry_timezone.or(parent.entry_timezone),
        }
    }
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
    min_credit_to_width: Option<f64>,
}

impl ScannerSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_dte: self.min_dte.or(parent.min_dte),
            max_dte: self.max_dte.or(parent.max_dte),
            short_delta_min: self.short_delta_min.or(parent.short_delta_min),
            short_delta_max: self.short_delta_max.or(parent.short_delta_max),
            widths: self.widths.or(parent.widths),
            min_open_interest: self.min_open_interest.or(parent.min_open_interest),
            max_leg_spread_pct: self.max_leg_spread_pct.or(parent.max_leg_spread_pct),
            min_return_on_risk: self.min_return_on_risk.or(parent.min_return_on_risk),
            min_credit_to_width: self.min_credit_to_width.or(parent.min_credit_to_width),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IronCondorSection {
    min_return_on_risk: Option<f64>,
    wing_min_return_on_risk: Option<f64>,
    require_equal_widths: Option<bool>,
}

impl IronCondorSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_return_on_risk: self.min_return_on_risk.or(parent.min_return_on_risk),
            wing_min_return_on_risk: self
                .wing_min_return_on_risk
                .or(parent.wing_min_return_on_risk),
            require_equal_widths: self.require_equal_widths.or(parent.require_equal_widths),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DebitScannerSection {
    min_dte: Option<i64>,
    max_dte: Option<i64>,
    long_delta_min: Option<f64>,
    long_delta_max: Option<f64>,
    widths: Option<Vec<f64>>,
    min_open_interest: Option<u64>,
    max_leg_spread_pct: Option<f64>,
    max_debit_to_width: Option<f64>,
    min_debit_to_width: Option<f64>,
    min_reward_to_risk: Option<f64>,
}

impl DebitScannerSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_dte: self.min_dte.or(parent.min_dte),
            max_dte: self.max_dte.or(parent.max_dte),
            long_delta_min: self.long_delta_min.or(parent.long_delta_min),
            long_delta_max: self.long_delta_max.or(parent.long_delta_max),
            widths: self.widths.or(parent.widths),
            min_open_interest: self.min_open_interest.or(parent.min_open_interest),
            max_leg_spread_pct: self.max_leg_spread_pct.or(parent.max_leg_spread_pct),
            max_debit_to_width: self.max_debit_to_width.or(parent.max_debit_to_width),
            min_debit_to_width: self.min_debit_to_width.or(parent.min_debit_to_width),
            min_reward_to_risk: self.min_reward_to_risk.or(parent.min_reward_to_risk),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NakedScannerSection {
    min_dte: Option<i64>,
    max_dte: Option<i64>,
    short_delta_min: Option<f64>,
    short_delta_max: Option<f64>,
    min_open_interest: Option<u64>,
    max_spread_pct: Option<f64>,
    min_credit: Option<f64>,
    min_bid_size: Option<u64>,
    min_ask_size: Option<u64>,
    min_daily_volume: Option<u64>,
    min_implied_volatility: Option<f64>,
    max_implied_volatility: Option<f64>,
    min_annualized_premium_yield: Option<f64>,
    max_buying_power_usage_pct: Option<f64>,
    min_return_on_buying_power: Option<f64>,
    min_breakeven_pop: Option<f64>,
    max_probability_of_touch: Option<f64>,
    min_distance_to_breakeven_pct: Option<f64>,
    min_expected_move_coverage: Option<f64>,
    min_score: Option<f64>,
}

impl NakedScannerSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            min_dte: self.min_dte.or(parent.min_dte),
            max_dte: self.max_dte.or(parent.max_dte),
            short_delta_min: self.short_delta_min.or(parent.short_delta_min),
            short_delta_max: self.short_delta_max.or(parent.short_delta_max),
            min_open_interest: self.min_open_interest.or(parent.min_open_interest),
            max_spread_pct: self.max_spread_pct.or(parent.max_spread_pct),
            min_credit: self.min_credit.or(parent.min_credit),
            min_bid_size: self.min_bid_size.or(parent.min_bid_size),
            min_ask_size: self.min_ask_size.or(parent.min_ask_size),
            min_daily_volume: self.min_daily_volume.or(parent.min_daily_volume),
            min_implied_volatility: self
                .min_implied_volatility
                .or(parent.min_implied_volatility),
            max_implied_volatility: self
                .max_implied_volatility
                .or(parent.max_implied_volatility),
            min_annualized_premium_yield: self
                .min_annualized_premium_yield
                .or(parent.min_annualized_premium_yield),
            max_buying_power_usage_pct: self
                .max_buying_power_usage_pct
                .or(parent.max_buying_power_usage_pct),
            min_return_on_buying_power: self
                .min_return_on_buying_power
                .or(parent.min_return_on_buying_power),
            min_breakeven_pop: self.min_breakeven_pop.or(parent.min_breakeven_pop),
            max_probability_of_touch: self
                .max_probability_of_touch
                .or(parent.max_probability_of_touch),
            min_distance_to_breakeven_pct: self
                .min_distance_to_breakeven_pct
                .or(parent.min_distance_to_breakeven_pct),
            min_expected_move_coverage: self
                .min_expected_move_coverage
                .or(parent.min_expected_move_coverage),
            min_score: self.min_score.or(parent.min_score),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ManagementSection {
    stale_entry_secs: Option<u64>,
    stale_close_secs: Option<u64>,
    close_regular_hours_only: Option<bool>,
    close_start: Option<String>,
    close_end: Option<String>,
    close_price_cushion: Option<f64>,
    max_close_attempts: Option<u32>,
    close_reprice_cooldown_secs: Option<u64>,
    profit_target_close_fraction: Option<f64>,
    stop_loss_close_multiple: Option<f64>,
    max_hold_secs: Option<u64>,
    expiration_exit_days: Option<i64>,
}

impl ManagementSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            stale_entry_secs: self.stale_entry_secs.or(parent.stale_entry_secs),
            stale_close_secs: self.stale_close_secs.or(parent.stale_close_secs),
            close_regular_hours_only: self
                .close_regular_hours_only
                .or(parent.close_regular_hours_only),
            close_start: self.close_start.or(parent.close_start),
            close_end: self.close_end.or(parent.close_end),
            close_price_cushion: self.close_price_cushion.or(parent.close_price_cushion),
            max_close_attempts: self.max_close_attempts.or(parent.max_close_attempts),
            close_reprice_cooldown_secs: self
                .close_reprice_cooldown_secs
                .or(parent.close_reprice_cooldown_secs),
            profit_target_close_fraction: self
                .profit_target_close_fraction
                .or(parent.profit_target_close_fraction),
            stop_loss_close_multiple: self
                .stop_loss_close_multiple
                .or(parent.stop_loss_close_multiple),
            max_hold_secs: self.max_hold_secs.or(parent.max_hold_secs),
            expiration_exit_days: self.expiration_exit_days.or(parent.expiration_exit_days),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RiskSection {
    max_active_entries: Option<usize>,
    max_daily_submits: Option<usize>,
    max_open_orders: Option<usize>,
    max_active_entries_per_underlying: Option<usize>,
    max_active_entries_per_sector: Option<usize>,
    sectors: BTreeMap<String, String>,
}

impl RiskSection {
    fn merge_parent(self, parent: Self) -> Self {
        Self {
            max_active_entries: self.max_active_entries.or(parent.max_active_entries),
            max_daily_submits: self.max_daily_submits.or(parent.max_daily_submits),
            max_open_orders: self.max_open_orders.or(parent.max_open_orders),
            max_active_entries_per_underlying: self
                .max_active_entries_per_underlying
                .or(parent.max_active_entries_per_underlying),
            max_active_entries_per_sector: self
                .max_active_entries_per_sector
                .or(parent.max_active_entries_per_sector),
            sectors: merge_map(self.sectors, parent.sectors),
        }
    }
}

fn load_runtime_config_file_from_env() -> anyhow::Result<RuntimeConfigFile> {
    if let Some(path) = env::var_os("ALPACA_CONFIG_PATH") {
        let path = PathBuf::from(path);
        return load_runtime_config_file(&path, true);
    }
    if let Some(path) = current_fleet_account_config_path()? {
        return load_runtime_config_file(&path, true);
    }

    let path = default_config_path();
    if path.exists() {
        load_runtime_config_file(&path, false)
    } else {
        Ok(RuntimeConfigFile::default())
    }
}

fn current_fleet_account_config_path() -> anyhow::Result<Option<PathBuf>> {
    let Some(fleet) = load_fleet_config_from_env()? else {
        return Ok(None);
    };
    Ok(fleet
        .current_account()
        .and_then(|account| fleet.config_file(account)))
}

fn load_runtime_config_file(path: &Path, explicit: bool) -> anyhow::Result<RuntimeConfigFile> {
    match path.try_exists() {
        Ok(false) if !explicit => return Ok(RuntimeConfigFile::default()),
        Ok(_) => {}
        Err(error) => {
            anyhow::bail!(
                "failed to inspect Alpaca runtime config {}: {error}",
                path.display()
            );
        }
    }
    load_runtime_config_file_with_extends(path, &mut Vec::new())
}

fn load_runtime_config_file_with_extends(
    path: &Path,
    stack: &mut Vec<PathBuf>,
) -> anyhow::Result<RuntimeConfigFile> {
    let identity = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&identity) {
        let chain = stack
            .iter()
            .chain(std::iter::once(&identity))
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" -> ");
        anyhow::bail!("circular Alpaca runtime config inheritance: {chain}");
    }

    let raw = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "failed to read Alpaca runtime config {}: {error}",
            path.display()
        )
    })?;
    stack.push(identity);
    let result = (|| {
        let config = parse_runtime_config_at_path(&raw, path)?;
        if let Some(parent_path) = config.extends.as_deref() {
            let parent_path = resolve_config_extends_path(path, parent_path);
            let parent = load_runtime_config_file_with_extends(&parent_path, stack)?;
            Ok(config.merge_parent(parent))
        } else {
            Ok(config)
        }
    })();
    stack.pop();
    result
}

#[cfg(test)]
fn parse_runtime_config(raw: &str) -> anyhow::Result<RuntimeConfigFile> {
    toml::from_str(raw).map_err(|error| anyhow::anyhow!("invalid Alpaca runtime config: {error}"))
}

fn parse_runtime_config_at_path(raw: &str, path: &Path) -> anyhow::Result<RuntimeConfigFile> {
    toml::from_str(raw).map_err(|error| {
        anyhow::anyhow!("invalid Alpaca runtime config {}: {error}", path.display())
    })
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
    let dry_run_strategy_values = env::var("ALPACA_DRY_RUN_STRATEGIES")
        .ok()
        .map(|value| split_strings([value]))
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| file.runtime.dry_run_strategies.clone());
    let dry_run_strategy_config = dry_run_strategy_config_from_values(dry_run_strategy_values)?;
    let scanner = scanner_config_from_file(&file.scanner);
    let stale_entry_secs = file.management.stale_entry_secs.unwrap_or(900);
    let fleet = load_fleet_config_from_env()?;
    let mut config = IndexCreditConfig {
        underlyings: underlyings_from_sources(cli_underlyings, &file.index),
        spread_kinds: strategy_config.credit_kinds,
        iron_condor_enabled: strategy_config.iron_condor_enabled,
        debit_kinds: strategy_config.debit_kinds,
        naked_kinds: strategy_config.naked_kinds,
        dry_run_spread_kinds: dry_run_strategy_config.credit_kinds,
        iron_condor_dry_run: dry_run_strategy_config.iron_condor_enabled,
        dry_run_debit_kinds: dry_run_strategy_config.debit_kinds,
        dry_run_naked_kinds: dry_run_strategy_config.naked_kinds,
        max_active_entries: env_parse("ALPACA_MAX_ACTIVE_ENTRIES").or(file.risk.max_active_entries),
        max_daily_submits: env_parse("ALPACA_MAX_DAILY_SUBMITS").or(file.risk.max_daily_submits),
        max_open_orders: env_parse("ALPACA_MAX_OPEN_ORDERS").or(file.risk.max_open_orders),
        max_active_entries_per_underlying: env_parse("ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING")
            .or(file.risk.max_active_entries_per_underlying),
        max_active_entries_per_sector: env_parse("ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR")
            .or(file.risk.max_active_entries_per_sector),
        sectors: sector_map_from_file(file.risk.sectors),
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
        stale_entry_secs,
        stale_close_secs: file.management.stale_close_secs.unwrap_or(stale_entry_secs),
        close_enabled: env_bool("ALPACA_CLOSE")
            .or(file.runtime.close)
            .unwrap_or(false),
        close_regular_hours_only: env_bool("ALPACA_CLOSE_REGULAR_HOURS_ONLY")
            .or(file.management.close_regular_hours_only)
            .unwrap_or(true),
        close_start: parse_time_value(file.management.close_start.as_deref(), "09:30")?,
        close_end: parse_time_value(file.management.close_end.as_deref(), "16:00")?,
        close_price_cushion: env_parse("ALPACA_CLOSE_PRICE_CUSHION")
            .or(file.management.close_price_cushion)
            .unwrap_or(0.0)
            .max(0.0),
        max_close_attempts: env_parse("ALPACA_MAX_CLOSE_ATTEMPTS")
            .or(file.management.max_close_attempts)
            .unwrap_or(3),
        close_reprice_cooldown_secs: env_parse("ALPACA_CLOSE_REPRICE_COOLDOWN_SECS")
            .or(file.management.close_reprice_cooldown_secs)
            .unwrap_or(30),
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
        candidate_ledger_enabled: file.runtime.candidate_ledger_enabled.unwrap_or(true),
        candidate_ledger_dir: file.runtime.candidate_ledger_dir.unwrap_or_default(),
        candidate_ledger_max_candidates: file.runtime.candidate_ledger_max_candidates.unwrap_or(10),
        iron_condor_scanner: iron_condor_scanner_config_from_file(&scanner, &file.iron_condor),
        debit_scanner: debit_scanner_config_from_file(&file.debit_scanner),
        naked_scanner: naked_scanner_config_from_file(&file.naked_scanner),
        naked_1_3dte_scanner: naked_1_3dte_scanner_config_from_file(&file.naked_1_3dte_scanner),
        scanner,
        fleet,
        fleet_account_id: None,
        fleet_policy_blocks: Vec::new(),
    };
    apply_fleet_policy(&mut config);
    if config.candidate_ledger_dir.as_os_str().is_empty() {
        config.candidate_ledger_dir =
            default_candidate_ledger_dir(&config.state_path, config.fleet_account_id.as_deref());
    }
    Ok(config)
}

fn strategy_config_from_values(values: Vec<String>) -> anyhow::Result<StrategyConfig> {
    let mut kinds = Vec::new();
    let mut iron_condor_enabled = false;
    let mut debit_kinds = Vec::new();
    let mut naked_kinds = Vec::new();
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
            "call_debit" | "index_call_debit_entry" | "earnings_call_debit_entry" => {
                debit_kinds.push(DebitSpreadKind::Call);
            }
            "put_debit" | "index_put_debit_entry" | "earnings_put_debit_entry" => {
                debit_kinds.push(DebitSpreadKind::Put);
            }
            "debit" | "long_premium" | "directional" => {
                debit_kinds.push(DebitSpreadKind::Call);
                debit_kinds.push(DebitSpreadKind::Put);
            }
            "naked_call" | "short_call" | "index_naked_call_entry" => {
                naked_kinds.push(NakedOptionKind::Call);
            }
            "naked_put" | "short_put" | "index_naked_put_entry" => {
                naked_kinds.push(NakedOptionKind::Put);
            }
            "naked_call_1_3dte" | "short_call_1_3dte" | "index_naked_call_1_3dte_entry" => {
                naked_kinds.push(NakedOptionKind::CallOneToThreeDte);
            }
            "naked_put_1_3dte" | "short_put_1_3dte" | "index_naked_put_1_3dte_entry" => {
                naked_kinds.push(NakedOptionKind::PutOneToThreeDte);
            }
            "naked_1_3dte" | "undefined_risk_1_3dte" | "short_premium_1_3dte" => {
                naked_kinds.push(NakedOptionKind::CallOneToThreeDte);
                naked_kinds.push(NakedOptionKind::PutOneToThreeDte);
            }
            "naked" | "undefined_risk" | "short_premium_undefined" => {
                naked_kinds.push(NakedOptionKind::Call);
                naked_kinds.push(NakedOptionKind::Put);
            }
            other => anyhow::bail!("unsupported Alpaca strategy value {other}"),
        }
    }
    if kinds.is_empty() && !iron_condor_enabled && debit_kinds.is_empty() && naked_kinds.is_empty()
    {
        kinds.push(CreditSpreadKind::Put);
    }
    kinds.sort_by_key(|kind| match kind {
        CreditSpreadKind::Put => 0,
        CreditSpreadKind::Call => 1,
    });
    kinds.dedup();
    debit_kinds.sort_by_key(|kind| match kind {
        DebitSpreadKind::Call => 0,
        DebitSpreadKind::Put => 1,
    });
    debit_kinds.dedup();
    naked_kinds.sort_by_key(|kind| match kind {
        NakedOptionKind::Call => 0,
        NakedOptionKind::Put => 1,
        NakedOptionKind::CallOneToThreeDte => 2,
        NakedOptionKind::PutOneToThreeDte => 3,
    });
    naked_kinds.dedup();
    Ok(StrategyConfig {
        credit_kinds: kinds,
        iron_condor_enabled,
        debit_kinds,
        naked_kinds,
    })
}

fn dry_run_strategy_config_from_values(values: Vec<String>) -> anyhow::Result<StrategyConfig> {
    if values.is_empty() {
        return Ok(StrategyConfig {
            credit_kinds: Vec::new(),
            iron_condor_enabled: false,
            debit_kinds: Vec::new(),
            naked_kinds: Vec::new(),
        });
    }
    strategy_config_from_values(values)
}

fn apply_fleet_policy(config: &mut IndexCreditConfig) {
    let Some(fleet) = &config.fleet else {
        return;
    };
    if let Some(account) = fleet.current_account() {
        config.fleet_account_id = Some(account.id.clone());
        if !account.enabled {
            config
                .fleet_policy_blocks
                .push(format!("fleet_account_disabled:{}", account.id));
        }
        if has_defined_risk_strategies(config) && !account.permissions.defined_risk {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_defined_risk_required:{}",
                account.id
            ));
        }
        if !config.debit_kinds.is_empty() && !account.permissions.long_premium {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_long_premium_required:{}",
                account.id
            ));
        }
        if has_undefined_risk_strategies(config) && !account.permissions.undefined_risk {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_undefined_risk_required:{}",
                account.id
            ));
        }
        if config.naked_kinds.iter().any(|kind| kind.is_call()) && !account.permissions.naked_calls
        {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_naked_calls_required:{}",
                account.id
            ));
        }
        if config.naked_kinds.iter().any(|kind| kind.is_put()) && !account.permissions.naked_puts {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_naked_puts_required:{}",
                account.id
            ));
        }
        if let Some(limit) = account.risk_budget.max_active_entries {
            config.max_active_entries = Some(min_limit(config.max_active_entries, limit));
        }
        if let Some(limit) = account.risk_budget.max_buying_power_pct {
            config.naked_scanner.max_buying_power_usage_pct =
                config.naked_scanner.max_buying_power_usage_pct.min(limit);
            config.naked_1_3dte_scanner.max_buying_power_usage_pct = config
                .naked_1_3dte_scanner
                .max_buying_power_usage_pct
                .min(limit);
        }
    } else {
        config
            .fleet_policy_blocks
            .push("fleet_account_unmatched".to_string());
    }

    if fleet.config.fleet.kill_switch {
        config
            .fleet_policy_blocks
            .push("fleet_kill_switch_enabled".to_string());
    }
    if !config.fleet_policy_blocks.is_empty() {
        config.kill_switch = true;
        config.submit_enabled = false;
    }
}

fn has_defined_risk_strategies(config: &IndexCreditConfig) -> bool {
    !config.spread_kinds.is_empty() || config.iron_condor_enabled
}

fn has_undefined_risk_strategies(config: &IndexCreditConfig) -> bool {
    !config.naked_kinds.is_empty()
}

fn min_limit(current: Option<usize>, fleet_limit: usize) -> usize {
    current.map_or(fleet_limit, |current| current.min(fleet_limit))
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

fn record_scanner_ledger_result(config: &IndexCreditConfig, trade_date: &str, payload: Value) {
    config.record_candidate_ledger(trade_date, "scanner_result", payload);
}

fn record_credit_candidate_ledger(
    config: &IndexCreditConfig,
    trade_date: &str,
    underlying: &str,
    strategy: &str,
    candidates: &[SpreadCandidate],
) {
    for (index, candidate) in candidates
        .iter()
        .take(config.candidate_ledger_candidate_limit(candidates.len()))
        .enumerate()
    {
        let payload =
            credit_candidate_ledger_payload(underlying, strategy, Some(index + 1), candidate);
        config.record_candidate_ledger(trade_date, "candidate", payload.clone());
        record_high_score_candidate_alert(
            config,
            trade_date,
            strategy,
            underlying,
            &candidate.short.symbol,
            &[&candidate.short.symbol, &candidate.long.symbol],
            candidate.score,
            payload,
        );
    }
}

fn record_debit_candidate_ledger(
    config: &IndexCreditConfig,
    trade_date: &str,
    underlying: &str,
    strategy: &str,
    candidates: &[DebitSpreadCandidate],
) {
    for (index, candidate) in candidates
        .iter()
        .take(config.candidate_ledger_candidate_limit(candidates.len()))
        .enumerate()
    {
        let payload =
            debit_candidate_ledger_payload(underlying, strategy, Some(index + 1), candidate);
        config.record_candidate_ledger(trade_date, "candidate", payload.clone());
        record_high_score_candidate_alert(
            config,
            trade_date,
            strategy,
            underlying,
            &candidate.long.symbol,
            &[&candidate.long.symbol, &candidate.short.symbol],
            candidate.score,
            payload,
        );
    }
}

fn record_iron_condor_candidate_ledger(
    config: &IndexCreditConfig,
    trade_date: &str,
    underlying: &str,
    candidates: &[IronCondorCandidate],
) {
    for (index, candidate) in candidates
        .iter()
        .take(config.candidate_ledger_candidate_limit(candidates.len()))
        .enumerate()
    {
        let payload = iron_condor_candidate_ledger_payload(underlying, Some(index + 1), candidate);
        config.record_candidate_ledger(trade_date, "candidate", payload.clone());
        record_high_score_candidate_alert(
            config,
            trade_date,
            "index_iron_condor_entry",
            underlying,
            &candidate.put.short.symbol,
            &[
                &candidate.put.short.symbol,
                &candidate.put.long.symbol,
                &candidate.call.short.symbol,
                &candidate.call.long.symbol,
            ],
            candidate.score,
            payload,
        );
    }
}

fn record_naked_candidate_ledger(
    config: &IndexCreditConfig,
    trade_date: &str,
    underlying: &str,
    strategy: &str,
    options_buying_power: Option<f64>,
    candidates: &[NakedOptionCandidate],
) {
    for (index, candidate) in candidates
        .iter()
        .take(config.candidate_ledger_candidate_limit(candidates.len()))
        .enumerate()
    {
        let payload = naked_candidate_ledger_payload(
            underlying,
            strategy,
            options_buying_power,
            Some(index + 1),
            candidate,
        );
        config.record_candidate_ledger(trade_date, "candidate", payload.clone());
        record_high_score_candidate_alert(
            config,
            trade_date,
            strategy,
            underlying,
            &candidate.short.symbol,
            &[&candidate.short.symbol],
            candidate.score,
            payload,
        );
    }
}

/// Builds the shared candidate-ledger payload for a credit-spread candidate.
#[must_use]
pub fn credit_candidate_ledger_payload(
    underlying: &str,
    strategy: &str,
    rank: Option<usize>,
    candidate: &SpreadCandidate,
) -> Value {
    let mut payload = json!({
        "underlying": underlying,
        "strategy": strategy,
        "candidate_type": "credit_spread",
        "short_symbol": &candidate.short.symbol,
        "long_symbol": &candidate.long.symbol,
        "width": candidate.width,
        "credit": candidate.credit,
        "max_loss": candidate.max_loss,
        "return_on_risk": candidate.return_on_risk,
        "score": candidate.score,
        "short": scored_contract_ledger_payload(&candidate.short),
        "long": scored_contract_ledger_payload(&candidate.long),
    });
    insert_optional_rank(&mut payload, rank);
    payload
}

/// Builds the shared candidate-ledger payload for a debit-spread candidate.
#[must_use]
pub fn debit_candidate_ledger_payload(
    underlying: &str,
    strategy: &str,
    rank: Option<usize>,
    candidate: &DebitSpreadCandidate,
) -> Value {
    let mut payload = json!({
        "underlying": underlying,
        "strategy": strategy,
        "candidate_type": "debit_spread",
        "long_symbol": &candidate.long.symbol,
        "short_symbol": &candidate.short.symbol,
        "width": candidate.width,
        "debit": candidate.debit,
        "max_profit": candidate.max_profit,
        "max_loss": candidate.max_loss,
        "reward_to_risk": candidate.reward_to_risk,
        "score": candidate.score,
        "long": scored_contract_ledger_payload(&candidate.long),
        "short": scored_contract_ledger_payload(&candidate.short),
    });
    insert_optional_rank(&mut payload, rank);
    payload
}

/// Builds the shared candidate-ledger payload for an iron-condor candidate.
#[must_use]
pub fn iron_condor_candidate_ledger_payload(
    underlying: &str,
    rank: Option<usize>,
    candidate: &IronCondorCandidate,
) -> Value {
    let mut payload = json!({
        "underlying": underlying,
        "strategy": "index_iron_condor_entry",
        "candidate_type": "iron_condor",
        "short_put_symbol": &candidate.put.short.symbol,
        "long_put_symbol": &candidate.put.long.symbol,
        "short_call_symbol": &candidate.call.short.symbol,
        "long_call_symbol": &candidate.call.long.symbol,
        "credit": candidate.credit,
        "max_loss": candidate.max_loss,
        "return_on_risk": candidate.return_on_risk,
        "score": candidate.score,
        "put": spread_candidate_ledger_payload(&candidate.put),
        "call": spread_candidate_ledger_payload(&candidate.call),
    });
    insert_optional_rank(&mut payload, rank);
    payload
}

/// Builds the shared candidate-ledger payload for a naked-option candidate.
#[must_use]
pub fn naked_candidate_ledger_payload(
    underlying: &str,
    strategy: &str,
    options_buying_power: Option<f64>,
    rank: Option<usize>,
    candidate: &NakedOptionCandidate,
) -> Value {
    let mut payload = json!({
        "underlying": underlying,
        "strategy": strategy,
        "candidate_type": "naked_option",
        "short_symbol": &candidate.short.symbol,
        "credit": candidate.credit,
        "account_options_buying_power": options_buying_power,
        "capital_requirement_model": candidate.capital_requirement_model.as_str(),
        "estimated_buying_power_requirement": candidate.estimated_buying_power_requirement,
        "buying_power_usage_pct": candidate.buying_power_usage_pct,
        "return_on_buying_power": candidate.return_on_buying_power,
        "annualized_premium_yield": annualized_premium_yield(
            candidate.credit,
            candidate.short.strike,
            candidate.short.dte,
        ),
        "score": candidate.score,
        "short": scored_contract_ledger_payload(&candidate.short),
    });
    insert_optional_rank(&mut payload, rank);
    payload
}

/// Returns a stable key for one candidate independent of account and trade date.
#[must_use]
pub fn candidate_alert_identity_key(strategy: &str, underlying: &str, symbols: &[&str]) -> String {
    format!(
        "{}|{}|{}",
        strategy,
        underlying,
        symbols
            .iter()
            .copied()
            .filter(|symbol| !symbol.is_empty())
            .collect::<Vec<_>>()
            .join("|")
    )
}

/// Returns a stable key for a typed candidate alert.
#[must_use]
pub fn candidate_alert_key(alert_type: &str, identity_key: &str) -> String {
    format!("{alert_type}|{identity_key}")
}

fn record_high_score_candidate_alert(
    config: &IndexCreditConfig,
    trade_date: &str,
    strategy: &str,
    underlying: &str,
    primary_symbol: &str,
    symbols: &[&str],
    score: f64,
    payload: Value,
) {
    if score < high_score_candidate_alert_threshold(strategy, payload_candidate_type(&payload)) {
        return;
    }
    let identity_key = candidate_alert_identity_key(strategy, underlying, symbols);
    let mut alert_payload = payload;
    insert_string_field(
        &mut alert_payload,
        "candidate_identity_key",
        identity_key.clone(),
    );
    insert_string_field(
        &mut alert_payload,
        "primary_symbol",
        primary_symbol.to_string(),
    );
    config.record_candidate_alert_ledger(
        trade_date,
        HIGH_SCORE_CANDIDATE_ALERT,
        "info",
        candidate_alert_key(HIGH_SCORE_CANDIDATE_ALERT, &identity_key),
        alert_payload,
    );
}

fn high_score_candidate_alert_threshold(strategy: &str, candidate_type: Option<&str>) -> f64 {
    if strategy.contains("naked") && strategy.contains("1_3dte") {
        CANDIDATE_ALERT_NAKED_ONE_TO_THREE_DTE_MIN_SCORE
    } else if strategy.contains("naked") || candidate_type == Some("naked_option") {
        CANDIDATE_ALERT_NAKED_MIN_SCORE
    } else if candidate_type == Some("iron_condor") {
        CANDIDATE_ALERT_IRON_CONDOR_MIN_SCORE
    } else if candidate_type == Some("debit_spread") {
        CANDIDATE_ALERT_DEBIT_MIN_SCORE
    } else {
        CANDIDATE_ALERT_CREDIT_MIN_SCORE
    }
}

fn payload_candidate_type(payload: &Value) -> Option<&str> {
    payload.get("candidate_type").and_then(Value::as_str)
}

fn insert_optional_rank(payload: &mut Value, rank: Option<usize>) {
    if let Some(rank) = rank {
        insert_value_field(payload, "rank", Value::from(rank));
    }
}

fn insert_string_field(payload: &mut Value, key: &str, value: String) {
    insert_value_field(payload, key, Value::String(value));
}

fn insert_value_field(payload: &mut Value, key: &str, value: Value) {
    if let Value::Object(fields) = payload {
        fields.insert(key.to_string(), value);
    }
}

fn spread_candidate_ledger_payload(candidate: &SpreadCandidate) -> Value {
    json!({
        "short_symbol": &candidate.short.symbol,
        "long_symbol": &candidate.long.symbol,
        "width": candidate.width,
        "credit": candidate.credit,
        "max_loss": candidate.max_loss,
        "return_on_risk": candidate.return_on_risk,
        "score": candidate.score,
        "short": scored_contract_ledger_payload(&candidate.short),
        "long": scored_contract_ledger_payload(&candidate.long),
    })
}

fn scored_contract_ledger_payload(contract: &ScoredContract) -> Value {
    json!({
        "symbol": &contract.symbol,
        "expiration_date": &contract.expiration_date,
        "dte": contract.dte,
        "strike": contract.strike,
        "bid": contract.bid,
        "ask": contract.ask,
        "delta_abs": contract.delta_abs,
        "spread_pct": contract.spread_pct,
        "bid_size": contract.bid_size,
        "ask_size": contract.ask_size,
        "volume": contract.volume,
        "open_interest": contract.open_interest,
        "implied_volatility": contract.implied_volatility,
        "metrics": contract.metrics.as_ref().map(option_metrics_ledger_payload),
    })
}

fn option_metrics_ledger_payload(metrics: &OptionCandidateMetrics) -> Value {
    json!({
        "underlying_price": metrics.underlying_price,
        "breakeven": metrics.breakeven,
        "strike_itm_probability": metrics.strike_itm_probability,
        "delta_pop_proxy": metrics.delta_pop_proxy,
        "breakeven_pop": metrics.breakeven_pop,
        "probability_of_touch_est": metrics.probability_of_touch_est,
        "expected_move": metrics.expected_move,
        "expected_move_pct": metrics.expected_move_pct,
        "distance_to_strike_pct": metrics.distance_to_strike_pct,
        "distance_to_breakeven_pct": metrics.distance_to_breakeven_pct,
        "expected_move_coverage": metrics.expected_move_coverage,
        "capital_requirement_model": metrics.capital_requirement_model.as_str(),
        "estimated_buying_power_requirement": metrics.estimated_buying_power_requirement,
        "return_on_buying_power": metrics.return_on_buying_power,
        "model_delta_abs": metrics.model_delta_abs,
        "model_gamma": metrics.model_gamma,
        "model_theta": metrics.model_theta,
        "model_vega": metrics.model_vega,
    })
}

fn merge_vec<T>(child: Vec<T>, parent: Vec<T>) -> Vec<T> {
    if child.is_empty() { parent } else { child }
}

fn merge_map<K: Ord, V>(child: BTreeMap<K, V>, parent: BTreeMap<K, V>) -> BTreeMap<K, V> {
    let mut merged = parent;
    merged.extend(child);
    merged
}

fn resolve_config_extends_path(config_path: &Path, parent_path: &Path) -> PathBuf {
    if parent_path.is_absolute() {
        parent_path.to_path_buf()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(parent_path)
    }
}

fn no_candidate_reason(
    contract_count: usize,
    snapshot_count: usize,
    scoreable_count: usize,
) -> &'static str {
    if contract_count == 0 {
        "no_contracts"
    } else if snapshot_count == 0 {
        "no_snapshots"
    } else if scoreable_count == 0 {
        "no_scoreable_spreads"
    } else {
        "no_ranked_candidate"
    }
}

fn account_options_buying_power(account: &crate::http::models::AlpacaAccount) -> Option<f64> {
    parse_account_amount(
        account
            .options_buying_power
            .as_deref()
            .or(account.buying_power.as_deref())
            .or(account.cash.as_deref()),
    )
}

fn parse_account_amount(value: Option<&str>) -> Option<f64> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.0)
}

fn format_optional_pct(value: Option<f64>) -> String {
    value.map_or_else(
        || "n/a".to_string(),
        |value| format!("{:.2}%", value * 100.0),
    )
}

fn format_rejection_counts(rejections: &BTreeMap<String, usize>) -> String {
    if rejections.is_empty() {
        return "none".to_string();
    }
    rejections
        .iter()
        .map(|(reason, count)| format!("{reason}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn active_underlying_count(state: &StrategyState, underlying: &str) -> usize {
    state
        .entries
        .iter()
        .filter(|entry| entry.is_active() && entry.underlying.eq_ignore_ascii_case(underlying))
        .count()
}

fn active_sector_count(
    state: &StrategyState,
    sectors: &BTreeMap<String, String>,
    sector: &str,
) -> usize {
    state
        .entries
        .iter()
        .filter(|entry| {
            entry.is_active()
                && sectors
                    .get(&entry.underlying.to_ascii_uppercase())
                    .is_some_and(|entry_sector| entry_sector == sector)
        })
        .count()
}

fn fleet_has_active_underlying_elsewhere(config: &IndexCreditConfig, underlying: &str) -> bool {
    let Some(fleet) = &config.fleet else {
        return false;
    };
    let Some(account_id) = config.fleet_account_id.as_deref() else {
        return false;
    };
    fleet
        .active_underlyings_excluding(account_id)
        .contains(&underlying.to_ascii_uppercase())
}

fn fleet_active_underlying_count(config: &IndexCreditConfig, underlying: &str) -> usize {
    config
        .fleet
        .as_ref()
        .map(|fleet| {
            fleet
                .exposure()
                .active_entries_by_underlying
                .get(&underlying.to_ascii_uppercase())
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn fleet_sector_limit_state(
    config: &IndexCreditConfig,
    underlying: &str,
) -> Option<(String, usize, usize)> {
    let fleet = config.fleet.as_ref()?;
    let limit = fleet.config.fleet.max_active_entries_per_sector?;
    let sector = fleet
        .config
        .fleet
        .sectors
        .get(&underlying.to_ascii_uppercase())?
        .clone();
    let current = fleet
        .exposure()
        .active_entries_by_sector
        .get(&sector)
        .copied()
        .unwrap_or(0);
    Some((sector, current, limit))
}

fn default_underlyings() -> Vec<String> {
    [
        "SPY", "QQQ", "IWM", "DIA", "GLD", "GDX", "SLV", "TLT", "XLE", "XLF", "XLK", "XLV", "XLY",
        "XLI", "XLP", "XLU", "XLB", "XLC", "SMH", "USO", "XOP", "XOM",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn sector_map_from_file(config: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let map = if config.is_empty() {
        default_sector_map()
    } else {
        config
    };
    map.into_iter()
        .map(|(underlying, sector)| (underlying.to_ascii_uppercase(), sector))
        .collect()
}

fn default_sector_map() -> BTreeMap<String, String> {
    [
        ("SPY", "broad_index"),
        ("QQQ", "broad_index"),
        ("IWM", "broad_index"),
        ("DIA", "broad_index"),
        ("GLD", "metals"),
        ("GDX", "metals"),
        ("SLV", "metals"),
        ("TLT", "rates"),
        ("XLE", "energy"),
        ("USO", "energy"),
        ("XOP", "energy"),
        ("XOM", "energy"),
        ("XLB", "materials"),
        ("XLC", "communication_services"),
        ("XLF", "financials"),
        ("XLK", "technology"),
        ("XLV", "healthcare"),
        ("XLY", "consumer_discretionary"),
        ("XLI", "industrials"),
        ("XLP", "consumer_staples"),
        ("XLU", "utilities"),
        ("SMH", "semiconductors"),
    ]
    .into_iter()
    .map(|(underlying, sector)| (underlying.to_string(), sector.to_string()))
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
        min_credit_to_width: config.min_credit_to_width.unwrap_or(0.08),
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

fn debit_scanner_config_from_file(config: &DebitScannerSection) -> DebitSpreadScannerConfig {
    DebitSpreadScannerConfig {
        min_dte: config.min_dte.unwrap_or(5),
        max_dte: config.max_dte.unwrap_or(45),
        long_delta_min: config.long_delta_min.unwrap_or(0.45),
        long_delta_max: config.long_delta_max.unwrap_or(0.65),
        widths: config.widths.clone().unwrap_or_else(|| vec![2.0, 3.0, 5.0]),
        min_open_interest: config.min_open_interest.unwrap_or(200),
        max_leg_spread_pct: config.max_leg_spread_pct.unwrap_or(0.15),
        max_debit_to_width: config.max_debit_to_width.unwrap_or(0.55),
        min_debit_to_width: config.min_debit_to_width.unwrap_or(0.20),
        min_reward_to_risk: config.min_reward_to_risk.unwrap_or(0.75),
    }
}

fn naked_scanner_config_from_file(config: &NakedScannerSection) -> NakedOptionScannerConfig {
    NakedOptionScannerConfig {
        min_dte: config.min_dte.unwrap_or(5),
        max_dte: config.max_dte.unwrap_or(14),
        short_delta_min: config.short_delta_min.unwrap_or(0.10),
        short_delta_max: config.short_delta_max.unwrap_or(0.20),
        min_open_interest: config.min_open_interest.unwrap_or(500),
        max_spread_pct: config.max_spread_pct.unwrap_or(0.12),
        min_credit: config.min_credit.unwrap_or(0.25),
        min_bid_size: config.min_bid_size.unwrap_or(1),
        min_ask_size: config.min_ask_size.unwrap_or(1),
        min_daily_volume: config.min_daily_volume.unwrap_or(1),
        min_implied_volatility: config.min_implied_volatility.unwrap_or(0.0),
        max_implied_volatility: config.max_implied_volatility.unwrap_or(1.50),
        min_annualized_premium_yield: config.min_annualized_premium_yield.unwrap_or(0.10),
        max_buying_power_usage_pct: config.max_buying_power_usage_pct.unwrap_or(0.10),
        min_return_on_buying_power: config.min_return_on_buying_power.unwrap_or(0.0005),
        min_breakeven_pop: config.min_breakeven_pop.unwrap_or(0.65),
        max_probability_of_touch: config.max_probability_of_touch.unwrap_or(0.70),
        min_distance_to_breakeven_pct: config.min_distance_to_breakeven_pct.unwrap_or(0.005),
        min_expected_move_coverage: config.min_expected_move_coverage.unwrap_or(0.75),
        min_score: config.min_score.unwrap_or(55.0),
    }
}

fn naked_1_3dte_scanner_config_from_file(config: &NakedScannerSection) -> NakedOptionScannerConfig {
    NakedOptionScannerConfig {
        min_dte: config.min_dte.unwrap_or(1),
        max_dte: config.max_dte.unwrap_or(3),
        short_delta_min: config.short_delta_min.unwrap_or(0.06),
        short_delta_max: config.short_delta_max.unwrap_or(0.14),
        min_open_interest: config.min_open_interest.unwrap_or(300),
        max_spread_pct: config.max_spread_pct.unwrap_or(0.08),
        min_credit: config.min_credit.unwrap_or(0.12),
        min_bid_size: config.min_bid_size.unwrap_or(1),
        min_ask_size: config.min_ask_size.unwrap_or(1),
        min_daily_volume: config.min_daily_volume.unwrap_or(100),
        min_implied_volatility: config.min_implied_volatility.unwrap_or(0.12),
        max_implied_volatility: config.max_implied_volatility.unwrap_or(1.00),
        min_annualized_premium_yield: config.min_annualized_premium_yield.unwrap_or(0.12),
        max_buying_power_usage_pct: config.max_buying_power_usage_pct.unwrap_or(0.10),
        min_return_on_buying_power: config.min_return_on_buying_power.unwrap_or(0.0004),
        min_breakeven_pop: config.min_breakeven_pop.unwrap_or(0.72),
        max_probability_of_touch: config.max_probability_of_touch.unwrap_or(0.40),
        min_distance_to_breakeven_pct: config.min_distance_to_breakeven_pct.unwrap_or(0.004),
        min_expected_move_coverage: config.min_expected_move_coverage.unwrap_or(1.10),
        min_score: config.min_score.unwrap_or(72.0),
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
    fn runtime_config_inherits_parent_sections() {
        let parent = parse_runtime_config(
            r#"
[runtime]
max_iterations = 0
candidate_ledger_enabled = true
candidate_ledger_max_candidates = 5

[index]
underlyings = ["SPY", "GLD"]
quantity = 1

[naked_scanner]
max_buying_power_usage_pct = 0.10
min_score = 70.0

[risk]
max_active_entries_per_underlying = 1

[risk.sectors]
SPY = "broad_index"
GLD = "metals"
"#,
        )
        .unwrap();
        let child = parse_runtime_config(
            r#"
extends = "base.toml"

[runtime]
strategies = ["naked_put"]
candidate_ledger_max_candidates = 20

[naked_scanner]
max_buying_power_usage_pct = 0.03

[risk]
max_active_entries = 3

[risk.sectors]
GDX = "metals"
"#,
        )
        .unwrap();

        let merged = child.merge_parent(parent);

        assert!(merged.extends.is_none());
        assert_eq!(merged.runtime.strategies, vec!["naked_put"]);
        assert_eq!(merged.runtime.max_iterations, Some(0));
        assert_eq!(merged.runtime.candidate_ledger_enabled, Some(true));
        assert_eq!(merged.runtime.candidate_ledger_max_candidates, Some(20));
        assert_eq!(merged.index.underlyings, vec!["SPY", "GLD"]);
        assert_eq!(merged.index.quantity, Some(1));
        assert_eq!(merged.naked_scanner.max_buying_power_usage_pct, Some(0.03),);
        assert_eq!(merged.naked_scanner.min_score, Some(70.0));
        assert_eq!(merged.risk.max_active_entries, Some(3));
        assert_eq!(merged.risk.max_active_entries_per_underlying, Some(1));
        assert_eq!(
            merged.risk.sectors.get("SPY").map(String::as_str),
            Some("broad_index"),
        );
        assert_eq!(
            merged.risk.sectors.get("GDX").map(String::as_str),
            Some("metals"),
        );
    }

    #[test]
    fn parse_runtime_config_reads_strategy_scanner_and_management_sections() {
        let config = parse_runtime_config(
            r#"
[runtime]
strategies = ["put", "iron_condor"]
dry_run_strategies = ["iron_condor"]
max_iterations = 0
submit = false
manage = true
close = true
kill_switch = true
state_path = "/tmp/alpaca-state.json"
candidate_ledger_enabled = true
candidate_ledger_dir = "/tmp/candidate-ledger"
candidate_ledger_max_candidates = 7

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
min_credit_to_width = 0.09

[iron_condor]
min_return_on_risk = 0.20
wing_min_return_on_risk = 0.11
require_equal_widths = true

[debit_scanner]
min_dte = 10
max_dte = 35
long_delta_min = 0.45
long_delta_max = 0.60
widths = [3.0, 5.0]
min_open_interest = 250
max_leg_spread_pct = 0.12
max_debit_to_width = 0.50
min_debit_to_width = 0.25
min_reward_to_risk = 0.80

[naked_scanner]
min_dte = 5
max_dte = 14
short_delta_min = 0.08
short_delta_max = 0.16
min_open_interest = 500
max_spread_pct = 0.08
min_credit = 0.20
min_bid_size = 1
min_ask_size = 1
min_daily_volume = 50
min_implied_volatility = 0.15
max_implied_volatility = 0.90
min_annualized_premium_yield = 0.10
max_buying_power_usage_pct = 0.10
min_return_on_buying_power = 0.0005
min_breakeven_pop = 0.70
max_probability_of_touch = 0.45
min_distance_to_breakeven_pct = 0.005
min_expected_move_coverage = 1.00
min_score = 70.0

[naked_1_3dte_scanner]
min_dte = 1
max_dte = 3
short_delta_min = 0.06
short_delta_max = 0.14
min_open_interest = 300
max_spread_pct = 0.08
min_credit = 0.12
min_bid_size = 1
min_ask_size = 1
min_daily_volume = 100
min_implied_volatility = 0.12
max_implied_volatility = 1.00
min_annualized_premium_yield = 0.12
max_buying_power_usage_pct = 0.10
min_return_on_buying_power = 0.0004
min_breakeven_pop = 0.72
max_probability_of_touch = 0.40
min_distance_to_breakeven_pct = 0.004
min_expected_move_coverage = 1.10
min_score = 72.0

[risk]
max_active_entries = 1
max_daily_submits = 1
max_open_orders = 1
max_active_entries_per_underlying = 1
max_active_entries_per_sector = 3

[risk.sectors]
SPY = "broad_index"
QQQ = "broad_index"

[management]
stale_entry_secs = 600
stale_close_secs = 120
close_regular_hours_only = true
close_start = "09:30"
close_end = "16:00"
close_price_cushion = 0.02
max_close_attempts = 4
close_reprice_cooldown_secs = 45
profit_target_close_fraction = 0.45
stop_loss_close_multiple = 1.8
max_hold_secs = 3600
expiration_exit_days = 2
"#,
        )
        .unwrap();

        assert_eq!(config.runtime.strategies, vec!["put", "iron_condor"]);
        assert_eq!(config.runtime.dry_run_strategies, vec!["iron_condor"]);
        assert_eq!(config.runtime.candidate_ledger_enabled, Some(true));
        assert_eq!(
            config.runtime.candidate_ledger_dir,
            Some(PathBuf::from("/tmp/candidate-ledger")),
        );
        assert_eq!(config.runtime.candidate_ledger_max_candidates, Some(7));
        assert_eq!(config.index.underlyings, vec!["SPY", "QQQ"]);
        assert_eq!(config.scanner.widths, Some(vec![2.0, 5.0]));
        assert_eq!(config.scanner.min_credit_to_width, Some(0.09));
        assert_eq!(config.iron_condor.min_return_on_risk, Some(0.20));
        assert_eq!(config.debit_scanner.widths, Some(vec![3.0, 5.0]));
        assert_eq!(config.debit_scanner.min_debit_to_width, Some(0.25));
        assert_eq!(config.naked_scanner.min_dte, Some(5));
        assert_eq!(config.naked_scanner.max_spread_pct, Some(0.08));
        assert_eq!(config.naked_scanner.min_credit, Some(0.20));
        assert_eq!(config.naked_scanner.min_daily_volume, Some(50));
        assert_eq!(config.naked_scanner.min_implied_volatility, Some(0.15));
        assert_eq!(
            config.naked_scanner.min_annualized_premium_yield,
            Some(0.10)
        );
        assert_eq!(config.naked_scanner.max_buying_power_usage_pct, Some(0.10));
        assert_eq!(
            config.naked_scanner.min_return_on_buying_power,
            Some(0.0005)
        );
        assert_eq!(config.naked_scanner.min_breakeven_pop, Some(0.70));
        assert_eq!(config.naked_scanner.max_probability_of_touch, Some(0.45));
        assert_eq!(
            config.naked_scanner.min_distance_to_breakeven_pct,
            Some(0.005)
        );
        assert_eq!(config.naked_scanner.min_expected_move_coverage, Some(1.00));
        assert_eq!(config.naked_scanner.min_score, Some(70.0));
        assert_eq!(config.naked_1_3dte_scanner.min_dte, Some(1));
        assert_eq!(config.naked_1_3dte_scanner.max_dte, Some(3));
        assert_eq!(config.naked_1_3dte_scanner.min_credit, Some(0.12));
        assert_eq!(
            config.naked_1_3dte_scanner.max_buying_power_usage_pct,
            Some(0.10)
        );
        assert_eq!(
            config.naked_1_3dte_scanner.min_return_on_buying_power,
            Some(0.0004)
        );
        assert_eq!(
            config.naked_1_3dte_scanner.max_probability_of_touch,
            Some(0.40)
        );
        assert_eq!(
            config.naked_1_3dte_scanner.min_expected_move_coverage,
            Some(1.10)
        );
        assert_eq!(config.naked_1_3dte_scanner.min_score, Some(72.0));
        assert_eq!(config.risk.max_active_entries, Some(1));
        assert_eq!(config.risk.max_daily_submits, Some(1));
        assert_eq!(config.risk.max_open_orders, Some(1));
        assert_eq!(config.risk.max_active_entries_per_underlying, Some(1));
        assert_eq!(config.risk.max_active_entries_per_sector, Some(3));
        assert_eq!(
            config.risk.sectors.get("SPY").map(String::as_str),
            Some("broad_index"),
        );
        assert_eq!(config.management.stale_entry_secs, Some(600));
        assert_eq!(config.management.stale_close_secs, Some(120));
        assert_eq!(config.management.close_regular_hours_only, Some(true));
        assert_eq!(config.management.close_start.as_deref(), Some("09:30"));
        assert_eq!(config.management.close_end.as_deref(), Some("16:00"));
        assert_eq!(config.management.close_price_cushion, Some(0.02));
        assert_eq!(config.management.max_close_attempts, Some(4));
        assert_eq!(config.management.close_reprice_cooldown_secs, Some(45));
    }

    #[test]
    fn strategy_config_accepts_combined_four_leg_strategy() {
        let config = strategy_config_from_values(vec!["both,iron_condor".to_string()]).unwrap();

        assert_eq!(
            config.credit_kinds,
            vec![CreditSpreadKind::Put, CreditSpreadKind::Call],
        );
        assert!(config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert!(config.naked_kinds.is_empty());
    }

    #[test]
    fn strategy_config_accepts_debit_strategies() {
        let config = strategy_config_from_values(vec!["call_debit,put_debit".to_string()]).unwrap();

        assert!(config.credit_kinds.is_empty());
        assert!(!config.iron_condor_enabled);
        assert_eq!(
            config.debit_kinds,
            vec![DebitSpreadKind::Call, DebitSpreadKind::Put],
        );
        assert!(config.naked_kinds.is_empty());
    }

    #[test]
    fn strategy_config_accepts_naked_strategies() {
        let config = strategy_config_from_values(vec![
            "naked_call,naked_put,naked_call_1_3dte,naked_put_1_3dte".to_string(),
        ])
        .unwrap();

        assert!(config.credit_kinds.is_empty());
        assert!(!config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert_eq!(
            config.naked_kinds,
            vec![
                NakedOptionKind::Call,
                NakedOptionKind::Put,
                NakedOptionKind::CallOneToThreeDte,
                NakedOptionKind::PutOneToThreeDte,
            ],
        );
    }

    #[test]
    fn dry_run_strategy_config_has_no_default_strategy() {
        let config = dry_run_strategy_config_from_values(Vec::new()).unwrap();

        assert!(config.credit_kinds.is_empty());
        assert!(!config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert!(config.naked_kinds.is_empty());

        let config = dry_run_strategy_config_from_values(vec!["put".to_string()]).unwrap();
        assert_eq!(config.credit_kinds, vec![CreditSpreadKind::Put]);
        assert!(!config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert!(config.naked_kinds.is_empty());
    }
}
