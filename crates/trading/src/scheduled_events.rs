//! Source-neutral scheduled-event custom data contracts.
//!
//! These types model non-price event inputs such as earnings dates. Source adapters write
//! observations, resolvers write canonical decisions, and approval policy writes the runtime-safe
//! approved view. The trading runtime should consume approved records instead of provider payloads.

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Once},
};

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
