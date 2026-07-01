//! Alpaca strategy-state conversion for selected option entries.

use nautilus_trading::options::entries::{
    SelectedOptionsEntry, credit_spread_strategy_name, debit_spread_strategy_name,
    naked_option_strategy_name,
};

use crate::{
    runtime::{StrategyStateEntryDraft, StrategyStateSpreadLeg},
    spread_plan::{OptionSpreadPlan, selected_entry_spread_plan},
};

/// Builds a strategy-state draft for an accepted broker submission.
#[must_use]
pub fn selected_entry_state_entry_draft(
    entry: &SelectedOptionsEntry,
    profile_id: Option<String>,
    trade_date: &str,
    order_list_id: &str,
    quantity: u64,
    submitted_at_utc: Option<String>,
    parent_order_id: Option<String>,
) -> StrategyStateEntryDraft {
    let risk_capital_usd = entry.risk_capital_usd(quantity);
    let spread_state = selected_entry_spread_plan(entry, Default::default())
        .ok()
        .flatten()
        .map(|plan| spread_state_fields(&plan));
    match entry {
        SelectedOptionsEntry::Credit(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            profile_id: profile_id.clone(),
            underlying: entry.underlying.clone(),
            strategy: credit_spread_strategy_name(entry.kind).to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.short.symbol.clone(),
            long_symbol: entry.candidate.long.symbol.clone(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: entry.candidate.credit,
            debit: None,
            risk_capital_usd,
            score: entry.candidate.score,
            parent_order_id,
            submitted_at_utc,
            spread_instrument_id: spread_state
                .as_ref()
                .map(|state| state.spread_instrument_id.clone()),
            spread_raw_symbol: spread_state
                .as_ref()
                .map(|state| state.spread_raw_symbol.clone()),
            spread_legs: spread_state
                .as_ref()
                .map_or_else(Vec::new, |state| state.spread_legs.clone()),
            entry_pricing_source: Some("single_option_spread_order".to_string()),
        },
        SelectedOptionsEntry::IronCondor(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            profile_id: profile_id.clone(),
            underlying: entry.underlying.clone(),
            strategy: "iron_condor".to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.put.short.symbol.clone(),
            long_symbol: entry.candidate.put.long.symbol.clone(),
            short_call_symbol: Some(entry.candidate.call.short.symbol.clone()),
            long_call_symbol: Some(entry.candidate.call.long.symbol.clone()),
            quantity,
            credit: entry.candidate.credit,
            debit: None,
            risk_capital_usd,
            score: entry.candidate.score,
            parent_order_id,
            submitted_at_utc,
            spread_instrument_id: spread_state
                .as_ref()
                .map(|state| state.spread_instrument_id.clone()),
            spread_raw_symbol: spread_state
                .as_ref()
                .map(|state| state.spread_raw_symbol.clone()),
            spread_legs: spread_state
                .as_ref()
                .map_or_else(Vec::new, |state| state.spread_legs.clone()),
            entry_pricing_source: Some("single_option_spread_order".to_string()),
        },
        SelectedOptionsEntry::Debit(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            profile_id: profile_id.clone(),
            underlying: entry.underlying.clone(),
            strategy: debit_spread_strategy_name(entry.kind).to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.short.symbol.clone(),
            long_symbol: entry.candidate.long.symbol.clone(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: -entry.candidate.debit,
            debit: Some(entry.candidate.debit),
            risk_capital_usd,
            score: entry.candidate.score,
            parent_order_id,
            submitted_at_utc,
            spread_instrument_id: spread_state
                .as_ref()
                .map(|state| state.spread_instrument_id.clone()),
            spread_raw_symbol: spread_state
                .as_ref()
                .map(|state| state.spread_raw_symbol.clone()),
            spread_legs: spread_state
                .as_ref()
                .map_or_else(Vec::new, |state| state.spread_legs.clone()),
            entry_pricing_source: Some("single_option_spread_order".to_string()),
        },
        SelectedOptionsEntry::NakedOption(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
            profile_id,
            underlying: entry.underlying.clone(),
            strategy: naked_option_strategy_name(entry.kind).to_string(),
            order_list_id: order_list_id.to_string(),
            short_symbol: entry.candidate.short.symbol.clone(),
            long_symbol: String::new(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity,
            credit: entry.candidate.credit,
            debit: None,
            risk_capital_usd,
            score: entry.candidate.score,
            parent_order_id,
            submitted_at_utc,
            spread_instrument_id: None,
            spread_raw_symbol: None,
            spread_legs: Vec::new(),
            entry_pricing_source: Some("single_leg_order".to_string()),
        },
    }
}

#[derive(Clone)]
struct SpreadStateFields {
    spread_instrument_id: String,
    spread_raw_symbol: String,
    spread_legs: Vec<StrategyStateSpreadLeg>,
}

fn spread_state_fields(plan: &OptionSpreadPlan) -> SpreadStateFields {
    SpreadStateFields {
        spread_instrument_id: plan.instrument_id.to_string(),
        spread_raw_symbol: plan.raw_symbol.to_string(),
        spread_legs: plan
            .legs
            .iter()
            .map(|leg| StrategyStateSpreadLeg {
                symbol: leg.symbol.clone(),
                instrument_id: leg.instrument_id.to_string(),
                ratio: leg.ratio,
            })
            .collect(),
    }
}
