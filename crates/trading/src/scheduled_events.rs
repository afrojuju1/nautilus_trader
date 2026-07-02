//! Source-neutral scheduled-event custom data contracts.
//!
//! These types model non-price event inputs such as earnings dates. Source adapters write
//! observations, resolvers write canonical decisions, and approval policy writes the runtime-safe
//! approved view. The trading runtime should consume approved records instead of provider payloads.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Once},
};

use chrono::{DateTime, Datelike, Duration, NaiveDate, SecondsFormat, Utc};
use nautilus_core::UnixNanos;
use nautilus_model::data::{CustomData, DataType};
use nautilus_persistence_macros::custom_data;
use nautilus_serialization::ensure_custom_data_registered;

/// Environment variable that overrides the scheduled-event catalog root.
pub const SCHEDULED_EVENT_CATALOG_ENV: &str = "NAUTILUS_SCHEDULED_EVENT_CATALOG";

const STATE_DIR: &str = "nautilus_trader";
const SCHEDULED_EVENTS_DIR: &str = "scheduled_events";
const CATALOG_DIR: &str = "catalog";
const RAW_DIR: &str = "raw";

/// Initial deterministic resolver version for scheduled-event observations.
pub const SCHEDULED_EVENT_RESOLVER_VERSION: &str = "scheduled-event-resolver:v1";
/// Initial approval policy version for runtime-safe scheduled events.
pub const SCHEDULED_EVENT_APPROVAL_POLICY_VERSION: &str = "scheduled-event-approval:v1";

const STATUS_CONFIRMED: &str = "confirmed";
const STATUS_CONFLICTED: &str = "conflicted";
const STATUS_UNCERTAIN: &str = "uncertain";
const STATUS_REJECTED: &str = "rejected";

const APPROVAL_APPROVED: &str = "approved";
const APPROVAL_BLOCK_ONLY: &str = "block_only";
const APPROVAL_REJECTED: &str = "rejected";

const TIMING_BEFORE_OPEN: &str = "before_open";
const TIMING_AFTER_CLOSE: &str = "after_close";
const TIMING_DURING_SESSION: &str = "during_session";
const TIMING_UNKNOWN: &str = "unknown";

/// A normalized provider or manual observation for one scheduled event.
///
/// `ts_init` is the ingest/write timestamp used for catalog ordering. `event_date` carries the
/// scheduled-event date as a `YYYY-MM-DD` string, and `ts_event` should be the best event/as-of
/// timestamp available for the observation.
#[custom_data]
pub struct ScheduledEventObservation {
    /// Stable observation id assigned by the source adapter.
    pub observation_id: String,
    /// Neutral event type, such as `earnings_report`.
    pub event_type: String,
    /// Observation source, such as `alpha_vantage` or `manual_override`.
    pub source: String,
    /// Provider-native event id or a deterministic manual id.
    pub source_event_id: String,
    /// Canonical underlying symbol.
    pub underlying: String,
    /// Scheduled event date as `YYYY-MM-DD`.
    pub event_date: String,
    /// Normalized timing: `before_open`, `after_close`, `during_session`, or `unknown`.
    pub timing: String,
    /// Timezone used by the source event date and timing fields.
    pub timezone: String,
    /// Provider published timestamp in UTC when available.
    pub source_published_at_utc: String,
    /// Source fetch timestamp in UTC.
    pub source_fetched_at_utc: String,
    /// URI or local path for retained raw evidence.
    pub raw_uri: String,
    /// SHA-256 digest for the retained raw evidence.
    pub raw_sha256: String,
    /// Stable delimited or JSON string of source quality flags.
    pub quality_flags: String,
    /// Best event/as-of timestamp available for this observation.
    pub ts_event: UnixNanos,
    /// Ingest/write timestamp for catalog ordering.
    pub ts_init: UnixNanos,
}

impl ScheduledEventObservation {
    /// Nautilus custom data type name.
    pub const TYPE_NAME: &'static str = "ScheduledEventObservation";

    /// Returns the catalog identifier for an observation partition.
    #[must_use]
    pub fn catalog_identifier(event_type: &str, source: &str) -> String {
        format!(
            "{}.{}",
            catalog_identifier_component(event_type),
            catalog_identifier_component(source)
        )
    }

    /// Returns the Nautilus data type for this scheduled-event observation.
    #[must_use]
    pub fn data_type(identifier: Option<String>) -> DataType {
        DataType::new(Self::TYPE_NAME, None, identifier)
    }

    /// Wraps this record as Nautilus custom data with its observation partition identifier.
    #[must_use]
    pub fn into_custom_data(self, identifier: Option<String>) -> CustomData {
        CustomData::new(Arc::new(self), Self::data_type(identifier))
    }
}

/// A resolver decision for a canonical scheduled event.
///
/// Resolver decisions preserve conflict and confidence evidence before approval policy filters the
/// runtime-facing event set.
#[custom_data]
pub struct ScheduledEventDecision {
    /// Stable canonical id assigned by the resolver.
    pub canonical_event_id: String,
    /// Neutral event type, such as `earnings_report`.
    pub event_type: String,
    /// Canonical underlying symbol.
    pub underlying: String,
    /// Scheduled event date as `YYYY-MM-DD`.
    pub event_date: String,
    /// Normalized timing: `before_open`, `after_close`, `during_session`, or `unknown`.
    pub timing: String,
    /// Resolver status: `confirmed`, `conflicted`, `uncertain`, or `rejected`.
    pub status: String,
    /// Resolver confidence in the normalized event.
    pub confidence: f64,
    /// Stable delimited or JSON string of source names used.
    pub sources_used: String,
    /// Human-readable conflict reason, empty when not conflicted.
    pub conflict_reason: String,
    /// Resolver implementation or policy version.
    pub resolver_version: String,
    /// Decision timestamp in UTC.
    pub decided_at_utc: String,
    /// Inclusive validity start timestamp in UTC.
    pub valid_from_utc: String,
    /// Exclusive validity end timestamp in UTC.
    pub valid_until_utc: String,
    /// Event/as-of timestamp for this decision.
    pub ts_event: UnixNanos,
    /// Ingest/write timestamp for catalog ordering.
    pub ts_init: UnixNanos,
}

impl ScheduledEventDecision {
    /// Nautilus custom data type name.
    pub const TYPE_NAME: &'static str = "ScheduledEventDecision";

    /// Returns the catalog identifier for a decision partition.
    #[must_use]
    pub fn catalog_identifier(event_type: &str) -> String {
        catalog_identifier_component(event_type)
    }

    /// Returns the Nautilus data type for this scheduled-event decision.
    #[must_use]
    pub fn data_type(identifier: Option<String>) -> DataType {
        DataType::new(Self::TYPE_NAME, None, identifier)
    }

    /// Wraps this record as Nautilus custom data with its decision partition identifier.
    #[must_use]
    pub fn into_custom_data(self, identifier: Option<String>) -> CustomData {
        CustomData::new(Arc::new(self), Self::data_type(identifier))
    }
}

/// A policy-approved scheduled event for read-only runtime consumption.
///
/// Approval records are the only scheduled-event dataset that strategy runtime code should load.
#[custom_data]
pub struct ApprovedScheduledEvent {
    /// Stable canonical id assigned by the resolver.
    pub canonical_event_id: String,
    /// Neutral event type, such as `earnings_report`.
    pub event_type: String,
    /// Canonical underlying symbol.
    pub underlying: String,
    /// Scheduled event date as `YYYY-MM-DD`.
    pub event_date: String,
    /// Normalized timing: `before_open`, `after_close`, `during_session`, or `unknown`.
    pub timing: String,
    /// Approval status: `approved`, `block_only`, or `rejected`.
    pub approval_status: String,
    /// Stable delimited or JSON string of source names behind the approved view.
    pub source_set: String,
    /// Approval policy version.
    pub policy_version: String,
    /// Number of calendar days to block before the event.
    pub block_days_before: i64,
    /// Number of calendar days to block after the event.
    pub block_days_after: i64,
    /// Approval timestamp in UTC.
    pub approved_at_utc: String,
    /// Inclusive validity start timestamp in UTC.
    pub valid_from_utc: String,
    /// Exclusive validity end timestamp in UTC.
    pub valid_until_utc: String,
    /// Diagnostic approval or rejection reason.
    pub diagnostic_reason: String,
    /// Event/as-of timestamp for this approved view.
    pub ts_event: UnixNanos,
    /// Ingest/write timestamp for catalog ordering.
    pub ts_init: UnixNanos,
}

impl ApprovedScheduledEvent {
    /// Nautilus custom data type name.
    pub const TYPE_NAME: &'static str = "ApprovedScheduledEvent";

    /// Returns the catalog identifier for an approved-event partition.
    #[must_use]
    pub fn catalog_identifier(event_type: &str) -> String {
        catalog_identifier_component(event_type)
    }

    /// Returns the Nautilus data type for this approved scheduled event.
    #[must_use]
    pub fn data_type(identifier: Option<String>) -> DataType {
        DataType::new(Self::TYPE_NAME, None, identifier)
    }

    /// Wraps this record as Nautilus custom data with its approved partition identifier.
    #[must_use]
    pub fn into_custom_data(self, identifier: Option<String>) -> CustomData {
        CustomData::new(Arc::new(self), Self::data_type(identifier))
    }
}

/// Deterministic configuration for resolving scheduled-event observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledEventResolverConfig {
    /// Resolver version stamped onto decision records.
    pub resolver_version: String,
    /// Higher-priority source labels come first.
    pub source_precedence: Vec<String>,
    /// Number of days the decision remains valid from `decided_at_utc`.
    pub valid_for_days: i64,
}

impl Default for ScheduledEventResolverConfig {
    fn default() -> Self {
        Self {
            resolver_version: SCHEDULED_EVENT_RESOLVER_VERSION.to_string(),
            source_precedence: vec!["manual_override".to_string(), "alpha_vantage".to_string()],
            valid_for_days: 7,
        }
    }
}

/// Conservative approval policy for runtime-safe scheduled events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledEventApprovalPolicy {
    /// Approval policy version stamped onto approved-event records.
    pub policy_version: String,
    /// Number of calendar days to block before the event.
    pub block_days_before: i64,
    /// Number of calendar days to block after the event.
    pub block_days_after: i64,
    /// Whether unknown timing can become an approved tradeable event.
    pub allow_unknown_timing: bool,
    /// Whether weekend event dates can be consumed by runtime.
    pub allow_weekend_events: bool,
    /// Whether symbols outside common listed-equity shape can be consumed by runtime.
    pub allow_non_common_symbols: bool,
}

impl Default for ScheduledEventApprovalPolicy {
    fn default() -> Self {
        Self {
            policy_version: SCHEDULED_EVENT_APPROVAL_POLICY_VERSION.to_string(),
            block_days_before: 1,
            block_days_after: 1,
            allow_unknown_timing: false,
            allow_weekend_events: false,
            allow_non_common_symbols: false,
        }
    }
}

/// Resolves observations into canonical scheduled-event decisions.
#[must_use]
pub fn resolve_scheduled_event_observations(
    observations: &[ScheduledEventObservation],
    config: &ScheduledEventResolverConfig,
    decided_at_utc: DateTime<Utc>,
) -> Vec<ScheduledEventDecision> {
    let mut groups: BTreeMap<ObservationGroupKey, Vec<ScheduledEventObservation>> = BTreeMap::new();

    for observation in dedupe_observations(observations, config) {
        let key = ObservationGroupKey {
            event_type: observation.event_type.trim().to_string(),
            underlying: normalize_underlying(&observation.underlying),
            event_date: observation.event_date.trim().to_string(),
        };
        groups.entry(key).or_default().push(observation);
    }

    groups
        .into_iter()
        .map(|(key, group)| resolve_observation_group(key, &group, config, decided_at_utc))
        .collect()
}

/// Applies runtime approval policy to resolver decisions.
#[must_use]
pub fn approve_scheduled_event_decisions(
    decisions: &[ScheduledEventDecision],
    policy: &ScheduledEventApprovalPolicy,
    approved_at_utc: DateTime<Utc>,
) -> Vec<ApprovedScheduledEvent> {
    decisions
        .iter()
        .map(|decision| approve_decision(decision, policy, approved_at_utc))
        .collect()
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ObservationGroupKey {
    event_type: String,
    underlying: String,
    event_date: String,
}

fn dedupe_observations(
    observations: &[ScheduledEventObservation],
    config: &ScheduledEventResolverConfig,
) -> Vec<ScheduledEventObservation> {
    let mut by_source_event: BTreeMap<(String, String), ScheduledEventObservation> =
        BTreeMap::new();

    for observation in observations {
        let key = (
            observation.source.trim().to_string(),
            observation.source_event_id.trim().to_string(),
        );
        match by_source_event.get(&key) {
            Some(existing)
                if compare_observation_freshness(observation, existing, config).is_lt() => {}
            _ => {
                by_source_event.insert(key, observation.clone());
            }
        }
    }

    by_source_event.into_values().collect()
}

fn resolve_observation_group(
    key: ObservationGroupKey,
    group: &[ScheduledEventObservation],
    config: &ScheduledEventResolverConfig,
    decided_at_utc: DateTime<Utc>,
) -> ScheduledEventDecision {
    let mut sorted = group.to_vec();
    sorted.sort_by(|left, right| compare_observation_freshness(left, right, config));

    let sources_used = sorted_sources(group, config).join("|");
    let known_timings = sorted
        .iter()
        .filter_map(|observation| known_timing(&observation.timing))
        .collect::<BTreeSet<_>>();
    let chosen_timing = sorted
        .iter()
        .find_map(|observation| known_timing(&observation.timing))
        .unwrap_or_else(|| TIMING_UNKNOWN.to_string());
    let (status, confidence, conflict_reason) = resolver_status(&key, &known_timings);
    let valid_from_utc = format_utc(decided_at_utc);
    let valid_until_utc = format_utc(decided_at_utc + Duration::days(config.valid_for_days));

    ScheduledEventDecision {
        canonical_event_id: canonical_event_id(&key.event_type, &key.underlying, &key.event_date),
        event_type: key.event_type,
        underlying: key.underlying,
        event_date: key.event_date.clone(),
        timing: chosen_timing,
        status,
        confidence,
        sources_used,
        conflict_reason,
        resolver_version: config.resolver_version.clone(),
        decided_at_utc: format_utc(decided_at_utc),
        valid_from_utc,
        valid_until_utc,
        ts_event: unix_nanos_from_date_string(&key.event_date),
        ts_init: unix_nanos_from_utc(decided_at_utc),
    }
}

fn resolver_status(
    key: &ObservationGroupKey,
    known_timings: &BTreeSet<String>,
) -> (String, f64, String) {
    if key.event_type.is_empty() || key.underlying.is_empty() {
        return (
            STATUS_REJECTED.to_string(),
            0.0,
            "missing_event_type_or_underlying".to_string(),
        );
    }
    if NaiveDate::parse_from_str(&key.event_date, "%Y-%m-%d").is_err() {
        return (
            STATUS_REJECTED.to_string(),
            0.0,
            "invalid_event_date".to_string(),
        );
    }
    if known_timings.len() > 1 {
        return (
            STATUS_CONFLICTED.to_string(),
            0.25,
            format!(
                "timing_disagreement:{}",
                known_timings.iter().cloned().collect::<Vec<_>>().join("|")
            ),
        );
    }
    if known_timings.is_empty() {
        return (
            STATUS_UNCERTAIN.to_string(),
            0.4,
            "timing_unknown".to_string(),
        );
    }

    (STATUS_CONFIRMED.to_string(), 0.9, String::new())
}

fn approve_decision(
    decision: &ScheduledEventDecision,
    policy: &ScheduledEventApprovalPolicy,
    approved_at_utc: DateTime<Utc>,
) -> ApprovedScheduledEvent {
    let (approval_status, diagnostic_reason) = approval_status_and_reason(decision, policy);

    ApprovedScheduledEvent {
        canonical_event_id: decision.canonical_event_id.clone(),
        event_type: decision.event_type.clone(),
        underlying: decision.underlying.clone(),
        event_date: decision.event_date.clone(),
        timing: decision.timing.clone(),
        approval_status,
        source_set: decision.sources_used.clone(),
        policy_version: policy.policy_version.clone(),
        block_days_before: policy.block_days_before,
        block_days_after: policy.block_days_after,
        approved_at_utc: format_utc(approved_at_utc),
        valid_from_utc: decision.valid_from_utc.clone(),
        valid_until_utc: decision.valid_until_utc.clone(),
        diagnostic_reason,
        ts_event: decision.ts_event,
        ts_init: unix_nanos_from_utc(approved_at_utc),
    }
}

fn approval_status_and_reason(
    decision: &ScheduledEventDecision,
    policy: &ScheduledEventApprovalPolicy,
) -> (String, String) {
    let Ok(event_date) = NaiveDate::parse_from_str(&decision.event_date, "%Y-%m-%d") else {
        return (
            APPROVAL_REJECTED.to_string(),
            "invalid_event_date".to_string(),
        );
    };
    if !policy.allow_non_common_symbols && !is_common_listed_equity_symbol(&decision.underlying) {
        return (
            APPROVAL_REJECTED.to_string(),
            "non_common_symbol".to_string(),
        );
    }
    if !policy.allow_weekend_events && !is_weekday(event_date) {
        return (
            APPROVAL_REJECTED.to_string(),
            "weekend_event_date".to_string(),
        );
    }
    if decision.status == STATUS_CONFIRMED
        && (policy.allow_unknown_timing || known_timing(&decision.timing).is_some())
    {
        return (APPROVAL_APPROVED.to_string(), "confirmed".to_string());
    }
    if matches!(
        decision.status.as_str(),
        STATUS_CONFLICTED | STATUS_UNCERTAIN
    ) || decision.timing == TIMING_UNKNOWN
    {
        return (
            APPROVAL_BLOCK_ONLY.to_string(),
            format!("{}:{}", decision.status, decision.conflict_reason),
        );
    }

    (
        APPROVAL_REJECTED.to_string(),
        format!("decision_status:{}", decision.status),
    )
}

fn compare_observation_freshness(
    left: &ScheduledEventObservation,
    right: &ScheduledEventObservation,
    config: &ScheduledEventResolverConfig,
) -> std::cmp::Ordering {
    source_rank(&left.source, config)
        .cmp(&source_rank(&right.source, config))
        .then_with(|| right.ts_init.as_u64().cmp(&left.ts_init.as_u64()))
        .then_with(|| left.observation_id.cmp(&right.observation_id))
}

fn sorted_sources(
    observations: &[ScheduledEventObservation],
    config: &ScheduledEventResolverConfig,
) -> Vec<String> {
    let mut sources = observations
        .iter()
        .map(|observation| observation.source.trim().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| {
        source_rank(left, config)
            .cmp(&source_rank(right, config))
            .then_with(|| left.cmp(right))
    });
    sources
}

fn source_rank(source: &str, config: &ScheduledEventResolverConfig) -> usize {
    config
        .source_precedence
        .iter()
        .position(|candidate| candidate == source)
        .unwrap_or(config.source_precedence.len())
}

fn known_timing(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "before_open" | "before" | "bmo" | "pre-market" => Some(TIMING_BEFORE_OPEN.to_string()),
        "after_close" | "after" | "amc" | "post-market" => Some(TIMING_AFTER_CLOSE.to_string()),
        "during_session" | "during" => Some(TIMING_DURING_SESSION.to_string()),
        "unknown" | "unk" | "" => None,
        _ => None,
    }
}

fn normalize_underlying(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

fn canonical_event_id(event_type: &str, underlying: &str, event_date: &str) -> String {
    format!("{event_type}:{underlying}:{event_date}")
}

fn is_weekday(date: NaiveDate) -> bool {
    date.weekday().number_from_monday() <= 5
}

fn is_common_listed_equity_symbol(symbol: &str) -> bool {
    let len = symbol.len();
    (1..=4).contains(&len) && symbol.chars().all(|ch| ch.is_ascii_uppercase())
}

fn unix_nanos_from_date_string(value: &str) -> UnixNanos {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .and_then(|datetime| datetime.and_utc().timestamp_nanos_opt())
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(UnixNanos::from)
        .unwrap_or_default()
}

fn unix_nanos_from_utc(value: DateTime<Utc>) -> UnixNanos {
    value
        .timestamp_nanos_opt()
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(UnixNanos::from)
        .unwrap_or_default()
}

fn format_utc(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Registers all scheduled-event custom data types for JSON and Arrow catalog use.
pub fn ensure_scheduled_event_custom_data_registered() {
    static ONCE: Once = Once::new();

    ONCE.call_once(|| {
        ensure_custom_data_registered::<ScheduledEventObservation>();
        ensure_custom_data_registered::<ScheduledEventDecision>();
        ensure_custom_data_registered::<ApprovedScheduledEvent>();
    });
}

/// Returns the configured scheduled-event catalog path.
///
/// `NAUTILUS_SCHEDULED_EVENT_CATALOG` takes precedence. Empty values fall back to the default state
/// directory.
#[must_use]
pub fn scheduled_event_catalog_path() -> PathBuf {
    scheduled_event_catalog_path_from_env(
        env::var_os(SCHEDULED_EVENT_CATALOG_ENV),
        env::var_os("XDG_STATE_HOME"),
        env::var_os("HOME"),
    )
}

/// Returns the documented default scheduled-event catalog path.
///
/// The default is `$XDG_STATE_HOME/nautilus_trader/scheduled_events/catalog`, then
/// `$HOME/.local/state/nautilus_trader/scheduled_events/catalog`, and finally a relative
/// `nautilus_trader/scheduled_events/catalog` path if neither environment variable is available.
#[must_use]
pub fn default_scheduled_event_catalog_path() -> PathBuf {
    default_scheduled_event_catalog_path_from_env(
        env::var_os("XDG_STATE_HOME"),
        env::var_os("HOME"),
    )
}

/// Returns the scheduled-event root beside the catalog.
#[must_use]
pub fn scheduled_event_root_path() -> PathBuf {
    let catalog_path = scheduled_event_catalog_path();
    parent_or_current(&catalog_path).to_path_buf()
}

/// Returns the raw evidence root beside the catalog.
#[must_use]
pub fn scheduled_event_raw_path() -> PathBuf {
    scheduled_event_root_path().join(RAW_DIR)
}

fn scheduled_event_catalog_path_from_env(
    catalog_env: Option<OsString>,
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> PathBuf {
    non_empty_path(catalog_env)
        .unwrap_or_else(|| default_scheduled_event_catalog_path_from_env(xdg_state_home, home))
}

fn default_scheduled_event_catalog_path_from_env(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> PathBuf {
    state_home_path(xdg_state_home, home)
        .join(STATE_DIR)
        .join(SCHEDULED_EVENTS_DIR)
        .join(CATALOG_DIR)
}

fn state_home_path(xdg_state_home: Option<OsString>, home: Option<OsString>) -> PathBuf {
    if let Some(path) = non_empty_path(xdg_state_home) {
        return path;
    }

    non_empty_path(home)
        .map(|path| path.join(".local").join("state"))
        .unwrap_or_default()
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|value| !value.as_os_str().is_empty())
        .map(PathBuf::from)
}

fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn catalog_identifier_component(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use nautilus_model::data::{CustomDataTrait, Data};
    use nautilus_persistence::backend::catalog::ParquetDataCatalog;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn scheduled_event_catalog_path_uses_env_override() {
        let path = scheduled_event_catalog_path_from_env(
            Some(OsString::from("/tmp/nautilus/events/catalog")),
            Some(OsString::from("/state")),
            Some(OsString::from("/home/ade")),
        );

        assert_eq!(path, PathBuf::from("/tmp/nautilus/events/catalog"));
    }

    #[test]
    fn default_scheduled_event_catalog_path_prefers_xdg_state_home() {
        let path = default_scheduled_event_catalog_path_from_env(
            Some(OsString::from("/state")),
            Some(OsString::from("/home/ade")),
        );

        assert_eq!(
            path,
            PathBuf::from("/state/nautilus_trader/scheduled_events/catalog")
        );
    }

    #[test]
    fn default_scheduled_event_catalog_path_falls_back_to_home_state() {
        let path =
            default_scheduled_event_catalog_path_from_env(None, Some(OsString::from("/home/ade")));

        assert_eq!(
            path,
            PathBuf::from("/home/ade/.local/state/nautilus_trader/scheduled_events/catalog")
        );
    }

    #[test]
    fn default_scheduled_event_catalog_path_uses_relative_state_without_env() {
        let path = default_scheduled_event_catalog_path_from_env(None, None);

        assert_eq!(
            path,
            PathBuf::from("nautilus_trader/scheduled_events/catalog")
        );
    }

    #[test]
    fn resolver_marks_known_timing_conflict_and_approval_blocks_only() {
        let observations = vec![
            observation("AAPL", "2026-05-05", "before_open", "alpha_vantage", 1),
            observation("AAPL", "2026-05-05", "after_close", "manual_override", 2),
        ];

        let decisions = resolve_scheduled_event_observations(
            &observations,
            &ScheduledEventResolverConfig::default(),
            fixed_utc(),
        );
        let approved = approve_scheduled_event_decisions(
            &decisions,
            &ScheduledEventApprovalPolicy::default(),
            fixed_utc(),
        );

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].timing, "after_close");
        assert_eq!(decisions[0].status, "conflicted");
        assert_eq!(decisions[0].sources_used, "manual_override|alpha_vantage");
        assert!(decisions[0].conflict_reason.contains("timing_disagreement"));
        assert_eq!(approved[0].approval_status, "block_only");
        assert!(
            approved[0]
                .diagnostic_reason
                .contains("timing_disagreement")
        );
    }

    #[test]
    fn approval_blocks_unknown_common_weekday_events() {
        let observations = vec![observation(
            "IWM",
            "2026-05-05",
            "unknown",
            "alpha_vantage",
            1,
        )];

        let decisions = resolve_scheduled_event_observations(
            &observations,
            &ScheduledEventResolverConfig::default(),
            fixed_utc(),
        );
        let approved = approve_scheduled_event_decisions(
            &decisions,
            &ScheduledEventApprovalPolicy::default(),
            fixed_utc(),
        );

        assert_eq!(decisions[0].status, "uncertain");
        assert_eq!(decisions[0].conflict_reason, "timing_unknown");
        assert_eq!(approved[0].approval_status, "block_only");
        assert!(approved[0].diagnostic_reason.contains("timing_unknown"));
    }

    #[test]
    fn approval_rejects_weekend_and_non_common_symbols() {
        let observations = vec![
            observation("QQQ", "2026-05-09", "after_close", "alpha_vantage", 1),
            observation("NABZY", "2026-05-05", "after_close", "alpha_vantage", 2),
        ];

        let decisions = resolve_scheduled_event_observations(
            &observations,
            &ScheduledEventResolverConfig::default(),
            fixed_utc(),
        );
        let approved = approve_scheduled_event_decisions(
            &decisions,
            &ScheduledEventApprovalPolicy::default(),
            fixed_utc(),
        );

        let by_underlying = approved
            .iter()
            .map(|event| (event.underlying.as_str(), event))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_underlying["QQQ"].approval_status, "rejected");
        assert_eq!(by_underlying["QQQ"].diagnostic_reason, "weekend_event_date");
        assert_eq!(by_underlying["NABZY"].approval_status, "rejected");
        assert_eq!(
            by_underlying["NABZY"].diagnostic_reason,
            "non_common_symbol"
        );
    }

    #[test]
    fn scheduled_event_custom_data_round_trips_through_catalog() {
        ensure_scheduled_event_custom_data_registered();
        let temp_dir = TempDir::new().unwrap();
        let mut catalog = ParquetDataCatalog::new(temp_dir.path(), None, None, None, None);

        assert_custom_roundtrip(
            &mut catalog,
            ScheduledEventObservation::TYPE_NAME,
            ScheduledEventObservation::catalog_identifier("earnings_report", "alpha_vantage"),
            vec![sample_observation(1), sample_observation(2)],
        );
        assert_custom_roundtrip(
            &mut catalog,
            ScheduledEventDecision::TYPE_NAME,
            ScheduledEventDecision::catalog_identifier("earnings_report"),
            vec![sample_decision(3), sample_decision(4)],
        );
        assert_custom_roundtrip(
            &mut catalog,
            ApprovedScheduledEvent::TYPE_NAME,
            ApprovedScheduledEvent::catalog_identifier("earnings_report"),
            vec![sample_approved_event(5), sample_approved_event(6)],
        );
    }

    fn assert_custom_roundtrip<T>(
        catalog: &mut ParquetDataCatalog,
        type_name: &str,
        identifier: String,
        original: Vec<T>,
    ) where
        T: Clone + CustomDataTrait + std::fmt::Debug + PartialEq + 'static,
    {
        let data_type = DataType::new(type_name, None, Some(identifier.clone()));
        let custom_data: Vec<CustomData> = original
            .iter()
            .cloned()
            .map(|record| {
                let data: Arc<dyn CustomDataTrait> = Arc::new(record);
                CustomData::new(data, data_type.clone())
            })
            .collect();

        catalog
            .write_custom_data_batch(custom_data, None, None, Some(false))
            .unwrap();

        let identifiers = vec![identifier.clone()];
        let loaded = catalog
            .query_custom_data_dynamic(type_name, Some(&identifiers), None, None, None, None, true)
            .unwrap();

        assert_eq!(loaded.len(), original.len());

        for (expected, actual) in original.iter().zip(loaded.iter()) {
            let Data::Custom(custom) = actual else {
                panic!("expected Data::Custom");
            };

            assert_eq!(custom.data_type.type_name(), type_name);
            assert_eq!(custom.data_type.identifier(), Some(identifier.as_str()));

            let actual = custom
                .data
                .as_any()
                .downcast_ref::<T>()
                .expect("expected custom data type");
            assert_eq!(expected, actual);
        }
    }

    fn sample_observation(ts: u64) -> ScheduledEventObservation {
        ScheduledEventObservation {
            observation_id: format!("alpha_vantage:AAPL:2026-07-{ts:02}"),
            event_type: "earnings_report".to_string(),
            source: "alpha_vantage".to_string(),
            source_event_id: format!("AAPL:2026-07-{ts:02}"),
            underlying: "AAPL".to_string(),
            event_date: format!("2026-07-{ts:02}"),
            timing: "after_close".to_string(),
            timezone: "America/New_York".to_string(),
            source_published_at_utc: String::new(),
            source_fetched_at_utc: "2026-07-01T12:00:00Z".to_string(),
            raw_uri: "file:///tmp/raw/alpha_vantage.json".to_string(),
            raw_sha256: "abc123".to_string(),
            quality_flags: "source_timing_present".to_string(),
            ts_event: UnixNanos::from(ts),
            ts_init: UnixNanos::from(ts),
        }
    }

    fn observation(
        underlying: &str,
        event_date: &str,
        timing: &str,
        source: &str,
        ts: u64,
    ) -> ScheduledEventObservation {
        ScheduledEventObservation {
            observation_id: format!("{source}:{underlying}:{event_date}:{timing}"),
            event_type: "earnings_report".to_string(),
            source: source.to_string(),
            source_event_id: format!("{underlying}:{event_date}:{timing}"),
            underlying: underlying.to_string(),
            event_date: event_date.to_string(),
            timing: timing.to_string(),
            timezone: "America/New_York".to_string(),
            source_published_at_utc: String::new(),
            source_fetched_at_utc: "2026-05-01T12:00:00Z".to_string(),
            raw_uri: "file:///tmp/raw.csv".to_string(),
            raw_sha256: "abc123".to_string(),
            quality_flags: String::new(),
            ts_event: unix_nanos_from_date_string(event_date),
            ts_init: UnixNanos::from(ts),
        }
    }

    fn fixed_utc() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap()
    }

    fn sample_decision(ts: u64) -> ScheduledEventDecision {
        ScheduledEventDecision {
            canonical_event_id: format!("earnings_report:AAPL:2026-07-{ts:02}"),
            event_type: "earnings_report".to_string(),
            underlying: "AAPL".to_string(),
            event_date: format!("2026-07-{ts:02}"),
            timing: "after_close".to_string(),
            status: "confirmed".to_string(),
            confidence: 0.95,
            sources_used: "alpha_vantage".to_string(),
            conflict_reason: String::new(),
            resolver_version: "scheduled-event-resolver:v1".to_string(),
            decided_at_utc: "2026-07-01T12:05:00Z".to_string(),
            valid_from_utc: "2026-07-01T12:05:00Z".to_string(),
            valid_until_utc: "2026-07-08T00:00:00Z".to_string(),
            ts_event: UnixNanos::from(ts),
            ts_init: UnixNanos::from(ts),
        }
    }

    fn sample_approved_event(ts: u64) -> ApprovedScheduledEvent {
        ApprovedScheduledEvent {
            canonical_event_id: format!("earnings_report:AAPL:2026-07-{ts:02}"),
            event_type: "earnings_report".to_string(),
            underlying: "AAPL".to_string(),
            event_date: format!("2026-07-{ts:02}"),
            timing: "after_close".to_string(),
            approval_status: "approved".to_string(),
            source_set: "alpha_vantage".to_string(),
            policy_version: "scheduled-event-approval:v1".to_string(),
            block_days_before: 1,
            block_days_after: 1,
            approved_at_utc: "2026-07-01T12:10:00Z".to_string(),
            valid_from_utc: "2026-07-01T12:10:00Z".to_string(),
            valid_until_utc: "2026-07-08T00:00:00Z".to_string(),
            diagnostic_reason: "source timing confirmed".to_string(),
            ts_event: UnixNanos::from(ts),
            ts_init: UnixNanos::from(ts),
        }
    }
}
