//! Broker/state reconciliation helpers for Alpaca option strategy state.

use std::collections::BTreeSet;

use serde_json::json;

use crate::{
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{AlpacaOrder, AlpacaPosition, ListOrdersRequest},
    },
    runtime::{StrategyState, StrategyStateEntry, emit_operator_event},
};

/// Summary of one broker/state reconciliation pass.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StrategyStateReconciliationReport {
    /// Whether persisted strategy state was changed.
    pub changed: bool,
    /// Open broker position symbols not represented by active strategy state.
    pub unmanaged_position_symbols: Vec<String>,
    /// Open broker order symbols not represented by active strategy state.
    pub unmanaged_open_order_symbols: Vec<String>,
    /// Active state entry symbols with only a partial broker-position match.
    pub partial_position_symbols: Vec<String>,
}

impl StrategyStateReconciliationReport {
    /// Returns `true` when broker state is not fully represented by active strategy state.
    #[must_use]
    pub fn has_unmanaged_broker_state(&self) -> bool {
        !self.unmanaged_position_symbols.is_empty()
            || !self.unmanaged_open_order_symbols.is_empty()
            || !self.partial_position_symbols.is_empty()
    }
}

/// Reconciles persisted strategy state against current Alpaca broker orders and positions.
///
/// # Errors
///
/// Returns an error if Alpaca broker reads fail.
pub async fn reconcile_strategy_state(
    client: &AlpacaHttpClient,
    state: &mut StrategyState,
) -> anyhow::Result<StrategyStateReconciliationReport> {
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    let position_symbols = position_symbols(&positions);
    let open_order_symbols = order_symbols(&open_orders);
    let active_symbols = active_state_symbols(state);
    let unmanaged_position_symbols = position_symbols
        .difference(&active_symbols)
        .cloned()
        .collect::<Vec<_>>();
    let unmanaged_open_order_symbols = open_order_symbols
        .difference(&active_symbols)
        .cloned()
        .collect::<Vec<_>>();

    if !unmanaged_position_symbols.is_empty() {
        println!(
            "reconcile: unmanaged_positions symbols={}",
            unmanaged_position_symbols.join(","),
        );
        emit_operator_event(
            "reconciliation_warning",
            json!({
                "reason": "unmanaged_positions",
                "symbols": unmanaged_position_symbols.clone(),
            }),
        );
    }
    if !unmanaged_open_order_symbols.is_empty() {
        println!(
            "reconcile: unmanaged_open_orders symbols={}",
            unmanaged_open_order_symbols.join(","),
        );
        emit_operator_event(
            "reconciliation_warning",
            json!({
                "reason": "unmanaged_open_orders",
                "symbols": unmanaged_open_order_symbols.clone(),
            }),
        );
    }

    let mut report = StrategyStateReconciliationReport {
        unmanaged_position_symbols,
        unmanaged_open_order_symbols,
        ..Default::default()
    };
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
                report.changed = true;
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
                report.changed = true;
            }
            ReconciliationAction::PartialPosition => {
                let symbols = entry
                    .symbols()
                    .into_iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                println!(
                    "reconcile: partial_position underlying={} symbols={}",
                    entry.underlying,
                    symbols.join(","),
                );
                emit_operator_event(
                    "reconciliation_warning",
                    json!({
                        "reason": "partial_position",
                        "underlying": entry.underlying,
                        "symbols": symbols.clone(),
                    }),
                );
                report.partial_position_symbols.extend(symbols);
            }
        }
    }

    report.partial_position_symbols.sort();
    report.partial_position_symbols.dedup();
    Ok(report)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReconciliationAction {
    None,
    MarkClosed,
    MarkCanceled,
    PartialPosition,
}

pub(crate) fn reconciliation_action(
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

async fn lookup_parent_order_snapshot(
    client: &AlpacaHttpClient,
    order_list_id: &str,
) -> anyhow::Result<Option<AlpacaOrder>> {
    match client.order_by_client_order_id(order_list_id, true).await {
        Ok(order) => Ok(Some(order)),
        Err(Error::HttpStatus { status, .. }) if status == 404 => Ok(None),
        Err(error) => Err(error.into()),
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
