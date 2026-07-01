//! Source-neutral option-universe intent and resolution.

use std::collections::BTreeMap;

use nautilus_core::UnixNanos;
use nautilus_model::{
    enums::OptionKind,
    identifiers::{InstrumentId, OptionSeriesId},
};
use serde::{Deserialize, Serialize};

/// Default maximum selected expirations for one profile and underlying.
pub const DEFAULT_MAX_EXPIRATIONS_PER_INTENT: usize = 1;

/// Source-neutral option strategy family used for universe selection.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum OptionUniverseStrategyFamily {
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

impl OptionUniverseStrategyFamily {
    /// Returns the stable strategy label.
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

    /// Returns the option sides required to scan this strategy family.
    #[must_use]
    pub const fn required_sides(self) -> OptionUniverseRequiredSides {
        match self {
            Self::PutCredit | Self::PutDebit | Self::NakedPut | Self::NakedPutOneToThreeDte => {
                OptionUniverseRequiredSides::Puts
            }
            Self::CallCredit | Self::CallDebit | Self::NakedCall | Self::NakedCallOneToThreeDte => {
                OptionUniverseRequiredSides::Calls
            }
            Self::IronCondor => OptionUniverseRequiredSides::CallsAndPuts,
        }
    }
}

/// Required option sides for one universe intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OptionUniverseRequiredSides {
    /// Call options are required.
    Calls,
    /// Put options are required.
    Puts,
    /// Calls and puts are required for the same series.
    CallsAndPuts,
}

impl OptionUniverseRequiredSides {
    /// Returns the stable diagnostic label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Puts => "puts",
            Self::CallsAndPuts => "calls_and_puts",
        }
    }

    /// Returns whether the supplied side is required.
    #[must_use]
    pub const fn requires(self, option_kind: OptionKind) -> bool {
        match (self, option_kind) {
            (Self::Calls | Self::CallsAndPuts, OptionKind::Call) => true,
            (Self::Puts | Self::CallsAndPuts, OptionKind::Put) => true,
            _ => false,
        }
    }

    /// Returns whether available side counts satisfy this requirement.
    #[must_use]
    pub const fn is_satisfied_by(self, call_count: usize, put_count: usize) -> bool {
        match self {
            Self::Calls => call_count > 0,
            Self::Puts => put_count > 0,
            Self::CallsAndPuts => call_count > 0 && put_count > 0,
        }
    }
}

/// Inclusive DTE window for one universe intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OptionDteWindow {
    /// Minimum days to expiration.
    pub min_dte: i64,
    /// Maximum days to expiration.
    pub max_dte: i64,
}

impl OptionDteWindow {
    /// Creates a new inclusive DTE window.
    #[must_use]
    pub const fn new(min_dte: i64, max_dte: i64) -> Self {
        Self { min_dte, max_dte }
    }

    /// Returns whether the window bounds are ordered.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.min_dte <= self.max_dte
    }

    /// Returns whether `dte` is inside the inclusive window.
    #[must_use]
    pub const fn contains(self, dte: i64) -> bool {
        self.is_valid() && self.min_dte <= dte && dte <= self.max_dte
    }

    /// Returns the integer midpoint used for diagnostics.
    #[must_use]
    pub const fn midpoint_dte(self) -> i64 {
        (self.min_dte + self.max_dte) / 2
    }
}

/// Selection policy for choosing concrete expirations from a DTE window.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OptionUniverseSelectionPolicy {
    /// Prefer the expiration nearest the DTE window midpoint.
    WindowMidpoint,
    /// Prefer the expiration nearest an explicit target DTE.
    TargetDte {
        /// Target days to expiration.
        target_dte: i64,
    },
}

impl Default for OptionUniverseSelectionPolicy {
    fn default() -> Self {
        Self::WindowMidpoint
    }
}

impl OptionUniverseSelectionPolicy {
    /// Returns the stable diagnostic label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowMidpoint => "window_midpoint",
            Self::TargetDte { .. } => "target_dte",
        }
    }

    /// Returns the target DTE shown in selected-series diagnostics.
    #[must_use]
    pub const fn diagnostic_target_dte(self, window: OptionDteWindow) -> i64 {
        match self {
            Self::WindowMidpoint => window.midpoint_dte(),
            Self::TargetDte { target_dte } => target_dte,
        }
    }

    fn distance_key(self, window: OptionDteWindow, dte: i64) -> i64 {
        match self {
            Self::WindowMidpoint => (dte * 2 - window.min_dte - window.max_dte).abs(),
            Self::TargetDte { target_dte } => (dte - target_dte).abs(),
        }
    }
}

/// Strategy-owned option-universe intent for one profile and underlying.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OptionUniverseIntent {
    /// Stable strategy profile identifier.
    pub profile_id: String,
    /// Underlying symbol requested by the strategy profile.
    pub underlying: String,
    /// Strategy family requesting an option universe.
    pub strategy_family: OptionUniverseStrategyFamily,
    /// Required option sides for this profile.
    pub required_sides: OptionUniverseRequiredSides,
    /// Inclusive DTE window requested by this profile.
    pub dte_window: OptionDteWindow,
    /// Expiration selection policy.
    pub selection_policy: OptionUniverseSelectionPolicy,
    /// Maximum concrete expirations to select for this intent.
    pub max_expirations: usize,
}

impl OptionUniverseIntent {
    /// Creates an intent using the family default side requirement and midpoint selection.
    #[must_use]
    pub fn from_family(
        profile_id: impl Into<String>,
        underlying: impl Into<String>,
        strategy_family: OptionUniverseStrategyFamily,
        dte_window: OptionDteWindow,
    ) -> Self {
        Self {
            profile_id: profile_id.into(),
            underlying: underlying.into(),
            strategy_family,
            required_sides: strategy_family.required_sides(),
            dte_window,
            selection_policy: OptionUniverseSelectionPolicy::default(),
            max_expirations: DEFAULT_MAX_EXPIRATIONS_PER_INTENT,
        }
    }

    /// Returns the effective max expiration count, clamped to at least one.
    #[must_use]
    pub const fn effective_max_expirations(&self) -> usize {
        if self.max_expirations == 0 {
            DEFAULT_MAX_EXPIRATIONS_PER_INTENT
        } else {
            self.max_expirations
        }
    }
}

/// Source-neutral option contract input for universe resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OptionUniverseContract {
    /// Concrete option instrument ID.
    pub instrument_id: InstrumentId,
    /// Series identifier for the option contract.
    pub series_id: OptionSeriesId,
    /// Whether this contract is a call or put.
    pub option_kind: OptionKind,
}

impl OptionUniverseContract {
    /// Creates a new resolver contract input.
    #[must_use]
    pub const fn new(
        instrument_id: InstrumentId,
        series_id: OptionSeriesId,
        option_kind: OptionKind,
    ) -> Self {
        Self {
            instrument_id,
            series_id,
            option_kind,
        }
    }
}

/// Provider-level error for one profile or underlying.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OptionUniverseProviderError {
    /// Optional profile ID. `None` means the error applies to every profile for the underlying.
    pub profile_id: Option<String>,
    /// Underlying affected by the provider error.
    pub underlying: String,
    /// Stable provider-independent error code or message.
    pub message: String,
}

impl OptionUniverseProviderError {
    /// Creates a provider error for every profile on one underlying.
    #[must_use]
    pub fn for_underlying(underlying: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            profile_id: None,
            underlying: underlying.into(),
            message: message.into(),
        }
    }

    /// Creates a provider error for one profile and underlying.
    #[must_use]
    pub fn for_profile(
        profile_id: impl Into<String>,
        underlying: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            profile_id: Some(profile_id.into()),
            underlying: underlying.into(),
            message: message.into(),
        }
    }

    fn matches_intent(&self, intent: &OptionUniverseIntent) -> bool {
        let profile_matches = self
            .profile_id
            .as_ref()
            .is_none_or(|profile_id| profile_id == &intent.profile_id);
        profile_matches && self.underlying.eq_ignore_ascii_case(&intent.underlying)
    }
}

/// Structured reason for a selected concrete series.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OptionUniverseSelectionReason {
    /// Selected because it is nearest the window midpoint.
    NearestWindowMidpoint {
        /// Diagnostic target DTE.
        target_dte: i64,
    },
    /// Selected because it is nearest the explicit target DTE.
    NearestTargetDte {
        /// Configured target DTE.
        target_dte: i64,
    },
}

impl OptionUniverseSelectionReason {
    /// Returns the stable diagnostic label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NearestWindowMidpoint { .. } => "nearest_window_midpoint",
            Self::NearestTargetDte { .. } => "nearest_target_dte",
        }
    }
}

/// Structured skip reason for unresolved universe intent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OptionUniverseSkipReason {
    /// No option contracts were available inside the DTE window.
    NoContractsInWindow,
    /// Contracts existed for the underlying, but all were outside the DTE window.
    OutsideDteWindow,
    /// At least one expiration matched the DTE window, but required call/put sides were missing.
    MissingRequiredSide {
        /// Required option sides.
        required_sides: OptionUniverseRequiredSides,
    },
    /// The instrument provider reported an error for this intent.
    ProviderError {
        /// Provider-independent error code or message.
        message: String,
    },
}

impl OptionUniverseSkipReason {
    /// Returns the stable diagnostic label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NoContractsInWindow => "no_contracts_in_window",
            Self::OutsideDteWindow => "outside_dte_window",
            Self::MissingRequiredSide { .. } => "missing_required_side",
            Self::ProviderError { .. } => "provider_error",
        }
    }
}

/// Available expiration metadata used in selected and skipped diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OptionUniverseExpirationCoverage {
    /// Series identifier for this expiration.
    pub series_id: OptionSeriesId,
    /// Expiration timestamp.
    pub expiration_ns: UnixNanos,
    /// Days to expiration at the resolver evaluation time.
    pub dte: i64,
    /// Number of call contracts available for the series.
    pub call_count: usize,
    /// Number of put contracts available for the series.
    pub put_count: usize,
    /// Total contracts available for the series.
    pub contract_count: usize,
    /// Whether the series DTE is inside the requested window.
    pub in_dte_window: bool,
}

/// Selected concrete option series for one universe intent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedOptionUniverseSeries {
    /// Stable strategy profile identifier.
    pub profile_id: String,
    /// Underlying symbol requested by the strategy profile.
    pub underlying: String,
    /// Strategy family requesting the series.
    pub strategy_family: OptionUniverseStrategyFamily,
    /// Required option sides for this profile.
    pub required_sides: OptionUniverseRequiredSides,
    /// Selected series coverage.
    pub coverage: OptionUniverseExpirationCoverage,
    /// Why this series was selected.
    pub reason: OptionUniverseSelectionReason,
}

/// Skipped universe intent with structured diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkippedOptionUniverseIntent {
    /// Stable strategy profile identifier.
    pub profile_id: String,
    /// Underlying symbol requested by the strategy profile.
    pub underlying: String,
    /// Strategy family requesting the series.
    pub strategy_family: OptionUniverseStrategyFamily,
    /// Required option sides for this profile.
    pub required_sides: OptionUniverseRequiredSides,
    /// Inclusive DTE window requested by this profile.
    pub dte_window: OptionDteWindow,
    /// Why the intent was skipped.
    pub reason: OptionUniverseSkipReason,
    /// Available expirations for the underlying at evaluation time.
    pub available_expirations: Vec<OptionUniverseExpirationCoverage>,
}

/// Resolution output for a batch of universe intents.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OptionUniverseResolution {
    /// Selected concrete option series.
    pub selected: Vec<ResolvedOptionUniverseSeries>,
    /// Skipped intents with structured reasons.
    pub skipped: Vec<SkippedOptionUniverseIntent>,
}

impl OptionUniverseResolution {
    /// Returns whether no intents produced selected or skipped results.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.selected.is_empty() && self.skipped.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SeriesCoverage {
    call_count: usize,
    put_count: usize,
    contract_count: usize,
}

impl SeriesCoverage {
    fn record(&mut self, option_kind: OptionKind) {
        self.contract_count += 1;
        match option_kind {
            OptionKind::Call => self.call_count += 1,
            OptionKind::Put => self.put_count += 1,
        }
    }
}

/// Resolves source-neutral universe intents into concrete option series.
#[must_use]
pub fn resolve_option_universe(
    intents: &[OptionUniverseIntent],
    contracts: &[OptionUniverseContract],
    provider_errors: &[OptionUniverseProviderError],
    evaluation_time: UnixNanos,
) -> OptionUniverseResolution {
    let mut resolution = OptionUniverseResolution::default();
    for intent in intents {
        if let Some(provider_error) = provider_errors
            .iter()
            .find(|error| error.matches_intent(intent))
        {
            resolution.skipped.push(skip_provider_error(
                intent,
                provider_error.message.clone(),
                Vec::new(),
            ));
            continue;
        }

        resolve_intent(intent, contracts, evaluation_time, &mut resolution);
    }
    resolution
}

/// Returns the DTE for an option series at the supplied evaluation time.
#[must_use]
pub fn option_series_dte(series_id: OptionSeriesId, evaluation_time: UnixNanos) -> i64 {
    days_to_expiration(series_id.expiration_ns, evaluation_time)
}

/// Returns whether a selected series still satisfies the intent DTE window.
#[must_use]
pub fn option_series_matches_intent_dte(
    intent: &OptionUniverseIntent,
    series_id: OptionSeriesId,
    evaluation_time: UnixNanos,
) -> bool {
    intent
        .dte_window
        .contains(option_series_dte(series_id, evaluation_time))
}

fn resolve_intent(
    intent: &OptionUniverseIntent,
    contracts: &[OptionUniverseContract],
    evaluation_time: UnixNanos,
    resolution: &mut OptionUniverseResolution,
) {
    let mut series = BTreeMap::<OptionSeriesId, SeriesCoverage>::new();
    for contract in contracts.iter().filter(|contract| {
        contract
            .series_id
            .underlying
            .as_str()
            .eq_ignore_ascii_case(&intent.underlying)
    }) {
        series
            .entry(contract.series_id)
            .or_default()
            .record(contract.option_kind);
    }

    let available = available_expirations(intent, &series, evaluation_time);
    if series.is_empty() {
        resolution.skipped.push(skip_intent(
            intent,
            OptionUniverseSkipReason::NoContractsInWindow,
            available,
        ));
        return;
    }

    let mut in_window = available
        .iter()
        .copied()
        .filter(|coverage| coverage.in_dte_window)
        .collect::<Vec<_>>();
    if in_window.is_empty() {
        resolution.skipped.push(skip_intent(
            intent,
            OptionUniverseSkipReason::OutsideDteWindow,
            available,
        ));
        return;
    }

    in_window.retain(|coverage| {
        intent
            .required_sides
            .is_satisfied_by(coverage.call_count, coverage.put_count)
    });
    if in_window.is_empty() {
        resolution.skipped.push(skip_intent(
            intent,
            OptionUniverseSkipReason::MissingRequiredSide {
                required_sides: intent.required_sides,
            },
            available,
        ));
        return;
    }

    in_window.sort_by_key(|coverage| {
        (
            intent
                .selection_policy
                .distance_key(intent.dte_window, coverage.dte),
            coverage.dte,
            coverage.expiration_ns,
            coverage.series_id,
        )
    });

    let reason = selection_reason(intent.selection_policy, intent.dte_window);
    for coverage in in_window
        .into_iter()
        .take(intent.effective_max_expirations())
    {
        resolution.selected.push(ResolvedOptionUniverseSeries {
            profile_id: intent.profile_id.clone(),
            underlying: intent.underlying.clone(),
            strategy_family: intent.strategy_family,
            required_sides: intent.required_sides,
            coverage,
            reason,
        });
    }
}

fn available_expirations(
    intent: &OptionUniverseIntent,
    series: &BTreeMap<OptionSeriesId, SeriesCoverage>,
    evaluation_time: UnixNanos,
) -> Vec<OptionUniverseExpirationCoverage> {
    series
        .iter()
        .map(|(series_id, coverage)| {
            let dte = days_to_expiration(series_id.expiration_ns, evaluation_time);
            OptionUniverseExpirationCoverage {
                series_id: *series_id,
                expiration_ns: series_id.expiration_ns,
                dte,
                call_count: coverage.call_count,
                put_count: coverage.put_count,
                contract_count: coverage.contract_count,
                in_dte_window: intent.dte_window.contains(dte),
            }
        })
        .collect()
}

fn days_to_expiration(expiration_ns: UnixNanos, evaluation_time: UnixNanos) -> i64 {
    let expiration_date = expiration_ns.to_datetime_utc().date_naive();
    let evaluation_date = evaluation_time.to_datetime_utc().date_naive();
    expiration_date
        .signed_duration_since(evaluation_date)
        .num_days()
}

fn selection_reason(
    policy: OptionUniverseSelectionPolicy,
    window: OptionDteWindow,
) -> OptionUniverseSelectionReason {
    match policy {
        OptionUniverseSelectionPolicy::WindowMidpoint => {
            OptionUniverseSelectionReason::NearestWindowMidpoint {
                target_dte: window.midpoint_dte(),
            }
        }
        OptionUniverseSelectionPolicy::TargetDte { target_dte } => {
            OptionUniverseSelectionReason::NearestTargetDte { target_dte }
        }
    }
}

fn skip_intent(
    intent: &OptionUniverseIntent,
    reason: OptionUniverseSkipReason,
    available_expirations: Vec<OptionUniverseExpirationCoverage>,
) -> SkippedOptionUniverseIntent {
    SkippedOptionUniverseIntent {
        profile_id: intent.profile_id.clone(),
        underlying: intent.underlying.clone(),
        strategy_family: intent.strategy_family,
        required_sides: intent.required_sides,
        dte_window: intent.dte_window,
        reason,
        available_expirations,
    }
}

fn skip_provider_error(
    intent: &OptionUniverseIntent,
    message: String,
    available_expirations: Vec<OptionUniverseExpirationCoverage>,
) -> SkippedOptionUniverseIntent {
    skip_intent(
        intent,
        OptionUniverseSkipReason::ProviderError { message },
        available_expirations,
    )
}
