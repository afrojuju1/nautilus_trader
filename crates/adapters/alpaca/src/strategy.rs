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

//! Credit and debit spread scanner logic shared by smoke binaries and strategy scaffolds.

use std::collections::BTreeMap;

use chrono::{NaiveDate, Utc};

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Result,
        models::{AlpacaOptionContract, AlpacaOptionSnapshot, AlpacaOptionType},
    },
};

mod chain;
mod scoring;

use chain::{load_option_chain_snapshot_at, load_underlying_price};
use scoring::merge_rejection_counts;
pub use scoring::{
    annualized_premium_yield, build_candidates, build_candidates_for_kind,
    build_candidates_for_kind_with_rejections, build_debit_candidates_for_kind,
    build_debit_candidates_for_kind_with_rejections, build_iron_condor_candidates,
    build_iron_condor_candidates_with_rejections, build_naked_option_candidates,
    build_naked_option_candidates_with_capital,
    build_naked_option_candidates_with_capital_and_rejections,
    estimated_naked_option_buying_power_requirement, option_candidate_metrics, score_contracts,
    score_contracts_with_rejections, score_contracts_with_rejections_at, score_debit_contracts,
    score_debit_contracts_with_rejections, score_debit_contracts_with_rejections_at,
    score_naked_option_contracts, score_naked_option_contracts_with_rejections,
    score_naked_option_contracts_with_rejections_at,
};

const SCANNER_RISK_FREE_RATE: f64 = 0.0425;
const DAYS_PER_YEAR: f64 = 365.25;
const OPTION_CONTRACT_MULTIPLIER: f64 = 100.0;

/// Vertical credit spread family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreditSpreadKind {
    /// Put credit spread.
    Put,
    /// Call credit spread.
    Call,
}

impl CreditSpreadKind {
    fn option_type(self) -> AlpacaOptionType {
        match self {
            Self::Put => AlpacaOptionType::Put,
            Self::Call => AlpacaOptionType::Call,
        }
    }

    fn long_strike(self, short_strike: f64, width: f64) -> f64 {
        match self {
            Self::Put => short_strike - width,
            Self::Call => short_strike + width,
        }
    }
}

/// Vertical debit spread family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebitSpreadKind {
    /// Call debit spread.
    Call,
    /// Put debit spread.
    Put,
}

impl DebitSpreadKind {
    fn option_type(self) -> AlpacaOptionType {
        match self {
            Self::Call => AlpacaOptionType::Call,
            Self::Put => AlpacaOptionType::Put,
        }
    }

    fn short_strike(self, long_strike: f64, width: f64) -> f64 {
        match self {
            Self::Call => long_strike + width,
            Self::Put => long_strike - width,
        }
    }
}

/// Naked short option family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NakedOptionKind {
    /// Naked short call.
    Call,
    /// Naked short put.
    Put,
    /// Naked short call using the 1-3 DTE profile.
    CallOneToThreeDte,
    /// Naked short put using the 1-3 DTE profile.
    PutOneToThreeDte,
}

impl NakedOptionKind {
    fn option_type(self) -> AlpacaOptionType {
        match self {
            Self::Call | Self::CallOneToThreeDte => AlpacaOptionType::Call,
            Self::Put | Self::PutOneToThreeDte => AlpacaOptionType::Put,
        }
    }

    /// Returns `true` for call-side naked option strategies.
    #[must_use]
    pub const fn is_call(self) -> bool {
        matches!(self, Self::Call | Self::CallOneToThreeDte)
    }

    /// Returns `true` for put-side naked option strategies.
    #[must_use]
    pub const fn is_put(self) -> bool {
        matches!(self, Self::Put | Self::PutOneToThreeDte)
    }

    /// Returns `true` for the 1-3 DTE profile.
    #[must_use]
    pub const fn is_one_to_three_dte(self) -> bool {
        matches!(self, Self::CallOneToThreeDte | Self::PutOneToThreeDte)
    }
}

/// Model used to estimate short-option buying-power requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionCapitalRequirementModel {
    /// Cash-secured short put reserve.
    CashSecuredPut,
    /// Reg-T style short-call margin estimate.
    RegTShortCallEstimate,
}

impl OptionCapitalRequirementModel {
    /// Returns a stable diagnostic name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CashSecuredPut => "cash_secured_put",
            Self::RegTShortCallEstimate => "reg_t_short_call_estimate",
        }
    }
}

/// Account capital context for naked-option candidate ranking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NakedOptionCapitalContext {
    /// Current account options buying power.
    pub options_buying_power: Option<f64>,
    /// Configured strategy quantity.
    pub quantity: u64,
}

/// Configuration for the credit spread scanner.
#[derive(Clone, Debug, PartialEq)]
pub struct PutCreditScannerConfig {
    /// Minimum days to expiration.
    pub min_dte: i64,
    /// Maximum days to expiration.
    pub max_dte: i64,
    /// Minimum absolute short-leg delta.
    pub short_delta_min: f64,
    /// Maximum absolute short-leg delta.
    pub short_delta_max: f64,
    /// Allowed spread widths.
    pub widths: Vec<f64>,
    /// Minimum open interest per contract.
    pub min_open_interest: u64,
    /// Maximum bid/ask spread as a fraction of midpoint per leg.
    pub max_leg_spread_pct: f64,
    /// Minimum credit / max loss.
    pub min_return_on_risk: f64,
    /// Minimum credit as a fraction of spread width.
    pub min_credit_to_width: f64,
}

impl Default for PutCreditScannerConfig {
    fn default() -> Self {
        Self {
            min_dte: 5,
            max_dte: 10,
            short_delta_min: 0.18,
            short_delta_max: 0.28,
            widths: vec![2.0, 3.0, 5.0],
            min_open_interest: 200,
            max_leg_spread_pct: 0.15,
            min_return_on_risk: 0.13,
            min_credit_to_width: 0.08,
        }
    }
}

/// Configuration for the debit spread scanner.
#[derive(Clone, Debug, PartialEq)]
pub struct DebitSpreadScannerConfig {
    /// Minimum days to expiration.
    pub min_dte: i64,
    /// Maximum days to expiration.
    pub max_dte: i64,
    /// Minimum absolute long-leg delta.
    pub long_delta_min: f64,
    /// Maximum absolute long-leg delta.
    pub long_delta_max: f64,
    /// Allowed spread widths.
    pub widths: Vec<f64>,
    /// Minimum open interest per contract.
    pub min_open_interest: u64,
    /// Maximum bid/ask spread as a fraction of midpoint per leg.
    pub max_leg_spread_pct: f64,
    /// Maximum debit as a fraction of spread width.
    pub max_debit_to_width: f64,
    /// Minimum debit as a fraction of spread width.
    pub min_debit_to_width: f64,
    /// Minimum max-profit / max-loss.
    pub min_reward_to_risk: f64,
}

impl Default for DebitSpreadScannerConfig {
    fn default() -> Self {
        Self {
            min_dte: 5,
            max_dte: 45,
            long_delta_min: 0.45,
            long_delta_max: 0.65,
            widths: vec![2.0, 3.0, 5.0],
            min_open_interest: 200,
            max_leg_spread_pct: 0.15,
            max_debit_to_width: 0.55,
            min_debit_to_width: 0.20,
            min_reward_to_risk: 0.75,
        }
    }
}

/// Configuration for naked short option entries.
#[derive(Clone, Debug, PartialEq)]
pub struct NakedOptionScannerConfig {
    /// Minimum days to expiration.
    pub min_dte: i64,
    /// Maximum days to expiration.
    pub max_dte: i64,
    /// Minimum absolute short-option delta.
    pub short_delta_min: f64,
    /// Maximum absolute short-option delta.
    pub short_delta_max: f64,
    /// Minimum open interest.
    pub min_open_interest: u64,
    /// Maximum bid/ask spread as a fraction of midpoint.
    pub max_spread_pct: f64,
    /// Minimum option credit.
    pub min_credit: f64,
    /// Minimum displayed bid size.
    pub min_bid_size: u64,
    /// Minimum displayed ask size.
    pub min_ask_size: u64,
    /// Minimum current-day option volume.
    pub min_daily_volume: u64,
    /// Minimum implied volatility.
    pub min_implied_volatility: f64,
    /// Maximum implied volatility.
    pub max_implied_volatility: f64,
    /// Minimum annualized premium yield, using credit / strike / DTE.
    pub min_annualized_premium_yield: f64,
    /// Maximum estimated buying-power usage as a fraction of account options buying power.
    pub max_buying_power_usage_pct: f64,
    /// Minimum credit / estimated buying-power requirement.
    pub min_return_on_buying_power: f64,
    /// Minimum probability of expiring beyond breakeven.
    pub min_breakeven_pop: f64,
    /// Maximum estimated probability of touching the short strike.
    pub max_probability_of_touch: f64,
    /// Minimum distance from spot to breakeven as a fraction of spot.
    pub min_distance_to_breakeven_pct: f64,
    /// Minimum breakeven distance measured in one-standard-deviation expected moves.
    pub min_expected_move_coverage: f64,
    /// Minimum composite scanner score.
    pub min_score: f64,
}

impl Default for NakedOptionScannerConfig {
    fn default() -> Self {
        Self {
            min_dte: 5,
            max_dte: 14,
            short_delta_min: 0.10,
            short_delta_max: 0.20,
            min_open_interest: 500,
            max_spread_pct: 0.12,
            min_credit: 0.25,
            min_bid_size: 1,
            min_ask_size: 1,
            min_daily_volume: 1,
            min_implied_volatility: 0.0,
            max_implied_volatility: 1.50,
            min_annualized_premium_yield: 0.10,
            max_buying_power_usage_pct: 0.10,
            min_return_on_buying_power: 0.0005,
            min_breakeven_pop: 0.65,
            max_probability_of_touch: 0.70,
            min_distance_to_breakeven_pct: 0.005,
            min_expected_move_coverage: 0.75,
            min_score: 55.0,
        }
    }
}

/// Configuration for the iron-condor scanner.
#[derive(Clone, Debug, PartialEq)]
pub struct IronCondorScannerConfig {
    /// Vertical credit-spread scanner configuration for both wings.
    pub credit: PutCreditScannerConfig,
    /// Minimum total credit / max loss.
    pub min_return_on_risk: f64,
    /// Whether put and call wing widths must match.
    pub require_equal_widths: bool,
}

impl Default for IronCondorScannerConfig {
    fn default() -> Self {
        Self {
            credit: PutCreditScannerConfig::default(),
            min_return_on_risk: 0.18,
            require_equal_widths: true,
        }
    }
}

/// Derived option metrics used by scanner ranking and diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionCandidateMetrics {
    /// Underlying spot price used for the calculation.
    pub underlying_price: f64,
    /// Short-option breakeven at expiration.
    pub breakeven: f64,
    /// Probability of expiring in-the-money at the short strike.
    pub strike_itm_probability: f64,
    /// `1 - absolute delta`, useful as a fast POP proxy.
    pub delta_pop_proxy: f64,
    /// Probability of expiring beyond breakeven.
    pub breakeven_pop: f64,
    /// Estimated probability of touching the short strike before expiration.
    pub probability_of_touch_est: f64,
    /// One-standard-deviation expected move in price units.
    pub expected_move: f64,
    /// One-standard-deviation expected move as a fraction of spot.
    pub expected_move_pct: f64,
    /// Directional distance from spot to strike as a fraction of spot.
    pub distance_to_strike_pct: f64,
    /// Directional distance from spot to breakeven as a fraction of spot.
    pub distance_to_breakeven_pct: f64,
    /// Breakeven distance divided by one-standard-deviation expected move.
    pub expected_move_coverage: f64,
    /// Capital model used for the buying-power estimate.
    pub capital_requirement_model: OptionCapitalRequirementModel,
    /// Estimated buying-power requirement per contract.
    pub estimated_buying_power_requirement: f64,
    /// Credit divided by estimated buying-power requirement.
    pub return_on_buying_power: f64,
    /// Model absolute delta from the Nautilus Black-Scholes calculation.
    pub model_delta_abs: f64,
    /// Model gamma from the Nautilus Black-Scholes calculation.
    pub model_gamma: f64,
    /// Model theta from the Nautilus Black-Scholes calculation.
    pub model_theta: f64,
    /// Model vega from the Nautilus Black-Scholes calculation.
    pub model_vega: f64,
}

/// One scored option contract eligible for strategy candidate building.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoredContract {
    /// Alpaca option symbol.
    pub symbol: String,
    /// Contract expiration date.
    pub expiration_date: String,
    /// Calendar days to expiration from the scan date.
    pub dte: i64,
    /// Strike price.
    pub strike: f64,
    /// Bid price.
    pub bid: f64,
    /// Ask price.
    pub ask: f64,
    /// Absolute delta.
    pub delta_abs: f64,
    /// Bid/ask spread as a fraction of midpoint.
    pub spread_pct: f64,
    /// Displayed bid size.
    pub bid_size: u64,
    /// Displayed ask size.
    pub ask_size: u64,
    /// Current-day option volume.
    pub volume: u64,
    /// Contract open interest.
    pub open_interest: u64,
    /// Implied volatility, if present.
    pub implied_volatility: Option<f64>,
    /// Derived option metrics, when spot and IV were available.
    pub metrics: Option<OptionCandidateMetrics>,
}

/// One vertical credit spread candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct SpreadCandidate {
    /// Short put leg.
    pub short: ScoredContract,
    /// Long put hedge leg.
    pub long: ScoredContract,
    /// Strike width.
    pub width: f64,
    /// Net credit.
    pub credit: f64,
    /// Maximum loss.
    pub max_loss: f64,
    /// Credit / max loss.
    pub return_on_risk: f64,
    /// Scanner score.
    pub score: f64,
}

/// One vertical debit spread candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct DebitSpreadCandidate {
    /// Long option leg.
    pub long: ScoredContract,
    /// Short option leg.
    pub short: ScoredContract,
    /// Strike width.
    pub width: f64,
    /// Net debit.
    pub debit: f64,
    /// Maximum profit.
    pub max_profit: f64,
    /// Maximum loss.
    pub max_loss: f64,
    /// Max profit / max loss.
    pub reward_to_risk: f64,
    /// Scanner score.
    pub score: f64,
}

/// One four-leg iron-condor candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct IronCondorCandidate {
    /// Put credit wing.
    pub put: SpreadCandidate,
    /// Call credit wing.
    pub call: SpreadCandidate,
    /// Total net credit.
    pub credit: f64,
    /// Maximum possible loss.
    pub max_loss: f64,
    /// Credit / max loss.
    pub return_on_risk: f64,
    /// Scanner score.
    pub score: f64,
}

/// One naked short option candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct NakedOptionCandidate {
    /// Short option contract.
    pub short: ScoredContract,
    /// Entry credit.
    pub credit: f64,
    /// Capital model used for the buying-power estimate.
    pub capital_requirement_model: OptionCapitalRequirementModel,
    /// Estimated buying-power requirement for the configured quantity.
    pub estimated_buying_power_requirement: f64,
    /// Estimated buying-power usage as a fraction of account options buying power.
    pub buying_power_usage_pct: Option<f64>,
    /// Total credit divided by estimated buying-power requirement.
    pub return_on_buying_power: f64,
    /// Scanner score.
    pub score: f64,
}

/// Scan result for one underlying.
#[derive(Clone, Debug, PartialEq)]
pub struct PutCreditScanResult {
    /// Underlying symbol.
    pub underlying: String,
    /// Number of contracts loaded.
    pub contract_count: usize,
    /// Number of snapshots loaded.
    pub snapshot_count: usize,
    /// Number of scoreable contracts.
    pub scoreable_count: usize,
    /// Counts of scanner rejection reasons.
    pub rejection_counts: BTreeMap<String, usize>,
    /// Ranked candidates.
    pub candidates: Vec<SpreadCandidate>,
}

/// Scan result for one debit-spread underlying.
#[derive(Clone, Debug, PartialEq)]
pub struct DebitSpreadScanResult {
    /// Underlying symbol.
    pub underlying: String,
    /// Number of contracts loaded.
    pub contract_count: usize,
    /// Number of snapshots loaded.
    pub snapshot_count: usize,
    /// Number of scoreable contracts.
    pub scoreable_count: usize,
    /// Counts of scanner rejection reasons.
    pub rejection_counts: BTreeMap<String, usize>,
    /// Ranked debit-spread candidates.
    pub candidates: Vec<DebitSpreadCandidate>,
}

/// Scan result for one iron-condor underlying.
#[derive(Clone, Debug, PartialEq)]
pub struct IronCondorScanResult {
    /// Underlying symbol.
    pub underlying: String,
    /// Number of contracts loaded across put and call scans.
    pub contract_count: usize,
    /// Number of snapshots loaded across put and call scans.
    pub snapshot_count: usize,
    /// Number of scoreable contracts across put and call scans.
    pub scoreable_count: usize,
    /// Counts of scanner rejection reasons.
    pub rejection_counts: BTreeMap<String, usize>,
    /// Ranked iron-condor candidates.
    pub candidates: Vec<IronCondorCandidate>,
}

/// Scan result for one naked option underlying.
#[derive(Clone, Debug, PartialEq)]
pub struct NakedOptionScanResult {
    /// Underlying symbol.
    pub underlying: String,
    /// Number of contracts loaded.
    pub contract_count: usize,
    /// Number of snapshots loaded.
    pub snapshot_count: usize,
    /// Number of scoreable contracts.
    pub scoreable_count: usize,
    /// Counts of scanner rejection reasons.
    pub rejection_counts: BTreeMap<String, usize>,
    /// Ranked naked-option candidates.
    pub candidates: Vec<NakedOptionCandidate>,
}

/// Loads chain data and ranks put credit spread candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_put_credit_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &PutCreditScannerConfig,
    underlying: impl Into<String>,
) -> Result<PutCreditScanResult> {
    scan_credit_spread_underlying(
        client,
        data_config,
        config,
        underlying,
        CreditSpreadKind::Put,
    )
    .await
}

/// Loads chain data and ranks put credit spread candidates for one underlying using an explicit scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_put_credit_underlying_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &PutCreditScannerConfig,
    underlying: impl Into<String>,
    scan_date: NaiveDate,
) -> Result<PutCreditScanResult> {
    scan_credit_spread_underlying_at(
        client,
        data_config,
        config,
        underlying,
        CreditSpreadKind::Put,
        scan_date,
    )
    .await
}

/// Loads chain data and ranks call credit spread candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_call_credit_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &PutCreditScannerConfig,
    underlying: impl Into<String>,
) -> Result<PutCreditScanResult> {
    scan_credit_spread_underlying(
        client,
        data_config,
        config,
        underlying,
        CreditSpreadKind::Call,
    )
    .await
}

/// Loads chain data and ranks call credit spread candidates for one underlying using an explicit scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_call_credit_underlying_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &PutCreditScannerConfig,
    underlying: impl Into<String>,
    scan_date: NaiveDate,
) -> Result<PutCreditScanResult> {
    scan_credit_spread_underlying_at(
        client,
        data_config,
        config,
        underlying,
        CreditSpreadKind::Call,
        scan_date,
    )
    .await
}

/// Loads chain data and ranks call debit spread candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_call_debit_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &DebitSpreadScannerConfig,
    underlying: impl Into<String>,
) -> Result<DebitSpreadScanResult> {
    scan_debit_spread_underlying(
        client,
        data_config,
        config,
        underlying,
        DebitSpreadKind::Call,
    )
    .await
}

/// Loads chain data and ranks call debit spread candidates for one underlying using an explicit scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_call_debit_underlying_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &DebitSpreadScannerConfig,
    underlying: impl Into<String>,
    scan_date: NaiveDate,
) -> Result<DebitSpreadScanResult> {
    scan_debit_spread_underlying_at(
        client,
        data_config,
        config,
        underlying,
        DebitSpreadKind::Call,
        scan_date,
    )
    .await
}

/// Loads chain data and ranks put debit spread candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_put_debit_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &DebitSpreadScannerConfig,
    underlying: impl Into<String>,
) -> Result<DebitSpreadScanResult> {
    scan_debit_spread_underlying(
        client,
        data_config,
        config,
        underlying,
        DebitSpreadKind::Put,
    )
    .await
}

/// Loads chain data and ranks put debit spread candidates for one underlying using an explicit scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_put_debit_underlying_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &DebitSpreadScannerConfig,
    underlying: impl Into<String>,
    scan_date: NaiveDate,
) -> Result<DebitSpreadScanResult> {
    scan_debit_spread_underlying_at(
        client,
        data_config,
        config,
        underlying,
        DebitSpreadKind::Put,
        scan_date,
    )
    .await
}

/// Loads chain data and ranks naked call candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_naked_call_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &NakedOptionScannerConfig,
    underlying: impl Into<String>,
) -> Result<NakedOptionScanResult> {
    scan_naked_option_underlying_with_capital(
        client,
        data_config,
        config,
        underlying,
        NakedOptionKind::Call,
        None,
    )
    .await
}

/// Loads chain data and ranks naked put candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_naked_put_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &NakedOptionScannerConfig,
    underlying: impl Into<String>,
) -> Result<NakedOptionScanResult> {
    scan_naked_option_underlying_with_capital(
        client,
        data_config,
        config,
        underlying,
        NakedOptionKind::Put,
        None,
    )
    .await
}

/// Loads chain data and ranks four-leg iron-condor candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_iron_condor_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &IronCondorScannerConfig,
    underlying: impl Into<String>,
) -> Result<IronCondorScanResult> {
    scan_iron_condor_underlying_at(
        client,
        data_config,
        config,
        underlying,
        Utc::now().date_naive(),
    )
    .await
}

/// Loads chain data and ranks four-leg iron-condor candidates for one underlying using an explicit scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_iron_condor_underlying_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &IronCondorScannerConfig,
    underlying: impl Into<String>,
    scan_date: NaiveDate,
) -> Result<IronCondorScanResult> {
    let underlying = underlying.into();
    let put = scan_put_credit_underlying_at(
        client,
        data_config,
        &config.credit,
        underlying.clone(),
        scan_date,
    )
    .await?;
    let call = scan_call_credit_underlying_at(
        client,
        data_config,
        &config.credit,
        underlying.clone(),
        scan_date,
    )
    .await?;
    let (candidates, build_rejections) =
        build_iron_condor_candidates_with_rejections(&put.candidates, &call.candidates, config);
    let mut rejection_counts = put.rejection_counts.clone();
    merge_rejection_counts(&mut rejection_counts, &call.rejection_counts);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    Ok(IronCondorScanResult {
        underlying,
        contract_count: put.contract_count + call.contract_count,
        snapshot_count: put.snapshot_count + call.snapshot_count,
        scoreable_count: put.scoreable_count + call.scoreable_count,
        rejection_counts,
        candidates,
    })
}

/// Ranks four-leg iron-condor candidates from already loaded scanner inputs.
#[must_use]
pub fn scan_iron_condor_snapshots_at(
    underlying: impl Into<String>,
    put_contracts: &[AlpacaOptionContract],
    put_snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    call_contracts: &[AlpacaOptionContract],
    call_snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &IronCondorScannerConfig,
    scan_date: NaiveDate,
) -> IronCondorScanResult {
    let underlying = underlying.into();
    let put = scan_credit_spread_snapshot_at(
        underlying.clone(),
        put_contracts,
        put_snapshots,
        &config.credit,
        CreditSpreadKind::Put,
        scan_date,
    );
    let call = scan_credit_spread_snapshot_at(
        underlying.clone(),
        call_contracts,
        call_snapshots,
        &config.credit,
        CreditSpreadKind::Call,
        scan_date,
    );
    let (candidates, build_rejections) =
        build_iron_condor_candidates_with_rejections(&put.candidates, &call.candidates, config);
    let mut rejection_counts = put.rejection_counts.clone();
    merge_rejection_counts(&mut rejection_counts, &call.rejection_counts);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    IronCondorScanResult {
        underlying,
        contract_count: put.contract_count + call.contract_count,
        snapshot_count: put.snapshot_count + call.snapshot_count,
        scoreable_count: put.scoreable_count + call.scoreable_count,
        rejection_counts,
        candidates,
    }
}

/// Loads chain data and ranks vertical credit spread candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_credit_spread_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &PutCreditScannerConfig,
    underlying: impl Into<String>,
    kind: CreditSpreadKind,
) -> Result<PutCreditScanResult> {
    scan_credit_spread_underlying_at(
        client,
        data_config,
        config,
        underlying,
        kind,
        Utc::now().date_naive(),
    )
    .await
}

/// Loads chain data and ranks vertical credit spread candidates for one underlying using an explicit scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_credit_spread_underlying_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &PutCreditScannerConfig,
    underlying: impl Into<String>,
    kind: CreditSpreadKind,
    scan_date: NaiveDate,
) -> Result<PutCreditScanResult> {
    let underlying = underlying.into();
    let chain = load_option_chain_snapshot_at(
        client,
        data_config,
        &underlying,
        config.min_dte,
        config.max_dte,
        kind.option_type(),
        scan_date,
    )
    .await?;

    Ok(scan_credit_spread_snapshot_at(
        underlying,
        &chain.contracts,
        &chain.snapshots,
        config,
        kind,
        scan_date,
    ))
}

/// Ranks vertical credit spread candidates from already loaded scanner inputs.
#[must_use]
pub fn scan_credit_spread_snapshot_at(
    underlying: impl Into<String>,
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &PutCreditScannerConfig,
    kind: CreditSpreadKind,
    scan_date: NaiveDate,
) -> PutCreditScanResult {
    let underlying = underlying.into();
    let (scored, mut rejection_counts) =
        score_contracts_with_rejections_at(contracts, snapshots, config, scan_date);
    let (candidates, build_rejections) =
        build_candidates_for_kind_with_rejections(&scored, config, kind);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    PutCreditScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    }
}

/// Loads chain data and ranks vertical debit spread candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_debit_spread_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &DebitSpreadScannerConfig,
    underlying: impl Into<String>,
    kind: DebitSpreadKind,
) -> Result<DebitSpreadScanResult> {
    scan_debit_spread_underlying_at(
        client,
        data_config,
        config,
        underlying,
        kind,
        Utc::now().date_naive(),
    )
    .await
}

/// Loads chain data and ranks vertical debit spread candidates for one underlying using an explicit scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_debit_spread_underlying_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &DebitSpreadScannerConfig,
    underlying: impl Into<String>,
    kind: DebitSpreadKind,
    scan_date: NaiveDate,
) -> Result<DebitSpreadScanResult> {
    let underlying = underlying.into();
    let chain = load_option_chain_snapshot_at(
        client,
        data_config,
        &underlying,
        config.min_dte,
        config.max_dte,
        kind.option_type(),
        scan_date,
    )
    .await?;

    Ok(scan_debit_spread_snapshot_at(
        underlying,
        &chain.contracts,
        &chain.snapshots,
        config,
        kind,
        scan_date,
    ))
}

/// Ranks vertical debit spread candidates from already loaded scanner inputs.
#[must_use]
pub fn scan_debit_spread_snapshot_at(
    underlying: impl Into<String>,
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &DebitSpreadScannerConfig,
    kind: DebitSpreadKind,
    scan_date: NaiveDate,
) -> DebitSpreadScanResult {
    let underlying = underlying.into();
    let (scored, mut rejection_counts) =
        score_debit_contracts_with_rejections_at(contracts, snapshots, config, scan_date);
    let (candidates, build_rejections) =
        build_debit_candidates_for_kind_with_rejections(&scored, config, kind);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    DebitSpreadScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    }
}

/// Loads chain data and ranks naked short option candidates for one underlying.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_naked_option_underlying(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &NakedOptionScannerConfig,
    underlying: impl Into<String>,
    kind: NakedOptionKind,
) -> Result<NakedOptionScanResult> {
    scan_naked_option_underlying_with_capital(client, data_config, config, underlying, kind, None)
        .await
}

/// Loads chain data and ranks naked short-option candidates with account capital context.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_naked_option_underlying_with_capital(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &NakedOptionScannerConfig,
    underlying: impl Into<String>,
    kind: NakedOptionKind,
    capital: Option<NakedOptionCapitalContext>,
) -> Result<NakedOptionScanResult> {
    scan_naked_option_underlying_with_capital_at(
        client,
        data_config,
        config,
        underlying,
        kind,
        capital,
        Utc::now().date_naive(),
    )
    .await
}

/// Loads chain data and ranks naked short-option candidates with account capital context and scan date.
///
/// # Errors
///
/// Returns an error if Alpaca contract or snapshot requests fail.
pub async fn scan_naked_option_underlying_with_capital_at(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &NakedOptionScannerConfig,
    underlying: impl Into<String>,
    kind: NakedOptionKind,
    capital: Option<NakedOptionCapitalContext>,
    scan_date: NaiveDate,
) -> Result<NakedOptionScanResult> {
    let underlying = underlying.into();
    let chain = load_option_chain_snapshot_at(
        client,
        data_config,
        &underlying,
        config.min_dte,
        config.max_dte,
        kind.option_type(),
        scan_date,
    )
    .await?;
    let underlying_price = load_underlying_price(client, data_config, &underlying).await?;

    Ok(scan_naked_option_snapshot_at(
        underlying,
        &chain.contracts,
        &chain.snapshots,
        config,
        kind,
        underlying_price,
        capital,
        scan_date,
    ))
}

/// Ranks naked short-option candidates from already loaded scanner inputs.
#[must_use]
pub fn scan_naked_option_snapshot_at(
    underlying: impl Into<String>,
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &NakedOptionScannerConfig,
    kind: NakedOptionKind,
    underlying_price: f64,
    capital: Option<NakedOptionCapitalContext>,
    scan_date: NaiveDate,
) -> NakedOptionScanResult {
    let underlying = underlying.into();
    let (scored, mut rejection_counts) = score_naked_option_contracts_with_rejections_at(
        contracts,
        snapshots,
        config,
        kind,
        underlying_price,
        scan_date,
    );
    let (candidates, build_rejections) =
        build_naked_option_candidates_with_capital_and_rejections(&scored, config, capital);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    NakedOptionScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_credit_candidates_use_higher_long_strike() {
        let config = PutCreditScannerConfig {
            widths: vec![3.0],
            min_return_on_risk: 0.01,
            ..Default::default()
        };
        let contracts = vec![
            scored("SPY-C-710", 710.0, 2.50, 2.60),
            scored("SPY-C-713", 713.0, 2.00, 2.05),
        ];

        let candidates = build_candidates_for_kind(&contracts, &config, CreditSpreadKind::Call);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].short.symbol, "SPY-C-710");
        assert_eq!(candidates[0].long.symbol, "SPY-C-713");
    }

    #[test]
    fn put_credit_candidates_use_lower_long_strike() {
        let config = PutCreditScannerConfig {
            widths: vec![3.0],
            min_return_on_risk: 0.01,
            ..Default::default()
        };
        let contracts = vec![
            scored("SPY-P-705", 705.0, 2.00, 2.05),
            scored("SPY-P-708", 708.0, 2.50, 2.60),
        ];

        let candidates = build_candidates_for_kind(&contracts, &config, CreditSpreadKind::Put);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].short.symbol, "SPY-P-708");
        assert_eq!(candidates[0].long.symbol, "SPY-P-705");
    }

    #[test]
    fn call_debit_candidates_use_higher_short_strike() {
        let config = DebitSpreadScannerConfig {
            widths: vec![3.0],
            long_delta_min: 0.20,
            long_delta_max: 0.25,
            min_reward_to_risk: 0.01,
            max_debit_to_width: 0.90,
            ..Default::default()
        };
        let contracts = vec![
            scored("SPY-C-710", 710.0, 2.50, 2.60),
            scored("SPY-C-713", 713.0, 2.00, 2.05),
        ];

        let candidates =
            build_debit_candidates_for_kind(&contracts, &config, DebitSpreadKind::Call);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].long.symbol, "SPY-C-710");
        assert_eq!(candidates[0].short.symbol, "SPY-C-713");
        assert!((candidates[0].debit - 0.60).abs() < 0.01);
    }

    #[test]
    fn put_debit_candidates_use_lower_short_strike() {
        let config = DebitSpreadScannerConfig {
            widths: vec![3.0],
            long_delta_min: 0.20,
            long_delta_max: 0.25,
            min_reward_to_risk: 0.01,
            max_debit_to_width: 0.90,
            ..Default::default()
        };
        let contracts = vec![
            scored("SPY-P-705", 705.0, 2.00, 2.05),
            scored("SPY-P-708", 708.0, 2.50, 2.60),
        ];

        let candidates = build_debit_candidates_for_kind(&contracts, &config, DebitSpreadKind::Put);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].long.symbol, "SPY-P-708");
        assert_eq!(candidates[0].short.symbol, "SPY-P-705");
        assert!((candidates[0].debit - 0.60).abs() < 0.01);
    }

    #[test]
    fn credit_candidates_require_min_credit_to_width() {
        let config = PutCreditScannerConfig {
            widths: vec![3.0],
            min_return_on_risk: 0.01,
            min_credit_to_width: 0.05,
            ..Default::default()
        };
        let contracts = vec![
            scored("SPY-P-705", 705.0, 2.00, 2.05),
            scored("SPY-P-708", 708.0, 2.10, 2.15),
        ];

        let candidates = build_candidates_for_kind(&contracts, &config, CreditSpreadKind::Put);

        assert!(candidates.is_empty());
    }

    #[test]
    fn debit_candidates_require_min_debit_to_width() {
        let config = DebitSpreadScannerConfig {
            widths: vec![3.0],
            long_delta_min: 0.20,
            long_delta_max: 0.25,
            min_reward_to_risk: 0.01,
            min_debit_to_width: 0.25,
            max_debit_to_width: 0.90,
            ..Default::default()
        };
        let contracts = vec![
            scored("SPY-C-710", 710.0, 2.50, 2.60),
            scored("SPY-C-713", 713.0, 2.00, 2.05),
        ];

        let candidates =
            build_debit_candidates_for_kind(&contracts, &config, DebitSpreadKind::Call);

        assert!(candidates.is_empty());
    }

    #[test]
    fn scanner_score_prefers_centered_dte_when_other_inputs_match() {
        let config = PutCreditScannerConfig {
            min_dte: 5,
            max_dte: 15,
            widths: vec![3.0],
            min_return_on_risk: 0.01,
            ..Default::default()
        };
        let contracts = vec![
            scored_with_expiry("SPY260510P00705000", "2026-05-10", 5, 705.0, 2.00, 2.05),
            scored_with_expiry("SPY260510P00708000", "2026-05-10", 5, 708.0, 2.50, 2.60),
            scored_with_expiry("SPY260515P00705000", "2026-05-15", 10, 705.0, 2.00, 2.05),
            scored_with_expiry("SPY260515P00708000", "2026-05-15", 10, 708.0, 2.50, 2.60),
        ];

        let candidates = build_candidates_for_kind(&contracts, &config, CreditSpreadKind::Put);

        assert_eq!(candidates[0].short.expiration_date, "2026-05-15");
    }

    #[test]
    fn naked_option_candidates_rank_single_short_options() {
        let config = NakedOptionScannerConfig {
            min_credit: 0.20,
            min_score: 0.0,
            ..Default::default()
        };
        let contracts = vec![
            scored("SPY-C-710", 710.0, 0.30, 0.34),
            scored("SPY-C-713", 713.0, 0.50, 0.54),
        ];

        let candidates = build_naked_option_candidates(&contracts, &config);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].short.symbol, "SPY-C-713");
        assert!((candidates[0].credit - 0.50).abs() < 0.01);
    }

    #[test]
    fn naked_option_candidates_require_min_score() {
        let config = NakedOptionScannerConfig {
            min_credit: 0.20,
            min_score: 200.0,
            min_annualized_premium_yield: 0.25,
            ..Default::default()
        };
        let contracts = vec![scored_with_expiry(
            "SPY-C-713",
            "2026-05-15",
            10,
            713.0,
            0.30,
            0.34,
        )];

        let candidates = build_naked_option_candidates(&contracts, &config);

        assert!(candidates.is_empty());
    }

    #[test]
    fn iron_condor_candidates_combine_put_and_call_credit_wings() {
        let credit_config = PutCreditScannerConfig {
            widths: vec![3.0],
            min_return_on_risk: 0.01,
            ..Default::default()
        };
        let config = IronCondorScannerConfig {
            credit: credit_config.clone(),
            min_return_on_risk: 0.01,
            require_equal_widths: true,
        };
        let puts = build_candidates_for_kind(
            &[
                scored("SPY-P-705", 705.0, 2.00, 2.05),
                scored("SPY-P-708", 708.0, 2.50, 2.60),
            ],
            &credit_config,
            CreditSpreadKind::Put,
        );
        let calls = build_candidates_for_kind(
            &[
                scored("SPY-C-710", 710.0, 2.50, 2.60),
                scored("SPY-C-713", 713.0, 2.00, 2.05),
            ],
            &credit_config,
            CreditSpreadKind::Call,
        );

        let candidates = build_iron_condor_candidates(&puts, &calls, &config);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].put.short.symbol, "SPY-P-708");
        assert_eq!(candidates[0].put.long.symbol, "SPY-P-705");
        assert_eq!(candidates[0].call.short.symbol, "SPY-C-710");
        assert_eq!(candidates[0].call.long.symbol, "SPY-C-713");
        assert!((candidates[0].credit - 0.90).abs() < 0.01);
        assert!((candidates[0].max_loss - 2.10).abs() < 0.01);
    }

    fn scored(symbol: &str, strike: f64, bid: f64, ask: f64) -> ScoredContract {
        scored_with_expiry(symbol, "2026-05-15", 10, strike, bid, ask)
    }

    fn scored_with_expiry(
        symbol: &str,
        expiration_date: &str,
        dte: i64,
        strike: f64,
        bid: f64,
        ask: f64,
    ) -> ScoredContract {
        ScoredContract {
            symbol: symbol.to_string(),
            expiration_date: expiration_date.to_string(),
            dte,
            strike,
            bid,
            ask,
            delta_abs: 0.22,
            spread_pct: 0.02,
            bid_size: 10,
            ask_size: 10,
            volume: 100,
            open_interest: 1_000,
            implied_volatility: Some(0.20),
            metrics: option_candidate_metrics(
                if symbol.contains("-C-") {
                    NakedOptionKind::Call
                } else {
                    NakedOptionKind::Put
                },
                if symbol.contains("-C-") {
                    strike - 10.0
                } else {
                    strike + 10.0
                },
                strike,
                bid,
                dte,
                0.20,
                0.22,
            ),
        }
    }
}
