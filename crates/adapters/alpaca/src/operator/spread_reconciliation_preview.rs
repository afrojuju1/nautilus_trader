//! Read-only preview of broker spread state mapped onto Nautilus spread identity.

use std::collections::BTreeSet;

use nautilus_core::UnixNanos;
use serde::Serialize;

use crate::{
    http::models::{AlpacaActivity, AlpacaOrder, AlpacaPosition},
    runtime::{StrategyState, StrategyStateEntry},
    spread_plan::{OptionSpreadPlan, strategy_state_spread_plan},
};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SpreadReconciliationPreview {
    pub(crate) summary: SpreadReconciliationSummary,
    pub(crate) state_spreads: Vec<StateSpreadPreview>,
    pub(crate) unmanaged_extra_legs: Vec<BrokerLegPreview>,
    pub(crate) unknown_parent_leg_mappings: Vec<BrokerOrderMappingPreview>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SpreadReconciliationSummary {
    pub(crate) state_spreads: usize,
    pub(crate) matched_spreads: usize,
    pub(crate) partial_spreads: usize,
    pub(crate) missing_spreads: usize,
    pub(crate) broker_mleg_orders: usize,
    pub(crate) broker_position_legs: usize,
    pub(crate) unmanaged_extra_legs: usize,
    pub(crate) unknown_parent_leg_mappings: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StateSpreadPreview {
    pub(crate) status: String,
    pub(crate) trade_date: String,
    pub(crate) underlying: String,
    pub(crate) strategy: String,
    pub(crate) order_list_id: String,
    pub(crate) close_order_list_id: Option<String>,
    pub(crate) spread_instrument_id: String,
    pub(crate) spread_symbol: String,
    pub(crate) legs: Vec<SpreadLegPreview>,
    pub(crate) position_status: String,
    pub(crate) open_order_status: String,
    pub(crate) matched_position_symbols: Vec<String>,
    pub(crate) matched_open_order_symbols: Vec<String>,
    pub(crate) missing_symbols: Vec<String>,
    pub(crate) open_orders: Vec<BrokerOrderRef>,
    pub(crate) recent_orders: Vec<BrokerOrderRef>,
    pub(crate) recent_activities: Vec<BrokerActivityRef>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SpreadLegPreview {
    pub(crate) symbol: String,
    pub(crate) instrument_id: String,
    pub(crate) ratio: i64,
    pub(crate) position_present: bool,
    pub(crate) open_order_present: bool,
    pub(crate) recent_activity_seen: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrokerOrderRef {
    pub(crate) source: String,
    pub(crate) id: Option<String>,
    pub(crate) client_order_id: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) order_class: Option<String>,
    pub(crate) symbols: Vec<String>,
    pub(crate) mapping: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrokerActivityRef {
    pub(crate) id: Option<String>,
    pub(crate) activity_type: Option<String>,
    pub(crate) order_id: Option<String>,
    pub(crate) symbol: Option<String>,
    pub(crate) side: Option<String>,
    pub(crate) qty: Option<String>,
    pub(crate) price: Option<String>,
    pub(crate) transaction_time: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrokerLegPreview {
    pub(crate) source: String,
    pub(crate) symbol: String,
    pub(crate) qty: Option<String>,
    pub(crate) side: Option<String>,
    pub(crate) order_id: Option<String>,
    pub(crate) client_order_id: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BrokerOrderMappingPreview {
    pub(crate) reason: String,
    pub(crate) id: Option<String>,
    pub(crate) client_order_id: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) order_class: Option<String>,
    pub(crate) symbols: Vec<String>,
    pub(crate) candidate_order_list_ids: Vec<String>,
}

pub(crate) fn build_spread_reconciliation_preview(
    state: &StrategyState,
    positions: &[AlpacaPosition],
    open_orders: &[AlpacaOrder],
    recent_orders: &[AlpacaOrder],
    activities: &[AlpacaActivity],
) -> SpreadReconciliationPreview {
    let position_symbols = position_symbols(positions);
    let open_order_symbols = order_symbols(open_orders);
    let activity_symbols = activity_symbols(activities);
    let state_spread_inputs = state_spread_inputs(state);
    let active_spread_symbols = state_spread_inputs
        .iter()
        .flat_map(|(_, _, symbols)| symbols.iter().cloned())
        .collect::<BTreeSet<_>>();
    let expected_sets = state_spread_inputs
        .iter()
        .map(|(entry, _, symbols)| (entry.order_list_id.clone(), symbols.clone()))
        .collect::<Vec<_>>();

    let state_spreads = state_spread_inputs
        .iter()
        .map(|(entry, plan, expected_symbols)| {
            build_state_spread_preview(
                entry,
                plan,
                expected_symbols,
                &position_symbols,
                &open_order_symbols,
                &activity_symbols,
                open_orders,
                recent_orders,
                activities,
            )
        })
        .collect::<Vec<_>>();
    let unmanaged_extra_legs = unmanaged_extra_legs(positions, open_orders, &active_spread_symbols);
    let unknown_parent_leg_mappings = unknown_parent_leg_mappings(open_orders, &expected_sets);
    let summary = SpreadReconciliationSummary {
        state_spreads: state_spreads.len(),
        matched_spreads: state_spreads
            .iter()
            .filter(|spread| spread.status == "matched")
            .count(),
        partial_spreads: state_spreads
            .iter()
            .filter(|spread| spread.status == "partial")
            .count(),
        missing_spreads: state_spreads
            .iter()
            .filter(|spread| spread.status == "missing")
            .count(),
        broker_mleg_orders: open_orders
            .iter()
            .filter(|order| order.order_class.as_deref() == Some("mleg"))
            .count(),
        broker_position_legs: positions
            .iter()
            .filter(|position| option_position_symbol(position).is_some())
            .count(),
        unmanaged_extra_legs: unmanaged_extra_legs.len(),
        unknown_parent_leg_mappings: unknown_parent_leg_mappings.len(),
    };

    SpreadReconciliationPreview {
        summary,
        state_spreads,
        unmanaged_extra_legs,
        unknown_parent_leg_mappings,
    }
}

fn state_spread_inputs(
    state: &StrategyState,
) -> Vec<(&StrategyStateEntry, OptionSpreadPlan, BTreeSet<String>)> {
    state
        .entries
        .iter()
        .filter(|entry| entry.is_active())
        .filter_map(|entry| {
            let plan = match strategy_state_spread_plan(entry, UnixNanos::default()) {
                Ok(Some(plan)) => plan,
                Ok(None) => return None,
                Err(error) => {
                    log::warn!(
                        "Failed to build spread reconciliation preview plan: order_list_id={} error={error:#}",
                        entry.order_list_id
                    );
                    return None;
                }
            };
            let symbols = plan
                .legs
                .iter()
                .map(|leg| leg.symbol.clone())
                .collect::<BTreeSet<_>>();
            Some((entry, plan, symbols))
        })
        .collect()
}

#[expect(clippy::too_many_arguments)]
fn build_state_spread_preview(
    entry: &StrategyStateEntry,
    plan: &OptionSpreadPlan,
    expected_symbols: &BTreeSet<String>,
    position_symbols: &BTreeSet<String>,
    open_order_symbols: &BTreeSet<String>,
    activity_symbols: &BTreeSet<String>,
    open_orders: &[AlpacaOrder],
    recent_orders: &[AlpacaOrder],
    activities: &[AlpacaActivity],
) -> StateSpreadPreview {
    let matched_position_symbols = intersection(expected_symbols, position_symbols);
    let matched_open_order_symbols = intersection(expected_symbols, open_order_symbols);
    let represented_symbols = matched_position_symbols
        .iter()
        .chain(matched_open_order_symbols.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing_symbols = expected_symbols
        .difference(&represented_symbols)
        .cloned()
        .collect::<Vec<_>>();
    let status = match represented_symbols.len() {
        0 => "missing",
        len if len == expected_symbols.len() => "matched",
        _ => "partial",
    }
    .to_string();
    let position_status = symbol_set_status(expected_symbols, &matched_position_symbols);
    let open_order_status = symbol_set_status(expected_symbols, &matched_open_order_symbols);
    let open_orders = matching_order_refs(open_orders, expected_symbols, "open_order");
    let recent_orders = matching_order_refs(recent_orders, expected_symbols, "recent_order");
    let recent_activities = activities
        .iter()
        .filter(|activity| {
            activity
                .symbol
                .as_ref()
                .is_some_and(|symbol| expected_symbols.contains(symbol))
        })
        .map(activity_ref)
        .collect::<Vec<_>>();
    let legs = plan
        .legs
        .iter()
        .map(|leg| SpreadLegPreview {
            symbol: leg.symbol.clone(),
            instrument_id: leg.instrument_id.to_string(),
            ratio: leg.ratio,
            position_present: position_symbols.contains(&leg.symbol),
            open_order_present: open_order_symbols.contains(&leg.symbol),
            recent_activity_seen: activity_symbols.contains(&leg.symbol),
        })
        .collect();

    StateSpreadPreview {
        status,
        trade_date: entry.trade_date.clone(),
        underlying: entry.underlying.clone(),
        strategy: entry.strategy.clone(),
        order_list_id: entry.order_list_id.clone(),
        close_order_list_id: entry.close_order_list_id.clone(),
        spread_instrument_id: plan.instrument_id.to_string(),
        spread_symbol: plan.raw_symbol.to_string(),
        legs,
        position_status,
        open_order_status,
        matched_position_symbols,
        matched_open_order_symbols,
        missing_symbols,
        open_orders,
        recent_orders,
        recent_activities,
    }
}

fn matching_order_refs(
    orders: &[AlpacaOrder],
    expected_symbols: &BTreeSet<String>,
    source: &str,
) -> Vec<BrokerOrderRef> {
    orders
        .iter()
        .filter_map(|order| {
            let symbols = order_symbol_set(order);
            if symbols.is_empty() || symbols.is_disjoint(expected_symbols) {
                return None;
            }
            let mapping = if &symbols == expected_symbols {
                "matched"
            } else {
                "partial"
            };
            Some(order_ref(order, source, mapping, symbols))
        })
        .collect()
}

fn unmanaged_extra_legs(
    positions: &[AlpacaPosition],
    open_orders: &[AlpacaOrder],
    active_spread_symbols: &BTreeSet<String>,
) -> Vec<BrokerLegPreview> {
    let mut legs = positions
        .iter()
        .filter_map(|position| {
            let symbol = non_empty(position.symbol.as_deref())?;
            (!active_spread_symbols.contains(symbol)).then(|| BrokerLegPreview {
                source: "position".to_string(),
                symbol: symbol.to_string(),
                qty: position.qty.clone(),
                side: position.side.clone(),
                order_id: None,
                client_order_id: None,
                status: None,
            })
        })
        .collect::<Vec<_>>();

    for order in open_orders {
        for leg in option_order_leg_previews(order) {
            if active_spread_symbols.contains(&leg.symbol) {
                continue;
            }
            legs.push(leg);
        }
    }
    legs
}

fn unknown_parent_leg_mappings(
    open_orders: &[AlpacaOrder],
    expected_sets: &[(String, BTreeSet<String>)],
) -> Vec<BrokerOrderMappingPreview> {
    open_orders
        .iter()
        .filter_map(|order| {
            let symbols = order_symbol_set(order);
            let is_mleg = order.order_class.as_deref() == Some("mleg");
            if symbols.is_empty() {
                return is_mleg.then(|| {
                    broker_order_mapping_preview(order, symbols, "missing_leg_symbols", Vec::new())
                });
            }

            let exact_matches = expected_sets
                .iter()
                .filter(|(_, expected)| *expected == symbols)
                .map(|(order_list_id, _)| order_list_id.clone())
                .collect::<Vec<_>>();
            if exact_matches.len() == 1 {
                return None;
            }

            let partial_matches = expected_sets
                .iter()
                .filter(|(_, expected)| !expected.is_disjoint(&symbols))
                .map(|(order_list_id, _)| order_list_id.clone())
                .collect::<Vec<_>>();
            let (reason, candidates) = if exact_matches.len() > 1 {
                ("ambiguous_spread_identity", exact_matches)
            } else if !partial_matches.is_empty() {
                ("partial_spread_leg_mapping", partial_matches)
            } else if is_mleg {
                ("unmanaged_mleg_parent", Vec::new())
            } else {
                return None;
            };
            Some(broker_order_mapping_preview(
                order, symbols, reason, candidates,
            ))
        })
        .collect()
}

fn broker_order_mapping_preview(
    order: &AlpacaOrder,
    symbols: BTreeSet<String>,
    reason: &str,
    candidate_order_list_ids: Vec<String>,
) -> BrokerOrderMappingPreview {
    BrokerOrderMappingPreview {
        reason: reason.to_string(),
        id: order.id.clone(),
        client_order_id: order.client_order_id.clone(),
        status: order.status.clone(),
        order_class: order.order_class.clone(),
        symbols: symbols.into_iter().collect(),
        candidate_order_list_ids,
    }
}

fn order_ref(
    order: &AlpacaOrder,
    source: &str,
    mapping: &str,
    symbols: BTreeSet<String>,
) -> BrokerOrderRef {
    BrokerOrderRef {
        source: source.to_string(),
        id: order.id.clone(),
        client_order_id: order.client_order_id.clone(),
        status: order.status.clone(),
        order_class: order.order_class.clone(),
        symbols: symbols.into_iter().collect(),
        mapping: mapping.to_string(),
    }
}

fn activity_ref(activity: &AlpacaActivity) -> BrokerActivityRef {
    BrokerActivityRef {
        id: activity.id.clone(),
        activity_type: activity.activity_type.clone(),
        order_id: activity.order_id.clone(),
        symbol: activity.symbol.clone(),
        side: activity.side.clone(),
        qty: activity.qty.clone(),
        price: activity.price.clone(),
        transaction_time: activity.transaction_time.clone(),
    }
}

fn position_symbols(positions: &[AlpacaPosition]) -> BTreeSet<String> {
    positions
        .iter()
        .filter_map(|position| option_position_symbol(position).map(ToString::to_string))
        .collect()
}

fn order_symbols(orders: &[AlpacaOrder]) -> BTreeSet<String> {
    orders
        .iter()
        .flat_map(|order| order_symbol_set(order).into_iter())
        .collect::<BTreeSet<_>>()
}

fn activity_symbols(activities: &[AlpacaActivity]) -> BTreeSet<String> {
    activities
        .iter()
        .filter_map(|activity| non_empty(activity.symbol.as_deref()).map(ToString::to_string))
        .collect()
}

fn order_symbol_set(order: &AlpacaOrder) -> BTreeSet<String> {
    let mut symbols = Vec::new();
    push_option_order_symbols(order, &mut symbols);
    symbols.into_iter().collect()
}

fn push_option_order_symbols(order: &AlpacaOrder, symbols: &mut Vec<String>) {
    if let Some(legs) = &order.legs {
        for leg in legs {
            push_option_order_symbols(leg, symbols);
        }
    }
    if order.asset_class.as_deref() == Some("us_option")
        && let Some(symbol) = non_empty(order.symbol.as_deref())
    {
        symbols.push(symbol.to_string());
    }
}

fn option_order_leg_previews(order: &AlpacaOrder) -> Vec<BrokerLegPreview> {
    let mut legs = Vec::new();
    push_option_order_leg_previews(order, &mut legs);
    legs
}

fn push_option_order_leg_previews(order: &AlpacaOrder, legs: &mut Vec<BrokerLegPreview>) {
    if let Some(nested_legs) = &order.legs {
        for leg in nested_legs {
            push_option_order_leg_previews(leg, legs);
        }
    }
    if order.asset_class.as_deref() == Some("us_option")
        && let Some(symbol) = non_empty(order.symbol.as_deref())
    {
        legs.push(BrokerLegPreview {
            source: "open_order".to_string(),
            symbol: symbol.to_string(),
            qty: order.qty.clone(),
            side: order.side.clone(),
            order_id: order.id.clone(),
            client_order_id: order.client_order_id.clone(),
            status: order.status.clone(),
        });
    }
}

fn option_position_symbol(position: &AlpacaPosition) -> Option<&str> {
    (position.asset_class.as_deref() == Some("us_option"))
        .then(|| non_empty(position.symbol.as_deref()))
        .flatten()
}

fn intersection(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Vec<String> {
    left.intersection(right).cloned().collect()
}

fn symbol_set_status(expected_symbols: &BTreeSet<String>, matched_symbols: &[String]) -> String {
    match matched_symbols.len() {
        0 => "missing",
        len if len == expected_symbols.len() => "matched",
        _ => "partial",
    }
    .to_string()
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
