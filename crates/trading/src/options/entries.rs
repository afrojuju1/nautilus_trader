//! Source-neutral selected-entry metadata for options strategies.

use super::candidates::{
    CreditSpreadKind, DebitSpreadCandidate, DebitSpreadKind, IronCondorCandidate,
    NakedOptionCandidate, NakedOptionKind, SpreadCandidate,
};

const OPTION_CONTRACT_MULTIPLIER: f64 = 100.0;

/// Returns the strategy name for a credit-spread kind.
#[must_use]
pub const fn credit_spread_strategy_name(kind: CreditSpreadKind) -> &'static str {
    match kind {
        CreditSpreadKind::Put => "put_credit",
        CreditSpreadKind::Call => "call_credit",
    }
}

/// Returns the strategy name for a debit-spread kind.
#[must_use]
pub const fn debit_spread_strategy_name(kind: DebitSpreadKind) -> &'static str {
    match kind {
        DebitSpreadKind::Call => "call_debit",
        DebitSpreadKind::Put => "put_debit",
    }
}

/// Returns the strategy name for a naked short option kind.
#[must_use]
pub const fn naked_option_strategy_name(kind: NakedOptionKind) -> &'static str {
    match kind {
        NakedOptionKind::Call => "naked_call",
        NakedOptionKind::Put => "naked_put",
        NakedOptionKind::CallOneToThreeDte => "naked_call_1_3dte",
        NakedOptionKind::PutOneToThreeDte => "naked_put_1_3dte",
    }
}

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

    /// Returns the selected entry risk-capital estimate in USD for the supplied order quantity.
    ///
    /// Defined-risk spreads use scanner max loss. Naked options use the scanner buying-power
    /// estimate because short calls do not have finite maximum loss.
    #[must_use]
    pub fn risk_capital_usd(&self, quantity: u64) -> Option<f64> {
        let quantity = quantity.max(1) as f64;
        let value = match self {
            Self::Credit(entry) => entry.candidate.max_loss * OPTION_CONTRACT_MULTIPLIER * quantity,
            Self::IronCondor(entry) => {
                entry.candidate.max_loss * OPTION_CONTRACT_MULTIPLIER * quantity
            }
            Self::Debit(entry) => entry.candidate.max_loss * OPTION_CONTRACT_MULTIPLIER * quantity,
            Self::NakedOption(entry) => entry.candidate.estimated_buying_power_requirement,
        };
        (value.is_finite() && value > 0.0).then_some(value)
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
}
