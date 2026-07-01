//! Pure selected-entry planning for the Alpaca options account strategy.

use nautilus_trading::options::{
    candidates::{CreditSpreadKind, DebitSpreadKind, NakedOptionKind},
    entries::{
        EntryPremiumKind, OptionEntryDescriptor, SelectedDebitEntry, SelectedEntry,
        SelectedIronCondorEntry, SelectedNakedOptionEntry, SelectedOptionsEntry,
    },
};

use crate::options_runtime::{
    AlpacaOptionsCandidateProfile, OptionsCandidateSet, ProfiledOptionsEntry,
};

/// Pure account-strategy entry plan produced from scanner-selected candidates.
#[derive(Clone, Debug)]
pub struct AlpacaOptionsEntryPlan {
    /// Originating scanner profile, when the plan came from scanner candidate data.
    pub profile: Option<AlpacaOptionsCandidateProfile>,
    /// Selected strategy-family candidate.
    pub entry: SelectedOptionsEntry,
    /// Stable family metadata.
    pub family: AlpacaOptionsEntryFamily,
    /// Underlying symbol.
    pub underlying: String,
    /// Stable strategy name.
    pub strategy: &'static str,
    /// Candidate symbols in canonical identity order.
    pub symbols: Vec<String>,
    /// Scanner score.
    pub score: f64,
    /// Entry premium direction.
    pub premium_kind: EntryPremiumKind,
    /// Entry premium per spread or option.
    pub premium: f64,
    /// Planned order legs. The account strategy owns converting these into Nautilus orders.
    pub order_legs: Vec<AlpacaOptionsEntryPlanLeg>,
}

impl AlpacaOptionsEntryPlan {
    fn from_entry(entry: SelectedOptionsEntry, family: AlpacaOptionsEntryFamily) -> Self {
        let descriptor = entry.descriptor();
        let order_legs = planned_order_legs(&entry);
        Self::from_parts(None, entry, family, descriptor, order_legs)
    }

    fn from_profiled_entry(entry: ProfiledOptionsEntry, family: AlpacaOptionsEntryFamily) -> Self {
        let descriptor = entry.descriptor();
        let order_legs = planned_order_legs(entry.selected_entry());
        let ProfiledOptionsEntry { profile, entry } = entry;
        Self::from_parts(Some(profile), entry, family, descriptor, order_legs)
    }

    fn from_parts(
        profile: Option<AlpacaOptionsCandidateProfile>,
        entry: SelectedOptionsEntry,
        family: AlpacaOptionsEntryFamily,
        descriptor: OptionEntryDescriptor,
        order_legs: Vec<AlpacaOptionsEntryPlanLeg>,
    ) -> Self {
        Self {
            profile,
            entry,
            family,
            underlying: descriptor.underlying,
            strategy: descriptor.strategy,
            symbols: descriptor.symbols,
            score: descriptor.score,
            premium_kind: descriptor.premium_kind,
            premium: descriptor.premium,
            order_legs,
        }
    }

    /// Returns `true` when the entry plan represents a two-leg vertical spread.
    #[must_use]
    pub fn is_vertical_spread(&self) -> bool {
        matches!(
            self.family,
            AlpacaOptionsEntryFamily::PutCredit
                | AlpacaOptionsEntryFamily::CallCredit
                | AlpacaOptionsEntryFamily::PutDebit
                | AlpacaOptionsEntryFamily::CallDebit
        )
    }
}

/// Stable strategy family for a planned selected entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlpacaOptionsEntryFamily {
    /// Put credit spread.
    PutCredit,
    /// Call credit spread.
    CallCredit,
    /// Iron condor.
    IronCondor,
    /// Put debit spread.
    PutDebit,
    /// Call debit spread.
    CallDebit,
    /// Naked short put.
    NakedPut,
    /// Naked short call.
    NakedCall,
    /// 1-3 DTE naked short put.
    NakedPutOneToThreeDte,
    /// 1-3 DTE naked short call.
    NakedCallOneToThreeDte,
}

impl AlpacaOptionsEntryFamily {
    /// Returns the stable runtime family label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PutCredit => "put_credit",
            Self::CallCredit => "call_credit",
            Self::IronCondor => "iron_condor",
            Self::PutDebit => "put_debit",
            Self::CallDebit => "call_debit",
            Self::NakedPut => "naked_put",
            Self::NakedCall => "naked_call",
            Self::NakedPutOneToThreeDte => "naked_put_1_3dte",
            Self::NakedCallOneToThreeDte => "naked_call_1_3dte",
        }
    }
}

/// Planned order-leg action. The account strategy owns actual order construction and submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlpacaOptionsPlanSide {
    /// Buy the leg.
    Buy,
    /// Sell the leg.
    Sell,
}

impl AlpacaOptionsPlanSide {
    /// Returns the stable diagnostic label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }
}

/// Planned leg metadata for an entry plan.
#[derive(Clone, Debug, PartialEq)]
pub struct AlpacaOptionsEntryPlanLeg {
    /// Option symbol.
    pub symbol: String,
    /// Planned side.
    pub side: AlpacaOptionsPlanSide,
}

/// Plans the selected candidate from a scanner candidate set.
#[must_use]
pub fn plan_selected_candidate(candidates: &OptionsCandidateSet) -> Option<AlpacaOptionsEntryPlan> {
    plan_profiled_selected_entry(candidates.selected_entry()?.clone())
}

/// Plans one selected entry without reading account state or submitting orders.
#[must_use]
pub fn plan_selected_entry(entry: SelectedOptionsEntry) -> Option<AlpacaOptionsEntryPlan> {
    match entry {
        SelectedOptionsEntry::Credit(entry) => match entry.kind {
            CreditSpreadKind::Put => Some(put_credit::plan(entry)),
            CreditSpreadKind::Call => Some(call_credit::plan(entry)),
        },
        SelectedOptionsEntry::IronCondor(entry) => Some(iron_condor::plan(entry)),
        SelectedOptionsEntry::Debit(entry) => match entry.kind {
            DebitSpreadKind::Put => Some(put_debit::plan(entry)),
            DebitSpreadKind::Call => Some(call_debit::plan(entry)),
        },
        SelectedOptionsEntry::NakedOption(entry) => match entry.kind {
            NakedOptionKind::Put => Some(naked_option::plan_put(entry)),
            NakedOptionKind::Call => Some(naked_option::plan_call(entry)),
            NakedOptionKind::PutOneToThreeDte => Some(naked_option::plan_put_1_3dte(entry)),
            NakedOptionKind::CallOneToThreeDte => Some(naked_option::plan_call_1_3dte(entry)),
        },
    }
}

/// Plans one profiled selected entry without reading account state or submitting orders.
#[must_use]
pub fn plan_profiled_selected_entry(entry: ProfiledOptionsEntry) -> Option<AlpacaOptionsEntryPlan> {
    match entry.selected_entry() {
        SelectedOptionsEntry::Credit(selected) => match selected.kind {
            CreditSpreadKind::Put => Some(put_credit::plan_profiled(entry)),
            CreditSpreadKind::Call => Some(call_credit::plan_profiled(entry)),
        },
        SelectedOptionsEntry::IronCondor(_) => Some(iron_condor::plan_profiled(entry)),
        SelectedOptionsEntry::Debit(selected) => match selected.kind {
            DebitSpreadKind::Put => Some(put_debit::plan_profiled(entry)),
            DebitSpreadKind::Call => Some(call_debit::plan_profiled(entry)),
        },
        SelectedOptionsEntry::NakedOption(selected) => match selected.kind {
            NakedOptionKind::Put => Some(naked_option::plan_put_profiled(entry)),
            NakedOptionKind::Call => Some(naked_option::plan_call_profiled(entry)),
            NakedOptionKind::PutOneToThreeDte => {
                Some(naked_option::plan_put_1_3dte_profiled(entry))
            }
            NakedOptionKind::CallOneToThreeDte => {
                Some(naked_option::plan_call_1_3dte_profiled(entry))
            }
        },
    }
}

/// Pure put-credit planner.
pub mod put_credit {
    use super::*;

    /// Plans a put-credit selected entry.
    #[must_use]
    pub fn plan(entry: SelectedEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::Credit(entry),
            AlpacaOptionsEntryFamily::PutCredit,
        )
    }

    /// Plans a profiled put-credit selected entry.
    #[must_use]
    pub fn plan_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(entry, AlpacaOptionsEntryFamily::PutCredit)
    }
}

/// Pure call-credit planner.
pub mod call_credit {
    use super::*;

    /// Plans a call-credit selected entry.
    #[must_use]
    pub fn plan(entry: SelectedEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::Credit(entry),
            AlpacaOptionsEntryFamily::CallCredit,
        )
    }

    /// Plans a profiled call-credit selected entry.
    #[must_use]
    pub fn plan_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(entry, AlpacaOptionsEntryFamily::CallCredit)
    }
}

/// Pure iron-condor planner.
pub mod iron_condor {
    use super::*;

    /// Plans an iron-condor selected entry.
    #[must_use]
    pub fn plan(entry: SelectedIronCondorEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::IronCondor(entry),
            AlpacaOptionsEntryFamily::IronCondor,
        )
    }

    /// Plans a profiled iron-condor selected entry.
    #[must_use]
    pub fn plan_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(entry, AlpacaOptionsEntryFamily::IronCondor)
    }
}

/// Pure put-debit planner.
pub mod put_debit {
    use super::*;

    /// Plans a put-debit selected entry.
    #[must_use]
    pub fn plan(entry: SelectedDebitEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::Debit(entry),
            AlpacaOptionsEntryFamily::PutDebit,
        )
    }

    /// Plans a profiled put-debit selected entry.
    #[must_use]
    pub fn plan_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(entry, AlpacaOptionsEntryFamily::PutDebit)
    }
}

/// Pure call-debit planner.
pub mod call_debit {
    use super::*;

    /// Plans a call-debit selected entry.
    #[must_use]
    pub fn plan(entry: SelectedDebitEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::Debit(entry),
            AlpacaOptionsEntryFamily::CallDebit,
        )
    }

    /// Plans a profiled call-debit selected entry.
    #[must_use]
    pub fn plan_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(entry, AlpacaOptionsEntryFamily::CallDebit)
    }
}

/// Pure naked-option planners.
pub mod naked_option {
    use super::*;

    /// Plans a naked short-put selected entry.
    #[must_use]
    pub fn plan_put(entry: SelectedNakedOptionEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::NakedOption(entry),
            AlpacaOptionsEntryFamily::NakedPut,
        )
    }

    /// Plans a profiled naked short-put selected entry.
    #[must_use]
    pub fn plan_put_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(entry, AlpacaOptionsEntryFamily::NakedPut)
    }

    /// Plans a naked short-call selected entry.
    #[must_use]
    pub fn plan_call(entry: SelectedNakedOptionEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::NakedOption(entry),
            AlpacaOptionsEntryFamily::NakedCall,
        )
    }

    /// Plans a profiled naked short-call selected entry.
    #[must_use]
    pub fn plan_call_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(entry, AlpacaOptionsEntryFamily::NakedCall)
    }

    /// Plans a 1-3 DTE naked short-put selected entry.
    #[must_use]
    pub fn plan_put_1_3dte(entry: SelectedNakedOptionEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::NakedOption(entry),
            AlpacaOptionsEntryFamily::NakedPutOneToThreeDte,
        )
    }

    /// Plans a profiled 1-3 DTE naked short-put selected entry.
    #[must_use]
    pub fn plan_put_1_3dte_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(
            entry,
            AlpacaOptionsEntryFamily::NakedPutOneToThreeDte,
        )
    }

    /// Plans a 1-3 DTE naked short-call selected entry.
    #[must_use]
    pub fn plan_call_1_3dte(entry: SelectedNakedOptionEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_entry(
            SelectedOptionsEntry::NakedOption(entry),
            AlpacaOptionsEntryFamily::NakedCallOneToThreeDte,
        )
    }

    /// Plans a profiled 1-3 DTE naked short-call selected entry.
    #[must_use]
    pub fn plan_call_1_3dte_profiled(entry: ProfiledOptionsEntry) -> AlpacaOptionsEntryPlan {
        AlpacaOptionsEntryPlan::from_profiled_entry(
            entry,
            AlpacaOptionsEntryFamily::NakedCallOneToThreeDte,
        )
    }
}

fn planned_order_legs(entry: &SelectedOptionsEntry) -> Vec<AlpacaOptionsEntryPlanLeg> {
    match entry {
        SelectedOptionsEntry::Credit(entry) => vec![
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.short.symbol.clone(),
                side: AlpacaOptionsPlanSide::Sell,
            },
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.long.symbol.clone(),
                side: AlpacaOptionsPlanSide::Buy,
            },
        ],
        SelectedOptionsEntry::IronCondor(entry) => vec![
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.put.short.symbol.clone(),
                side: AlpacaOptionsPlanSide::Sell,
            },
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.put.long.symbol.clone(),
                side: AlpacaOptionsPlanSide::Buy,
            },
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.call.short.symbol.clone(),
                side: AlpacaOptionsPlanSide::Sell,
            },
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.call.long.symbol.clone(),
                side: AlpacaOptionsPlanSide::Buy,
            },
        ],
        SelectedOptionsEntry::Debit(entry) => vec![
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.long.symbol.clone(),
                side: AlpacaOptionsPlanSide::Buy,
            },
            AlpacaOptionsEntryPlanLeg {
                symbol: entry.candidate.short.symbol.clone(),
                side: AlpacaOptionsPlanSide::Sell,
            },
        ],
        SelectedOptionsEntry::NakedOption(entry) => vec![AlpacaOptionsEntryPlanLeg {
            symbol: entry.candidate.short.symbol.clone(),
            side: AlpacaOptionsPlanSide::Sell,
        }],
    }
}
