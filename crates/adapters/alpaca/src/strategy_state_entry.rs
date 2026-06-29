//! Alpaca strategy-state conversion for selected option entries.

use nautilus_trading::options::entries::{
    SelectedOptionsEntry, credit_spread_strategy_name, debit_spread_strategy_name,
    naked_option_strategy_name,
};

use crate::runtime::StrategyStateEntryDraft;

/// Builds a strategy-state draft for an accepted broker submission.
#[must_use]
pub fn selected_entry_state_entry_draft(
    entry: &SelectedOptionsEntry,
    trade_date: &str,
    order_list_id: &str,
    quantity: u64,
    submitted_at_utc: Option<String>,
    parent_order_id: Option<String>,
) -> StrategyStateEntryDraft {
    let risk_capital_usd = entry.risk_capital_usd(quantity);
    match entry {
        SelectedOptionsEntry::Credit(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
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
        },
        SelectedOptionsEntry::IronCondor(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
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
        },
        SelectedOptionsEntry::Debit(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
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
        },
        SelectedOptionsEntry::NakedOption(entry) => StrategyStateEntryDraft {
            trade_date: trade_date.to_string(),
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
        },
    }
}
