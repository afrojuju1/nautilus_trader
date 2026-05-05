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
use time::{Duration, OffsetDateTime};

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Result,
        models::{
            AlpacaOptionContract, AlpacaOptionSnapshot, AlpacaOptionType, OptionSnapshotsRequest,
        },
    },
    providers::AlpacaOptionContractProvider,
};

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
}

impl NakedOptionKind {
    fn option_type(self) -> AlpacaOptionType {
        match self {
            Self::Call => AlpacaOptionType::Call,
            Self::Put => AlpacaOptionType::Put,
        }
    }
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
    /// Contract open interest.
    pub open_interest: u64,
    /// Implied volatility, if present.
    pub implied_volatility: Option<f64>,
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
    scan_naked_option_underlying(
        client,
        data_config,
        config,
        underlying,
        NakedOptionKind::Call,
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
    scan_naked_option_underlying(
        client,
        data_config,
        config,
        underlying,
        NakedOptionKind::Put,
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
    let candidates = build_iron_condor_candidates(&put.candidates, &call.candidates, config);
    Ok(IronCondorScanResult {
        underlying,
        contract_count: put.contract_count + call.contract_count,
        snapshot_count: put.snapshot_count + call.snapshot_count,
        scoreable_count: put.scoreable_count + call.scoreable_count,
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

    let scored = score_contracts(&contracts, &snapshots, config);
    let candidates = build_candidates_for_kind(&scored, config, kind);
    Ok(PutCreditScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
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

    let scored = score_debit_contracts(&contracts, &snapshots, config);
    let candidates = build_debit_candidates_for_kind(&scored, config, kind);
    Ok(DebitSpreadScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
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

    let scored = score_naked_option_contracts(&contracts, &snapshots, config);
    let candidates = build_naked_option_candidates(&scored, config);
    Ok(NakedOptionScanResult {
        underlying,
        contract_count: contracts.len(),
        snapshot_count: snapshots.len(),
        scoreable_count: scored.len(),
        candidates,
    })
}

/// Scores contracts that have enough quote, Greek, and liquidity data.
#[must_use]
pub fn score_contracts(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &PutCreditScannerConfig,
) -> Vec<ScoredContract> {
    contracts
        .iter()
        .filter_map(|contract| {
            let open_interest = contract
                .open_interest
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())?;
            if open_interest < config.min_open_interest {
                return None;
            }

            let snapshot = snapshots.get(&contract.symbol)?;
            let quote = snapshot.latest_quote.as_ref()?;
            let bid = quote.bid_price?;
            let ask = quote.ask_price?;
            let midpoint = quote.midpoint()?;
            let spread_pct = (ask - bid) / midpoint;
            if spread_pct > config.max_leg_spread_pct {
                return None;
            }

            let delta_abs = snapshot.greeks.as_ref()?.delta?.abs();
            if delta_abs < config.short_delta_min || delta_abs > config.short_delta_max {
                return None;
            }

            Some(ScoredContract {
                symbol: contract.symbol.clone(),
                expiration_date: contract.expiration_date.clone(),
                dte: days_to_expiration(&contract.expiration_date)?,
                strike: contract.strike_price.parse::<f64>().ok()?,
                bid,
                ask,
                delta_abs,
                spread_pct,
                open_interest,
                implied_volatility: snapshot.implied_volatility,
            })
        })
        .collect()
}

/// Scores contracts for long-premium debit-spread entries.
#[must_use]
pub fn score_debit_contracts(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &DebitSpreadScannerConfig,
) -> Vec<ScoredContract> {
    contracts
        .iter()
        .filter_map(|contract| {
            let open_interest = contract
                .open_interest
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())?;
            if open_interest < config.min_open_interest {
                return None;
            }

            let snapshot = snapshots.get(&contract.symbol)?;
            let quote = snapshot.latest_quote.as_ref()?;
            let bid = quote.bid_price?;
            let ask = quote.ask_price?;
            let midpoint = quote.midpoint()?;
            let spread_pct = (ask - bid) / midpoint;
            if spread_pct > config.max_leg_spread_pct {
                return None;
            }

            let delta_abs = snapshot.greeks.as_ref()?.delta?.abs();
            if delta_abs < config.long_delta_min || delta_abs > config.long_delta_max {
                return None;
            }

            Some(ScoredContract {
                symbol: contract.symbol.clone(),
                expiration_date: contract.expiration_date.clone(),
                dte: days_to_expiration(&contract.expiration_date)?,
                strike: contract.strike_price.parse::<f64>().ok()?,
                bid,
                ask,
                delta_abs,
                spread_pct,
                open_interest,
                implied_volatility: snapshot.implied_volatility,
            })
        })
        .collect()
}

/// Scores contracts for naked short option entries.
#[must_use]
pub fn score_naked_option_contracts(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &NakedOptionScannerConfig,
) -> Vec<ScoredContract> {
    contracts
        .iter()
        .filter_map(|contract| {
            let open_interest = contract
                .open_interest
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())?;
            if open_interest < config.min_open_interest {
                return None;
            }

            let snapshot = snapshots.get(&contract.symbol)?;
            let quote = snapshot.latest_quote.as_ref()?;
            let bid = quote.bid_price?;
            let ask = quote.ask_price?;
            let midpoint = quote.midpoint()?;
            let spread_pct = (ask - bid) / midpoint;
            if spread_pct > config.max_spread_pct || bid < config.min_credit {
                return None;
            }

            let delta_abs = snapshot.greeks.as_ref()?.delta?.abs();
            if delta_abs < config.short_delta_min || delta_abs > config.short_delta_max {
                return None;
            }

            Some(ScoredContract {
                symbol: contract.symbol.clone(),
                expiration_date: contract.expiration_date.clone(),
                dte: days_to_expiration(&contract.expiration_date)?,
                strike: contract.strike_price.parse::<f64>().ok()?,
                bid,
                ask,
                delta_abs,
                spread_pct,
                open_interest,
                implied_volatility: snapshot.implied_volatility,
            })
        })
        .collect()
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
    for short in contracts {
        for width in &config.widths {
            let long_strike = kind.long_strike(short.strike, *width);
            let Some(long) = by_expiration_strike
                .get(&(short.expiration_date.clone(), strike_key(long_strike)))
                .copied()
            else {
                continue;
            };

            let credit = short.bid - long.ask;
            if credit <= 0.0 {
                continue;
            }
            let max_loss = width - credit;
            if max_loss <= 0.0 {
                continue;
            }
            let return_on_risk = credit / max_loss;
            if return_on_risk < config.min_return_on_risk {
                continue;
            }
            let credit_to_width = credit / *width;
            if credit_to_width < config.min_credit_to_width {
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
    candidates
}

/// Builds and ranks vertical debit spread candidates from scored contracts.
#[must_use]
pub fn build_debit_candidates_for_kind(
    contracts: &[ScoredContract],
    config: &DebitSpreadScannerConfig,
    kind: DebitSpreadKind,
) -> Vec<DebitSpreadCandidate> {
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
    for long in contracts {
        for width in &config.widths {
            let short_strike = kind.short_strike(long.strike, *width);
            let Some(short) = by_expiration_strike
                .get(&(long.expiration_date.clone(), strike_key(short_strike)))
                .copied()
            else {
                continue;
            };

            let debit = long.ask - short.bid;
            if debit <= 0.0 || debit >= *width {
                continue;
            }
            let debit_to_width = debit / *width;
            if debit_to_width > config.max_debit_to_width
                || debit_to_width < config.min_debit_to_width
            {
                continue;
            }
            let max_profit = width - debit;
            let reward_to_risk = max_profit / debit;
            if reward_to_risk < config.min_reward_to_risk {
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
    candidates
}

/// Builds and ranks naked short option candidates from scored contracts.
#[must_use]
pub fn build_naked_option_candidates(
    contracts: &[ScoredContract],
    config: &NakedOptionScannerConfig,
) -> Vec<NakedOptionCandidate> {
    let mut candidates = contracts
        .iter()
        .map(|short| {
            let delta_midpoint = (config.short_delta_min + config.short_delta_max) / 2.0;
            let delta_half_range = (config.short_delta_max - config.short_delta_min) / 2.0;
            let delta_score =
                (1.0 - ((short.delta_abs - delta_midpoint).abs() / delta_half_range)).max(0.0);
            let credit_score = (short.bid / config.min_credit.max(0.01)).min(3.0) / 3.0;
            let term_score = term_score(short.dte, config.min_dte, config.max_dte);
            let liquidity_score = liquidity_score(short.open_interest, config.min_open_interest);
            let spread_penalty = (short.spread_pct / config.max_spread_pct).min(1.0);
            let score = delta_score * 35.0
                + credit_score * 30.0
                + term_score * 15.0
                + liquidity_score * 10.0
                - spread_penalty * 10.0;
            NakedOptionCandidate {
                short: short.clone(),
                credit: short.bid,
                score,
            }
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// Builds and ranks four-leg iron-condor candidates from put and call credit candidates.
#[must_use]
pub fn build_iron_condor_candidates(
    put_candidates: &[SpreadCandidate],
    call_candidates: &[SpreadCandidate],
    config: &IronCondorScannerConfig,
) -> Vec<IronCondorCandidate> {
    let mut candidates = Vec::new();
    for put in put_candidates {
        for call in call_candidates {
            if put.short.expiration_date != call.short.expiration_date {
                continue;
            }
            if put.short.strike >= call.short.strike {
                continue;
            }
            if config.require_equal_widths && strike_key(put.width) != strike_key(call.width) {
                continue;
            }

            let credit = put.credit + call.credit;
            let max_width = put.width.max(call.width);
            let max_loss = max_width - credit;
            if max_loss <= 0.0 {
                continue;
            }
            let return_on_risk = credit / max_loss;
            if return_on_risk < config.min_return_on_risk {
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
    candidates
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
            open_interest: 1_000,
            implied_volatility: Some(0.20),
        }
    }
}
