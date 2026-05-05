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

use std::collections::{BTreeMap, HashMap};

use chrono::{NaiveDate, Utc};
use nautilus_model::data::greeks::black_scholes_greeks;
use time::{Duration, OffsetDateTime};

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Result,
        models::{
            AlpacaOptionContract, AlpacaOptionSnapshot, AlpacaOptionType, OptionSnapshotsRequest,
            StockSnapshotsRequest,
        },
    },
    providers::AlpacaOptionContractProvider,
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
            min_dte: 7,
            max_dte: 21,
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
    let underlying = underlying.into();
    let put =
        scan_put_credit_underlying(client, data_config, &config.credit, underlying.clone()).await?;
    let call = scan_call_credit_underlying(client, data_config, &config.credit, underlying.clone())
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
    let underlying = underlying.into();
    let today = OffsetDateTime::now_utc().date();
    let min_expiration = (today + Duration::days(config.min_dte)).to_string();
    let max_expiration = (today + Duration::days(config.max_dte)).to_string();
    let provider = AlpacaOptionContractProvider::new(client.clone());

    let contracts = provider
        .load_active_contracts(
            underlying.clone(),
            min_expiration,
            max_expiration,
            Some(kind.option_type()),
        )
        .await?;
    let symbols = contracts
        .iter()
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();
    let mut snapshots_request = OptionSnapshotsRequest::for_symbols(symbols);
    snapshots_request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&snapshots_request).await?.snapshots;

    let (scored, mut rejection_counts) =
        score_contracts_with_rejections(&contracts, &snapshots, config);
    let (candidates, build_rejections) =
        build_candidates_for_kind_with_rejections(&scored, config, kind);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    Ok(PutCreditScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    })
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
    let underlying = underlying.into();
    let today = OffsetDateTime::now_utc().date();
    let min_expiration = (today + Duration::days(config.min_dte)).to_string();
    let max_expiration = (today + Duration::days(config.max_dte)).to_string();
    let provider = AlpacaOptionContractProvider::new(client.clone());

    let contracts = provider
        .load_active_contracts(
            underlying.clone(),
            min_expiration,
            max_expiration,
            Some(kind.option_type()),
        )
        .await?;
    let symbols = contracts
        .iter()
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();
    let mut snapshots_request = OptionSnapshotsRequest::for_symbols(symbols);
    snapshots_request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&snapshots_request).await?.snapshots;

    let (scored, mut rejection_counts) =
        score_debit_contracts_with_rejections(&contracts, &snapshots, config);
    let (candidates, build_rejections) =
        build_debit_candidates_for_kind_with_rejections(&scored, config, kind);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    Ok(DebitSpreadScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    })
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
    let underlying = underlying.into();
    let today = OffsetDateTime::now_utc().date();
    let min_expiration = (today + Duration::days(config.min_dte)).to_string();
    let max_expiration = (today + Duration::days(config.max_dte)).to_string();
    let provider = AlpacaOptionContractProvider::new(client.clone());

    let contracts = provider
        .load_active_contracts(
            underlying.clone(),
            min_expiration,
            max_expiration,
            Some(kind.option_type()),
        )
        .await?;
    let symbols = contracts
        .iter()
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();
    let mut snapshots_request = OptionSnapshotsRequest::for_symbols(symbols);
    snapshots_request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&snapshots_request).await?.snapshots;
    let underlying_price = load_underlying_price(client, data_config, &underlying).await?;

    let (scored, mut rejection_counts) = score_naked_option_contracts_with_rejections(
        &contracts,
        &snapshots,
        config,
        kind,
        underlying_price,
    );
    let (candidates, build_rejections) =
        build_naked_option_candidates_with_capital_and_rejections(&scored, config, capital);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    Ok(NakedOptionScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    })
}

async fn load_underlying_price(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    underlying: &str,
) -> Result<f64> {
    let mut request = StockSnapshotsRequest::for_symbols([underlying.to_string()]);
    request.feed = Some(data_config.stock_feed.as_str().to_string());
    let snapshots = client.stock_snapshots(&request).await?.snapshots;
    snapshots
        .get(underlying)
        .or_else(|| {
            snapshots
                .iter()
                .find(|(symbol, _)| symbol.eq_ignore_ascii_case(underlying))
                .map(|(_, snapshot)| snapshot)
        })
        .and_then(|snapshot| snapshot.latest_price())
        .ok_or_else(|| {
            crate::http::error::Error::Validation(format!(
                "stock snapshot missing latest price for {underlying}"
            ))
        })
}

/// Scores contracts that have enough quote, Greek, and liquidity data.
#[must_use]
pub fn score_contracts(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &PutCreditScannerConfig,
) -> Vec<ScoredContract> {
    score_contracts_with_rejections(contracts, snapshots, config).0
}

/// Scores contracts and counts filter rejections.
#[must_use]
pub fn score_contracts_with_rejections(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &PutCreditScannerConfig,
) -> (Vec<ScoredContract>, BTreeMap<String, usize>) {
    let mut scored = Vec::new();
    let mut rejections = BTreeMap::new();
    for contract in contracts {
        let Some(open_interest) = contract
            .open_interest
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            record_rejection(&mut rejections, "missing_open_interest");
            continue;
        };
        if open_interest < config.min_open_interest {
            record_rejection(&mut rejections, "min_open_interest");
            continue;
        }

        let Some(snapshot) = snapshots.get(&contract.symbol) else {
            record_rejection(&mut rejections, "missing_snapshot");
            continue;
        };
        let Some(quote) = snapshot.latest_quote.as_ref() else {
            record_rejection(&mut rejections, "missing_quote");
            continue;
        };
        let Some(bid) = quote.bid_price else {
            record_rejection(&mut rejections, "missing_bid");
            continue;
        };
        let Some(ask) = quote.ask_price else {
            record_rejection(&mut rejections, "missing_ask");
            continue;
        };
        let Some(midpoint) = quote.midpoint() else {
            record_rejection(&mut rejections, "missing_midpoint");
            continue;
        };
        let spread_pct = (ask - bid) / midpoint;
        if spread_pct > config.max_leg_spread_pct {
            record_rejection(&mut rejections, "max_leg_spread_pct");
            continue;
        }

        let Some(delta_abs) = snapshot
            .greeks
            .as_ref()
            .and_then(|greeks| greeks.delta)
            .map(f64::abs)
        else {
            record_rejection(&mut rejections, "missing_delta");
            continue;
        };
        if delta_abs < config.short_delta_min || delta_abs > config.short_delta_max {
            record_rejection(&mut rejections, "short_delta_range");
            continue;
        }
        let Some(dte) = days_to_expiration(&contract.expiration_date) else {
            record_rejection(&mut rejections, "invalid_expiration");
            continue;
        };
        let Ok(strike) = contract.strike_price.parse::<f64>() else {
            record_rejection(&mut rejections, "invalid_strike");
            continue;
        };

        scored.push(ScoredContract {
            symbol: contract.symbol.clone(),
            expiration_date: contract.expiration_date.clone(),
            dte,
            strike,
            bid,
            ask,
            delta_abs,
            spread_pct,
            bid_size: quote.bid_size.unwrap_or(0),
            ask_size: quote.ask_size.unwrap_or(0),
            volume: snapshot
                .daily_bar
                .as_ref()
                .and_then(|bar| bar.volume)
                .unwrap_or(0),
            open_interest,
            implied_volatility: snapshot.implied_volatility,
            metrics: None,
        });
    }
    (scored, rejections)
}

/// Scores contracts for long-premium debit-spread entries.
#[must_use]
pub fn score_debit_contracts(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &DebitSpreadScannerConfig,
) -> Vec<ScoredContract> {
    score_debit_contracts_with_rejections(contracts, snapshots, config).0
}

/// Scores debit-spread contracts and counts filter rejections.
#[must_use]
pub fn score_debit_contracts_with_rejections(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &DebitSpreadScannerConfig,
) -> (Vec<ScoredContract>, BTreeMap<String, usize>) {
    let mut scored = Vec::new();
    let mut rejections = BTreeMap::new();
    for contract in contracts {
        let Some(open_interest) = contract
            .open_interest
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            record_rejection(&mut rejections, "missing_open_interest");
            continue;
        };
        if open_interest < config.min_open_interest {
            record_rejection(&mut rejections, "min_open_interest");
            continue;
        }

        let Some(snapshot) = snapshots.get(&contract.symbol) else {
            record_rejection(&mut rejections, "missing_snapshot");
            continue;
        };
        let Some(quote) = snapshot.latest_quote.as_ref() else {
            record_rejection(&mut rejections, "missing_quote");
            continue;
        };
        let Some(bid) = quote.bid_price else {
            record_rejection(&mut rejections, "missing_bid");
            continue;
        };
        let Some(ask) = quote.ask_price else {
            record_rejection(&mut rejections, "missing_ask");
            continue;
        };
        let Some(midpoint) = quote.midpoint() else {
            record_rejection(&mut rejections, "missing_midpoint");
            continue;
        };
        let spread_pct = (ask - bid) / midpoint;
        if spread_pct > config.max_leg_spread_pct {
            record_rejection(&mut rejections, "max_leg_spread_pct");
            continue;
        }

        let Some(delta_abs) = snapshot
            .greeks
            .as_ref()
            .and_then(|greeks| greeks.delta)
            .map(f64::abs)
        else {
            record_rejection(&mut rejections, "missing_delta");
            continue;
        };
        if delta_abs < config.long_delta_min || delta_abs > config.long_delta_max {
            record_rejection(&mut rejections, "long_delta_range");
            continue;
        }
        let Some(dte) = days_to_expiration(&contract.expiration_date) else {
            record_rejection(&mut rejections, "invalid_expiration");
            continue;
        };
        let Ok(strike) = contract.strike_price.parse::<f64>() else {
            record_rejection(&mut rejections, "invalid_strike");
            continue;
        };

        scored.push(ScoredContract {
            symbol: contract.symbol.clone(),
            expiration_date: contract.expiration_date.clone(),
            dte,
            strike,
            bid,
            ask,
            delta_abs,
            spread_pct,
            bid_size: quote.bid_size.unwrap_or(0),
            ask_size: quote.ask_size.unwrap_or(0),
            volume: snapshot
                .daily_bar
                .as_ref()
                .and_then(|bar| bar.volume)
                .unwrap_or(0),
            open_interest,
            implied_volatility: snapshot.implied_volatility,
            metrics: None,
        });
    }
    (scored, rejections)
}

/// Scores contracts for naked short option entries.
#[must_use]
pub fn score_naked_option_contracts(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &NakedOptionScannerConfig,
    kind: NakedOptionKind,
    underlying_price: f64,
) -> Vec<ScoredContract> {
    score_naked_option_contracts_with_rejections(
        contracts,
        snapshots,
        config,
        kind,
        underlying_price,
    )
    .0
}

/// Scores naked-option contracts and counts filter rejections.
#[must_use]
pub fn score_naked_option_contracts_with_rejections(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &NakedOptionScannerConfig,
    kind: NakedOptionKind,
    underlying_price: f64,
) -> (Vec<ScoredContract>, BTreeMap<String, usize>) {
    let mut scored = Vec::new();
    let mut rejections = BTreeMap::new();
    for contract in contracts {
        let Some(open_interest) = contract
            .open_interest
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            record_rejection(&mut rejections, "missing_open_interest");
            continue;
        };
        if open_interest < config.min_open_interest {
            record_rejection(&mut rejections, "min_open_interest");
            continue;
        }

        let Some(snapshot) = snapshots.get(&contract.symbol) else {
            record_rejection(&mut rejections, "missing_snapshot");
            continue;
        };
        let Some(quote) = snapshot.latest_quote.as_ref() else {
            record_rejection(&mut rejections, "missing_quote");
            continue;
        };
        let Some(bid) = quote.bid_price else {
            record_rejection(&mut rejections, "missing_bid");
            continue;
        };
        let Some(ask) = quote.ask_price else {
            record_rejection(&mut rejections, "missing_ask");
            continue;
        };
        let Some(midpoint) = quote.midpoint() else {
            record_rejection(&mut rejections, "missing_midpoint");
            continue;
        };
        let bid_size = quote.bid_size.unwrap_or(0);
        let ask_size = quote.ask_size.unwrap_or(0);
        if bid_size < config.min_bid_size {
            record_rejection(&mut rejections, "min_bid_size");
            continue;
        }
        if ask_size < config.min_ask_size {
            record_rejection(&mut rejections, "min_ask_size");
            continue;
        }
        let volume = snapshot
            .daily_bar
            .as_ref()
            .and_then(|bar| bar.volume)
            .unwrap_or(0);
        if volume < config.min_daily_volume {
            record_rejection(&mut rejections, "min_daily_volume");
            continue;
        }
        let spread_pct = (ask - bid) / midpoint;
        if spread_pct > config.max_spread_pct {
            record_rejection(&mut rejections, "max_spread_pct");
            continue;
        }
        if bid < config.min_credit {
            record_rejection(&mut rejections, "min_credit");
            continue;
        }

        let Some(implied_volatility) = snapshot.implied_volatility else {
            record_rejection(&mut rejections, "missing_implied_volatility");
            continue;
        };
        if implied_volatility < config.min_implied_volatility {
            record_rejection(&mut rejections, "min_implied_volatility");
            continue;
        }
        if implied_volatility > config.max_implied_volatility {
            record_rejection(&mut rejections, "max_implied_volatility");
            continue;
        }
        let Ok(strike) = contract.strike_price.parse::<f64>() else {
            record_rejection(&mut rejections, "invalid_strike");
            continue;
        };
        let Some(dte) = days_to_expiration(&contract.expiration_date) else {
            record_rejection(&mut rejections, "invalid_expiration");
            continue;
        };
        let annualized_yield = annualized_premium_yield(bid, strike, dte);
        if annualized_yield < config.min_annualized_premium_yield {
            record_rejection(&mut rejections, "min_annualized_premium_yield");
            continue;
        }

        let Some(delta_abs) = snapshot
            .greeks
            .as_ref()
            .and_then(|greeks| greeks.delta)
            .map(f64::abs)
        else {
            record_rejection(&mut rejections, "missing_delta");
            continue;
        };
        if delta_abs < config.short_delta_min || delta_abs > config.short_delta_max {
            record_rejection(&mut rejections, "short_delta_range");
            continue;
        }
        let Some(metrics) = option_candidate_metrics(
            kind,
            underlying_price,
            strike,
            bid,
            dte,
            implied_volatility,
            delta_abs,
        ) else {
            record_rejection(&mut rejections, "metrics_unavailable");
            continue;
        };
        if metrics.breakeven_pop < config.min_breakeven_pop {
            record_rejection(&mut rejections, "min_breakeven_pop");
            continue;
        }
        if metrics.probability_of_touch_est > config.max_probability_of_touch {
            record_rejection(&mut rejections, "max_probability_of_touch");
            continue;
        }
        if metrics.distance_to_breakeven_pct < config.min_distance_to_breakeven_pct {
            record_rejection(&mut rejections, "min_distance_to_breakeven_pct");
            continue;
        }
        if metrics.expected_move_coverage < config.min_expected_move_coverage {
            record_rejection(&mut rejections, "min_expected_move_coverage");
            continue;
        }
        if metrics.return_on_buying_power < config.min_return_on_buying_power {
            record_rejection(&mut rejections, "min_return_on_buying_power");
            continue;
        }

        scored.push(ScoredContract {
            symbol: contract.symbol.clone(),
            expiration_date: contract.expiration_date.clone(),
            dte,
            strike,
            bid,
            ask,
            delta_abs,
            spread_pct,
            bid_size,
            ask_size,
            volume,
            open_interest,
            implied_volatility: Some(implied_volatility),
            metrics: Some(metrics),
        });
    }
    (scored, rejections)
}

/// Calculates scanner metrics for one short-option candidate.
#[must_use]
pub fn option_candidate_metrics(
    kind: NakedOptionKind,
    underlying_price: f64,
    strike: f64,
    credit: f64,
    dte: i64,
    implied_volatility: f64,
    delta_abs: f64,
) -> Option<OptionCandidateMetrics> {
    if underlying_price <= 0.0
        || strike <= 0.0
        || credit <= 0.0
        || dte <= 0
        || implied_volatility <= 0.0
    {
        return None;
    }

    let years = dte as f64 / DAYS_PER_YEAR;
    let is_call = kind.is_call();
    let breakeven = match kind {
        NakedOptionKind::Call | NakedOptionKind::CallOneToThreeDte => strike + credit,
        NakedOptionKind::Put | NakedOptionKind::PutOneToThreeDte => strike - credit,
    };
    if breakeven <= 0.0 {
        return None;
    }
    let (capital_requirement_model, estimated_buying_power_requirement) =
        estimated_naked_option_buying_power_requirement(kind, underlying_price, strike, credit)?;
    let return_on_buying_power =
        (credit * OPTION_CONTRACT_MULTIPLIER) / estimated_buying_power_requirement;

    let strike_greeks = black_scholes_greeks(
        underlying_price,
        SCANNER_RISK_FREE_RATE,
        SCANNER_RISK_FREE_RATE,
        implied_volatility,
        is_call,
        strike,
        years,
    );
    let breakeven_greeks = black_scholes_greeks(
        underlying_price,
        SCANNER_RISK_FREE_RATE,
        SCANNER_RISK_FREE_RATE,
        implied_volatility,
        is_call,
        breakeven,
        years,
    );

    let strike_itm_probability = strike_greeks.itm_prob.clamp(0.0, 1.0);
    let breakeven_breach_probability = breakeven_greeks.itm_prob.clamp(0.0, 1.0);
    let breakeven_pop = 1.0 - breakeven_breach_probability;
    let expected_move = underlying_price * implied_volatility * years.sqrt();
    let expected_move_pct = expected_move / underlying_price;
    let distance_to_strike_pct = directional_distance_pct(kind, underlying_price, strike);
    let distance_to_breakeven_pct = directional_distance_pct(kind, underlying_price, breakeven);
    let expected_move_coverage = if expected_move_pct > 0.0 {
        distance_to_breakeven_pct / expected_move_pct
    } else {
        0.0
    };

    Some(OptionCandidateMetrics {
        underlying_price,
        breakeven,
        strike_itm_probability,
        delta_pop_proxy: (1.0 - delta_abs).clamp(0.0, 1.0),
        breakeven_pop,
        probability_of_touch_est: (2.0 * strike_itm_probability).clamp(0.0, 1.0),
        expected_move,
        expected_move_pct,
        distance_to_strike_pct,
        distance_to_breakeven_pct,
        expected_move_coverage,
        capital_requirement_model,
        estimated_buying_power_requirement,
        return_on_buying_power,
        model_delta_abs: strike_greeks.delta.abs(),
        model_gamma: strike_greeks.gamma,
        model_theta: strike_greeks.theta,
        model_vega: strike_greeks.vega,
    })
}

/// Estimates one-contract buying-power requirement for a naked short option.
#[must_use]
pub fn estimated_naked_option_buying_power_requirement(
    kind: NakedOptionKind,
    underlying_price: f64,
    strike: f64,
    credit: f64,
) -> Option<(OptionCapitalRequirementModel, f64)> {
    if underlying_price <= 0.0 || strike <= 0.0 || credit <= 0.0 {
        return None;
    }

    let premium = credit * OPTION_CONTRACT_MULTIPLIER;
    match kind {
        NakedOptionKind::Put | NakedOptionKind::PutOneToThreeDte => Some((
            OptionCapitalRequirementModel::CashSecuredPut,
            strike * OPTION_CONTRACT_MULTIPLIER,
        )),
        NakedOptionKind::Call | NakedOptionKind::CallOneToThreeDte => {
            let underlying_notional = underlying_price * OPTION_CONTRACT_MULTIPLIER;
            let out_of_the_money =
                (strike - underlying_price).max(0.0) * OPTION_CONTRACT_MULTIPLIER;
            let twenty_percent_test = 0.20 * underlying_notional - out_of_the_money;
            let ten_percent_test = 0.10 * underlying_notional;
            Some((
                OptionCapitalRequirementModel::RegTShortCallEstimate,
                premium + twenty_percent_test.max(ten_percent_test).max(0.0),
            ))
        }
    }
}

/// Builds and ranks put credit spread candidates from scored put contracts.
#[must_use]
pub fn build_candidates(
    contracts: &[ScoredContract],
    config: &PutCreditScannerConfig,
) -> Vec<SpreadCandidate> {
    build_candidates_for_kind(contracts, config, CreditSpreadKind::Put)
}

/// Builds and ranks vertical credit spread candidates from scored contracts.
#[must_use]
pub fn build_candidates_for_kind(
    contracts: &[ScoredContract],
    config: &PutCreditScannerConfig,
    kind: CreditSpreadKind,
) -> Vec<SpreadCandidate> {
    build_candidates_for_kind_with_rejections(contracts, config, kind).0
}

/// Builds and ranks vertical credit spread candidates and counts build rejections.
#[must_use]
pub fn build_candidates_for_kind_with_rejections(
    contracts: &[ScoredContract],
    config: &PutCreditScannerConfig,
    kind: CreditSpreadKind,
) -> (Vec<SpreadCandidate>, BTreeMap<String, usize>) {
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
    let mut rejections = BTreeMap::new();
    for short in contracts {
        for width in &config.widths {
            let long_strike = kind.long_strike(short.strike, *width);
            let Some(long) = by_expiration_strike
                .get(&(short.expiration_date.clone(), strike_key(long_strike)))
                .copied()
            else {
                record_rejection(&mut rejections, "missing_long_leg");
                continue;
            };

            let credit = short.bid - long.ask;
            if credit <= 0.0 {
                record_rejection(&mut rejections, "non_positive_credit");
                continue;
            }
            let max_loss = width - credit;
            if max_loss <= 0.0 {
                record_rejection(&mut rejections, "non_positive_max_loss");
                continue;
            }
            let return_on_risk = credit / max_loss;
            if return_on_risk < config.min_return_on_risk {
                record_rejection(&mut rejections, "min_return_on_risk");
                continue;
            }
            let credit_to_width = credit / *width;
            if credit_to_width < config.min_credit_to_width {
                record_rejection(&mut rejections, "min_credit_to_width");
                continue;
            }

            let delta_midpoint = (config.short_delta_min + config.short_delta_max) / 2.0;
            let delta_half_range = (config.short_delta_max - config.short_delta_min) / 2.0;
            let delta_score =
                (1.0 - ((short.delta_abs - delta_midpoint).abs() / delta_half_range)).max(0.0);
            let ror_score = (return_on_risk / config.min_return_on_risk).min(2.0) / 2.0;
            let credit_score = credit_to_width.min(0.5) / 0.5;
            let term_score = term_score(short.dte, config.min_dte, config.max_dte);
            let liquidity_score = liquidity_score(
                short.open_interest.min(long.open_interest),
                config.min_open_interest,
            );
            let spread_penalty =
                ((short.spread_pct + long.spread_pct) / (2.0 * config.max_leg_spread_pct)).min(1.0);
            let score = delta_score * 35.0
                + ror_score * 30.0
                + credit_score * 25.0
                + term_score * 10.0
                + liquidity_score * 5.0
                - spread_penalty * 10.0;

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
    (candidates, rejections)
}

/// Builds and ranks vertical debit spread candidates from scored contracts.
#[must_use]
pub fn build_debit_candidates_for_kind(
    contracts: &[ScoredContract],
    config: &DebitSpreadScannerConfig,
    kind: DebitSpreadKind,
) -> Vec<DebitSpreadCandidate> {
    build_debit_candidates_for_kind_with_rejections(contracts, config, kind).0
}

/// Builds and ranks vertical debit spread candidates and counts build rejections.
#[must_use]
pub fn build_debit_candidates_for_kind_with_rejections(
    contracts: &[ScoredContract],
    config: &DebitSpreadScannerConfig,
    kind: DebitSpreadKind,
) -> (Vec<DebitSpreadCandidate>, BTreeMap<String, usize>) {
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
    let mut rejections = BTreeMap::new();
    for long in contracts {
        for width in &config.widths {
            let short_strike = kind.short_strike(long.strike, *width);
            let Some(short) = by_expiration_strike
                .get(&(long.expiration_date.clone(), strike_key(short_strike)))
                .copied()
            else {
                record_rejection(&mut rejections, "missing_short_leg");
                continue;
            };

            let debit = long.ask - short.bid;
            if debit <= 0.0 || debit >= *width {
                record_rejection(&mut rejections, "invalid_debit");
                continue;
            }
            let debit_to_width = debit / *width;
            if debit_to_width > config.max_debit_to_width
                || debit_to_width < config.min_debit_to_width
            {
                record_rejection(&mut rejections, "debit_to_width_range");
                continue;
            }
            let max_profit = width - debit;
            let reward_to_risk = max_profit / debit;
            if reward_to_risk < config.min_reward_to_risk {
                record_rejection(&mut rejections, "min_reward_to_risk");
                continue;
            }

            let delta_midpoint = (config.long_delta_min + config.long_delta_max) / 2.0;
            let delta_half_range = (config.long_delta_max - config.long_delta_min) / 2.0;
            let delta_score =
                (1.0 - ((long.delta_abs - delta_midpoint).abs() / delta_half_range)).max(0.0);
            let reward_score = (reward_to_risk / config.min_reward_to_risk).min(2.0) / 2.0;
            let debit_score = (1.0 - debit_to_width / config.max_debit_to_width).max(0.0);
            let term_score = term_score(long.dte, config.min_dte, config.max_dte);
            let liquidity_score = liquidity_score(
                long.open_interest.min(short.open_interest),
                config.min_open_interest,
            );
            let spread_penalty =
                ((long.spread_pct + short.spread_pct) / (2.0 * config.max_leg_spread_pct)).min(1.0);
            let score = delta_score * 35.0
                + reward_score * 30.0
                + debit_score * 25.0
                + term_score * 10.0
                + liquidity_score * 5.0
                - spread_penalty * 10.0;

            candidates.push(DebitSpreadCandidate {
                long: long.clone(),
                short: short.clone(),
                width: *width,
                debit,
                max_profit,
                max_loss: debit,
                reward_to_risk,
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
    (candidates, rejections)
}

/// Builds and ranks naked short option candidates from scored contracts.
#[must_use]
pub fn build_naked_option_candidates(
    contracts: &[ScoredContract],
    config: &NakedOptionScannerConfig,
) -> Vec<NakedOptionCandidate> {
    build_naked_option_candidates_with_capital(contracts, config, None)
}

/// Builds and ranks naked short option candidates with account capital context.
#[must_use]
pub fn build_naked_option_candidates_with_capital(
    contracts: &[ScoredContract],
    config: &NakedOptionScannerConfig,
    capital: Option<NakedOptionCapitalContext>,
) -> Vec<NakedOptionCandidate> {
    build_naked_option_candidates_with_capital_and_rejections(contracts, config, capital).0
}

/// Builds and ranks naked short option candidates with capital context and rejection counts.
#[must_use]
pub fn build_naked_option_candidates_with_capital_and_rejections(
    contracts: &[ScoredContract],
    config: &NakedOptionScannerConfig,
    capital: Option<NakedOptionCapitalContext>,
) -> (Vec<NakedOptionCandidate>, BTreeMap<String, usize>) {
    let quantity = capital.map_or(1, |context| context.quantity.max(1)) as f64;
    let options_buying_power = capital.and_then(|context| {
        context
            .options_buying_power
            .filter(|options_buying_power| *options_buying_power > 0.0)
    });
    let mut candidates = Vec::new();
    let mut rejections = BTreeMap::new();
    for short in contracts {
        let Some(metrics) = short.metrics.as_ref() else {
            record_rejection(&mut rejections, "missing_metrics");
            continue;
        };
        let estimated_buying_power_requirement =
            metrics.estimated_buying_power_requirement * quantity;
        if estimated_buying_power_requirement <= 0.0 {
            record_rejection(&mut rejections, "invalid_buying_power_requirement");
            continue;
        }
        let total_credit = short.bid * OPTION_CONTRACT_MULTIPLIER * quantity;
        let return_on_buying_power = total_credit / estimated_buying_power_requirement;
        if return_on_buying_power < config.min_return_on_buying_power {
            record_rejection(&mut rejections, "min_return_on_buying_power");
            continue;
        }
        let buying_power_usage_pct = options_buying_power
            .map(|buying_power| estimated_buying_power_requirement / buying_power);
        if buying_power_usage_pct.is_some_and(|usage| usage > config.max_buying_power_usage_pct) {
            record_rejection(&mut rejections, "max_buying_power_usage_pct");
            continue;
        }

        let delta_score = centered_score(
            short.delta_abs,
            config.short_delta_min,
            config.short_delta_max,
        );
        let credit_score = capped_ratio_score(short.bid, config.min_credit.max(0.01), 3.0);
        let annualized_yield = annualized_premium_yield(short.bid, short.strike, short.dte);
        let yield_score = capped_ratio_score(
            annualized_yield,
            config.min_annualized_premium_yield.max(0.01),
            2.0,
        );
        let term_score = term_score(short.dte, config.min_dte, config.max_dte);
        let open_interest_score = liquidity_score(short.open_interest, config.min_open_interest);
        let size_score = (liquidity_score(short.bid_size, config.min_bid_size)
            + liquidity_score(short.ask_size, config.min_ask_size))
            / 2.0;
        let volume_score = liquidity_score(short.volume, config.min_daily_volume);
        let iv_score = short
            .implied_volatility
            .map(|iv| {
                centered_score(
                    iv,
                    config.min_implied_volatility,
                    config.max_implied_volatility,
                )
            })
            .unwrap_or(0.0);
        let breakeven_pop_score = capped_ratio_score(
            metrics.breakeven_pop,
            config.min_breakeven_pop.max(0.01),
            1.25,
        );
        let touch_score = 1.0
            - (metrics.probability_of_touch_est / config.max_probability_of_touch.max(0.01))
                .clamp(0.0, 1.0);
        let breakeven_distance_score = capped_ratio_score(
            metrics.distance_to_breakeven_pct,
            config.min_distance_to_breakeven_pct.max(0.0025),
            4.0,
        );
        let expected_move_score = metrics.expected_move_coverage.clamp(0.0, 1.0);
        let capital_score = capped_ratio_score(
            return_on_buying_power,
            config.min_return_on_buying_power.max(0.0001),
            2.0,
        );
        let buying_power_usage_score = buying_power_usage_pct
            .map(|usage| {
                1.0 - (usage / config.max_buying_power_usage_pct.max(0.0001)).clamp(0.0, 1.0)
            })
            .unwrap_or(0.5);
        let spread_penalty = (short.spread_pct / config.max_spread_pct.max(0.01)).min(1.0);
        let score = breakeven_pop_score * 20.0
            + yield_score * 18.0
            + delta_score * 15.0
            + touch_score * 12.0
            + expected_move_score * 12.0
            + credit_score * 10.0
            + breakeven_distance_score * 8.0
            + term_score * 8.0
            + open_interest_score * 8.0
            + size_score * 6.0
            + volume_score * 6.0
            + iv_score * 5.0
            + capital_score * 6.0
            + buying_power_usage_score * 4.0
            - spread_penalty * 12.0;
        if score < config.min_score {
            record_rejection(&mut rejections, "min_score");
            continue;
        }
        candidates.push(NakedOptionCandidate {
            short: short.clone(),
            credit: short.bid,
            capital_requirement_model: metrics.capital_requirement_model,
            estimated_buying_power_requirement,
            buying_power_usage_pct,
            return_on_buying_power,
            score,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (candidates, rejections)
}

/// Builds and ranks four-leg iron-condor candidates from put and call credit candidates.
#[must_use]
pub fn build_iron_condor_candidates(
    put_candidates: &[SpreadCandidate],
    call_candidates: &[SpreadCandidate],
    config: &IronCondorScannerConfig,
) -> Vec<IronCondorCandidate> {
    build_iron_condor_candidates_with_rejections(put_candidates, call_candidates, config).0
}

/// Builds and ranks four-leg iron-condor candidates and counts build rejections.
#[must_use]
pub fn build_iron_condor_candidates_with_rejections(
    put_candidates: &[SpreadCandidate],
    call_candidates: &[SpreadCandidate],
    config: &IronCondorScannerConfig,
) -> (Vec<IronCondorCandidate>, BTreeMap<String, usize>) {
    let mut candidates = Vec::new();
    let mut rejections = BTreeMap::new();
    for put in put_candidates {
        for call in call_candidates {
            if put.short.expiration_date != call.short.expiration_date {
                record_rejection(&mut rejections, "expiration_mismatch");
                continue;
            }
            if put.short.strike >= call.short.strike {
                record_rejection(&mut rejections, "crossed_wings");
                continue;
            }
            if config.require_equal_widths && strike_key(put.width) != strike_key(call.width) {
                record_rejection(&mut rejections, "unequal_widths");
                continue;
            }

            let credit = put.credit + call.credit;
            let max_width = put.width.max(call.width);
            let max_loss = max_width - credit;
            if max_loss <= 0.0 {
                record_rejection(&mut rejections, "non_positive_max_loss");
                continue;
            }
            let return_on_risk = credit / max_loss;
            if return_on_risk < config.min_return_on_risk {
                record_rejection(&mut rejections, "min_return_on_risk");
                continue;
            }

            let wing_balance_penalty = ((put.credit - call.credit).abs() / credit).min(1.0);
            let score = (put.score + call.score) / 2.0
                + (return_on_risk / config.min_return_on_risk).min(2.0) * 15.0
                - wing_balance_penalty * 10.0;

            candidates.push(IronCondorCandidate {
                put: put.clone(),
                call: call.clone(),
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
    (candidates, rejections)
}

fn record_rejection(rejections: &mut BTreeMap<String, usize>, reason: &'static str) {
    *rejections.entry(reason.to_string()).or_insert(0) += 1;
}

fn merge_rejection_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
    for (reason, count) in source {
        *target.entry(reason.clone()).or_insert(0) += count;
    }
}

fn strike_key(strike: f64) -> i64 {
    (strike * 1_000.0).round() as i64
}

fn days_to_expiration(expiration_date: &str) -> Option<i64> {
    let expiration = NaiveDate::parse_from_str(expiration_date, "%Y-%m-%d").ok()?;
    Some(
        expiration
            .signed_duration_since(Utc::now().date_naive())
            .num_days(),
    )
}

fn term_score(dte: i64, min_dte: i64, max_dte: i64) -> f64 {
    if max_dte <= min_dte {
        return if dte == min_dte { 1.0 } else { 0.0 };
    }
    let midpoint = (min_dte + max_dte) as f64 / 2.0;
    let half_range = (max_dte - min_dte) as f64 / 2.0;
    (1.0 - ((dte as f64 - midpoint).abs() / half_range)).max(0.0)
}

fn centered_score(value: f64, min_value: f64, max_value: f64) -> f64 {
    if max_value <= min_value {
        return if (value - min_value).abs() < f64::EPSILON {
            1.0
        } else {
            0.0
        };
    }
    let midpoint = (min_value + max_value) / 2.0;
    let half_range = (max_value - min_value) / 2.0;
    (1.0 - ((value - midpoint).abs() / half_range)).clamp(0.0, 1.0)
}

fn capped_ratio_score(value: f64, minimum: f64, full_score_multiple: f64) -> f64 {
    if minimum <= 0.0 {
        return 1.0;
    }
    (value / (minimum * full_score_multiple.max(1.0))).clamp(0.0, 1.0)
}

fn directional_distance_pct(kind: NakedOptionKind, underlying_price: f64, threshold: f64) -> f64 {
    if underlying_price <= 0.0 {
        return 0.0;
    }
    match kind {
        NakedOptionKind::Call | NakedOptionKind::CallOneToThreeDte => {
            (threshold - underlying_price) / underlying_price
        }
        NakedOptionKind::Put | NakedOptionKind::PutOneToThreeDte => {
            (underlying_price - threshold) / underlying_price
        }
    }
}

/// Returns annualized premium yield using credit / strike / DTE.
#[must_use]
pub fn annualized_premium_yield(credit: f64, strike: f64, dte: i64) -> f64 {
    if credit <= 0.0 || strike <= 0.0 || dte <= 0 {
        return 0.0;
    }
    credit / strike * 365.0 / dte as f64
}

fn liquidity_score(open_interest: u64, min_open_interest: u64) -> f64 {
    if min_open_interest == 0 {
        return 1.0;
    }
    (open_interest as f64 / (min_open_interest as f64 * 5.0)).min(1.0)
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
