//! Candidate-ledger payload and identity helpers.

use serde_json::{Value, json};

use crate::{
    candidate_engine::{
        DebitSpreadCandidate, IronCondorCandidate, NakedOptionCandidate, OptionCandidateMetrics,
        ScoredContract, SpreadCandidate, annualized_premium_yield,
    },
    options_entry::SelectedOptionsEntry,
};

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
        "strategy": "iron_condor",
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

/// Builds the shared candidate-ledger payload for a selected options entry.
#[must_use]
pub fn selected_entry_candidate_ledger_payload(
    entry: &SelectedOptionsEntry,
    options_buying_power: Option<f64>,
    rank: Option<usize>,
) -> Value {
    let descriptor = entry.descriptor();
    match entry {
        SelectedOptionsEntry::Credit(entry) => credit_candidate_ledger_payload(
            &entry.underlying,
            descriptor.strategy,
            rank,
            &entry.candidate,
        ),
        SelectedOptionsEntry::IronCondor(entry) => {
            iron_condor_candidate_ledger_payload(&entry.underlying, rank, &entry.candidate)
        }
        SelectedOptionsEntry::Debit(entry) => debit_candidate_ledger_payload(
            &entry.underlying,
            descriptor.strategy,
            rank,
            &entry.candidate,
        ),
        SelectedOptionsEntry::NakedOption(entry) => naked_candidate_ledger_payload(
            &entry.underlying,
            descriptor.strategy,
            options_buying_power,
            rank,
            &entry.candidate,
        ),
    }
}

/// Builds selected-entry candidate-alert identity and payload.
#[must_use]
pub fn selected_entry_alert_payload(
    entry: &SelectedOptionsEntry,
    options_buying_power: Option<f64>,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
    quantity: u64,
) -> (String, Value) {
    let descriptor = entry.descriptor();
    let identity_key = candidate_alert_identity_key_from_strings(
        descriptor.strategy,
        &descriptor.underlying,
        &descriptor.symbols,
    );
    let mut payload = selected_entry_candidate_ledger_payload(entry, options_buying_power, None);
    insert_string_field(&mut payload, "candidate_identity_key", identity_key.clone());
    insert_string_field(&mut payload, "action", action.to_string());
    insert_string_field(&mut payload, "trade_date", trade_date.to_string());
    insert_value_field(&mut payload, "quantity", Value::from(quantity));
    if let Some(order_list_id) = order_list_id {
        insert_string_field(&mut payload, "order_list_id", order_list_id.to_string());
    }
    (identity_key, payload)
}

/// Returns a stable key for one candidate independent of account and trade date.
#[must_use]
pub fn candidate_alert_identity_key(strategy: &str, underlying: &str, symbols: &[&str]) -> String {
    candidate_identity_key(strategy, underlying, symbols.iter().copied())
}

/// Returns a stable key for one candidate from owned symbols.
#[must_use]
pub fn candidate_alert_identity_key_from_strings(
    strategy: &str,
    underlying: &str,
    symbols: &[String],
) -> String {
    candidate_identity_key(strategy, underlying, symbols.iter().map(String::as_str))
}

/// Returns a stable key for a typed candidate alert.
#[must_use]
pub fn candidate_alert_key(alert_type: &str, identity_key: &str) -> String {
    format!("{alert_type}|{identity_key}")
}

/// Inserts a string field into a JSON object payload.
pub fn insert_string_field(payload: &mut Value, key: &str, value: String) {
    insert_value_field(payload, key, Value::String(value));
}

/// Inserts a JSON field into an object payload and leaves non-object values unchanged.
pub fn insert_value_field(payload: &mut Value, key: &str, value: Value) {
    if let Value::Object(fields) = payload {
        fields.insert(key.to_string(), value);
    }
}

/// Inserts an optional rank field into an object payload.
pub fn insert_optional_rank(payload: &mut Value, rank: Option<usize>) {
    if let Some(rank) = rank {
        insert_value_field(payload, "rank", Value::from(rank));
    }
}

fn candidate_identity_key<'a>(
    strategy: &str,
    underlying: &str,
    symbols: impl IntoIterator<Item = &'a str>,
) -> String {
    format!(
        "{}|{}|{}",
        strategy,
        underlying,
        symbols
            .into_iter()
            .filter(|symbol| !symbol.is_empty())
            .collect::<Vec<_>>()
            .join("|")
    )
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
        "model_delta_abs": metrics.model_delta_abs,
        "model_gamma": metrics.model_gamma,
        "model_theta": metrics.model_theta,
        "model_vega": metrics.model_vega,
        "capital_requirement_model": metrics.capital_requirement_model.as_str(),
        "estimated_buying_power_requirement": metrics.estimated_buying_power_requirement,
    })
}
