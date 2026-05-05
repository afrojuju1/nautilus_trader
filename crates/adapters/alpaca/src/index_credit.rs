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
use serde_json::json;

use crate::{
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
        IronCondorCandidate, IronCondorScannerConfig, NakedOptionCandidate, NakedOptionKind,
        NakedOptionScannerConfig, PutCreditScannerConfig, SpreadCandidate,
        scan_call_credit_underlying, scan_call_debit_underlying, scan_iron_condor_underlying,
        scan_naked_call_underlying, scan_naked_put_underlying, scan_put_credit_underlying,
        scan_put_debit_underlying,
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
    /// Credit scanner config.
    pub scanner: PutCreditScannerConfig,
    /// Iron-condor scanner config.
    pub iron_condor_scanner: IronCondorScannerConfig,
    /// Debit-spread scanner config.
    pub debit_scanner: DebitSpreadScannerConfig,
    /// Naked-option scanner config.
    pub naked_scanner: NakedOptionScannerConfig,
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

        if state.has_submitted_underlying(trade_date, underlying) {
            println!("{underlying}: admission_rejected reason=daily_duplicate_state");
            emit_operator_event(
                "scanner_diagnostic",
                json!({
                    "underlying": underlying,
                    "reason": "daily_duplicate_state",
                }),
            );
            continue;
        }

        if fleet_has_active_underlying_elsewhere(config, underlying) {
            println!("{underlying}: admission_rejected reason=fleet_duplicate_underlying");
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
            let Some(best) = result.candidates.first() else {
                let reason = no_candidate_reason(
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                );
                println!(
                    "{underlying}: no_candidate strategy={} reason={} contracts={} snapshots={} scoreable={}",
                    credit_spread_strategy_name(*kind),
                    reason,
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
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
            let Some(best) = result.candidates.first() else {
                let reason = no_candidate_reason(
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                );
                println!(
                    "{underlying}: no_candidate strategy=index_iron_condor_entry reason={} contracts={} snapshots={} scoreable={}",
                    reason, result.contract_count, result.snapshot_count, result.scoreable_count,
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
            let Some(best) = result.candidates.first() else {
                let reason = no_candidate_reason(
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                );
                println!(
                    "{underlying}: no_candidate strategy={} reason={} contracts={} snapshots={} scoreable={}",
                    debit_spread_strategy_name(*kind),
                    reason,
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
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
            let result = match kind {
                NakedOptionKind::Call => {
                    scan_naked_call_underlying(
                        client,
                        data_config,
                        &config.naked_scanner,
                        underlying,
                    )
                    .await?
                }
                NakedOptionKind::Put => {
                    scan_naked_put_underlying(
                        client,
                        data_config,
                        &config.naked_scanner,
                        underlying,
                    )
                    .await?
                }
            };
            let Some(best) = result.candidates.first() else {
                let reason = no_candidate_reason(
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                );
                println!(
                    "{underlying}: no_candidate strategy={} reason={} contracts={} snapshots={} scoreable={}",
                    naked_option_strategy_name(*kind),
                    reason,
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
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

            println!(
                "{underlying}: candidate strategy={} short={} credit={:.2} delta={:.2} score={:.1}",
                naked_option_strategy_name(*kind),
                best.short.symbol,
                best.credit,
                best.short.delta_abs,
                best.score,
            );
            emit_operator_event(
                "scanner_diagnostic",
                json!({
                    "underlying": underlying,
                    "strategy": naked_option_strategy_name(*kind),
                    "result": "candidate",
                    "short_symbol": &best.short.symbol,
                    "credit": best.credit,
                    "delta_abs": best.short.delta_abs,
                    "score": best.score,
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
    runtime: RuntimeSection,
    index: IndexSection,
    scanner: ScannerSection,
    iron_condor: IronCondorSection,
    debit_scanner: DebitScannerSection,
    naked_scanner: NakedScannerSection,
    management: ManagementSection,
    risk: RiskSection,
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
    min_credit_to_width: Option<f64>,
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
        iron_condor_scanner: iron_condor_scanner_config_from_file(&scanner, &file.iron_condor),
        debit_scanner: debit_scanner_config_from_file(&file.debit_scanner),
        naked_scanner: naked_scanner_config_from_file(&file.naked_scanner),
        scanner,
        fleet,
        fleet_account_id: None,
        fleet_policy_blocks: Vec::new(),
    };
    apply_fleet_policy(&mut config);
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
        if config.naked_kinds.contains(&NakedOptionKind::Call) && !account.permissions.naked_calls {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_naked_calls_required:{}",
                account.id
            ));
        }
        if config.naked_kinds.contains(&NakedOptionKind::Put) && !account.permissions.naked_puts {
            config.fleet_policy_blocks.push(format!(
                "fleet_permission_naked_puts_required:{}",
                account.id
            ));
        }
        if let Some(limit) = account.risk_budget.max_active_entries {
            config.max_active_entries = Some(min_limit(config.max_active_entries, limit));
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
    ["SPY", "QQQ", "IWM", "DIA", "GLD"]
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
        ("TLT", "rates"),
        ("XLE", "energy"),
        ("XLF", "financials"),
        ("XLK", "technology"),
        ("XLV", "healthcare"),
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
        min_dte: config.min_dte.unwrap_or(7),
        max_dte: config.max_dte.unwrap_or(21),
        short_delta_min: config.short_delta_min.unwrap_or(0.10),
        short_delta_max: config.short_delta_max.unwrap_or(0.20),
        min_open_interest: config.min_open_interest.unwrap_or(500),
        max_spread_pct: config.max_spread_pct.unwrap_or(0.12),
        min_credit: config.min_credit.unwrap_or(0.25),
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
dry_run_strategies = ["iron_condor"]
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
min_dte = 7
max_dte = 21
short_delta_min = 0.10
short_delta_max = 0.20
min_open_interest = 500
max_spread_pct = 0.12
min_credit = 0.25

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
        assert_eq!(config.index.underlyings, vec!["SPY", "QQQ"]);
        assert_eq!(config.scanner.widths, Some(vec![2.0, 5.0]));
        assert_eq!(config.scanner.min_credit_to_width, Some(0.09));
        assert_eq!(config.iron_condor.min_return_on_risk, Some(0.20));
        assert_eq!(config.debit_scanner.widths, Some(vec![3.0, 5.0]));
        assert_eq!(config.debit_scanner.min_debit_to_width, Some(0.25));
        assert_eq!(config.naked_scanner.min_dte, Some(7));
        assert_eq!(config.naked_scanner.max_spread_pct, Some(0.12));
        assert_eq!(config.naked_scanner.min_credit, Some(0.25));
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
        let config = strategy_config_from_values(vec!["naked_call,naked_put".to_string()]).unwrap();

        assert!(config.credit_kinds.is_empty());
        assert!(!config.iron_condor_enabled);
        assert!(config.debit_kinds.is_empty());
        assert_eq!(
            config.naked_kinds,
            vec![NakedOptionKind::Call, NakedOptionKind::Put],
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
