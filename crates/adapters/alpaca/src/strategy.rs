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
    candidate_engine::{
        CandidateContract, CandidateMarketSnapshot, CandidateQuote, CreditSpreadKind,
        CreditSpreadScanResult, DebitSpreadKind, DebitSpreadScanResult, DebitSpreadScannerConfig,
        IronCondorScanResult, IronCondorScannerConfig, NakedOptionCapitalContext, NakedOptionKind,
        NakedOptionScanResult, NakedOptionScannerConfig, PutCreditScannerConfig,
        build_candidates_for_kind_with_rejections, build_debit_candidates_for_kind_with_rejections,
        build_iron_condor_candidates_with_rejections,
        build_naked_option_candidates_with_capital_and_rejections, merge_rejection_counts,
        score_contracts_with_rejections_at, score_debit_contracts_with_rejections_at,
        score_naked_option_contracts_with_rejections_at,
    },
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Result,
        models::{AlpacaOptionContract, AlpacaOptionSnapshot, AlpacaOptionType},
    },
};

mod chain;

use chain::{load_option_chain_snapshot_at, load_underlying_price};

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
) -> Result<CreditSpreadScanResult> {
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
) -> Result<CreditSpreadScanResult> {
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
) -> Result<CreditSpreadScanResult> {
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
) -> Result<CreditSpreadScanResult> {
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
) -> Result<CreditSpreadScanResult> {
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
) -> Result<CreditSpreadScanResult> {
    let underlying = underlying.into();
    let chain = load_option_chain_snapshot_at(
        client,
        data_config,
        &underlying,
        config.min_dte,
        config.max_dte,
        credit_option_type(kind),
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
) -> CreditSpreadScanResult {
    let underlying = underlying.into();
    let candidate_contracts = candidate_contracts(contracts);
    let candidate_snapshots = candidate_snapshots(snapshots);
    let (scored, mut rejection_counts) = score_contracts_with_rejections_at(
        &candidate_contracts,
        &candidate_snapshots,
        config,
        scan_date,
    );
    let (candidates, build_rejections) =
        build_candidates_for_kind_with_rejections(&scored, config, kind);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);
    CreditSpreadScanResult {
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
        debit_option_type(kind),
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
    let candidate_contracts = candidate_contracts(contracts);
    let candidate_snapshots = candidate_snapshots(snapshots);
    let (scored, mut rejection_counts) = score_debit_contracts_with_rejections_at(
        &candidate_contracts,
        &candidate_snapshots,
        config,
        scan_date,
    );
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
        naked_option_type(kind),
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
    let candidate_contracts = candidate_contracts(contracts);
    let candidate_snapshots = candidate_snapshots(snapshots);
    let (scored, mut rejection_counts) = score_naked_option_contracts_with_rejections_at(
        &candidate_contracts,
        &candidate_snapshots,
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

fn credit_option_type(kind: CreditSpreadKind) -> AlpacaOptionType {
    match kind {
        CreditSpreadKind::Put => AlpacaOptionType::Put,
        CreditSpreadKind::Call => AlpacaOptionType::Call,
    }
}

fn debit_option_type(kind: DebitSpreadKind) -> AlpacaOptionType {
    match kind {
        DebitSpreadKind::Call => AlpacaOptionType::Call,
        DebitSpreadKind::Put => AlpacaOptionType::Put,
    }
}

fn naked_option_type(kind: NakedOptionKind) -> AlpacaOptionType {
    match kind {
        NakedOptionKind::Call | NakedOptionKind::CallOneToThreeDte => AlpacaOptionType::Call,
        NakedOptionKind::Put | NakedOptionKind::PutOneToThreeDte => AlpacaOptionType::Put,
    }
}

fn candidate_contracts(contracts: &[AlpacaOptionContract]) -> Vec<CandidateContract> {
    contracts
        .iter()
        .map(|contract| CandidateContract {
            symbol: contract.symbol.clone(),
            expiration_date: contract.expiration_date.clone(),
            strike: contract.strike_price.parse::<f64>().ok(),
            open_interest: contract
                .open_interest
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok()),
        })
        .collect()
}

fn candidate_snapshots(
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
) -> BTreeMap<String, CandidateMarketSnapshot> {
    snapshots
        .iter()
        .map(|(symbol, snapshot)| {
            (
                symbol.clone(),
                CandidateMarketSnapshot {
                    quote: snapshot.latest_quote.as_ref().map(|quote| CandidateQuote {
                        bid: quote.bid_price,
                        ask: quote.ask_price,
                        bid_size: quote.bid_size.unwrap_or(0),
                        ask_size: quote.ask_size.unwrap_or(0),
                    }),
                    delta: snapshot.greeks.as_ref().and_then(|greeks| greeks.delta),
                    implied_volatility: snapshot.implied_volatility,
                    volume: snapshot
                        .daily_bar
                        .as_ref()
                        .and_then(|bar| bar.volume)
                        .unwrap_or(0),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate_engine::{
        ScoredContract, build_candidates_for_kind, build_debit_candidates_for_kind,
        build_iron_condor_candidates, build_naked_option_candidates, option_candidate_metrics,
    };

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
