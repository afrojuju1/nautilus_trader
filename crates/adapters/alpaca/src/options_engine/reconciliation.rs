//! Broker/state reconciliation helpers for the options engine.

use std::collections::BTreeSet;

use serde_json::json;

use crate::{
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaOrder, AlpacaPosition, ListOrdersRequest},
    },
    runtime::{StrategyState, StrategyStateEntry, emit_operator_event},
};

use super::lookup_parent_order_snapshot;

pub(super) async fn reconcile_strategy_state(
    client: &AlpacaHttpClient,
    state: &mut StrategyState,
) -> anyhow::Result<bool> {
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    let position_symbols = position_symbols(&positions);
    let open_order_symbols = order_symbols(&open_orders);
    let active_symbols = active_state_symbols(state);
    let unmanaged_symbols = position_symbols
        .difference(&active_symbols)
        .cloned()
        .collect::<Vec<_>>();
    if !unmanaged_symbols.is_empty() {
        println!(
            "reconcile: unmanaged_positions symbols={}",
            unmanaged_symbols.join(","),
        );
        emit_operator_event(
            "reconciliation_warning",
            json!({
                "reason": "unmanaged_positions",
                "symbols": unmanaged_symbols,
            }),
        );
    }

    let mut changed = false;
    for entry in state.entries.iter_mut().filter(|entry| entry.is_active()) {
        let mut action = reconciliation_action(entry, &position_symbols, &open_order_symbols, None);
        if action == ReconciliationAction::MarkClosed {
            let order_status = lookup_parent_order_snapshot(client, &entry.order_list_id)
                .await?
                .and_then(|order| order.status);
            action = reconciliation_action(
                entry,
                &position_symbols,
                &open_order_symbols,
                order_status.as_deref(),
            );
        }

        match action {
            ReconciliationAction::None => {}
            ReconciliationAction::MarkClosed => {
                println!(
                    "reconcile: mark_closed underlying={} order_list_id={} reason=broker_flat",
                    entry.underlying, entry.order_list_id,
                );
                emit_operator_event(
                    "reconciliation_repair",
                    json!({
                        "action": "mark_closed",
                        "reason": "broker_flat",
                        "underlying": entry.underlying,
                        "order_list_id": entry.order_list_id,
                    }),
                );
                entry.mark_closed(None);
                changed = true;
            }
            ReconciliationAction::MarkCanceled => {
                println!(
                    "reconcile: mark_canceled underlying={} order_list_id={} reason=entry_terminal_without_position",
                    entry.underlying, entry.order_list_id,
                );
                emit_operator_event(
                    "reconciliation_repair",
                    json!({
                        "action": "mark_canceled",
                        "reason": "entry_terminal_without_position",
                        "underlying": entry.underlying,
                        "order_list_id": entry.order_list_id,
                    }),
                );
                entry.mark_canceled();
                changed = true;
            }
            ReconciliationAction::PartialPosition => {
                println!(
                    "reconcile: partial_position underlying={} symbols={}",
                    entry.underlying,
                    entry.symbols().join(","),
                );
                emit_operator_event(
                    "reconciliation_warning",
                    json!({
                        "reason": "partial_position",
                        "underlying": entry.underlying,
                        "symbols": entry.symbols(),
                    }),
                );
            }
        }
    }

    Ok(changed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReconciliationAction {
    None,
    MarkClosed,
    MarkCanceled,
    PartialPosition,
}

pub(super) fn reconciliation_action(
    entry: &StrategyStateEntry,
    position_symbols: &BTreeSet<String>,
    open_order_symbols: &BTreeSet<String>,
    entry_order_status: Option<&str>,
) -> ReconciliationAction {
    let symbols = entry.symbols();
    let position_matches = symbols
        .iter()
        .filter(|symbol| {
            position_symbols
                .iter()
                .any(|candidate| candidate == *symbol)
        })
        .count();
    let open_order_matches = symbols
        .iter()
        .filter(|symbol| {
            open_order_symbols
                .iter()
                .any(|candidate| candidate == *symbol)
        })
        .count();

    if position_matches == 0 && open_order_matches == 0 {
        if matches!(
            entry_order_status,
            Some("canceled" | "expired" | "rejected")
        ) {
            ReconciliationAction::MarkCanceled
        } else {
            ReconciliationAction::MarkClosed
        }
    } else if position_matches > 0 && position_matches < symbols.len() {
        ReconciliationAction::PartialPosition
    } else {
        ReconciliationAction::None
    }
}

fn active_state_symbols(state: &StrategyState) -> BTreeSet<String> {
    state
        .entries
        .iter()
        .filter(|entry| entry.is_active())
        .flat_map(|entry| entry.symbols().into_iter().map(ToString::to_string))
        .collect()
}

fn position_symbols(positions: &[AlpacaPosition]) -> BTreeSet<String> {
    positions
        .iter()
        .filter_map(|position| position.symbol.clone())
        .collect()
}

fn order_symbols(orders: &[AlpacaOrder]) -> BTreeSet<String> {
    orders.iter().flat_map(AlpacaOrder::symbols).collect()
}
