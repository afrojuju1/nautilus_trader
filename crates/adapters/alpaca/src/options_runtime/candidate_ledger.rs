//! Candidate-ledger reporting helpers for options runtime selection.

use serde_json::Value;

use crate::{
    candidate_engine::{
        DebitSpreadCandidate, IronCondorCandidate, NakedOptionCandidate, SpreadCandidate,
    },
    candidate_payloads::{
        candidate_alert_identity_key, candidate_alert_key, credit_candidate_ledger_payload,
        debit_candidate_ledger_payload, insert_string_field, iron_condor_candidate_ledger_payload,
        naked_candidate_ledger_payload,
    },
};

use super::{
    AlpacaOptionsRuntimeConfig, CANDIDATE_ALERT_CREDIT_MIN_SCORE, CANDIDATE_ALERT_DEBIT_MIN_SCORE,
    CANDIDATE_ALERT_IRON_CONDOR_MIN_SCORE, CANDIDATE_ALERT_NAKED_MIN_SCORE,
    CANDIDATE_ALERT_NAKED_ONE_TO_THREE_DTE_MIN_SCORE, HIGH_SCORE_CANDIDATE_ALERT,
};

pub(super) async fn record_scanner_ledger_result(
    config: &AlpacaOptionsRuntimeConfig,
    trade_date: &str,
    payload: Value,
) {
    config
        .record_candidate_ledger(trade_date, "scanner_result", payload)
        .await;
}

pub(super) async fn record_credit_candidate_ledger(
    config: &AlpacaOptionsRuntimeConfig,
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
        config
            .record_candidate_ledger(trade_date, "candidate", payload.clone())
            .await;
        record_high_score_candidate_alert(
            config,
            trade_date,
            strategy,
            underlying,
            &candidate.short.symbol,
            &[&candidate.short.symbol, &candidate.long.symbol],
            candidate.score,
            payload,
        )
        .await;
    }
}

pub(super) async fn record_debit_candidate_ledger(
    config: &AlpacaOptionsRuntimeConfig,
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
        config
            .record_candidate_ledger(trade_date, "candidate", payload.clone())
            .await;
        record_high_score_candidate_alert(
            config,
            trade_date,
            strategy,
            underlying,
            &candidate.long.symbol,
            &[&candidate.long.symbol, &candidate.short.symbol],
            candidate.score,
            payload,
        )
        .await;
    }
}

pub(super) async fn record_iron_condor_candidate_ledger(
    config: &AlpacaOptionsRuntimeConfig,
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
        config
            .record_candidate_ledger(trade_date, "candidate", payload.clone())
            .await;
        record_high_score_candidate_alert(
            config,
            trade_date,
            "iron_condor",
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
        )
        .await;
    }
}

pub(super) async fn record_naked_candidate_ledger(
    config: &AlpacaOptionsRuntimeConfig,
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
        config
            .record_candidate_ledger(trade_date, "candidate", payload.clone())
            .await;
        record_high_score_candidate_alert(
            config,
            trade_date,
            strategy,
            underlying,
            &candidate.short.symbol,
            &[&candidate.short.symbol],
            candidate.score,
            payload,
        )
        .await;
    }
}

async fn record_high_score_candidate_alert(
    config: &AlpacaOptionsRuntimeConfig,
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
    config
        .record_candidate_alert_ledger(
            trade_date,
            HIGH_SCORE_CANDIDATE_ALERT,
            "info",
            candidate_alert_key(HIGH_SCORE_CANDIDATE_ALERT, &identity_key),
            alert_payload,
        )
        .await;
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
