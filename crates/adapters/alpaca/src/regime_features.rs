//! Regime feature evidence produced from Nautilus runtime data.

use std::{any::Any, collections::BTreeMap, sync::Arc};

use chrono::NaiveDate;
use nautilus_core::UnixNanos;
use nautilus_model::data::{
    Bar, CustomData, CustomDataTrait, DataType, HasTsInit,
    option_chain::{OptionChainSlice, OptionStrikeData},
};
use serde_json::{Value, json};

use crate::{earnings::EarningsEvent, options_entry::SelectedOptionsEntry};

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Current schema version for regime feature snapshots.
pub const REGIME_FEATURE_SCHEMA_VERSION: u16 = 1;
/// Deterministic feature version for Alpaca regime feature snapshots.
pub const ALPACA_REGIME_FEATURE_VERSION: &str = "alpaca_regime_features.v1";
/// Default fraction of option midprice considered a wide quote.
pub const DEFAULT_WIDE_QUOTE_SPREAD_PCT: f64 = 0.15;
/// Default minimum two-sided quotes for the chain-level liquidity group to be usable.
pub const DEFAULT_MIN_TWO_SIDED_QUOTES: usize = 1;
/// Default number of underlying bars required for a usable trend/volatility window.
pub const DEFAULT_UNDERLYING_BAR_LOOKBACK: usize = 20;
/// Default maximum accepted age for the latest underlying bar.
pub const DEFAULT_UNDERLYING_BAR_STALE_AFTER_SECS: u64 = 3 * 24 * 60 * 60;

/// Runtime feature groups available to future regime routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegimeFeatureGroup {
    /// Complete underlying bar window.
    UnderlyingBars,
    /// Trend and realized-volatility features derived from underlying bars.
    UnderlyingTrendVol,
    /// Option-chain or quote-cache liquidity summary.
    OptionLiquidity,
    /// Earnings or external market-event load.
    EventLoad,
    /// Portfolio stress summary.
    PortfolioContext,
    /// Breadth or configured proxy instrument context.
    Breadth,
}

impl RegimeFeatureGroup {
    /// Returns the stable wire name for this group.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnderlyingBars => "underlying_bars",
            Self::UnderlyingTrendVol => "underlying_trend_vol",
            Self::OptionLiquidity => "option_liquidity",
            Self::EventLoad => "event_load",
            Self::PortfolioContext => "portfolio_context",
            Self::Breadth => "breadth",
        }
    }
}

/// Freshness status for one regime feature group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeatureFreshnessStatus {
    /// Feature source satisfies the configured freshness policy.
    Fresh,
    /// Feature source is present but partial or lower quality.
    Degraded,
    /// Feature source is present but older than the configured policy.
    Stale,
    /// Feature source did not produce this group.
    Missing,
}

impl FeatureFreshnessStatus {
    /// Returns the stable wire name for this status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Degraded => "degraded",
            Self::Stale => "stale",
            Self::Missing => "missing",
        }
    }
}

/// Stable regime labels recorded by the router.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegimeLabel {
    /// Lower realized volatility and contained range behavior.
    QuietMeanReverting,
    /// Persistent directional movement with controlled volatility.
    DirectionalTrend,
    /// Elevated realized range with unstable direction.
    HighVolChop,
    /// Earnings, news, gaps, or stress dominate the decision.
    EventShock,
    /// Option liquidity is degraded enough to route conservatively.
    LiquidityStressed,
    /// Required features are missing, stale, degraded, or not yet classifiable.
    Unknown,
}

impl RegimeLabel {
    /// Returns the stable wire name for this label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QuietMeanReverting => "quiet_mean_reverting",
            Self::DirectionalTrend => "directional_trend",
            Self::HighVolChop => "high_vol_chop",
            Self::EventShock => "event_shock",
            Self::LiquidityStressed => "liquidity_stressed",
            Self::Unknown => "unknown",
        }
    }
}

/// Regime routing output consumed by candidate selection and ledgers.
#[derive(Clone, Debug, PartialEq)]
pub struct RegimeContext {
    /// Regime label.
    pub label: RegimeLabel,
    /// Deterministic confidence in `[0.0, 1.0]`.
    pub confidence: f64,
    /// Snapshot timestamp.
    pub as_of_ts: UnixNanos,
    /// Feature calculation version.
    pub feature_version: String,
    /// Feature freshness evidence.
    pub feature_freshness: Vec<FeatureFreshness>,
    /// Feature groups not usable by routing.
    pub unavailable_features: Vec<RegimeFeatureGroup>,
    /// Strategy-family weights. Empty means no weighting was applied.
    pub strategy_family_weights: BTreeMap<String, f64>,
    /// Strategy families blocked before candidate selection.
    pub blocked_strategy_families: Vec<String>,
    /// Threshold adjustments. Empty means no threshold adjustment was applied.
    pub threshold_adjustments: BTreeMap<String, f64>,
    /// Whether order-capable strategy code must dry-run this decision.
    pub dry_run_only: bool,
    /// Explanation codes for the routing decision.
    pub explanation_codes: Vec<String>,
}

impl RegimeContext {
    /// Returns `true` when this context blocks the candidate's strategy family.
    #[must_use]
    pub fn blocks_entry(&self, entry: &SelectedOptionsEntry) -> bool {
        self.blocks_strategy_family(entry.descriptor().candidate_type)
    }

    /// Returns `true` when this context blocks a strategy family.
    #[must_use]
    pub fn blocks_strategy_family(&self, strategy_family: &str) -> bool {
        self.blocked_strategy_families
            .iter()
            .any(|family| family == strategy_family)
    }

    /// Returns the compact routing action label.
    #[must_use]
    pub fn routing_action(&self) -> &'static str {
        if self.dry_run_only {
            "dry_run_only"
        } else if self.blocked_strategy_families.is_empty() {
            "allowed"
        } else {
            "blocked_families"
        }
    }

    /// Returns this context as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut payload = json!({
            "label": self.label.as_str(),
            "confidence": self.confidence,
            "as_of_ts": self.as_of_ts.as_u64(),
            "as_of_ts_utc": self.as_of_ts.to_rfc3339(),
            "feature_version": self.feature_version,
            "feature_freshness": self
                .feature_freshness
                .iter()
                .map(FeatureFreshness::to_json_value)
                .collect::<Vec<_>>(),
            "unavailable_features": self
                .unavailable_features
                .iter()
                .map(|group| group.as_str())
                .collect::<Vec<_>>(),
            "blocked_strategy_families": self.blocked_strategy_families,
            "dry_run_only": self.dry_run_only,
            "routing_action": self.routing_action(),
            "explanation_codes": self.explanation_codes,
        });
        if let Value::Object(fields) = &mut payload {
            if !self.strategy_family_weights.is_empty() {
                fields.insert(
                    "strategy_family_weights".to_string(),
                    json!(&self.strategy_family_weights),
                );
            }
            if !self.threshold_adjustments.is_empty() {
                fields.insert(
                    "threshold_adjustments".to_string(),
                    json!(&self.threshold_adjustments),
                );
            }
        }
        payload
    }
}

/// Summary of routing applied to ranked candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegimeRoutingSummary {
    /// Candidate count before routing.
    pub initial_candidates: usize,
    /// Candidate count after routing.
    pub routed_candidates: usize,
    /// Candidate count blocked before selection.
    pub blocked_candidates: usize,
}

impl RegimeRoutingSummary {
    /// Returns this summary as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "initial_candidates": self.initial_candidates,
            "routed_candidates": self.routed_candidates,
            "blocked_candidates": self.blocked_candidates,
        })
    }
}

/// Freshness evidence for one regime feature group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureFreshness {
    /// Feature group.
    pub group: RegimeFeatureGroup,
    /// Source that produced or should have produced the feature group.
    pub source: String,
    /// Latest source timestamp.
    pub latest_ts: Option<UnixNanos>,
    /// Age of the latest source timestamp in seconds.
    pub age_secs: Option<u64>,
    /// Freshness status.
    pub status: FeatureFreshnessStatus,
}

impl FeatureFreshness {
    /// Creates freshness evidence for a produced feature group.
    #[must_use]
    pub fn produced(
        group: RegimeFeatureGroup,
        source: impl Into<String>,
        latest_ts: Option<UnixNanos>,
        age_secs: Option<u64>,
        status: FeatureFreshnessStatus,
    ) -> Self {
        Self {
            group,
            source: source.into(),
            latest_ts,
            age_secs,
            status,
        }
    }

    /// Creates freshness evidence for a feature group that was not produced.
    #[must_use]
    pub fn missing(group: RegimeFeatureGroup, source: impl Into<String>) -> Self {
        Self {
            group,
            source: source.into(),
            latest_ts: None,
            age_secs: None,
            status: FeatureFreshnessStatus::Missing,
        }
    }

    /// Returns this freshness evidence as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "group": self.group.as_str(),
            "source": self.source,
            "latest_ts": self.latest_ts.map(|ts| ts.as_u64()),
            "latest_ts_utc": self.latest_ts.map(|ts| ts.to_rfc3339()),
            "age_secs": self.age_secs,
            "status": self.status.as_str(),
        })
    }
}

/// Configuration for deterministic regime feature snapshots.
#[derive(Clone, Debug, PartialEq)]
pub struct RegimeFeatureConfig {
    /// Maximum accepted option quote age. Zero disables stale classification.
    pub option_quote_stale_after_secs: u64,
    /// Maximum accepted age for the latest underlying bar. Zero disables stale classification.
    pub underlying_bar_stale_after_secs: u64,
    /// Fraction of midprice considered a wide quote for chain-level summaries.
    pub wide_quote_spread_pct: f64,
    /// Minimum two-sided option quotes required for fresh chain liquidity.
    pub min_two_sided_quotes: usize,
    /// Minimum underlying bars required for fresh bar coverage and trend/vol features.
    pub min_underlying_bars: usize,
}

impl Default for RegimeFeatureConfig {
    fn default() -> Self {
        Self {
            option_quote_stale_after_secs: 30,
            underlying_bar_stale_after_secs: DEFAULT_UNDERLYING_BAR_STALE_AFTER_SECS,
            wide_quote_spread_pct: DEFAULT_WIDE_QUOTE_SPREAD_PCT,
            min_two_sided_quotes: DEFAULT_MIN_TWO_SIDED_QUOTES,
            min_underlying_bars: DEFAULT_UNDERLYING_BAR_LOOKBACK,
        }
    }
}

/// Inputs from runtime-owned sources used to enrich one feature snapshot.
#[derive(Clone, Copy, Debug)]
pub struct RegimeFeatureInputs<'a> {
    /// Cached underlying bars for the option-chain underlying.
    pub underlying_bars: &'a [Bar],
    /// Approved earnings events from runtime event-shock configuration.
    pub earnings_events: &'a [EarningsEvent],
    /// Calendar days before an earnings report considered event load.
    pub event_shock_block_days_before_earnings: i64,
    /// Calendar days after an earnings report considered event load.
    pub event_shock_block_days_after_earnings: i64,
}

impl<'a> RegimeFeatureInputs<'a> {
    /// Returns an input set with no optional runtime sources.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            underlying_bars: &[],
            earnings_events: &[],
            event_shock_block_days_before_earnings: 0,
            event_shock_block_days_after_earnings: 0,
        }
    }
}

/// Underlying bar coverage features.
#[derive(Clone, Debug, PartialEq)]
pub struct UnderlyingBarFeatures {
    /// Source that produced the bar window.
    pub source: String,
    /// Number of bars in the retained window.
    pub bar_count: usize,
    /// First bar timestamp in the retained window.
    pub first_bar_ts: UnixNanos,
    /// Latest bar timestamp in the retained window.
    pub latest_bar_ts: UnixNanos,
    /// Close from the latest bar.
    pub latest_close: f64,
    /// Maximum latest-bar age in seconds relative to snapshot time.
    pub latest_bar_age_secs: Option<u64>,
}

impl UnderlyingBarFeatures {
    /// Returns this bar coverage summary as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "source": self.source,
            "bar_count": self.bar_count,
            "first_bar_ts": self.first_bar_ts.as_u64(),
            "first_bar_ts_utc": self.first_bar_ts.to_rfc3339(),
            "latest_bar_ts": self.latest_bar_ts.as_u64(),
            "latest_bar_ts_utc": self.latest_bar_ts.to_rfc3339(),
            "latest_close": self.latest_close,
            "latest_bar_age_secs": self.latest_bar_age_secs,
        })
    }
}

/// Trend and realized-volatility features derived from underlying bars.
#[derive(Clone, Debug, PartialEq)]
pub struct UnderlyingTrendVolFeatures {
    /// Source that produced the summary.
    pub source: String,
    /// Number of close-to-close returns in the summary.
    pub return_count: usize,
    /// Total close-to-close return over the retained window.
    pub window_return_pct: f64,
    /// Average close-to-close return over the retained window.
    pub mean_return_pct: f64,
    /// Realized close-to-close volatility over the retained window.
    pub realized_volatility_pct: f64,
}

impl UnderlyingTrendVolFeatures {
    /// Returns this trend/vol summary as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "source": self.source,
            "return_count": self.return_count,
            "window_return_pct": self.window_return_pct,
            "mean_return_pct": self.mean_return_pct,
            "realized_volatility_pct": self.realized_volatility_pct,
        })
    }
}

/// Earnings/event-load summary for the underlying.
#[derive(Clone, Debug, PartialEq)]
pub struct EventLoadFeatures {
    /// Source that produced the event load.
    pub source: String,
    /// Number of approved events for the underlying.
    pub underlying_event_count: usize,
    /// Number of events inside the configured event-shock window.
    pub active_event_count: usize,
    /// Nearest event distance in calendar days.
    pub nearest_days_to_report: Option<i64>,
    /// Whether configured event-shock policy currently blocks entries.
    pub blocks_new_entries: bool,
}

impl EventLoadFeatures {
    /// Returns this event-load summary as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "source": self.source,
            "underlying_event_count": self.underlying_event_count,
            "active_event_count": self.active_event_count,
            "nearest_days_to_report": self.nearest_days_to_report,
            "blocks_new_entries": self.blocks_new_entries,
        })
    }
}

/// Chain-level option liquidity features.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionLiquidityFeatures {
    /// Source that produced the summary.
    pub source: String,
    /// Number of option contracts in the slice.
    pub contract_count: usize,
    /// Number of quote records in the slice.
    pub quote_count: usize,
    /// Number of quotes with positive bid and ask prices.
    pub two_sided_quote_count: usize,
    /// Number of quotes with Greeks attached.
    pub greeks_count: usize,
    /// Number of quotes with an implied-volatility value attached.
    pub implied_volatility_count: usize,
    /// Number of contracts with open interest attached.
    pub open_interest_count: usize,
    /// Underlying price carried by the option-chain source, when available.
    pub underlying_price: Option<f64>,
    /// Median bid/ask spread as a fraction of midpoint.
    pub median_spread_pct: Option<f64>,
    /// Fraction of two-sided quotes wider than the configured threshold.
    pub wide_quote_ratio: Option<f64>,
    /// Minimum open interest observed across contracts with open interest.
    pub min_open_interest: Option<u64>,
    /// Maximum quote age in seconds relative to snapshot time.
    pub max_quote_age_secs: Option<u64>,
    /// Wide-quote threshold used for this snapshot.
    pub wide_quote_spread_pct: f64,
}

impl OptionLiquidityFeatures {
    /// Returns this liquidity summary as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "source": self.source,
            "contract_count": self.contract_count,
            "quote_count": self.quote_count,
            "two_sided_quote_count": self.two_sided_quote_count,
            "greeks_count": self.greeks_count,
            "implied_volatility_count": self.implied_volatility_count,
            "open_interest_count": self.open_interest_count,
            "underlying_price": self.underlying_price,
            "median_spread_pct": self.median_spread_pct,
            "wide_quote_ratio": self.wide_quote_ratio,
            "min_open_interest": self.min_open_interest,
            "max_quote_age_secs": self.max_quote_age_secs,
            "wide_quote_spread_pct": self.wide_quote_spread_pct,
        })
    }
}

/// Feature snapshot consumed by future regime routing.
#[derive(Clone, Debug, PartialEq)]
pub struct RegimeFeatureSnapshot {
    /// Snapshot schema version.
    pub schema_version: u16,
    /// Deterministic feature calculation version.
    pub feature_version: String,
    /// Snapshot timestamp.
    pub as_of_ts: UnixNanos,
    /// Trade date derived from the snapshot timestamp.
    pub trade_date: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Underlying bar coverage feature group, when produced.
    pub underlying_bars: Option<UnderlyingBarFeatures>,
    /// Underlying trend/realized-vol feature group, when produced.
    pub underlying_trend_vol: Option<UnderlyingTrendVolFeatures>,
    /// Option-liquidity feature group, when produced.
    pub option_liquidity: Option<OptionLiquidityFeatures>,
    /// Earnings or event-load feature group, when produced.
    pub event_load: Option<EventLoadFeatures>,
    /// Freshness evidence for produced and unavailable groups.
    pub feature_freshness: Vec<FeatureFreshness>,
    /// Feature groups not usable by a router.
    pub unavailable_features: Vec<RegimeFeatureGroup>,
    /// Initialization timestamp.
    pub ts_init: UnixNanos,
}

impl RegimeFeatureSnapshot {
    /// Returns freshness evidence for a feature group.
    #[must_use]
    pub fn freshness_for(&self, group: RegimeFeatureGroup) -> Option<&FeatureFreshness> {
        self.feature_freshness
            .iter()
            .find(|freshness| freshness.group == group)
    }

    /// Returns this snapshot as compact JSON.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "feature_version": self.feature_version,
            "as_of_ts": self.as_of_ts.as_u64(),
            "as_of_ts_utc": self.as_of_ts.to_rfc3339(),
            "trade_date": self.trade_date,
            "underlying": self.underlying,
            "underlying_bars": self.underlying_bars.as_ref().map(UnderlyingBarFeatures::to_json_value),
            "underlying_trend_vol": self.underlying_trend_vol.as_ref().map(UnderlyingTrendVolFeatures::to_json_value),
            "option_liquidity": self.option_liquidity.as_ref().map(OptionLiquidityFeatures::to_json_value),
            "event_load": self.event_load.as_ref().map(EventLoadFeatures::to_json_value),
            "feature_freshness": self
                .feature_freshness
                .iter()
                .map(FeatureFreshness::to_json_value)
                .collect::<Vec<_>>(),
            "unavailable_features": self
                .unavailable_features
                .iter()
                .map(|group| group.as_str())
                .collect::<Vec<_>>(),
            "ts_init": self.ts_init.as_u64(),
        })
    }
}

/// Builds a fail-conservative routing context from one feature snapshot.
#[must_use]
pub fn regime_context_from_features(snapshot: &RegimeFeatureSnapshot) -> RegimeContext {
    let required_groups = [
        RegimeFeatureGroup::UnderlyingBars,
        RegimeFeatureGroup::UnderlyingTrendVol,
        RegimeFeatureGroup::OptionLiquidity,
        RegimeFeatureGroup::EventLoad,
    ];
    let mut required_missing = false;
    let mut required_stale = false;
    let mut explanation_codes = Vec::new();

    for group in required_groups {
        match snapshot
            .freshness_for(group)
            .map(|freshness| freshness.status)
        {
            Some(FeatureFreshnessStatus::Fresh) => {}
            Some(FeatureFreshnessStatus::Degraded) => {
                push_explanation_code(&mut explanation_codes, "required_feature_degraded");
            }
            Some(FeatureFreshnessStatus::Stale) => {
                required_stale = true;
                push_explanation_code(&mut explanation_codes, "required_feature_stale");
            }
            Some(FeatureFreshnessStatus::Missing) | None => {
                required_missing = true;
                push_explanation_code(&mut explanation_codes, "required_feature_missing");
            }
        }
    }

    if snapshot
        .option_liquidity
        .as_ref()
        .and_then(|liquidity| liquidity.wide_quote_ratio)
        .is_some_and(|ratio| ratio > 0.0)
    {
        push_explanation_code(&mut explanation_codes, "liquidity_wide_quotes");
    }

    if explanation_codes.is_empty() {
        push_explanation_code(&mut explanation_codes, "routing_thresholds_unavailable");
    }

    let confidence = if required_missing || required_stale {
        0.0
    } else {
        0.25
    };

    RegimeContext {
        label: RegimeLabel::Unknown,
        confidence,
        as_of_ts: snapshot.as_of_ts,
        feature_version: snapshot.feature_version.clone(),
        feature_freshness: snapshot.feature_freshness.clone(),
        unavailable_features: snapshot.unavailable_features.clone(),
        strategy_family_weights: BTreeMap::new(),
        blocked_strategy_families: vec!["naked_option".to_string()],
        threshold_adjustments: BTreeMap::new(),
        dry_run_only: true,
        explanation_codes,
    }
}

/// Applies regime routing to ranked entries before selection.
#[must_use]
pub fn apply_regime_routing(
    ranked_entries: &mut Vec<SelectedOptionsEntry>,
    context: &RegimeContext,
) -> RegimeRoutingSummary {
    let initial_candidates = ranked_entries.len();
    ranked_entries.retain(|entry| !context.blocks_entry(entry));
    let routed_candidates = ranked_entries.len();

    RegimeRoutingSummary {
        initial_candidates,
        routed_candidates,
        blocked_candidates: initial_candidates.saturating_sub(routed_candidates),
    }
}

/// Inserts regime context into an object payload.
pub fn insert_regime_context(payload: &mut Value, context: Option<&RegimeContext>) {
    let Some(context) = context else {
        return;
    };
    if let Value::Object(fields) = payload {
        fields.insert("regime_context".to_string(), context.to_json_value());
    }
}

/// Custom data type published by regime feature producers.
#[derive(Clone, Debug)]
pub struct RegimeFeatureData {
    /// Produced feature snapshot.
    pub snapshot: RegimeFeatureSnapshot,
}

impl RegimeFeatureData {
    const TYPE_NAME: &'static str = "RegimeFeatureData";

    /// Creates a custom-data payload from a feature snapshot.
    #[must_use]
    pub const fn new(snapshot: RegimeFeatureSnapshot) -> Self {
        Self { snapshot }
    }

    /// Returns the Nautilus custom data type used for feature snapshots.
    #[must_use]
    pub fn data_type() -> DataType {
        DataType::new(Self::TYPE_NAME, None, None)
    }

    /// Wraps this payload as Nautilus custom data.
    #[must_use]
    pub fn into_custom_data(self) -> CustomData {
        CustomData::from_arc(Arc::new(self))
    }
}

impl HasTsInit for RegimeFeatureData {
    fn ts_init(&self) -> UnixNanos {
        self.snapshot.ts_init
    }
}

impl CustomDataTrait for RegimeFeatureData {
    fn type_name(&self) -> &'static str {
        Self::TYPE_NAME
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn ts_event(&self) -> UnixNanos {
        self.snapshot.as_of_ts
    }

    fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(&self.snapshot.to_json_value())?)
    }

    fn clone_arc(&self) -> Arc<dyn CustomDataTrait> {
        Arc::new(self.clone())
    }

    fn eq_arc(&self, other: &dyn CustomDataTrait) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.snapshot == other.snapshot)
    }

    fn type_name_static() -> &'static str
    where
        Self: Sized,
    {
        Self::TYPE_NAME
    }
}

/// Builds a regime feature snapshot from one normalized option-chain slice.
#[must_use]
pub fn regime_feature_snapshot_from_option_chain(
    slice: &OptionChainSlice,
    config: &RegimeFeatureConfig,
    inputs: RegimeFeatureInputs<'_>,
    trade_date: impl Into<String>,
    ts_init: UnixNanos,
) -> RegimeFeatureSnapshot {
    let as_of_ts = if slice.ts_event.is_zero() {
        ts_init
    } else {
        slice.ts_event
    };
    let trade_date = trade_date.into();
    let underlying = slice.series_id.underlying.to_string();
    let underlying_bars = underlying_bar_features(inputs.underlying_bars, as_of_ts);
    let underlying_bar_freshness = underlying_bar_freshness(&underlying_bars, config, as_of_ts);
    let underlying_trend_vol =
        underlying_trend_vol_features(inputs.underlying_bars, config.min_underlying_bars);
    let underlying_trend_vol_freshness =
        underlying_trend_vol_freshness(&underlying_trend_vol, &underlying_bar_freshness);
    let liquidity = option_liquidity_features(slice, config, as_of_ts);
    let liquidity_freshness =
        option_liquidity_freshness(&liquidity, config, latest_quote_ts(slice), as_of_ts);
    let event_load = event_load_features(
        &underlying,
        inputs.earnings_events,
        &trade_date,
        inputs.event_shock_block_days_before_earnings,
        inputs.event_shock_block_days_after_earnings,
    );
    let event_load_freshness = event_load_freshness(&event_load, as_of_ts);
    let feature_freshness = vec![
        underlying_bar_freshness,
        underlying_trend_vol_freshness,
        liquidity_freshness,
        event_load_freshness,
    ];
    let unavailable_features = feature_freshness
        .iter()
        .filter(|freshness| {
            matches!(
                freshness.status,
                FeatureFreshnessStatus::Missing
                    | FeatureFreshnessStatus::Stale
                    | FeatureFreshnessStatus::Degraded
            )
        })
        .map(|freshness| freshness.group)
        .collect();

    RegimeFeatureSnapshot {
        schema_version: REGIME_FEATURE_SCHEMA_VERSION,
        feature_version: ALPACA_REGIME_FEATURE_VERSION.to_string(),
        as_of_ts,
        trade_date,
        underlying,
        underlying_bars,
        underlying_trend_vol,
        option_liquidity: liquidity,
        event_load,
        feature_freshness,
        unavailable_features,
        ts_init,
    }
}

fn underlying_bar_features(bars: &[Bar], as_of_ts: UnixNanos) -> Option<UnderlyingBarFeatures> {
    let first = bars.first()?;
    let latest = bars.last()?;
    let latest_close = latest.close.as_f64();
    if !latest_close.is_finite() || latest_close <= 0.0 {
        return None;
    }

    Some(UnderlyingBarFeatures {
        source: "nautilus_bar_cache".to_string(),
        bar_count: bars.len(),
        first_bar_ts: first.ts_event,
        latest_bar_ts: latest.ts_event,
        latest_close,
        latest_bar_age_secs: age_secs(as_of_ts, latest.ts_event),
    })
}

fn underlying_bar_freshness(
    features: &Option<UnderlyingBarFeatures>,
    config: &RegimeFeatureConfig,
    as_of_ts: UnixNanos,
) -> FeatureFreshness {
    let Some(features) = features else {
        return FeatureFreshness::missing(RegimeFeatureGroup::UnderlyingBars, "nautilus_bar_cache");
    };

    let age_secs = age_secs(as_of_ts, features.latest_bar_ts);
    let status = if features.bar_count < config.min_underlying_bars {
        FeatureFreshnessStatus::Degraded
    } else if config.underlying_bar_stale_after_secs > 0
        && age_secs.is_some_and(|age| age > config.underlying_bar_stale_after_secs)
    {
        FeatureFreshnessStatus::Stale
    } else {
        FeatureFreshnessStatus::Fresh
    };

    FeatureFreshness::produced(
        RegimeFeatureGroup::UnderlyingBars,
        "nautilus_bar_cache",
        Some(features.latest_bar_ts),
        age_secs,
        status,
    )
}

fn underlying_trend_vol_features(
    bars: &[Bar],
    min_underlying_bars: usize,
) -> Option<UnderlyingTrendVolFeatures> {
    if bars.len() < min_underlying_bars.max(2) {
        return None;
    }

    let closes = bars
        .iter()
        .map(|bar| bar.close.as_f64())
        .filter(|close| close.is_finite() && *close > 0.0)
        .collect::<Vec<_>>();
    if closes.len() < min_underlying_bars.max(2) {
        return None;
    }

    let returns = closes
        .windows(2)
        .filter_map(|pair| {
            let previous = pair[0];
            let current = pair[1];
            (previous > 0.0).then_some((current / previous) - 1.0)
        })
        .collect::<Vec<_>>();
    if returns.is_empty() {
        return None;
    }

    let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns
        .iter()
        .map(|value| {
            let diff = value - mean_return;
            diff * diff
        })
        .sum::<f64>()
        / returns.len() as f64;
    let first_close = *closes.first()?;
    let latest_close = *closes.last()?;
    Some(UnderlyingTrendVolFeatures {
        source: "nautilus_bar_cache".to_string(),
        return_count: returns.len(),
        window_return_pct: ((latest_close / first_close) - 1.0) * 100.0,
        mean_return_pct: mean_return * 100.0,
        realized_volatility_pct: variance.sqrt() * 100.0,
    })
}

fn underlying_trend_vol_freshness(
    features: &Option<UnderlyingTrendVolFeatures>,
    bar_freshness: &FeatureFreshness,
) -> FeatureFreshness {
    if features.is_none() {
        return FeatureFreshness::missing(
            RegimeFeatureGroup::UnderlyingTrendVol,
            "nautilus_bar_cache",
        );
    }

    FeatureFreshness::produced(
        RegimeFeatureGroup::UnderlyingTrendVol,
        "nautilus_bar_cache",
        bar_freshness.latest_ts,
        bar_freshness.age_secs,
        bar_freshness.status,
    )
}

fn event_load_features(
    underlying: &str,
    events: &[EarningsEvent],
    trade_date: &str,
    block_days_before_earnings: i64,
    block_days_after_earnings: i64,
) -> Option<EventLoadFeatures> {
    if events.is_empty() {
        return None;
    }

    let trade_date = NaiveDate::parse_from_str(trade_date, "%Y-%m-%d").ok()?;
    let underlying = underlying.to_ascii_uppercase();
    let mut underlying_event_count = 0;
    let mut active_event_count = 0;
    let mut nearest_days_to_report: Option<i64> = None;

    for event in events.iter().filter(|event| event.underlying == underlying) {
        underlying_event_count += 1;
        let days_to_report = event
            .report_date
            .signed_duration_since(trade_date)
            .num_days();
        nearest_days_to_report = Some(nearest_days_to_report.map_or(days_to_report, |nearest| {
            if days_to_report.abs() < nearest.abs() {
                days_to_report
            } else {
                nearest
            }
        }));
        if -block_days_after_earnings <= days_to_report
            && days_to_report <= block_days_before_earnings
        {
            active_event_count += 1;
        }
    }

    Some(EventLoadFeatures {
        source: "approved_earnings_events".to_string(),
        underlying_event_count,
        active_event_count,
        nearest_days_to_report,
        blocks_new_entries: active_event_count > 0,
    })
}

fn event_load_freshness(
    event_load: &Option<EventLoadFeatures>,
    as_of_ts: UnixNanos,
) -> FeatureFreshness {
    if event_load.is_none() {
        return FeatureFreshness::missing(
            RegimeFeatureGroup::EventLoad,
            "approved_earnings_events",
        );
    }

    FeatureFreshness::produced(
        RegimeFeatureGroup::EventLoad,
        "approved_earnings_events",
        Some(as_of_ts),
        Some(0),
        FeatureFreshnessStatus::Fresh,
    )
}

fn option_liquidity_features(
    slice: &OptionChainSlice,
    config: &RegimeFeatureConfig,
    as_of_ts: UnixNanos,
) -> Option<OptionLiquidityFeatures> {
    let mut quote_count = 0;
    let mut two_sided_quote_count = 0;
    let mut greeks_count = 0;
    let mut implied_volatility_count = 0;
    let mut open_interest_values = Vec::new();
    let mut spreads = Vec::new();
    let mut wide_quote_count = 0;
    let mut max_quote_age_secs: Option<u64> = None;
    let mut underlying_price = None;

    for data in slice.calls.values().chain(slice.puts.values()) {
        quote_count += 1;
        if data.greeks.is_some() {
            greeks_count += 1;
        }
        if let Some(greeks) = data.greeks.as_ref() {
            if implied_volatility(data).is_some() {
                implied_volatility_count += 1;
            }
            if let Some(open_interest) = non_negative_u64(greeks.open_interest) {
                open_interest_values.push(open_interest);
            }
            if underlying_price.is_none() {
                underlying_price = greeks
                    .underlying_price
                    .filter(|price| price.is_finite() && *price > 0.0);
            }
        }

        if let Some(age_secs) = age_secs(as_of_ts, data.quote.ts_event) {
            max_quote_age_secs = Some(max_quote_age_secs.map_or(age_secs, |max| max.max(age_secs)));
        }

        if let Some(spread_pct) = quote_spread_pct(data) {
            two_sided_quote_count += 1;
            if spread_pct > config.wide_quote_spread_pct {
                wide_quote_count += 1;
            }
            spreads.push(spread_pct);
        }
    }

    if quote_count == 0 {
        return None;
    }

    let wide_quote_ratio =
        (two_sided_quote_count > 0).then(|| wide_quote_count as f64 / two_sided_quote_count as f64);

    Some(OptionLiquidityFeatures {
        source: "option_chain".to_string(),
        contract_count: slice.call_count() + slice.put_count(),
        quote_count,
        two_sided_quote_count,
        greeks_count,
        implied_volatility_count,
        open_interest_count: open_interest_values.len(),
        underlying_price: underlying_price
            .or_else(|| slice.atm_strike.map(|strike| strike.as_f64())),
        median_spread_pct: median(&mut spreads),
        wide_quote_ratio,
        min_open_interest: open_interest_values.into_iter().min(),
        max_quote_age_secs,
        wide_quote_spread_pct: config.wide_quote_spread_pct,
    })
}

fn option_liquidity_freshness(
    liquidity: &Option<OptionLiquidityFeatures>,
    config: &RegimeFeatureConfig,
    latest_ts: Option<UnixNanos>,
    as_of_ts: UnixNanos,
) -> FeatureFreshness {
    let Some(liquidity) = liquidity else {
        return FeatureFreshness::missing(RegimeFeatureGroup::OptionLiquidity, "option_chain");
    };

    let age_secs = latest_ts.and_then(|ts| age_secs(as_of_ts, ts));
    let status = if liquidity.two_sided_quote_count < config.min_two_sided_quotes
        || liquidity.median_spread_pct.is_none()
    {
        FeatureFreshnessStatus::Degraded
    } else if config.option_quote_stale_after_secs > 0
        && liquidity
            .max_quote_age_secs
            .is_some_and(|age| age > config.option_quote_stale_after_secs)
    {
        FeatureFreshnessStatus::Stale
    } else {
        FeatureFreshnessStatus::Fresh
    };

    FeatureFreshness::produced(
        RegimeFeatureGroup::OptionLiquidity,
        "option_chain",
        latest_ts,
        age_secs,
        status,
    )
}

fn quote_spread_pct(data: &OptionStrikeData) -> Option<f64> {
    let bid = data.quote.bid_price.as_f64();
    let ask = data.quote.ask_price.as_f64();
    if !bid.is_finite() || !ask.is_finite() || bid <= 0.0 || ask <= 0.0 || ask < bid {
        return None;
    }
    let midpoint = (bid + ask) / 2.0;
    (midpoint > 0.0).then_some((ask - bid) / midpoint)
}

fn implied_volatility(data: &OptionStrikeData) -> Option<f64> {
    let greeks = data.greeks.as_ref()?;
    greeks
        .mark_iv
        .or_else(|| match (greeks.bid_iv, greeks.ask_iv) {
            (Some(bid), Some(ask)) if bid.is_finite() && ask.is_finite() && ask >= bid => {
                Some((bid + ask) / 2.0)
            }
            _ => None,
        })
        .or(greeks.bid_iv)
        .or(greeks.ask_iv)
        .filter(|iv| iv.is_finite() && *iv >= 0.0)
}

fn non_negative_u64(value: Option<f64>) -> Option<u64> {
    let value = value?;
    (value.is_finite() && value >= 0.0).then_some(value.floor() as u64)
}

fn latest_quote_ts(slice: &OptionChainSlice) -> Option<UnixNanos> {
    slice
        .calls
        .values()
        .chain(slice.puts.values())
        .map(|data| data.quote.ts_event)
        .filter(|ts| !ts.is_zero())
        .max()
}

fn age_secs(now: UnixNanos, ts: UnixNanos) -> Option<u64> {
    if ts.is_zero() {
        return None;
    }
    now.duration_since(&ts)
        .map(|nanos| nanos / NANOS_PER_SECOND)
        .or(Some(0))
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let midpoint = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[midpoint - 1] + values[midpoint]) / 2.0)
    } else {
        Some(values[midpoint])
    }
}

fn push_explanation_code(codes: &mut Vec<String>, code: &str) {
    if !codes.iter().any(|value| value == code) {
        codes.push(code.to_string());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nautilus_model::{
        data::{
            QuoteTick,
            greeks::OptionGreekValues,
            option_chain::{OptionGreeks, OptionStrikeData},
        },
        enums::GreeksConvention,
        identifiers::{InstrumentId, OptionSeriesId, Venue},
        types::{Price, Quantity},
    };
    use ustr::Ustr;

    use crate::{
        candidate_engine::{
            CreditSpreadKind, NakedOptionCandidate, NakedOptionKind, OptionCapitalRequirementModel,
            ScoredContract, SpreadCandidate,
        },
        options_entry::{SelectedEntry, SelectedNakedOptionEntry, SelectedOptionsEntry},
    };

    use super::*;

    #[test]
    fn option_chain_snapshot_reports_fresh_liquidity() {
        let slice = option_chain_slice(UnixNanos::from_seconds(105), UnixNanos::from_seconds(100));
        let snapshot = regime_feature_snapshot_from_option_chain(
            &slice,
            &RegimeFeatureConfig::default(),
            RegimeFeatureInputs::empty(),
            "2026-06-28",
            UnixNanos::from_seconds(106),
        );

        assert_eq!(snapshot.underlying, "SPY");
        let liquidity = snapshot.option_liquidity.as_ref().unwrap();
        assert_eq!(liquidity.contract_count, 2);
        assert_eq!(liquidity.two_sided_quote_count, 2);
        assert_eq!(liquidity.max_quote_age_secs, Some(5));
        assert_eq!(
            snapshot
                .freshness_for(RegimeFeatureGroup::OptionLiquidity)
                .unwrap()
                .status,
            FeatureFreshnessStatus::Fresh
        );
    }

    #[test]
    fn option_chain_snapshot_marks_required_bar_groups_unavailable() {
        let slice = option_chain_slice(UnixNanos::from_seconds(105), UnixNanos::from_seconds(100));
        let snapshot = regime_feature_snapshot_from_option_chain(
            &slice,
            &RegimeFeatureConfig::default(),
            RegimeFeatureInputs::empty(),
            "2026-06-28",
            UnixNanos::from_seconds(106),
        );

        assert!(
            snapshot
                .unavailable_features
                .contains(&RegimeFeatureGroup::UnderlyingBars)
        );
        assert!(
            snapshot
                .unavailable_features
                .contains(&RegimeFeatureGroup::UnderlyingTrendVol)
        );
        assert!(
            snapshot
                .unavailable_features
                .contains(&RegimeFeatureGroup::EventLoad)
        );
    }

    #[test]
    fn option_chain_snapshot_marks_stale_liquidity() {
        let slice = option_chain_slice(UnixNanos::from_seconds(200), UnixNanos::from_seconds(100));
        let config = RegimeFeatureConfig {
            option_quote_stale_after_secs: 30,
            ..Default::default()
        };
        let snapshot = regime_feature_snapshot_from_option_chain(
            &slice,
            &config,
            RegimeFeatureInputs::empty(),
            "2026-06-28",
            UnixNanos::from_seconds(201),
        );

        assert_eq!(
            snapshot
                .freshness_for(RegimeFeatureGroup::OptionLiquidity)
                .unwrap()
                .status,
            FeatureFreshnessStatus::Stale
        );
        assert!(
            snapshot
                .unavailable_features
                .contains(&RegimeFeatureGroup::OptionLiquidity)
        );
    }

    #[test]
    fn regime_context_fails_conservative_when_required_groups_are_missing() {
        let slice = option_chain_slice(UnixNanos::from_seconds(105), UnixNanos::from_seconds(100));
        let snapshot = regime_feature_snapshot_from_option_chain(
            &slice,
            &RegimeFeatureConfig::default(),
            RegimeFeatureInputs::empty(),
            "2026-06-28",
            UnixNanos::from_seconds(106),
        );
        let context = regime_context_from_features(&snapshot);

        assert_eq!(context.label, RegimeLabel::Unknown);
        assert_eq!(context.confidence, 0.0);
        assert!(context.dry_run_only);
        assert!(context.blocks_strategy_family("naked_option"));
        assert!(
            context
                .explanation_codes
                .contains(&"required_feature_missing".to_string())
        );
    }

    #[test]
    fn regime_routing_blocks_naked_options_before_selection() {
        let slice = option_chain_slice(UnixNanos::from_seconds(105), UnixNanos::from_seconds(100));
        let snapshot = regime_feature_snapshot_from_option_chain(
            &slice,
            &RegimeFeatureConfig::default(),
            RegimeFeatureInputs::empty(),
            "2026-06-28",
            UnixNanos::from_seconds(106),
        );
        let context = regime_context_from_features(&snapshot);
        let mut entries = vec![naked_entry(100.0), credit_entry(80.0)];

        let summary = apply_regime_routing(&mut entries, &context);

        assert_eq!(summary.initial_candidates, 2);
        assert_eq!(summary.routed_candidates, 1);
        assert_eq!(summary.blocked_candidates, 1);
        assert!(matches!(
            entries.first(),
            Some(SelectedOptionsEntry::Credit(_))
        ));
    }

    fn option_chain_slice(as_of_ts: UnixNanos, quote_ts: UnixNanos) -> OptionChainSlice {
        let strike = Price::from("500.00");
        let call_id = InstrumentId::from("SPY260702C00500000.OPRA");
        let put_id = InstrumentId::from("SPY260702P00500000.OPRA");
        let mut calls = BTreeMap::new();
        calls.insert(strike, strike_data(call_id, "1.00", "1.05", quote_ts));
        let mut puts = BTreeMap::new();
        puts.insert(strike, strike_data(put_id, "1.10", "1.15", quote_ts));

        OptionChainSlice {
            series_id: OptionSeriesId::new(
                Venue::new("OPRA"),
                Ustr::from("SPY"),
                Ustr::from("USD"),
                UnixNanos::from_seconds(1_783_036_800),
            ),
            atm_strike: Some(strike),
            calls,
            puts,
            ts_event: as_of_ts,
            ts_init: as_of_ts,
        }
    }

    fn strike_data(
        instrument_id: InstrumentId,
        bid: &str,
        ask: &str,
        quote_ts: UnixNanos,
    ) -> OptionStrikeData {
        OptionStrikeData {
            quote: QuoteTick::new(
                instrument_id,
                Price::from(bid),
                Price::from(ask),
                quantity_to_one(),
                quantity_to_one(),
                quote_ts,
                quote_ts,
            ),
            greeks: Some(OptionGreeks {
                instrument_id,
                convention: GreeksConvention::BlackScholes,
                greeks: OptionGreekValues::default(),
                mark_iv: Some(0.2),
                bid_iv: None,
                ask_iv: None,
                underlying_price: Some(500.0),
                open_interest: Some(250.0),
                ts_event: quote_ts,
                ts_init: quote_ts,
            }),
        }
    }

    fn quantity_to_one() -> Quantity {
        Quantity::from("1")
    }

    fn credit_entry(score: f64) -> SelectedOptionsEntry {
        SelectedOptionsEntry::Credit(SelectedEntry {
            underlying: "SPY".to_string(),
            kind: CreditSpreadKind::Put,
            candidate: SpreadCandidate {
                short: scored_contract("SPY260702P00500000", 500.0),
                long: scored_contract("SPY260702P00495000", 495.0),
                width: 5.0,
                credit: 1.0,
                max_loss: 4.0,
                return_on_risk: 0.25,
                score,
            },
        })
    }

    fn naked_entry(score: f64) -> SelectedOptionsEntry {
        SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
            underlying: "SPY".to_string(),
            kind: NakedOptionKind::Put,
            candidate: NakedOptionCandidate {
                short: scored_contract("SPY260702P00500000", 500.0),
                credit: 1.0,
                capital_requirement_model: OptionCapitalRequirementModel::CashSecuredPut,
                estimated_buying_power_requirement: 5_000.0,
                buying_power_usage_pct: Some(0.1),
                return_on_buying_power: 0.02,
                score,
            },
        })
    }

    fn scored_contract(symbol: &str, strike: f64) -> ScoredContract {
        ScoredContract {
            symbol: symbol.to_string(),
            expiration_date: "2026-07-02".to_string(),
            dte: 5,
            strike,
            bid: 1.0,
            ask: 1.1,
            delta_abs: 0.2,
            spread_pct: 0.05,
            bid_size: 1,
            ask_size: 1,
            volume: 0,
            open_interest: 250,
            implied_volatility: Some(0.2),
            metrics: None,
        }
    }
}
