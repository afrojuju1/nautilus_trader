//! Assignment, exercise, and expiration risk helpers for Alpaca options runtimes.

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Value, json};

use crate::{
    http::models::{AlpacaActivity, ListActivitiesRequest},
    options_entry::SelectedOptionsEntry,
    options_entry_admission::SubmissionBlock,
    runtime::emit_operator_event,
};

/// Stable Alpaca option lifecycle event kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionLifecycleEventKind {
    /// Option assignment activity.
    Assignment,
    /// Option exercise activity.
    Exercise,
    /// Option expiration activity.
    Expiration,
}

impl OptionLifecycleEventKind {
    /// Returns the Alpaca account-activity type.
    #[must_use]
    pub const fn activity_type(self) -> &'static str {
        match self {
            Self::Assignment => "OPASN",
            Self::Exercise => "OPEXC",
            Self::Expiration => "OPEXP",
        }
    }

    /// Returns the stable operator label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Assignment => "assignment",
            Self::Exercise => "exercise",
            Self::Expiration => "expiration",
        }
    }

    fn from_activity_type(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "OPASN" => Some(Self::Assignment),
            "OPEXC" => Some(Self::Exercise),
            "OPEXP" => Some(Self::Expiration),
            _ => None,
        }
    }

    const fn blocks_new_entries(self) -> bool {
        matches!(self, Self::Assignment | Self::Exercise)
    }
}

/// Runtime lifecycle risk configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptionLifecycleRiskConfig {
    /// Poll interval for account lifecycle activities. Zero disables background polling.
    pub poll_secs: u64,
    /// Account-activity lookback used by startup and daemon polls.
    pub activity_lookback_hours: u64,
    /// How long assignment/exercise activities block new entries.
    pub activity_block_hours: u64,
    /// Calendar DTE threshold that blocks new entries. Negative disables the block.
    pub expiration_entry_block_days: i64,
}

impl Default for OptionLifecycleRiskConfig {
    fn default() -> Self {
        Self {
            poll_secs: 300,
            activity_lookback_hours: 72,
            activity_block_hours: 24,
            expiration_entry_block_days: 0,
        }
    }
}

/// Normalized lifecycle activity evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptionLifecycleEvent {
    /// Stable event kind.
    pub kind: OptionLifecycleEventKind,
    /// Alpaca activity ID.
    pub id: Option<String>,
    /// Alpaca option symbol.
    pub symbol: Option<String>,
    /// Activity timestamp when available.
    pub occurred_at_utc: Option<DateTime<Utc>>,
    /// Raw activity date for date-only activities.
    pub activity_date: Option<String>,
    /// Activity quantity.
    pub quantity: Option<String>,
}

impl OptionLifecycleEvent {
    /// Returns stable JSON evidence.
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "activity_type": self.kind.activity_type(),
            "id": self.id,
            "symbol": self.symbol,
            "occurred_at_utc": self.occurred_at_utc.map(|value| value.to_rfc3339()),
            "activity_date": self.activity_date,
            "quantity": self.quantity,
        })
    }
}

/// Shared lifecycle risk state read by synchronous strategy callbacks.
#[derive(Clone, Debug)]
pub struct OptionLifecycleRiskHandle {
    config: OptionLifecycleRiskConfig,
    state: Arc<RwLock<OptionLifecycleRiskState>>,
}

impl OptionLifecycleRiskHandle {
    /// Creates a new lifecycle risk handle.
    #[must_use]
    pub fn new(config: OptionLifecycleRiskConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(OptionLifecycleRiskState::default())),
        }
    }

    /// Replaces the current lifecycle event snapshot.
    pub fn update_events(&self, events: Vec<OptionLifecycleEvent>, now: DateTime<Utc>) {
        let blocks = account_activity_blocks(&self.config, &events, now);
        if let Ok(mut state) = self.state.write() {
            state.events = events;
            state.account_blocks = blocks;
            state.last_poll_at_utc = Some(now);
            state.last_poll_error = None;
        }
    }

    /// Records a poll failure and blocks new entries until a successful poll refreshes state.
    pub fn record_poll_error(&self, error: String, now: DateTime<Utc>) {
        if let Ok(mut state) = self.state.write() {
            state.account_blocks = vec![format!("lifecycle_activity_poll_failed: {error}")];
            state.last_poll_at_utc = Some(now);
            state.last_poll_error = Some(error);
        }
    }

    /// Returns a lifecycle submission block for a selected entry, if one applies.
    #[must_use]
    pub fn submission_block(
        &self,
        selected: &SelectedOptionsEntry,
        now: DateTime<Utc>,
    ) -> Option<SubmissionBlock> {
        if let Some(block) = expiration_entry_block(&self.config, selected, now) {
            return Some(block);
        }

        let state = self.state.read().ok()?;
        (!state.account_blocks.is_empty()).then(|| SubmissionBlock {
            reason: "account_lifecycle_event".to_string(),
            current: None,
            limit: None,
            details: state.account_blocks.clone(),
        })
    }

    /// Returns the current lifecycle state snapshot.
    #[must_use]
    pub fn snapshot(&self) -> OptionLifecycleRiskState {
        self.state
            .read()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

/// Current lifecycle risk state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OptionLifecycleRiskState {
    /// Most recent normalized lifecycle events.
    pub events: Vec<OptionLifecycleEvent>,
    /// Account-level lifecycle reasons blocking new entries.
    pub account_blocks: Vec<String>,
    /// Last poll timestamp.
    pub last_poll_at_utc: Option<DateTime<Utc>>,
    /// Last poll error, if any.
    pub last_poll_error: Option<String>,
}

/// Builds an account-activity request for option lifecycle events after `after`.
#[must_use]
pub fn option_lifecycle_activity_request(after: DateTime<Utc>) -> ListActivitiesRequest {
    let mut request = ListActivitiesRequest::default();
    request.activity_types = vec![
        OptionLifecycleEventKind::Assignment
            .activity_type()
            .to_string(),
        OptionLifecycleEventKind::Exercise
            .activity_type()
            .to_string(),
        OptionLifecycleEventKind::Expiration
            .activity_type()
            .to_string(),
    ];
    request.direction = Some("asc".to_string());
    request.after = Some(after.to_rfc3339());
    request
}

/// Converts Alpaca account activities into normalized option lifecycle events.
#[must_use]
pub fn lifecycle_events_from_activities(
    activities: &[AlpacaActivity],
) -> Vec<OptionLifecycleEvent> {
    activities
        .iter()
        .filter_map(lifecycle_event_from_activity)
        .collect()
}

/// Emits operator evidence for one lifecycle poll.
pub fn emit_lifecycle_poll(events: &[OptionLifecycleEvent], blocks: &[String]) {
    emit_operator_event(
        "option_lifecycle_poll",
        json!({
            "events": events.iter().map(OptionLifecycleEvent::to_value).collect::<Vec<_>>(),
            "event_count": events.len(),
            "blocks": blocks,
        }),
    );
}

/// Emits operator evidence for one lifecycle poll failure.
pub fn emit_lifecycle_poll_error(error: &str) {
    emit_operator_event(
        "option_lifecycle_poll_error",
        json!({
            "reason": "activity_poll_failed",
            "error": error,
        }),
    );
}

fn lifecycle_event_from_activity(activity: &AlpacaActivity) -> Option<OptionLifecycleEvent> {
    let kind = OptionLifecycleEventKind::from_activity_type(activity.activity_type.as_deref()?)?;
    Some(OptionLifecycleEvent {
        kind,
        id: activity.id.clone(),
        symbol: activity.symbol.clone(),
        occurred_at_utc: activity
            .transaction_time
            .as_deref()
            .and_then(parse_activity_timestamp)
            .or_else(|| activity.date.as_deref().and_then(parse_activity_date)),
        activity_date: activity.date.clone(),
        quantity: activity.qty.clone(),
    })
}

fn account_activity_blocks(
    config: &OptionLifecycleRiskConfig,
    events: &[OptionLifecycleEvent],
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut blocks = events
        .iter()
        .filter(|event| event.kind.blocks_new_entries())
        .filter(|event| {
            event.occurred_at_utc.is_none_or(|occurred| {
                event_age_hours(occurred, now) <= config.activity_block_hours
            })
        })
        .map(|event| {
            format!(
                "{} activity id={} symbol={} age_hours={}",
                event.kind.as_str(),
                event.id.as_deref().unwrap_or("unknown"),
                event.symbol.as_deref().unwrap_or("unknown"),
                event.occurred_at_utc.map_or_else(
                    || "unknown".to_string(),
                    |occurred| { event_age_hours(occurred, now).to_string() }
                )
            )
        })
        .collect::<Vec<_>>();
    blocks.sort();
    blocks.dedup();
    blocks
}

fn expiration_entry_block(
    config: &OptionLifecycleRiskConfig,
    selected: &SelectedOptionsEntry,
    now: DateTime<Utc>,
) -> Option<SubmissionBlock> {
    if config.expiration_entry_block_days < 0 {
        return None;
    }
    let today = now.date_naive();
    selected.option_symbols().into_iter().find_map(|symbol| {
        let days = days_to_expiration_at(symbol, today)?;
        (days <= config.expiration_entry_block_days).then(|| SubmissionBlock {
            reason: "expiration_entry_block".to_string(),
            current: None,
            limit: None,
            details: vec![
                format!("symbol={symbol}"),
                format!("days_to_expiration={days}"),
                format!("limit_days={}", config.expiration_entry_block_days),
            ],
        })
    })
}

fn days_to_expiration_at(symbol: &str, today: NaiveDate) -> Option<i64> {
    let chars = symbol.as_bytes();
    for index in 0..chars.len().saturating_sub(6) {
        let date_slice = &chars[index..index + 6];
        let put_call = chars.get(index + 6).copied();
        if date_slice.iter().all(u8::is_ascii_digit) && matches!(put_call, Some(b'P' | b'C')) {
            let value = std::str::from_utf8(date_slice).ok()?;
            let year = 2000 + value[0..2].parse::<i32>().ok()?;
            let month = value[2..4].parse::<u32>().ok()?;
            let day = value[4..6].parse::<u32>().ok()?;
            let expiration = NaiveDate::from_ymd_opt(year, month, day)?;
            return Some(expiration.signed_duration_since(today).num_days());
        }
    }
    None
}

fn parse_activity_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn parse_activity_date(value: &str) -> Option<DateTime<Utc>> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|timestamp| timestamp.and_utc())
}

fn event_age_hours(occurred: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    now.signed_duration_since(occurred)
        .max(Duration::zero())
        .num_hours()
        .max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::models::AlpacaActivity;

    #[test]
    fn lifecycle_events_keep_assignment_exercise_and_expiration() {
        let activities = vec![
            activity("OPASN", "assign-1", "SPY260626P00500000", "2026-06-26"),
            activity("OPEXC", "exercise-1", "SPY260626C00500000", "2026-06-26"),
            activity("OPEXP", "expire-1", "SPY260626P00400000", "2026-06-26"),
            activity("FILL", "fill-1", "SPY260626P00400000", "2026-06-26"),
        ];

        let events = lifecycle_events_from_activities(&activities);

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, OptionLifecycleEventKind::Assignment);
        assert_eq!(events[1].kind, OptionLifecycleEventKind::Exercise);
        assert_eq!(events[2].kind, OptionLifecycleEventKind::Expiration);
    }

    #[test]
    fn lifecycle_handle_blocks_on_recent_assignment() {
        let handle = OptionLifecycleRiskHandle::new(OptionLifecycleRiskConfig::default());
        let now = DateTime::parse_from_rfc3339("2026-06-27T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        handle.update_events(
            vec![OptionLifecycleEvent {
                kind: OptionLifecycleEventKind::Assignment,
                id: Some("assign-1".to_string()),
                symbol: Some("SPY260626P00500000".to_string()),
                occurred_at_utc: Some(now - Duration::hours(2)),
                activity_date: None,
                quantity: Some("1".to_string()),
            }],
            now,
        );

        let state = handle.snapshot();

        assert_eq!(state.account_blocks.len(), 1);
        assert!(state.account_blocks[0].contains("assignment activity"));
    }

    fn activity(activity_type: &str, id: &str, symbol: &str, date: &str) -> AlpacaActivity {
        AlpacaActivity {
            activity_type: Some(activity_type.to_string()),
            id: Some(id.to_string()),
            cum_qty: None,
            leaves_qty: None,
            price: None,
            qty: Some("1".to_string()),
            side: None,
            symbol: Some(symbol.to_string()),
            transaction_time: None,
            order_id: None,
            activity_subtype: None,
            date: Some(date.to_string()),
            net_amount: None,
            cusip: None,
            per_share_amount: None,
        }
    }
}
