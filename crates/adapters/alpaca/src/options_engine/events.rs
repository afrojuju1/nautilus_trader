//! Operator events and selected-candidate alert payloads.

use serde_json::Value;

use crate::{
    options_runtime::{
        OptionsEngineConfig, SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry,
        SelectedNakedOptionEntry, SelectedOptionsEntry, candidate_alert_identity_key,
        candidate_alert_key, credit_candidate_ledger_payload, debit_candidate_ledger_payload,
        iron_condor_candidate_ledger_payload, naked_candidate_ledger_payload,
    },
    runtime::{debit_spread_strategy_name, emit_operator_event, naked_option_strategy_name},
};

use super::{
    CANDIDATE_SUBMIT_REJECTED_ALERT, SELECTED_CANDIDATE_ALERT, SubmitOutcome, strategy_name,
};

pub(super) async fn record_decision_event(
    config: &OptionsEngineConfig,
    trade_date: &str,
    payload: serde_json::Value,
) {
    emit_operator_event("decision", payload.clone());
    config
        .record_candidate_ledger(trade_date, "decision", payload)
        .await;
}

pub(super) async fn record_submit_result_event(
    config: &OptionsEngineConfig,
    trade_date: &str,
    payload: serde_json::Value,
) {
    emit_operator_event("submit_result", payload.clone());
    config
        .record_candidate_ledger(trade_date, "submit_result", payload)
        .await;
}

pub(super) async fn record_selected_candidate_alert(
    config: &OptionsEngineConfig,
    trade_date: &str,
    identity_key: &str,
    payload: Value,
) {
    config
        .record_candidate_alert_ledger(
            trade_date,
            SELECTED_CANDIDATE_ALERT,
            "info",
            candidate_alert_key(SELECTED_CANDIDATE_ALERT, identity_key),
            payload,
        )
        .await;
}

pub(super) async fn record_submit_rejected_candidate_alert(
    config: &OptionsEngineConfig,
    trade_date: &str,
    identity_key: &str,
    mut payload: Value,
    outcome: &SubmitOutcome,
    terminal_rejection_recorded: Option<bool>,
) {
    insert_value_field(&mut payload, "accepted", Value::from(outcome.accepted));
    insert_value_field(&mut payload, "rejected", Value::from(outcome.rejected));
    insert_value_field(
        &mut payload,
        "parent_order_id",
        outcome
            .parent_order_id
            .as_ref()
            .map_or(Value::Null, |value| Value::String(value.clone())),
    );
    insert_value_field(
        &mut payload,
        "rejection_reasons",
        serde_json::json!(&outcome.rejection_reasons),
    );
    if let Some(recorded) = terminal_rejection_recorded {
        insert_value_field(
            &mut payload,
            "terminal_rejection_recorded",
            Value::Bool(recorded),
        );
    }
    config
        .record_candidate_alert_ledger(
            trade_date,
            CANDIDATE_SUBMIT_REJECTED_ALERT,
            "warning",
            candidate_alert_key(CANDIDATE_SUBMIT_REJECTED_ALERT, identity_key),
            payload,
        )
        .await;
}

pub(super) fn selected_entry_alert_payload(
    entry: &SelectedOptionsEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    match entry {
        SelectedOptionsEntry::Credit(entry) => {
            selected_credit_alert_payload(entry, trade_date, action, order_list_id)
        }
        SelectedOptionsEntry::IronCondor(entry) => {
            selected_iron_condor_alert_payload(entry, trade_date, action, order_list_id)
        }
        SelectedOptionsEntry::Debit(entry) => {
            selected_debit_alert_payload(entry, trade_date, action, order_list_id)
        }
        SelectedOptionsEntry::NakedOption(entry) => {
            selected_naked_alert_payload(entry, trade_date, action, order_list_id)
        }
    }
}

pub(super) use crate::candidate_payloads::{insert_string_field, insert_value_field};

fn selected_credit_alert_payload(
    entry: &SelectedEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let strategy = strategy_name(entry.kind);
    let identity_key = candidate_alert_identity_key(
        strategy,
        &entry.underlying,
        &[&entry.candidate.short.symbol, &entry.candidate.long.symbol],
    );
    let mut payload =
        credit_candidate_ledger_payload(&entry.underlying, strategy, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn selected_iron_condor_alert_payload(
    entry: &SelectedIronCondorEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let identity_key = candidate_alert_identity_key(
        "iron_condor",
        &entry.underlying,
        &[
            &entry.candidate.put.short.symbol,
            &entry.candidate.put.long.symbol,
            &entry.candidate.call.short.symbol,
            &entry.candidate.call.long.symbol,
        ],
    );
    let mut payload =
        iron_condor_candidate_ledger_payload(&entry.underlying, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn selected_debit_alert_payload(
    entry: &SelectedDebitEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let strategy = debit_spread_strategy_name(entry.kind);
    let identity_key = candidate_alert_identity_key(
        strategy,
        &entry.underlying,
        &[&entry.candidate.long.symbol, &entry.candidate.short.symbol],
    );
    let mut payload =
        debit_candidate_ledger_payload(&entry.underlying, strategy, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn selected_naked_alert_payload(
    entry: &SelectedNakedOptionEntry,
    trade_date: &str,
    action: &str,
    order_list_id: Option<&str>,
) -> (String, Value) {
    let strategy = naked_option_strategy_name(entry.kind);
    let identity_key = candidate_alert_identity_key(
        strategy,
        &entry.underlying,
        &[&entry.candidate.short.symbol],
    );
    let mut payload =
        naked_candidate_ledger_payload(&entry.underlying, strategy, None, None, &entry.candidate);
    insert_selected_alert_fields(
        &mut payload,
        &identity_key,
        action,
        trade_date,
        order_list_id,
    );
    (identity_key, payload)
}

fn insert_selected_alert_fields(
    payload: &mut Value,
    identity_key: &str,
    action: &str,
    trade_date: &str,
    order_list_id: Option<&str>,
) {
    insert_string_field(payload, "candidate_identity_key", identity_key.to_string());
    insert_string_field(payload, "action", action.to_string());
    insert_string_field(payload, "trade_date", trade_date.to_string());
    if let Some(order_list_id) = order_list_id {
        insert_string_field(payload, "order_list_id", order_list_id.to_string());
    }
}
