//! Candidate-engine adapter for Nautilus option-chain snapshots.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use nautilus_model::data::option_chain::{OptionChainSlice, OptionGreeks, OptionStrikeData};

use crate::candidate_engine::{
    CandidateContract, CandidateMarketSnapshot, CandidateQuote, CreditSpreadKind,
    CreditSpreadScanResult, DebitSpreadKind, DebitSpreadScanResult, DebitSpreadScannerConfig,
    IronCondorScanResult, IronCondorScannerConfig, NakedOptionCapitalContext, NakedOptionKind,
    NakedOptionScanResult, NakedOptionScannerConfig, PutCreditScannerConfig,
    build_candidates_for_kind_with_rejections, build_debit_candidates_for_kind_with_rejections,
    build_iron_condor_candidates_with_rejections,
    build_naked_option_candidates_with_capital_and_rejections, merge_rejection_counts,
    score_contracts_with_rejections_at, score_debit_contracts_with_rejections_at,
    score_naked_option_contracts_with_rejections_at,
};

/// Normalized candidate-engine input for one side of an option-chain slice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OptionChainCandidateSide {
    /// Normalized contracts.
    pub contracts: Vec<CandidateContract>,
    /// Normalized market snapshots keyed by candidate symbol.
    pub snapshots: BTreeMap<String, CandidateMarketSnapshot>,
}

impl OptionChainCandidateSide {
    /// Returns the number of normalized contracts.
    #[must_use]
    pub fn contract_count(&self) -> usize {
        self.contracts.len()
    }

    /// Returns the number of normalized market snapshots.
    #[must_use]
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }
}

/// Normalized candidate-engine input for one Nautilus option-chain slice.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionChainCandidateInput {
    /// Underlying symbol from the option series.
    pub underlying: String,
    /// Expiration date shared by the option series.
    pub expiration_date: String,
    /// Call-side candidates.
    pub calls: OptionChainCandidateSide,
    /// Put-side candidates.
    pub puts: OptionChainCandidateSide,
    /// Underlying price from Greeks or ATM strike, when available.
    pub underlying_price: Option<f64>,
}

impl OptionChainCandidateInput {
    fn side_for_credit(&self, kind: CreditSpreadKind) -> &OptionChainCandidateSide {
        match kind {
            CreditSpreadKind::Put => &self.puts,
            CreditSpreadKind::Call => &self.calls,
        }
    }

    fn side_for_debit(&self, kind: DebitSpreadKind) -> &OptionChainCandidateSide {
        match kind {
            DebitSpreadKind::Put => &self.puts,
            DebitSpreadKind::Call => &self.calls,
        }
    }

    fn side_for_naked(&self, kind: NakedOptionKind) -> &OptionChainCandidateSide {
        match kind {
            NakedOptionKind::Put | NakedOptionKind::PutOneToThreeDte => &self.puts,
            NakedOptionKind::Call | NakedOptionKind::CallOneToThreeDte => &self.calls,
        }
    }
}

/// Converts a Nautilus option-chain slice into candidate-engine inputs.
#[must_use]
pub fn option_chain_candidate_input(slice: &OptionChainSlice) -> OptionChainCandidateInput {
    let expiration_date = slice
        .series_id
        .expiration_ns
        .to_datetime_utc()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();

    OptionChainCandidateInput {
        underlying: slice.series_id.underlying.to_string(),
        expiration_date: expiration_date.clone(),
        calls: candidate_side(&slice.calls, &expiration_date),
        puts: candidate_side(&slice.puts, &expiration_date),
        underlying_price: underlying_price(slice),
    }
}

/// Scores and ranks credit-spread candidates from a Nautilus option-chain slice.
#[must_use]
pub fn scan_credit_spread_option_chain(
    input: &OptionChainCandidateInput,
    config: &PutCreditScannerConfig,
    kind: CreditSpreadKind,
    scan_date: NaiveDate,
) -> CreditSpreadScanResult {
    let side = input.side_for_credit(kind);
    let (scored, mut rejection_counts) =
        score_contracts_with_rejections_at(&side.contracts, &side.snapshots, config, scan_date);
    let (candidates, build_rejections) =
        build_candidates_for_kind_with_rejections(&scored, config, kind);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);

    CreditSpreadScanResult {
        underlying: input.underlying.clone(),
        contract_count: side.contract_count(),
        snapshot_count: side.snapshot_count(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    }
}

/// Scores and ranks debit-spread candidates from a Nautilus option-chain slice.
#[must_use]
pub fn scan_debit_spread_option_chain(
    input: &OptionChainCandidateInput,
    config: &DebitSpreadScannerConfig,
    kind: DebitSpreadKind,
    scan_date: NaiveDate,
) -> DebitSpreadScanResult {
    let side = input.side_for_debit(kind);
    let (scored, mut rejection_counts) = score_debit_contracts_with_rejections_at(
        &side.contracts,
        &side.snapshots,
        config,
        scan_date,
    );
    let (candidates, build_rejections) =
        build_debit_candidates_for_kind_with_rejections(&scored, config, kind);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);

    DebitSpreadScanResult {
        underlying: input.underlying.clone(),
        contract_count: side.contract_count(),
        snapshot_count: side.snapshot_count(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    }
}

/// Scores and ranks iron-condor candidates from a Nautilus option-chain slice.
#[must_use]
pub fn scan_iron_condor_option_chain(
    input: &OptionChainCandidateInput,
    config: &IronCondorScannerConfig,
    scan_date: NaiveDate,
) -> IronCondorScanResult {
    let put =
        scan_credit_spread_option_chain(input, &config.credit, CreditSpreadKind::Put, scan_date);
    let call =
        scan_credit_spread_option_chain(input, &config.credit, CreditSpreadKind::Call, scan_date);
    let (candidates, build_rejections) =
        build_iron_condor_candidates_with_rejections(&put.candidates, &call.candidates, config);

    let mut rejection_counts = put.rejection_counts.clone();
    merge_rejection_counts(&mut rejection_counts, &call.rejection_counts);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);

    IronCondorScanResult {
        underlying: input.underlying.clone(),
        contract_count: put.contract_count + call.contract_count,
        snapshot_count: put.snapshot_count + call.snapshot_count,
        scoreable_count: put.scoreable_count + call.scoreable_count,
        rejection_counts,
        candidates,
    }
}

/// Scores and ranks naked-option candidates from a Nautilus option-chain slice.
#[must_use]
pub fn scan_naked_option_chain(
    input: &OptionChainCandidateInput,
    config: &NakedOptionScannerConfig,
    kind: NakedOptionKind,
    capital: Option<NakedOptionCapitalContext>,
    scan_date: NaiveDate,
) -> NakedOptionScanResult {
    let side = input.side_for_naked(kind);
    let Some(underlying_price) = input.underlying_price else {
        return NakedOptionScanResult {
            underlying: input.underlying.clone(),
            contract_count: side.contract_count(),
            snapshot_count: side.snapshot_count(),
            scoreable_count: 0,
            rejection_counts: BTreeMap::from([("missing_underlying_price".to_string(), 1)]),
            candidates: Vec::new(),
        };
    };

    let (scored, mut rejection_counts) = score_naked_option_contracts_with_rejections_at(
        &side.contracts,
        &side.snapshots,
        config,
        kind,
        underlying_price,
        scan_date,
    );
    let (candidates, build_rejections) =
        build_naked_option_candidates_with_capital_and_rejections(&scored, config, capital);
    merge_rejection_counts(&mut rejection_counts, &build_rejections);

    NakedOptionScanResult {
        underlying: input.underlying.clone(),
        contract_count: side.contract_count(),
        snapshot_count: side.snapshot_count(),
        scoreable_count: scored.len(),
        rejection_counts,
        candidates,
    }
}

fn candidate_side(
    strikes: &BTreeMap<nautilus_model::types::Price, OptionStrikeData>,
    expiration_date: &str,
) -> OptionChainCandidateSide {
    let mut side = OptionChainCandidateSide::default();
    for (strike, data) in strikes {
        let symbol = data.quote.instrument_id.symbol.to_string();
        side.contracts.push(CandidateContract {
            symbol: symbol.clone(),
            expiration_date: expiration_date.to_string(),
            strike: Some(strike.as_f64()),
            open_interest: data
                .greeks
                .as_ref()
                .and_then(|greeks| non_negative_u64(greeks.open_interest)),
        });
        side.snapshots.insert(
            symbol,
            CandidateMarketSnapshot {
                quote: Some(CandidateQuote {
                    bid: Some(data.quote.bid_price.as_f64()),
                    ask: Some(data.quote.ask_price.as_f64()),
                    bid_size: quantity_to_u64(data.quote.bid_size),
                    ask_size: quantity_to_u64(data.quote.ask_size),
                }),
                delta: data.greeks.as_ref().map(|greeks| greeks.delta),
                implied_volatility: data.greeks.as_ref().and_then(implied_volatility),
                volume: 0,
            },
        );
    }
    side
}

fn underlying_price(slice: &OptionChainSlice) -> Option<f64> {
    slice
        .calls
        .values()
        .chain(slice.puts.values())
        .filter_map(|data| data.greeks.as_ref()?.underlying_price)
        .find(|price| price.is_finite() && *price > 0.0)
        .or_else(|| slice.atm_strike.map(|strike| strike.as_f64()))
}

fn implied_volatility(greeks: &OptionGreeks) -> Option<f64> {
    greeks
        .mark_iv
        .or_else(|| match (greeks.bid_iv, greeks.ask_iv) {
            (Some(bid), Some(ask)) if bid.is_finite() && ask.is_finite() && ask >= bid => {
                Some((bid + ask) / 2.0)
            }
            _ => None,
        })
        .or(greeks.bid_iv)
        .or(greeks.ask_iv)
        .filter(|iv| iv.is_finite() && *iv >= 0.0)
}

fn quantity_to_u64(quantity: nautilus_model::types::Quantity) -> u64 {
    non_negative_u64(Some(quantity.as_f64())).unwrap_or(0)
}

fn non_negative_u64(value: Option<f64>) -> Option<u64> {
    let value = value?;
    (value.is_finite() && value >= 0.0).then_some(value.floor() as u64)
}
