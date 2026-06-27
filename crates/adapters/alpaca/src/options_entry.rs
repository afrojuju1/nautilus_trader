//! Shared selected-entry metadata for Alpaca options strategies.

use crate::{
    candidate_engine::{
        CreditSpreadKind, DebitSpreadCandidate, DebitSpreadKind, IronCondorCandidate,
        NakedOptionCandidate, NakedOptionKind, SpreadCandidate,
    },
    runtime::{
        StrategyStateEntryDraft, credit_spread_strategy_name, debit_spread_strategy_name,
        naked_option_strategy_name,
    },
};

/// Selected credit-spread candidate.
#[derive(Clone, Debug)]
pub struct SelectedEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Credit-spread kind.
    pub kind: CreditSpreadKind,
    /// Scored spread candidate.
    pub candidate: SpreadCandidate,
}

/// Selected iron-condor candidate.
#[derive(Clone, Debug)]
pub struct SelectedIronCondorEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Scored iron-condor candidate.
    pub candidate: IronCondorCandidate,
}

/// Selected long-premium debit-spread candidate.
#[derive(Clone, Debug)]
pub struct SelectedDebitEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Debit-spread kind.
    pub kind: DebitSpreadKind,
    /// Scored debit-spread candidate.
    pub candidate: DebitSpreadCandidate,
}

/// Selected naked short option candidate.
#[derive(Clone, Debug)]
pub struct SelectedNakedOptionEntry {
    /// Underlying symbol.
    pub underlying: String,
    /// Naked option kind.
    pub kind: NakedOptionKind,
    /// Scored naked-option candidate.
    pub candidate: NakedOptionCandidate,
}

/// Selected options strategy candidate.
#[derive(Clone, Debug)]
pub enum SelectedOptionsEntry {
    /// Two-leg credit spread.
    Credit(SelectedEntry),
    /// Four-leg iron condor.
    IronCondor(SelectedIronCondorEntry),
    /// Two-leg debit spread.
    Debit(SelectedDebitEntry),
    /// Single-leg naked short option.
    NakedOption(SelectedNakedOptionEntry),
}

/// Stable metadata shared by selection, submission, ledgers, and outcome tracking.
#[derive(Clone, Debug)]
pub struct OptionEntryDescriptor {
    /// Stable strategy name.
    pub strategy: &'static str,
    /// Underlying symbol.
    pub underlying: String,
    /// Candidate type written to analytical ledgers.
    pub candidate_type: &'static str,
    /// Candidate option symbols in canonical identity order.
    pub symbols: Vec<String>,
    /// Scanner score.
    pub score: f64,
    /// Entry premium kind.
    pub premium_kind: EntryPremiumKind,
    /// Entry premium per spread or option.
    pub premium: f64,
}

/// Premium direction for a selected entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryPremiumKind {
    /// Credit received at entry.
    Credit,
    /// Debit paid at entry.
    Debit,
}

impl EntryPremiumKind {
    /// Returns the stable ledger label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Credit => "credit",
            Self::Debit => "debit",
        }
    }
}

impl SelectedOptionsEntry {
    /// Returns shared selected-entry metadata.
    #[must_use]
    pub fn descriptor(&self) -> OptionEntryDescriptor {
        match self {
            Self::Credit(entry) => OptionEntryDescriptor {
                strategy: credit_spread_strategy_name(entry.kind),
                underlying: entry.underlying.clone(),
                candidate_type: "credit_spread",
                symbols: vec![
                    entry.candidate.short.symbol.clone(),
                    entry.candidate.long.symbol.clone(),
                ],
                score: entry.candidate.score,
                premium_kind: EntryPremiumKind::Credit,
                premium: entry.candidate.credit,
            },
            Self::IronCondor(entry) => OptionEntryDescriptor {
                strategy: "iron_condor",
                underlying: entry.underlying.clone(),
                candidate_type: "iron_condor",
                symbols: vec![
                    entry.candidate.put.short.symbol.clone(),
                    entry.candidate.put.long.symbol.clone(),
                    entry.candidate.call.short.symbol.clone(),
                    entry.candidate.call.long.symbol.clone(),
                ],
                score: entry.candidate.score,
                premium_kind: EntryPremiumKind::Credit,
                premium: entry.candidate.credit,
            },
            Self::Debit(entry) => OptionEntryDescriptor {
                strategy: debit_spread_strategy_name(entry.kind),
                underlying: entry.underlying.clone(),
                candidate_type: "debit_spread",
                symbols: vec![
                    entry.candidate.long.symbol.clone(),
                    entry.candidate.short.symbol.clone(),
                ],
                score: entry.candidate.score,
                premium_kind: EntryPremiumKind::Debit,
                premium: entry.candidate.debit,
            },
            Self::NakedOption(entry) => OptionEntryDescriptor {
                strategy: naked_option_strategy_name(entry.kind),
                underlying: entry.underlying.clone(),
                candidate_type: "naked_option",
                symbols: vec![entry.candidate.short.symbol.clone()],
                score: entry.candidate.score,
                premium_kind: EntryPremiumKind::Credit,
                premium: entry.candidate.credit,
            },
        }
    }

    /// Returns the scanner score.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.descriptor().score
    }

    /// Returns the underlying symbol.
    #[must_use]
    pub fn underlying(&self) -> &str {
        match self {
            Self::Credit(entry) => &entry.underlying,
            Self::IronCondor(entry) => &entry.underlying,
            Self::Debit(entry) => &entry.underlying,
            Self::NakedOption(entry) => &entry.underlying,
        }
    }

    /// Returns the stable strategy name.
    #[must_use]
    pub fn strategy_name(&self) -> &'static str {
        self.descriptor().strategy
    }

    /// Returns whether the selected entry is a single-leg naked option.
    #[must_use]
    pub const fn is_naked_option(&self) -> bool {
        matches!(self, Self::NakedOption(_))
    }

    /// Returns the entry premium kind.
    #[must_use]
    pub fn entry_premium_kind(&self) -> &'static str {
        self.descriptor().premium_kind.as_str()
    }

    /// Returns the entry premium per spread or option.
    #[must_use]
    pub fn entry_premium(&self) -> f64 {
        self.descriptor().premium
    }

    /// Returns the candidate option symbols.
    #[must_use]
    pub fn option_symbols(&self) -> Vec<&str> {
        match self {
            Self::Credit(entry) => {
                vec![&entry.candidate.short.symbol, &entry.candidate.long.symbol]
            }
            Self::IronCondor(entry) => vec![
                &entry.candidate.put.short.symbol,
                &entry.candidate.put.long.symbol,
                &entry.candidate.call.short.symbol,
                &entry.candidate.call.long.symbol,
            ],
            Self::Debit(entry) => {
                vec![&entry.candidate.long.symbol, &entry.candidate.short.symbol]
            }
            Self::NakedOption(entry) => vec![&entry.candidate.short.symbol],
        }
    }

    /// Builds a strategy-state draft for accepted broker submissions.
    #[must_use]
    pub fn state_entry_draft(
        &self,
        trade_date: &str,
        order_list_id: &str,
        quantity: u64,
        submitted_at_utc: Option<String>,
        parent_order_id: Option<String>,
    ) -> StrategyStateEntryDraft {
        match self {
            Self::Credit(entry) => StrategyStateEntryDraft {
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
                score: entry.candidate.score,
                parent_order_id,
                submitted_at_utc,
            },
            Self::IronCondor(entry) => StrategyStateEntryDraft {
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
                score: entry.candidate.score,
                parent_order_id,
                submitted_at_utc,
            },
            Self::Debit(entry) => StrategyStateEntryDraft {
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
                score: entry.candidate.score,
                parent_order_id,
                submitted_at_utc,
            },
            Self::NakedOption(entry) => StrategyStateEntryDraft {
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
                score: entry.candidate.score,
                parent_order_id,
                submitted_at_utc,
            },
        }
    }
}
