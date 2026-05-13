//! Contract scoring and candidate building for Alpaca options strategies.

use std::collections::{BTreeMap, HashMap};

use chrono::{NaiveDate, Utc};
use nautilus_model::data::greeks::black_scholes_greeks;

use crate::http::models::{AlpacaOptionContract, AlpacaOptionSnapshot};

use super::{
    CreditSpreadKind, DAYS_PER_YEAR, DebitSpreadCandidate, DebitSpreadKind,
    DebitSpreadScannerConfig, IronCondorCandidate, IronCondorScannerConfig, NakedOptionCandidate,
    NakedOptionCapitalContext, NakedOptionKind, NakedOptionScannerConfig,
    OPTION_CONTRACT_MULTIPLIER, OptionCandidateMetrics, OptionCapitalRequirementModel,
    PutCreditScannerConfig, SCANNER_RISK_FREE_RATE, ScoredContract, SpreadCandidate,
};

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
    score_contracts_with_rejections_at(contracts, snapshots, config, Utc::now().date_naive())
}

/// Scores contracts and counts filter rejections using an explicit scan date.
#[must_use]
pub fn score_contracts_with_rejections_at(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &PutCreditScannerConfig,
    scan_date: NaiveDate,
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
        let Some(dte) = days_to_expiration_from(&contract.expiration_date, scan_date) else {
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
    score_debit_contracts_with_rejections_at(contracts, snapshots, config, Utc::now().date_naive())
}

/// Scores debit-spread contracts and counts filter rejections using an explicit scan date.
#[must_use]
pub fn score_debit_contracts_with_rejections_at(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &DebitSpreadScannerConfig,
    scan_date: NaiveDate,
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
        let Some(dte) = days_to_expiration_from(&contract.expiration_date, scan_date) else {
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
    score_naked_option_contracts_with_rejections_at(
        contracts,
        snapshots,
        config,
        kind,
        underlying_price,
        Utc::now().date_naive(),
    )
}

/// Scores naked-option contracts and counts filter rejections using an explicit scan date.
#[must_use]
pub fn score_naked_option_contracts_with_rejections_at(
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    config: &NakedOptionScannerConfig,
    kind: NakedOptionKind,
    underlying_price: f64,
    scan_date: NaiveDate,
) -> (Vec<ScoredContract>, BTreeMap<String, usize>) {
    let mut scored = Vec::new();
    let mut rejections = BTreeMap::new();
    for contract in contracts {
        let open_interest = contract
            .open_interest
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok());
        if open_interest.is_some_and(|value| value < config.min_open_interest) {
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
        let open_interest = match open_interest {
            Some(open_interest) => open_interest,
            None => {
                if !allow_missing_open_interest_for_naked_option(
                    config, bid_size, ask_size, volume, spread_pct,
                ) {
                    record_rejection(&mut rejections, "missing_open_interest");
                    continue;
                }
                0
            }
        };

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
        let Some(dte) = days_to_expiration_from(&contract.expiration_date, scan_date) else {
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

fn allow_missing_open_interest_for_naked_option(
    config: &NakedOptionScannerConfig,
    bid_size: u64,
    ask_size: u64,
    volume: u64,
    spread_pct: f64,
) -> bool {
    let min_fallback_volume = config.min_daily_volume.saturating_mul(2).max(1);
    let max_fallback_spread_pct = config.max_spread_pct * 0.75;
    bid_size >= config.min_bid_size
        && ask_size >= config.min_ask_size
        && volume >= min_fallback_volume
        && spread_pct <= max_fallback_spread_pct
}

pub(super) fn merge_rejection_counts(
    target: &mut BTreeMap<String, usize>,
    source: &BTreeMap<String, usize>,
) {
    for (reason, count) in source {
        *target.entry(reason.clone()).or_insert(0) += count;
    }
}

fn strike_key(strike: f64) -> i64 {
    (strike * 1_000.0).round() as i64
}

fn days_to_expiration(expiration_date: &str) -> Option<i64> {
    days_to_expiration_from(expiration_date, Utc::now().date_naive())
}

fn days_to_expiration_from(expiration_date: &str, scan_date: NaiveDate) -> Option<i64> {
    let expiration = NaiveDate::parse_from_str(expiration_date, "%Y-%m-%d").ok()?;
    Some(expiration.signed_duration_since(scan_date).num_days())
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

pub(super) fn directional_distance_pct(
    kind: NakedOptionKind,
    underlying_price: f64,
    threshold: f64,
) -> f64 {
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
