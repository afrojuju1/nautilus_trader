//! Nautilus-native entry strategy for Alpaca option candidates.

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    str::FromStr,
    sync::Arc,
};

use chrono::{DateTime, NaiveDate, Utc};
use nautilus_common::{
    actor::DataActor, cache::CacheApi, factories::OrderFactory, timer::TimeEvent,
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    data::{CustomData, CustomDataTrait, DataType, HasTsInit},
    enums::{OrderSide, OrderStatus, TimeInForce},
    events::{OrderAccepted, OrderCanceled, OrderDenied, OrderFilled, OrderRejected},
    identifiers::{ClientId, ClientOrderId, InstrumentId, OrderListId},
    instruments::Instrument,
    orders::{Order, OrderAny},
    types::{Price, Quantity},
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{OrderApi, Strategy, StrategyConfig, StrategyCore},
};
use uuid::Uuid;

use crate::{
    candidate_ledger_persistence::CandidateLedgerPersistenceHandle,
    candidate_payloads::{
        candidate_alert_key, insert_string_field, insert_value_field, selected_entry_alert_payload,
    },
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    options_entry_admission::{
        EntryAdmissionConfig, EntryAdmissionSnapshot, EntryGateDecision, SubmissionBlock,
        UNCOVERED_OPTION_PERMISSION_REJECTION_REASON, entry_gate_decision,
        is_uncovered_option_permission_rejection, selected_submit_enabled,
        submission_block_for_selected,
    },
    options_lifecycle::OptionLifecycleRiskHandle,
    options_management::{
        AlpacaOptionsManagementConfig, CloseQuote, close_attempts_exhausted,
        close_quote_from_ticks, close_reason, close_reprice_cooldown_remaining_secs,
        emit_management_snapshot, management_instrument_ids,
    },
    options_runtime::{
        AlpacaOptionsRuntimeConfig, OptionsCandidateSet, OptionsScanOutcome, OptionsScanReport,
        SelectedOptionsEntry,
    },
    runtime::{StrategyState, StrategyStateEntry, StrategyStateEntryDraft, emit_operator_event},
    state_persistence::StrategyStatePersistenceHandle,
    storage::StrategyStateMutation,
};
use serde_json::{Value, json};

const SELECTED_CANDIDATE_ALERT: &str = "selected_candidate";
const CANDIDATE_SUBMIT_REJECTED_ALERT: &str = "candidate_submit_rejected";
const MANAGEMENT_TIMER: &str = "alpaca_options_management";

/// Custom data type published by option-chain scanner actors for entry strategies.
#[derive(Clone, Debug)]
pub struct OptionsCandidateData {
    /// Ranked option candidates discovered by the scanner.
    pub candidates: OptionsCandidateSet,
    ts_event: UnixNanos,
    ts_init: UnixNanos,
}

impl OptionsCandidateData {
    const TYPE_NAME: &'static str = "AlpacaOptionsCandidateData";

    /// Creates a new custom data payload from an candidate set.
    #[must_use]
    pub const fn new(
        candidates: OptionsCandidateSet,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Self {
        Self {
            candidates,
            ts_event,
            ts_init,
        }
    }

    /// Returns the Nautilus custom data type used for routing candidate payloads.
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

impl HasTsInit for OptionsCandidateData {
    fn ts_init(&self) -> UnixNanos {
        self.ts_init
    }
}

impl CustomDataTrait for OptionsCandidateData {
    fn type_name(&self) -> &'static str {
        Self::TYPE_NAME
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn ts_event(&self) -> UnixNanos {
        self.ts_event
    }

    fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(&serde_json::json!({
            "trade_date": self.candidates.trade_date,
            "ts_event": self.ts_event.as_u64(),
            "ts_init": self.ts_init.as_u64(),
            "scans": self
                .candidates
                .scans
                .iter()
                .map(scan_report_payload)
                .collect::<Vec<_>>(),
            "ranked_entries": self.candidates.ranked_entries().len(),
            "selected": self
                .candidates
                .selected_entry()
                .map(selected_entry_payload),
        }))?)
    }

    fn clone_arc(&self) -> Arc<dyn CustomDataTrait> {
        Arc::new(self.clone())
    }

    fn eq_arc(&self, other: &dyn CustomDataTrait) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            self.ts_event == other.ts_event
                && self.ts_init == other.ts_init
                && self.candidates.trade_date == other.candidates.trade_date
                && self.candidates.ranked_entries().len() == other.candidates.ranked_entries().len()
        })
    }

    fn type_name_static() -> &'static str
    where
        Self: Sized,
    {
        Self::TYPE_NAME
    }
}

/// Configuration for [`AlpacaOptionsStrategy`].
#[derive(Clone, Debug)]
pub struct AlpacaOptionsStrategyConfig {
    /// Nautilus base strategy configuration.
    pub base: StrategyConfig,
    /// Contract quantity per leg.
    pub quantity: u64,
    /// Execution client ID to route orders to.
    pub client_id: Option<ClientId>,
    /// Entry-admission gates owned by the strategy path.
    pub admission: EntryAdmissionConfig,
    /// Initial persisted strategy state loaded before the node starts.
    pub initial_state: StrategyState,
    /// Async state persistence boundary used when live submit is enabled.
    pub state_persistence: Option<StrategyStatePersistenceHandle>,
    /// Async candidate-ledger persistence boundary used for scanner and strategy evidence.
    pub candidate_ledger_persistence: Option<CandidateLedgerPersistenceHandle>,
    /// Shared lifecycle-risk state from the account-activity poller.
    pub lifecycle_risk: Option<OptionLifecycleRiskHandle>,
    /// Management settings owned by the live strategy runtime.
    pub management: AlpacaOptionsManagementConfig,
}

impl AlpacaOptionsStrategyConfig {
    /// Builds a config from a base strategy config and contract quantity.
    #[must_use]
    pub fn new(base: StrategyConfig, quantity: u64) -> Self {
        Self {
            base,
            quantity,
            client_id: Some(ClientId::from(ALPACA_CLIENT_ID)),
            admission: EntryAdmissionConfig::default(),
            initial_state: StrategyState::default(),
            state_persistence: None,
            candidate_ledger_persistence: None,
            lifecycle_risk: None,
            management: AlpacaOptionsManagementConfig::default(),
        }
    }

    /// Builds a strategy config from the Alpaca options runtime config.
    #[must_use]
    pub fn from_runtime_config(base: StrategyConfig, engine: &AlpacaOptionsRuntimeConfig) -> Self {
        Self {
            base,
            quantity: engine.quantity,
            client_id: Some(ClientId::from(ALPACA_CLIENT_ID)),
            admission: EntryAdmissionConfig::from_runtime_config(engine),
            initial_state: StrategyState::default(),
            state_persistence: None,
            candidate_ledger_persistence: None,
            lifecycle_risk: None,
            management: AlpacaOptionsManagementConfig::from_runtime_config(engine),
        }
    }
}

/// Submission result for one selected options entry.
#[derive(Clone, Debug)]
pub struct AlpacaOptionsSubmission {
    /// Submitted candidate.
    pub entry: SelectedOptionsEntry,
    /// Parent order-list ID, or the single client-order ID for one-leg entries.
    pub order_list_id: String,
    /// Number of orders sent through the Nautilus strategy API.
    pub order_count: usize,
}

#[derive(Clone, Debug)]
struct PendingEntrySubmission {
    entry: SelectedOptionsEntry,
    trade_date: String,
    order_list_id: String,
    submitted_at_utc: String,
    quantity: u64,
    order_count: usize,
    accepted: usize,
    rejected: usize,
    recorded: bool,
    rejection_reasons: Vec<String>,
}

#[derive(Clone, Debug)]
struct PendingCloseSubmission {
    entry_order_list_id: String,
    close_order_list_id: String,
    close_reason: String,
    order_count: usize,
    accepted: usize,
    rejected: usize,
    recorded: bool,
}

/// Nautilus strategy responsible for converting selected option candidates into orders.
#[derive(Debug)]
pub struct AlpacaOptionsStrategy {
    core: StrategyCore,
    config: AlpacaOptionsStrategyConfig,
    state: StrategyState,
    pending_submissions: BTreeMap<String, PendingEntrySubmission>,
    pending_client_order_ids: BTreeMap<String, String>,
    pending_close_submissions: BTreeMap<String, PendingCloseSubmission>,
    pending_close_client_order_ids: BTreeMap<String, String>,
    submitted_underlying_keys: BTreeSet<String>,
    recorded_candidate_alert_keys: BTreeSet<String>,
    management_quote_subscriptions: BTreeSet<InstrumentId>,
}

impl AlpacaOptionsStrategy {
    /// Creates a new [`AlpacaOptionsStrategy`].
    #[must_use]
    pub fn new(config: AlpacaOptionsStrategyConfig) -> Self {
        Self {
            core: StrategyCore::new(config.base.clone()),
            state: config.initial_state.clone(),
            config,
            pending_submissions: BTreeMap::new(),
            pending_client_order_ids: BTreeMap::new(),
            pending_close_submissions: BTreeMap::new(),
            pending_close_client_order_ids: BTreeMap::new(),
            submitted_underlying_keys: BTreeSet::new(),
            recorded_candidate_alert_keys: BTreeSet::new(),
            management_quote_subscriptions: BTreeSet::new(),
        }
    }

    /// Handles candidate data received from scanner actors.
    ///
    /// # Errors
    ///
    /// Returns an error if order construction or Nautilus strategy submission fails.
    ///
    /// # Panics
    ///
    /// Panics if submission is enabled and the strategy has not been registered with a Nautilus
    /// runtime.
    pub fn submit_candidate_data(
        &mut self,
        data: &OptionsCandidateData,
    ) -> anyhow::Result<Option<AlpacaOptionsSubmission>> {
        let Some(entry) = data.candidates.selected_entry().cloned() else {
            return Ok(None);
        };

        match entry_gate_decision(&self.config.admission, Utc::now()) {
            EntryGateDecision::Continue => {}
            EntryGateDecision::KillSwitch => {
                log::info!(
                    "Skipping Alpaca options entry: trade_date={} reason=kill_switch_enabled underlying={} strategy={}",
                    data.candidates.trade_date,
                    entry.underlying(),
                    entry.strategy_name()
                );
                emit_operator_event(
                    "entry_decision",
                    json!({
                        "action": "skipped",
                        "reason": "kill_switch_enabled",
                        "trade_date": data.candidates.trade_date,
                        "underlying": entry.underlying(),
                        "strategy": entry.strategy_name(),
                    }),
                );
                self.record_selected_candidate_alert(
                    &data.candidates.trade_date,
                    &entry,
                    "skipped",
                    None,
                    Some("kill_switch_enabled"),
                    None,
                    None,
                    &[],
                );
                return Ok(None);
            }
            EntryGateDecision::OutsideEntryWindow => {
                log::info!(
                    "Skipping Alpaca options entry: trade_date={} reason=outside_entry_window underlying={} strategy={}",
                    data.candidates.trade_date,
                    entry.underlying(),
                    entry.strategy_name()
                );
                emit_operator_event(
                    "entry_decision",
                    json!({
                        "action": "skipped",
                        "reason": "outside_entry_window",
                        "trade_date": data.candidates.trade_date,
                        "underlying": entry.underlying(),
                        "strategy": entry.strategy_name(),
                    }),
                );
                self.record_selected_candidate_alert(
                    &data.candidates.trade_date,
                    &entry,
                    "skipped",
                    None,
                    Some("outside_entry_window"),
                    None,
                    None,
                    &[],
                );
                return Ok(None);
            }
        }

        if !selected_submit_enabled(&self.config.admission, &entry) {
            log::info!(
                "Dry-run Alpaca options entry: underlying={} strategy={} symbols={} score={:.1}",
                entry.underlying(),
                entry.strategy_name(),
                entry.option_symbols().join(","),
                entry.score()
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "dry_run",
                    "reason": "submission_disabled",
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry.underlying(),
                    "strategy": entry.strategy_name(),
                    "symbols": entry.option_symbols(),
                    "score": entry.score(),
                }),
            );
            self.record_selected_candidate_alert(
                &data.candidates.trade_date,
                &entry,
                "dry_run",
                None,
                Some("submission_disabled"),
                None,
                None,
                &[],
            );
            return Ok(None);
        }

        if let Some(block) = self.lifecycle_submission_block(&entry) {
            self.log_entry_block(&data.candidates.trade_date, &entry, &block);
            return Ok(None);
        }

        if self.config.admission.submit_enabled && !self.candidate_ledger_persistence_ready() {
            log::error!(
                "Skipping Alpaca options entry: trade_date={} reason=candidate_ledger_unhealthy underlying={} strategy={} symbols={}",
                data.candidates.trade_date,
                entry.underlying(),
                entry.strategy_name(),
                entry.option_symbols().join(",")
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "skipped",
                    "reason": "candidate_ledger_unhealthy",
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry.underlying(),
                    "strategy": entry.strategy_name(),
                    "symbols": entry.option_symbols(),
                }),
            );
            return Ok(None);
        }

        if self.config.admission.submit_enabled && !self.state_persistence_ready() {
            log::error!(
                "Skipping Alpaca options entry: trade_date={} reason=state_persistence_unhealthy underlying={} strategy={} symbols={}",
                data.candidates.trade_date,
                entry.underlying(),
                entry.strategy_name(),
                entry.option_symbols().join(",")
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "skipped",
                    "reason": "state_persistence_unhealthy",
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry.underlying(),
                    "strategy": entry.strategy_name(),
                    "symbols": entry.option_symbols(),
                }),
            );
            self.record_selected_candidate_alert(
                &data.candidates.trade_date,
                &entry,
                "skipped",
                None,
                Some("state_persistence_unhealthy"),
                None,
                None,
                &[],
            );
            return Ok(None);
        }

        let snapshot = self.entry_admission_snapshot(&entry)?;
        if let Some(block) = submission_block_for_selected(
            &self.config.admission,
            &self.state,
            &entry,
            &data.candidates.trade_date,
            &snapshot,
        ) {
            self.log_entry_block(&data.candidates.trade_date, &entry, &block);
            return Ok(None);
        }

        let underlying_key = submitted_underlying_key(&data.candidates.trade_date, &entry);
        if !self
            .submitted_underlying_keys
            .insert(underlying_key.clone())
        {
            log::info!(
                "Skipping duplicate Alpaca options entry: key={} strategy={} symbols={}",
                underlying_key,
                entry.strategy_name(),
                entry.option_symbols().join(",")
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "skipped",
                    "reason": "duplicate_pending_submission",
                    "key": underlying_key,
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry.underlying(),
                    "strategy": entry.strategy_name(),
                    "symbols": entry.option_symbols(),
                }),
            );
            self.record_selected_candidate_alert(
                &data.candidates.trade_date,
                &entry,
                "skipped",
                None,
                Some("duplicate_pending_submission"),
                None,
                None,
                &[],
            );
            return Ok(None);
        }

        let order_list_id = entry_order_list_id(&data.candidates.trade_date, entry.underlying());
        match self.submit_selected_entry_with_trade_date(
            entry,
            &data.candidates.trade_date,
            &order_list_id,
        ) {
            Ok(submission) => Ok(Some(submission)),
            Err(error) => {
                self.submitted_underlying_keys.remove(&underlying_key);
                self.remove_pending_submission(&order_list_id);
                Err(error)
            }
        }
    }

    /// Submits the highest-ranked candidate, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if order construction or Nautilus strategy submission fails.
    ///
    /// # Panics
    ///
    /// Panics if the strategy has not been registered with a Nautilus runtime.
    pub fn submit_candidates(
        &mut self,
        candidates: OptionsCandidateSet,
        order_list_id: &str,
    ) -> anyhow::Result<Option<AlpacaOptionsSubmission>> {
        let Some(entry) = candidates.into_selected_entry() else {
            return Ok(None);
        };
        self.submit_selected_entry(entry, order_list_id).map(Some)
    }

    /// Submits one selected entry through the standard Nautilus strategy order flow.
    ///
    /// # Errors
    ///
    /// Returns an error if order construction or Nautilus strategy submission fails.
    ///
    /// # Panics
    ///
    /// Panics if the strategy has not been registered with a Nautilus runtime.
    pub fn submit_selected_entry(
        &mut self,
        entry: SelectedOptionsEntry,
        order_list_id: &str,
    ) -> anyhow::Result<AlpacaOptionsSubmission> {
        let orders = self.build_entry_orders(&entry, order_list_id)?;
        let order_count = orders.len();
        self.submit_entry_orders(orders, order_list_id)?;

        Ok(AlpacaOptionsSubmission {
            entry,
            order_list_id: order_list_id.to_string(),
            order_count,
        })
    }

    fn submit_selected_entry_with_trade_date(
        &mut self,
        entry: SelectedOptionsEntry,
        trade_date: &str,
        order_list_id: &str,
    ) -> anyhow::Result<AlpacaOptionsSubmission> {
        let orders = self.build_entry_orders(&entry, order_list_id)?;
        let client_order_ids = orders
            .iter()
            .map(|order| order.client_order_id().to_string())
            .collect::<Vec<_>>();
        let order_count = orders.len();
        self.record_pending_submission(
            entry.clone(),
            trade_date.to_string(),
            order_list_id.to_string(),
            order_count,
            client_order_ids,
        );
        self.record_selected_candidate_alert(
            trade_date,
            &entry,
            "submitted",
            Some(order_list_id),
            None,
            None,
            None,
            &[],
        );

        if let Err(error) = self.submit_entry_orders(orders, order_list_id) {
            self.remove_pending_submission(order_list_id);
            return Err(error);
        }

        Ok(AlpacaOptionsSubmission {
            entry,
            order_list_id: order_list_id.to_string(),
            order_count,
        })
    }

    fn build_entry_orders(
        &mut self,
        entry: &SelectedOptionsEntry,
        order_list_id: &str,
    ) -> anyhow::Result<Vec<OrderAny>> {
        let quantity = self.config.quantity;
        let mut order_api = self.order();
        build_selected_entry_orders(&mut order_api, entry, order_list_id, quantity)
    }

    fn submit_entry_orders(
        &mut self,
        mut orders: Vec<OrderAny>,
        order_list_id: &str,
    ) -> anyhow::Result<()> {
        if orders.len() == 1 {
            self.submit_order(orders.remove(0), None, self.config.client_id, None)?;
        } else {
            apply_order_list_id(&mut orders, order_list_id);
            self.submit_order_list(orders, None, self.config.client_id, None)?;
        }
        Ok(())
    }

    fn manage_active_entries(&mut self) -> anyhow::Result<()> {
        self.refresh_management_quote_subscriptions();
        let active_order_list_ids = self
            .state
            .entries
            .iter()
            .filter(|entry| entry.is_active())
            .map(|entry| entry.order_list_id.clone())
            .collect::<Vec<_>>();

        for order_list_id in active_order_list_ids {
            self.manage_active_entry(&order_list_id)?;
        }
        Ok(())
    }

    fn manage_active_entry(&mut self, order_list_id: &str) -> anyhow::Result<()> {
        let Some(entry) = self.state_entry(order_list_id).cloned() else {
            return Ok(());
        };
        if let Some(close_order_list_id) = entry.close_order_list_id.as_deref() {
            self.manage_existing_close_order(&entry, close_order_list_id)?;
            return Ok(());
        }

        match self.order_list_status(&entry.order_list_id) {
            OrderListRuntimeStatus::Filled => {}
            OrderListRuntimeStatus::Working => {
                self.manage_working_entry_order(&entry)?;
                return Ok(());
            }
            OrderListRuntimeStatus::TerminalWithoutFill => {
                self.mark_entry_canceled(&entry.order_list_id, "entry_terminal_without_fill")?;
                return Ok(());
            }
            OrderListRuntimeStatus::Missing => {
                emit_operator_event(
                    "management_block",
                    json!({
                        "action": "entry_status_unavailable",
                        "reason": "entry_order_missing_from_cache",
                        "underlying": entry.underlying,
                        "strategy": entry.strategy,
                        "order_list_id": entry.order_list_id,
                    }),
                );
                return Ok(());
            }
            OrderListRuntimeStatus::Partial => {
                emit_operator_event(
                    "management_block",
                    json!({
                        "action": "entry_status_unavailable",
                        "reason": "partial_entry_state",
                        "underlying": entry.underlying,
                        "strategy": entry.strategy,
                        "order_list_id": entry.order_list_id,
                    }),
                );
                return Ok(());
            }
        }

        let Some(close_quote) = self.close_quote_from_cache(&entry) else {
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "close_quote_unavailable",
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "short_symbol": entry.short_symbol,
                    "long_symbol": entry.long_symbol,
                }),
            );
            return Ok(());
        };

        let trigger = close_reason(&self.config.management, &entry, close_quote.debit);
        emit_management_snapshot(&entry, &close_quote, trigger.as_deref());
        let Some(trigger) = trigger else {
            return Ok(());
        };

        if !(self.config.management.manage_enabled && self.config.management.close_enabled) {
            let reason = if !self.config.management.manage_enabled {
                "management_disabled"
            } else {
                "close_disabled"
            };
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": reason,
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "trigger": trigger,
                    "manage_enabled": self.config.management.manage_enabled,
                    "close_enabled": self.config.management.close_enabled,
                }),
            );
            return Ok(());
        }

        if close_attempts_exhausted(&self.config.management, &entry) {
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "max_close_attempts",
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "trigger": trigger,
                    "attempts": entry.close_attempts,
                    "limit": self.config.management.max_close_attempts,
                }),
            );
            return Ok(());
        }

        if let Some(remaining_secs) =
            close_reprice_cooldown_remaining_secs(&self.config.management, &entry)
        {
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "close_reprice_cooldown",
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "trigger": trigger,
                    "remaining_secs": remaining_secs,
                }),
            );
            return Ok(());
        }

        if !self.config.management.inside_close_window(Utc::now()) {
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "outside_close_window",
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "trigger": trigger,
                    "window_start": self.config.management.close_start.to_string(),
                    "window_end": self.config.management.close_end.to_string(),
                    "timezone": self.config.management.entry_timezone.to_string(),
                }),
            );
            return Ok(());
        }

        let close_order_list_id = close_order_list_id(&entry);
        let submit_quote =
            close_quote.with_price_cushion(self.config.management.close_price_cushion);
        self.submit_close_entry(entry, submit_quote, trigger, close_order_list_id)?;
        Ok(())
    }

    fn manage_working_entry_order(&mut self, entry: &StrategyStateEntry) -> anyhow::Result<()> {
        if !self.config.management.manage_enabled || self.config.management.stale_entry_secs == 0 {
            return Ok(());
        }
        let Some(age) = age_secs_from_rfc3339(&entry.recorded_at_utc) else {
            return Ok(());
        };
        if age < self.config.management.stale_entry_secs {
            return Ok(());
        }

        emit_operator_event(
            "management_snapshot",
            json!({
                "action": "stale_entry_cancel",
                "underlying": entry.underlying,
                "strategy": entry.strategy,
                "order_list_id": entry.order_list_id,
                "age_secs": age,
                "limit_secs": self.config.management.stale_entry_secs,
            }),
        );
        self.cancel_order_list_orders(&entry.order_list_id)
    }

    fn manage_existing_close_order(
        &mut self,
        entry: &StrategyStateEntry,
        close_order_list_id: &str,
    ) -> anyhow::Result<()> {
        match self.order_list_status(close_order_list_id) {
            OrderListRuntimeStatus::Filled => {
                self.mark_entry_closed(&entry.order_list_id, close_order_list_id, None)?;
            }
            OrderListRuntimeStatus::Working => {
                self.manage_working_close_order(entry, close_order_list_id)?;
            }
            OrderListRuntimeStatus::TerminalWithoutFill => {
                self.clear_close_submission(&entry.order_list_id, "close_terminal_without_fill")?;
            }
            OrderListRuntimeStatus::Missing => {
                emit_operator_event(
                    "management_block",
                    json!({
                        "action": "close_status_unavailable",
                        "reason": "close_order_missing_from_cache",
                        "underlying": entry.underlying,
                        "strategy": entry.strategy,
                        "order_list_id": entry.order_list_id,
                        "close_order_list_id": close_order_list_id,
                    }),
                );
            }
            OrderListRuntimeStatus::Partial => {
                emit_operator_event(
                    "management_block",
                    json!({
                        "action": "close_status_unavailable",
                        "reason": "partial_close_state",
                        "underlying": entry.underlying,
                        "strategy": entry.strategy,
                        "order_list_id": entry.order_list_id,
                        "close_order_list_id": close_order_list_id,
                    }),
                );
            }
        }
        Ok(())
    }

    fn manage_working_close_order(
        &mut self,
        entry: &StrategyStateEntry,
        close_order_list_id: &str,
    ) -> anyhow::Result<()> {
        if !(self.config.management.manage_enabled && self.config.management.close_enabled)
            || self.config.management.stale_close_secs == 0
        {
            return Ok(());
        }
        let Some(last_submitted_at) = entry.last_close_submitted_at_utc.as_deref() else {
            return Ok(());
        };
        let Some(age) = age_secs_from_rfc3339(last_submitted_at) else {
            return Ok(());
        };
        if age < self.config.management.stale_close_secs {
            return Ok(());
        }

        emit_operator_event(
            "management_snapshot",
            json!({
                "action": "stale_close_cancel",
                "underlying": entry.underlying,
                "strategy": entry.strategy,
                "order_list_id": entry.order_list_id,
                "close_order_list_id": close_order_list_id,
                "age_secs": age,
                "limit_secs": self.config.management.stale_close_secs,
            }),
        );
        self.cancel_order_list_orders(close_order_list_id)
    }

    fn submit_close_entry(
        &mut self,
        entry: StrategyStateEntry,
        quote: CloseQuote,
        close_reason: String,
        close_order_list_id: String,
    ) -> anyhow::Result<()> {
        let orders = self.build_close_orders(&entry, &quote, &close_order_list_id)?;
        let client_order_ids = orders
            .iter()
            .map(|order| order.client_order_id().to_string())
            .collect::<Vec<_>>();
        let order_count = orders.len();
        self.record_pending_close_submission(
            entry.order_list_id.clone(),
            close_order_list_id.clone(),
            close_reason,
            order_count,
            client_order_ids,
        );

        if let Err(error) = self.submit_close_orders(orders, &close_order_list_id) {
            self.remove_pending_close_submission(&close_order_list_id);
            return Err(error);
        }
        Ok(())
    }

    fn build_close_orders(
        &mut self,
        entry: &StrategyStateEntry,
        quote: &CloseQuote,
        order_list_id: &str,
    ) -> anyhow::Result<Vec<OrderAny>> {
        let mut order_api = self.order();
        build_close_entry_orders(&mut order_api, entry, quote, order_list_id)
    }

    fn submit_close_orders(
        &mut self,
        mut orders: Vec<OrderAny>,
        order_list_id: &str,
    ) -> anyhow::Result<()> {
        if orders.len() == 1 {
            self.submit_order(orders.remove(0), None, self.config.client_id, None)?;
        } else {
            apply_order_list_id(&mut orders, order_list_id);
            self.submit_order_list(orders, None, self.config.client_id, None)?;
        }
        Ok(())
    }

    fn cancel_order_list_orders(&mut self, order_list_id: &str) -> anyhow::Result<()> {
        let client_order_ids = self
            .order_list_orders(order_list_id)
            .into_iter()
            .filter(|order| order.status().is_cancellable())
            .map(|order| order.client_order_id())
            .collect::<Vec<_>>();
        if client_order_ids.is_empty() {
            return Ok(());
        }

        for client_order_id in client_order_ids {
            self.cancel_order(client_order_id, self.config.client_id, None)?;
        }
        Ok(())
    }

    fn close_quote_from_cache(&self, entry: &StrategyStateEntry) -> Option<CloseQuote> {
        let cache = self.cache();
        close_quote_from_ticks(
            entry,
            cache.quote(&alpaca_instrument_id(&entry.short_symbol).ok()?),
            if entry.long_symbol.is_empty() {
                None
            } else {
                cache.quote(&alpaca_instrument_id(&entry.long_symbol).ok()?)
            },
            entry
                .short_call_symbol
                .as_deref()
                .and_then(|symbol| alpaca_instrument_id(symbol).ok())
                .and_then(|instrument_id| cache.quote(&instrument_id)),
            entry
                .long_call_symbol
                .as_deref()
                .and_then(|symbol| alpaca_instrument_id(symbol).ok())
                .and_then(|instrument_id| cache.quote(&instrument_id)),
        )
    }

    fn order_list_status(&self, order_list_id: &str) -> OrderListRuntimeStatus {
        let orders = self.order_list_orders(order_list_id);
        if orders.is_empty() {
            return OrderListRuntimeStatus::Missing;
        }
        if orders
            .iter()
            .all(|order| order.status() == OrderStatus::Filled)
        {
            return OrderListRuntimeStatus::Filled;
        }
        if orders
            .iter()
            .any(|order| order.is_open() || order.is_inflight())
        {
            return OrderListRuntimeStatus::Working;
        }
        if orders.iter().all(Order::is_closed) {
            return OrderListRuntimeStatus::TerminalWithoutFill;
        }
        OrderListRuntimeStatus::Partial
    }

    fn order_list_orders(&self, order_list_id: &str) -> Vec<OrderAny> {
        let cache = self.cache();
        let order_list_id = OrderListId::from(order_list_id);
        if let Some(order_list) = cache.order_list(&order_list_id) {
            return order_list
                .client_order_ids
                .iter()
                .filter_map(|client_order_id| cache.order(client_order_id))
                .collect();
        }

        let closed_orders = cache
            .client_order_ids_closed(None, None, None, None)
            .into_iter()
            .filter_map(|client_order_id| cache.order(&client_order_id));

        cache
            .orders_open(None, None, None, None, None)
            .into_iter()
            .chain(cache.orders_inflight(None, None, None, None, None))
            .chain(closed_orders)
            .filter(|order| {
                order.order_list_id() == Some(order_list_id)
                    || order.client_order_id().as_str() == order_list_id.as_str()
                    || order
                        .client_order_id()
                        .as_str()
                        .starts_with(&format!("{order_list_id}-"))
            })
            .collect()
    }

    fn refresh_management_quote_subscriptions(&mut self) {
        let desired = self
            .state
            .entries
            .iter()
            .filter(|entry| entry.is_active())
            .flat_map(|entry| match management_instrument_ids(entry) {
                Ok(instrument_ids) => instrument_ids,
                Err(error) => {
                    log::error!(
                        "Failed to build Alpaca management instrument IDs: order_list_id={} error={error:#}",
                        entry.order_list_id
                    );
                    Vec::new()
                }
            })
            .collect::<BTreeSet<_>>();

        for instrument_id in desired
            .difference(&self.management_quote_subscriptions)
            .copied()
            .collect::<Vec<_>>()
        {
            self.subscribe_quotes(instrument_id, self.config.client_id, None);
        }
        for instrument_id in self
            .management_quote_subscriptions
            .difference(&desired)
            .copied()
            .collect::<Vec<_>>()
        {
            self.unsubscribe_quotes(instrument_id, self.config.client_id, None);
        }
        self.management_quote_subscriptions = desired;
    }

    fn state_entry(&self, order_list_id: &str) -> Option<&StrategyStateEntry> {
        self.state
            .entries
            .iter()
            .find(|entry| entry.order_list_id == order_list_id)
    }

    fn state_entry_mut(&mut self, order_list_id: &str) -> Option<&mut StrategyStateEntry> {
        self.state
            .entries
            .iter_mut()
            .find(|entry| entry.order_list_id == order_list_id)
    }

    fn record_pending_close_submission(
        &mut self,
        entry_order_list_id: String,
        close_order_list_id: String,
        close_reason: String,
        order_count: usize,
        client_order_ids: Vec<String>,
    ) {
        for client_order_id in client_order_ids {
            self.pending_close_client_order_ids
                .insert(client_order_id, close_order_list_id.clone());
        }
        self.pending_close_submissions.insert(
            close_order_list_id.clone(),
            PendingCloseSubmission {
                entry_order_list_id,
                close_order_list_id,
                close_reason,
                order_count,
                accepted: 0,
                rejected: 0,
                recorded: false,
            },
        );
    }

    fn remove_pending_close_submission(&mut self, close_order_list_id: &str) {
        if let Some(pending) = self.pending_close_submissions.remove(close_order_list_id) {
            self.pending_close_client_order_ids
                .retain(|_, value| value != &pending.close_order_list_id);
        }
    }

    fn handle_close_order_accepted(&mut self, event: &OrderAccepted) -> bool {
        let client_order_id = event.client_order_id.to_string();
        let Some(close_order_list_id) = self
            .pending_close_client_order_ids
            .get(&client_order_id)
            .cloned()
        else {
            return false;
        };

        let mut state_update = None;
        let mut should_remove = false;
        if let Some(pending) = self.pending_close_submissions.get_mut(&close_order_list_id) {
            pending.accepted = pending.accepted.saturating_add(1);
            if !pending.recorded {
                state_update = Some((
                    pending.entry_order_list_id.clone(),
                    pending.close_order_list_id.clone(),
                    Some(event.venue_order_id.to_string()),
                    pending.close_reason.clone(),
                ));
                pending.recorded = true;
            }
            should_remove = pending.accepted + pending.rejected >= pending.order_count;
        }

        if let Some((entry_order_list_id, close_order_list_id, parent_order_id, close_reason)) =
            state_update
        {
            let mutation = if let Some(entry) = self.state_entry_mut(&entry_order_list_id) {
                entry.record_close_submission(
                    close_order_list_id.clone(),
                    parent_order_id.clone(),
                    close_reason.clone(),
                );
                Some(close_accepted_state_mutation(
                    event,
                    entry,
                    &close_order_list_id,
                    parent_order_id.as_deref(),
                    &close_reason,
                ))
            } else {
                None
            };
            if let Some(mutation) = mutation {
                self.persist_strategy_state_mutation(mutation);
            }
        }

        if should_remove {
            self.remove_pending_close_submission(&close_order_list_id);
        }
        true
    }

    fn handle_close_order_rejected(
        &mut self,
        client_order_id: &ClientOrderId,
        reason: &str,
    ) -> bool {
        let Some(close_order_list_id) = self
            .pending_close_client_order_ids
            .get(client_order_id.as_str())
            .cloned()
        else {
            return false;
        };

        let mut should_remove = false;
        if let Some(pending) = self.pending_close_submissions.get_mut(&close_order_list_id) {
            pending.rejected = pending.rejected.saturating_add(1);
            should_remove = pending.accepted + pending.rejected >= pending.order_count;
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_rejected",
                    "reason": reason,
                    "order_list_id": pending.entry_order_list_id,
                    "close_order_list_id": pending.close_order_list_id,
                    "accepted": pending.accepted,
                    "rejected": pending.rejected,
                }),
            );
        }

        if should_remove {
            self.remove_pending_close_submission(&close_order_list_id);
        }
        true
    }

    fn handle_close_order_filled(&mut self, event: &OrderFilled) -> bool {
        let Some(close_order_list_id) = self.close_order_list_id_for_client(&event.client_order_id)
        else {
            return false;
        };
        let Some(entry_order_list_id) = self.entry_order_list_id_for_close(&close_order_list_id)
        else {
            return false;
        };
        if self.order_list_status(&close_order_list_id) == OrderListRuntimeStatus::Filled {
            if let Err(error) = self.mark_entry_closed(
                &entry_order_list_id,
                &close_order_list_id,
                Some(event.venue_order_id.to_string()),
            ) {
                log::error!("Failed to mark Alpaca options entry closed: {error:#}");
            }
        }
        true
    }

    fn handle_close_order_canceled(&mut self, event: &OrderCanceled) -> bool {
        let Some(close_order_list_id) = self.close_order_list_id_for_client(&event.client_order_id)
        else {
            return false;
        };
        let Some(entry_order_list_id) = self.entry_order_list_id_for_close(&close_order_list_id)
        else {
            return false;
        };
        if self.order_list_status(&close_order_list_id)
            == OrderListRuntimeStatus::TerminalWithoutFill
            && let Err(error) = self.clear_close_submission(&entry_order_list_id, "close_canceled")
        {
            log::error!("Failed to clear Alpaca close submission: {error:#}");
        }
        true
    }

    fn handle_entry_order_canceled(&mut self, event: &OrderCanceled) -> bool {
        let client_order_id = event.client_order_id.to_string();
        let Some(order_list_id) = self
            .pending_client_order_ids
            .get(&client_order_id)
            .cloned()
            .or_else(|| self.order_list_id_for_client(&event.client_order_id))
        else {
            return false;
        };
        if self.order_list_status(&order_list_id) == OrderListRuntimeStatus::TerminalWithoutFill
            && let Err(error) = self.mark_entry_canceled(&order_list_id, "entry_canceled")
        {
            log::error!("Failed to mark Alpaca entry canceled: {error:#}");
        }
        true
    }

    fn close_order_list_id_for_client(&self, client_order_id: &ClientOrderId) -> Option<String> {
        self.pending_close_client_order_ids
            .get(client_order_id.as_str())
            .cloned()
            .or_else(|| self.order_list_id_for_client(client_order_id))
            .filter(|order_list_id| {
                self.state.entries.iter().any(|entry| {
                    entry.close_order_list_id.as_deref() == Some(order_list_id.as_str())
                })
            })
    }

    fn entry_order_list_id_for_close(&self, close_order_list_id: &str) -> Option<String> {
        self.state
            .entries
            .iter()
            .find(|entry| entry.close_order_list_id.as_deref() == Some(close_order_list_id))
            .map(|entry| entry.order_list_id.clone())
    }

    fn order_list_id_for_client(&self, client_order_id: &ClientOrderId) -> Option<String> {
        self.cache()
            .order(client_order_id)
            .and_then(|order| order.order_list_id())
            .map(|order_list_id| order_list_id.to_string())
    }

    fn mark_entry_closed(
        &mut self,
        entry_order_list_id: &str,
        close_order_list_id: &str,
        close_parent_order_id: Option<String>,
    ) -> anyhow::Result<()> {
        let Some(entry) = self.state_entry_mut(entry_order_list_id) else {
            return Ok(());
        };
        if entry.closed {
            return Ok(());
        }
        entry.mark_closed(close_parent_order_id.clone());
        let mutation = entry_closed_state_mutation(
            entry,
            close_order_list_id,
            close_parent_order_id.as_deref(),
        );
        let _ = entry;
        self.persist_strategy_state_mutation(mutation);
        Ok(())
    }

    fn mark_entry_canceled(
        &mut self,
        entry_order_list_id: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        let Some(entry) = self.state_entry_mut(entry_order_list_id) else {
            return Ok(());
        };
        if entry.canceled {
            return Ok(());
        }
        entry.mark_canceled();
        let mutation = entry_canceled_state_mutation(entry, reason);
        let _ = entry;
        self.persist_strategy_state_mutation(mutation);
        Ok(())
    }

    fn clear_close_submission(
        &mut self,
        entry_order_list_id: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        let Some(entry) = self.state_entry_mut(entry_order_list_id) else {
            return Ok(());
        };
        let close_order_list_id = entry.close_order_list_id.clone();
        entry.clear_close_submission();
        let mutation = close_cleared_state_mutation(entry, close_order_list_id.as_deref(), reason);
        let _ = entry;
        self.persist_strategy_state_mutation(mutation);
        Ok(())
    }

    fn record_pending_submission(
        &mut self,
        entry: SelectedOptionsEntry,
        trade_date: String,
        order_list_id: String,
        order_count: usize,
        client_order_ids: Vec<String>,
    ) {
        for client_order_id in client_order_ids {
            self.pending_client_order_ids
                .insert(client_order_id, order_list_id.clone());
        }
        let submitted_at_utc = Utc::now().to_rfc3339();
        self.pending_submissions.insert(
            order_list_id.clone(),
            PendingEntrySubmission {
                entry,
                trade_date,
                order_list_id,
                submitted_at_utc,
                quantity: self.config.quantity,
                order_count,
                accepted: 0,
                rejected: 0,
                recorded: false,
                rejection_reasons: Vec::new(),
            },
        );
    }

    fn handle_order_accepted(&mut self, event: OrderAccepted) {
        if self.handle_close_order_accepted(&event) {
            return;
        }

        let client_order_id = event.client_order_id.to_string();
        let Some(order_list_id) = self.pending_client_order_ids.get(&client_order_id).cloned()
        else {
            return;
        };

        let mut draft = None;
        let mut should_remove = false;
        if let Some(pending) = self.pending_submissions.get_mut(&order_list_id) {
            pending.accepted = pending.accepted.saturating_add(1);
            if !pending.recorded {
                draft = Some(pending.entry.state_entry_draft(
                    &pending.trade_date,
                    &pending.order_list_id,
                    pending.quantity,
                    Some(pending.submitted_at_utc.clone()),
                    Some(event.venue_order_id.to_string()),
                ));
                pending.recorded = true;
            }
            should_remove = pending.accepted + pending.rejected >= pending.order_count;
        }

        if let Some(draft) = draft {
            log::info!(
                "Recording accepted Alpaca options entry: order_list_id={} client_order_id={} venue_order_id={}",
                order_list_id,
                event.client_order_id,
                event.venue_order_id
            );
            let mutation = accepted_state_mutation(&event, &order_list_id, &draft);
            self.state.record_entry_submission(draft);
            self.persist_strategy_state_mutation(mutation);
        }

        if should_remove {
            self.remove_pending_submission(&order_list_id);
        }
    }

    fn handle_order_rejected(
        &mut self,
        client_order_id: ClientOrderId,
        reason: &str,
        event_id: UUID4,
        ts_event: UnixNanos,
        event_type: &'static str,
    ) {
        if self.handle_close_order_rejected(&client_order_id, reason) {
            return;
        }

        let client_order_id = client_order_id.to_string();
        let Some(order_list_id) = self.pending_client_order_ids.get(&client_order_id).cloned()
        else {
            return;
        };

        let mut rejected_state = None;
        let mut should_remove = false;
        let mut terminal_rejection_recorded = false;
        let mut submit_rejected_alert = None;
        if let Some(pending) = self.pending_submissions.get_mut(&order_list_id) {
            pending.rejected = pending.rejected.saturating_add(1);
            pending.rejection_reasons.push(reason.to_string());
            if pending.entry.is_naked_option() && pending.accepted == 0 && !pending.recorded {
                let close_reason = if pending
                    .rejection_reasons
                    .iter()
                    .any(|reason| is_uncovered_option_permission_rejection(reason))
                {
                    UNCOVERED_OPTION_PERMISSION_REJECTION_REASON
                } else {
                    "entry_rejected"
                };
                rejected_state = Some((
                    pending.entry.state_entry_draft(
                        &pending.trade_date,
                        &pending.order_list_id,
                        pending.quantity,
                        Some(pending.submitted_at_utc.clone()),
                        None,
                    ),
                    close_reason.to_string(),
                ));
                pending.recorded = true;
                terminal_rejection_recorded = true;
            }
            should_remove = pending.accepted + pending.rejected >= pending.order_count;
            if should_remove && pending.rejected > 0 {
                submit_rejected_alert = Some((
                    pending.trade_date.clone(),
                    pending.entry.clone(),
                    pending.order_list_id.clone(),
                    pending.quantity,
                    pending.accepted,
                    pending.rejected,
                    pending.rejection_reasons.clone(),
                    terminal_rejection_recorded,
                ));
            }
        }

        if let Some((draft, close_reason)) = rejected_state {
            log::info!(
                "Recording terminally rejected Alpaca options entry: order_list_id={} client_order_id={} reason={}",
                order_list_id,
                client_order_id,
                close_reason
            );
            let mutation = rejected_state_mutation(
                event_id,
                ts_event,
                event_type,
                &order_list_id,
                &client_order_id,
                reason,
                &draft,
                &close_reason,
            );
            self.state.record_entry_submission(draft);
            if let Some(entry) = self.state.entries.last_mut() {
                entry.mark_canceled();
                entry.close_reason = Some(close_reason);
            }
            self.persist_strategy_state_mutation(mutation);
        }

        if let Some((
            trade_date,
            entry,
            order_list_id,
            quantity,
            accepted,
            rejected,
            rejection_reasons,
            terminal_rejection_recorded,
        )) = submit_rejected_alert
        {
            self.record_submit_rejected_candidate_alert(
                &trade_date,
                &entry,
                &order_list_id,
                quantity,
                accepted,
                rejected,
                &rejection_reasons,
                terminal_rejection_recorded,
            );
        }

        if should_remove {
            self.remove_pending_submission(&order_list_id);
        }
    }

    fn remove_pending_submission(&mut self, order_list_id: &str) {
        self.pending_submissions.remove(order_list_id);
        self.pending_client_order_ids
            .retain(|_, pending_order_list_id| pending_order_list_id != order_list_id);
    }

    fn entry_admission_snapshot(
        &self,
        entry: &SelectedOptionsEntry,
    ) -> anyhow::Result<EntryAdmissionSnapshot> {
        let cache = self.cache();
        let open_order_count = open_broker_order_intent_count(&cache);
        let mut broker_admission_reasons = self.config.admission.account_admission_reasons.clone();
        let symbols = entry.option_symbols();
        let candidate_ids = symbols
            .iter()
            .map(|symbol| alpaca_instrument_id(symbol))
            .collect::<anyhow::Result<Vec<_>>>()?;

        for (symbol, instrument_id) in symbols.iter().zip(&candidate_ids) {
            if cache.has_positions_open(None, Some(instrument_id), None, None, None) {
                broker_admission_reasons
                    .push(format!("existing open position on candidate leg {symbol}"));
            }
            if cache.has_orders_open(None, Some(instrument_id), None, None, None)
                || cache.has_orders_inflight(None, Some(instrument_id), None, None, None)
            {
                broker_admission_reasons.push(format!(
                    "working order already references candidate leg {symbol}"
                ));
            }
        }

        for position in cache.positions_open(None, None, None, None, None) {
            if candidate_ids.contains(&position.instrument_id) {
                continue;
            }
            if instrument_underlying_matches(&cache, &position.instrument_id, entry.underlying()) {
                broker_admission_reasons.push(format!(
                    "existing open option position on underlying {}: {}",
                    entry.underlying(),
                    position.instrument_id.symbol
                ));
            }
        }

        for order in cache
            .orders_open(None, None, None, None, None)
            .into_iter()
            .chain(cache.orders_inflight(None, None, None, None, None))
        {
            let instrument_id = order.instrument_id();
            if candidate_ids.contains(&instrument_id) {
                continue;
            }
            if instrument_underlying_matches(&cache, &instrument_id, entry.underlying()) {
                broker_admission_reasons.push(format!(
                    "working order already references underlying {}: {}",
                    entry.underlying(),
                    instrument_id.symbol
                ));
            }
        }

        Ok(EntryAdmissionSnapshot {
            open_order_count,
            broker_admission_reasons,
        })
    }

    fn lifecycle_submission_block(&self, entry: &SelectedOptionsEntry) -> Option<SubmissionBlock> {
        self.config
            .lifecycle_risk
            .as_ref()
            .and_then(|risk| risk.submission_block(entry, Utc::now()))
    }

    fn log_entry_block(
        &mut self,
        trade_date: &str,
        entry: &SelectedOptionsEntry,
        block: &SubmissionBlock,
    ) {
        log::info!(
            "Skipping Alpaca options entry: trade_date={} reason={} current={:?} limit={:?} details={:?} underlying={} strategy={} symbols={}",
            trade_date,
            block.reason,
            block.current,
            block.limit,
            block.details,
            entry.underlying(),
            entry.strategy_name(),
            entry.option_symbols().join(",")
        );
        emit_operator_event(
            "entry_decision",
            json!({
                "action": "selected_but_blocked",
                "trade_date": trade_date,
                "reason": block.reason.clone(),
                "current": block.current,
                "limit": block.limit,
                "details": block.details.clone(),
                "underlying": entry.underlying(),
                "strategy": entry.strategy_name(),
                "symbols": entry.option_symbols(),
                "score": entry.score(),
            }),
        );
        self.record_selected_candidate_alert(
            trade_date,
            entry,
            "selected_but_blocked",
            None,
            Some(&block.reason),
            block.current,
            block.limit,
            &block.details,
        );
    }

    #[expect(clippy::too_many_arguments)]
    fn record_selected_candidate_alert(
        &mut self,
        trade_date: &str,
        entry: &SelectedOptionsEntry,
        action: &str,
        order_list_id: Option<&str>,
        reason: Option<&str>,
        current: Option<usize>,
        limit: Option<usize>,
        details: &[String],
    ) {
        let Some(persistence) = &self.config.candidate_ledger_persistence else {
            return;
        };
        let (identity_key, mut payload) = selected_entry_alert_payload(
            entry,
            None,
            trade_date,
            action,
            order_list_id,
            self.config.quantity,
        );
        let alert_key = candidate_alert_key(SELECTED_CANDIDATE_ALERT, &identity_key);
        if !self
            .recorded_candidate_alert_keys
            .insert(format!("{trade_date}|{alert_key}"))
        {
            return;
        }
        if let Some(reason) = reason {
            insert_string_field(&mut payload, "reason", reason.to_string());
        }
        insert_optional_usize(&mut payload, "current", current);
        insert_optional_usize(&mut payload, "limit", limit);
        if !details.is_empty() {
            insert_value_field(&mut payload, "details", json!(details));
        }
        if let Err(error) = persistence.append_candidate_alert(
            trade_date,
            SELECTED_CANDIDATE_ALERT,
            "info",
            alert_key,
            payload,
        ) {
            log::error!("Failed to enqueue Alpaca selected-candidate evidence: {error:#}");
        }
    }

    #[expect(clippy::too_many_arguments)]
    fn record_submit_rejected_candidate_alert(
        &mut self,
        trade_date: &str,
        entry: &SelectedOptionsEntry,
        order_list_id: &str,
        quantity: u64,
        accepted: usize,
        rejected: usize,
        rejection_reasons: &[String],
        terminal_rejection_recorded: bool,
    ) {
        let Some(persistence) = &self.config.candidate_ledger_persistence else {
            return;
        };
        let (identity_key, mut payload) = selected_entry_alert_payload(
            entry,
            None,
            trade_date,
            "submitted",
            Some(order_list_id),
            quantity,
        );
        let alert_key = candidate_alert_key(CANDIDATE_SUBMIT_REJECTED_ALERT, &identity_key);
        if !self
            .recorded_candidate_alert_keys
            .insert(format!("{trade_date}|{alert_key}"))
        {
            return;
        }
        insert_value_field(&mut payload, "accepted", Value::from(accepted));
        insert_value_field(&mut payload, "rejected", Value::from(rejected));
        insert_value_field(&mut payload, "rejection_reasons", json!(rejection_reasons));
        insert_value_field(
            &mut payload,
            "terminal_rejection_recorded",
            Value::Bool(terminal_rejection_recorded),
        );
        if let Err(error) = persistence.append_candidate_alert(
            trade_date,
            CANDIDATE_SUBMIT_REJECTED_ALERT,
            "warning",
            alert_key,
            payload,
        ) {
            log::error!("Failed to enqueue Alpaca submit-rejected evidence: {error:#}");
        }
    }

    fn state_persistence_ready(&self) -> bool {
        self.config
            .state_persistence
            .as_ref()
            .is_some_and(StrategyStatePersistenceHandle::is_healthy)
    }

    fn candidate_ledger_persistence_ready(&self) -> bool {
        self.config
            .candidate_ledger_persistence
            .as_ref()
            .is_some_and(CandidateLedgerPersistenceHandle::is_healthy)
    }

    fn persist_strategy_state_mutation(&self, mutation: anyhow::Result<StrategyStateMutation>) {
        let Some(persistence) = &self.config.state_persistence else {
            if self.config.admission.submit_enabled {
                log::error!("Alpaca strategy-state persistence is not configured");
            }
            return;
        };
        let mutation = match mutation {
            Ok(mutation) => mutation,
            Err(error) => {
                log::error!("Failed to build Alpaca strategy-state mutation: {error:#}");
                return;
            }
        };
        if let Err(error) = persistence.persist(mutation, self.state.clone()) {
            log::error!("Failed to enqueue Alpaca strategy-state mutation: {error:#}");
            emit_operator_event(
                "strategy_state_persistence_error",
                json!({
                    "reason": "enqueue_failed",
                    "error": error.to_string(),
                }),
            );
        }
    }
}

nautilus_strategy!(AlpacaOptionsStrategy, {
    fn external_order_claims(&self) -> Option<Vec<InstrumentId>> {
        let claims = self
            .state
            .entries
            .iter()
            .filter(|entry| entry.is_active())
            .flat_map(|entry| management_instrument_ids(entry).unwrap_or_default())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        (!claims.is_empty()).then_some(claims)
    }

    fn on_order_accepted(&mut self, event: OrderAccepted) {
        self.handle_order_accepted(event);
    }

    fn on_order_rejected(&mut self, event: OrderRejected) {
        self.handle_order_rejected(
            event.client_order_id,
            event.reason.as_str(),
            event.event_id,
            event.ts_event,
            "entry_rejected",
        );
    }

    fn on_order_denied(&mut self, event: OrderDenied) {
        self.handle_order_rejected(
            event.client_order_id,
            event.reason.as_str(),
            event.event_id,
            event.ts_event,
            "entry_denied",
        );
    }
});

impl DataActor for AlpacaOptionsStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.subscribe_data(OptionsCandidateData::data_type(), None, None);
        self.refresh_management_quote_subscriptions();
        if self.config.management.interval_secs > 0 {
            self.clock().set_timer(
                MANAGEMENT_TIMER,
                std::time::Duration::from_secs(self.config.management.interval_secs),
                None,
                None,
                None,
                None,
                None,
            )?;
        }
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.unsubscribe_data(OptionsCandidateData::data_type(), None, None);
        self.clock().cancel_timer(MANAGEMENT_TIMER);
        for instrument_id in self
            .management_quote_subscriptions
            .iter()
            .copied()
            .collect::<Vec<_>>()
        {
            self.unsubscribe_quotes(instrument_id, self.config.client_id, None);
        }
        self.management_quote_subscriptions.clear();
        Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        if event.name.as_str() == MANAGEMENT_TIMER {
            self.manage_active_entries()?;
        }
        Ok(())
    }

    fn on_data(&mut self, data: &CustomData) -> anyhow::Result<()> {
        let Some(candidates) = data.data.as_any().downcast_ref::<OptionsCandidateData>() else {
            return Ok(());
        };
        self.submit_candidate_data(candidates)?;
        Ok(())
    }

    fn on_order_filled(&mut self, event: &OrderFilled) -> anyhow::Result<()> {
        self.handle_close_order_filled(event);
        Ok(())
    }

    fn on_order_canceled(&mut self, event: &OrderCanceled) -> anyhow::Result<()> {
        if !self.handle_close_order_canceled(event) {
            self.handle_entry_order_canceled(event);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderListRuntimeStatus {
    Missing,
    Working,
    Filled,
    TerminalWithoutFill,
    Partial,
}

/// Builds Nautilus reduce-only orders for closing an active strategy-state entry.
///
/// # Errors
///
/// Returns an error when quantity, price, or Alpaca option symbol inputs are invalid.
pub fn build_close_entry_orders(
    orders: &mut impl OptionsEntryOrderCreator,
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
) -> anyhow::Result<Vec<OrderAny>> {
    let mut close_orders = vec![limit_option_order(
        orders,
        if entry.is_naked_option() {
            ClientOrderId::from(order_list_id)
        } else {
            labeled_client_order_id(order_list_id, "short-close")
        },
        &entry.short_symbol,
        OrderSide::Buy,
        entry.quantity,
        quote.short_ask,
        true,
    )?];

    if !entry.long_symbol.is_empty() {
        close_orders.push(limit_option_order(
            orders,
            labeled_client_order_id(order_list_id, "long-close"),
            &entry.long_symbol,
            OrderSide::Sell,
            entry.quantity,
            quote.long_bid,
            true,
        )?);
    }
    if let (
        Some(short_call_symbol),
        Some(long_call_symbol),
        Some(short_call_ask),
        Some(long_call_bid),
    ) = (
        entry.short_call_symbol.as_deref(),
        entry.long_call_symbol.as_deref(),
        quote.short_call_ask,
        quote.long_call_bid,
    ) {
        close_orders.push(limit_option_order(
            orders,
            labeled_client_order_id(order_list_id, "short-call-close"),
            short_call_symbol,
            OrderSide::Buy,
            entry.quantity,
            short_call_ask,
            true,
        )?);
        close_orders.push(limit_option_order(
            orders,
            labeled_client_order_id(order_list_id, "long-call-close"),
            long_call_symbol,
            OrderSide::Sell,
            entry.quantity,
            long_call_bid,
            true,
        )?);
    }

    Ok(close_orders)
}

/// Builds a unique order-list ID for one submitted entry.
#[must_use]
pub fn entry_order_list_id(trade_date: &str, underlying: &str) -> String {
    format!("options-entry-{trade_date}-{underlying}-{}", UUID4::new())
}

fn close_order_list_id(entry: &StrategyStateEntry) -> String {
    format!(
        "options-close-{}-{}-{}",
        entry.trade_date,
        entry.underlying,
        UUID4::new()
    )
}

/// Minimal order creation surface used by the Alpaca options entry strategy.
pub trait OptionsEntryOrderCreator {
    /// Creates a Nautilus limit order for one option leg.
    #[expect(clippy::too_many_arguments)]
    fn option_limit(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        quantity: Quantity,
        price: Price,
        reduce_only: bool,
        client_order_id: ClientOrderId,
    ) -> OrderAny;
}

impl OptionsEntryOrderCreator for OrderApi<'_> {
    fn option_limit(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        quantity: Quantity,
        price: Price,
        reduce_only: bool,
        client_order_id: ClientOrderId,
    ) -> OrderAny {
        self.limit(
            instrument_id,
            side,
            quantity,
            price,
            Some(TimeInForce::Day),
            None,
            None,
            Some(reduce_only),
            Some(false),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(client_order_id),
        )
    }
}

impl OptionsEntryOrderCreator for OrderFactory {
    fn option_limit(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        quantity: Quantity,
        price: Price,
        reduce_only: bool,
        client_order_id: ClientOrderId,
    ) -> OrderAny {
        self.limit(
            instrument_id,
            side,
            quantity,
            price,
            Some(TimeInForce::Day),
            None,
            None,
            Some(reduce_only),
            Some(false),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(client_order_id),
        )
    }
}

/// Builds Nautilus orders for a selected entry candidate.
///
/// # Errors
///
/// Returns an error when quantity, price, or Alpaca option symbol inputs are invalid.
pub fn build_selected_entry_orders(
    orders: &mut impl OptionsEntryOrderCreator,
    entry: &SelectedOptionsEntry,
    order_list_id: &str,
    quantity: u64,
) -> anyhow::Result<Vec<OrderAny>> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }

    Ok(match entry {
        SelectedOptionsEntry::Credit(entry) => vec![
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "short"),
                &entry.candidate.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.short.bid,
                false,
            )?,
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "long"),
                &entry.candidate.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.long.ask,
                false,
            )?,
        ],
        SelectedOptionsEntry::IronCondor(entry) => vec![
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "short-put"),
                &entry.candidate.put.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.put.short.bid,
                false,
            )?,
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "long-put"),
                &entry.candidate.put.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.put.long.ask,
                false,
            )?,
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "short-call"),
                &entry.candidate.call.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.call.short.bid,
                false,
            )?,
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "long-call"),
                &entry.candidate.call.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.call.long.ask,
                false,
            )?,
        ],
        SelectedOptionsEntry::Debit(entry) => vec![
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "long"),
                &entry.candidate.long.symbol,
                OrderSide::Buy,
                quantity,
                entry.candidate.long.ask,
                false,
            )?,
            limit_option_order(
                orders,
                labeled_client_order_id(order_list_id, "short"),
                &entry.candidate.short.symbol,
                OrderSide::Sell,
                quantity,
                entry.candidate.short.bid,
                false,
            )?,
        ],
        SelectedOptionsEntry::NakedOption(entry) => vec![limit_option_order(
            orders,
            ClientOrderId::from(order_list_id),
            &entry.candidate.short.symbol,
            OrderSide::Sell,
            quantity,
            entry.candidate.short.bid,
            false,
        )?],
    })
}

/// Applies a known order-list ID before handing orders to `Strategy::submit_order_list`.
pub fn apply_order_list_id(orders: &mut [OrderAny], order_list_id: &str) {
    let order_list_id = OrderListId::from(order_list_id);
    for order in orders {
        order.set_order_list_id(order_list_id);
    }
}

fn limit_option_order(
    orders: &mut impl OptionsEntryOrderCreator,
    client_order_id: ClientOrderId,
    symbol: &str,
    side: OrderSide,
    quantity: u64,
    limit_price: f64,
    reduce_only: bool,
) -> anyhow::Result<OrderAny> {
    if quantity == 0 {
        anyhow::bail!("order {client_order_id} quantity must be positive");
    }
    if limit_price <= 0.0 {
        anyhow::bail!("order {client_order_id} limit price must be positive");
    }

    Ok(orders.option_limit(
        alpaca_instrument_id(symbol)?,
        side,
        Quantity::new(quantity as f64, 0),
        Price::new(limit_price, 2),
        reduce_only,
        client_order_id,
    ))
}

fn alpaca_instrument_id(symbol: &str) -> anyhow::Result<InstrumentId> {
    Ok(InstrumentId::from_str(&format!("{symbol}.{ALPACA_VENUE}"))?)
}

fn labeled_client_order_id(order_list_id: &str, label: &str) -> ClientOrderId {
    let client_order_id = format!("{order_list_id}-{label}");
    ClientOrderId::from(client_order_id.as_str())
}

fn submitted_underlying_key(trade_date: &str, entry: &SelectedOptionsEntry) -> String {
    format!(
        "{}:{}:{}",
        trade_date,
        entry.strategy_name(),
        entry.underlying()
    )
}

fn instrument_underlying_matches(
    cache: &CacheApi<'_>,
    instrument_id: &InstrumentId,
    underlying: &str,
) -> bool {
    cache
        .instrument(instrument_id)
        .and_then(|instrument| instrument.underlying())
        .is_some_and(|instrument_underlying| {
            instrument_underlying
                .as_str()
                .eq_ignore_ascii_case(underlying)
        })
}

fn open_broker_order_intent_count(cache: &CacheApi<'_>) -> usize {
    let mut intent_ids = BTreeSet::new();
    for order in cache
        .orders_open(None, None, None, None, None)
        .into_iter()
        .chain(cache.orders_inflight(None, None, None, None, None))
    {
        if let Some(order_list_id) = order.order_list_id() {
            intent_ids.insert(format!("list:{order_list_id}"));
        } else {
            intent_ids.insert(format!("client:{}", order.client_order_id()));
        }
    }
    intent_ids.len()
}

fn insert_optional_usize(payload: &mut Value, key: &str, value: Option<usize>) {
    if let Some(value) = value {
        insert_value_field(payload, key, Value::from(value));
    }
}

fn accepted_state_mutation(
    event: &OrderAccepted,
    order_list_id: &str,
    draft: &StrategyStateEntryDraft,
) -> anyhow::Result<StrategyStateMutation> {
    let mut mutation = StrategyStateMutation::new(
        uuid_from_nautilus(event.event_id)?,
        "entry_accepted",
        serde_json::json!({
            "order_list_id": order_list_id,
            "client_order_id": event.client_order_id.to_string(),
            "venue_order_id": event.venue_order_id.to_string(),
            "instrument_id": event.instrument_id.to_string(),
            "account_id": event.account_id.to_string(),
            "reconciliation": event.reconciliation,
            "entry": state_entry_draft_payload(draft),
        }),
    );
    mutation.strategy = Some(draft.strategy.clone());
    mutation.underlying = Some(draft.underlying.clone());
    mutation.trade_date = parse_trade_date(&draft.trade_date);
    mutation.order_list_id = Some(order_list_id.to_string());
    mutation.client_order_id = Some(event.client_order_id.to_string());
    mutation.venue_order_id = Some(event.venue_order_id.to_string());
    mutation.ts_event = Some(event.ts_event.to_datetime_utc());
    Ok(mutation)
}

#[expect(clippy::too_many_arguments)]
fn rejected_state_mutation(
    event_id: UUID4,
    ts_event: UnixNanos,
    event_type: &str,
    order_list_id: &str,
    client_order_id: &str,
    reason: &str,
    draft: &StrategyStateEntryDraft,
    close_reason: &str,
) -> anyhow::Result<StrategyStateMutation> {
    let mut mutation = StrategyStateMutation::new(
        uuid_from_nautilus(event_id)?,
        event_type,
        serde_json::json!({
            "order_list_id": order_list_id,
            "client_order_id": client_order_id,
            "reason": reason,
            "close_reason": close_reason,
            "entry": state_entry_draft_payload(draft),
        }),
    );
    mutation.strategy = Some(draft.strategy.clone());
    mutation.underlying = Some(draft.underlying.clone());
    mutation.trade_date = parse_trade_date(&draft.trade_date);
    mutation.order_list_id = Some(order_list_id.to_string());
    mutation.client_order_id = Some(client_order_id.to_string());
    mutation.ts_event = Some(ts_event.to_datetime_utc());
    Ok(mutation)
}

fn close_accepted_state_mutation(
    event: &OrderAccepted,
    entry: &StrategyStateEntry,
    close_order_list_id: &str,
    close_parent_order_id: Option<&str>,
    close_reason: &str,
) -> anyhow::Result<StrategyStateMutation> {
    let mut mutation = StrategyStateMutation::new(
        uuid_from_nautilus(event.event_id)?,
        "close_accepted",
        serde_json::json!({
            "order_list_id": entry.order_list_id,
            "close_order_list_id": close_order_list_id,
            "close_parent_order_id": close_parent_order_id,
            "client_order_id": event.client_order_id.to_string(),
            "venue_order_id": event.venue_order_id.to_string(),
            "instrument_id": event.instrument_id.to_string(),
            "account_id": event.account_id.to_string(),
            "reconciliation": event.reconciliation,
            "close_reason": close_reason,
            "entry": state_entry_payload(entry),
        }),
    );
    populate_entry_mutation_fields(&mut mutation, entry);
    mutation.order_list_id = Some(close_order_list_id.to_string());
    mutation.client_order_id = Some(event.client_order_id.to_string());
    mutation.venue_order_id = Some(event.venue_order_id.to_string());
    mutation.ts_event = Some(event.ts_event.to_datetime_utc());
    Ok(mutation)
}

fn entry_closed_state_mutation(
    entry: &StrategyStateEntry,
    close_order_list_id: &str,
    close_parent_order_id: Option<&str>,
) -> anyhow::Result<StrategyStateMutation> {
    let mut mutation = StrategyStateMutation::new(
        Uuid::new_v4(),
        "entry_closed",
        serde_json::json!({
            "order_list_id": entry.order_list_id,
            "close_order_list_id": close_order_list_id,
            "close_parent_order_id": close_parent_order_id,
            "entry": state_entry_payload(entry),
        }),
    );
    populate_entry_mutation_fields(&mut mutation, entry);
    mutation.order_list_id = Some(close_order_list_id.to_string());
    mutation.venue_order_id = close_parent_order_id.map(ToString::to_string);
    mutation.ts_event = Some(Utc::now());
    Ok(mutation)
}

fn entry_canceled_state_mutation(
    entry: &StrategyStateEntry,
    reason: &str,
) -> anyhow::Result<StrategyStateMutation> {
    let mut mutation = StrategyStateMutation::new(
        Uuid::new_v4(),
        "entry_canceled",
        serde_json::json!({
            "order_list_id": entry.order_list_id,
            "reason": reason,
            "entry": state_entry_payload(entry),
        }),
    );
    populate_entry_mutation_fields(&mut mutation, entry);
    mutation.order_list_id = Some(entry.order_list_id.clone());
    mutation.ts_event = Some(Utc::now());
    Ok(mutation)
}

fn close_cleared_state_mutation(
    entry: &StrategyStateEntry,
    close_order_list_id: Option<&str>,
    reason: &str,
) -> anyhow::Result<StrategyStateMutation> {
    let mut mutation = StrategyStateMutation::new(
        Uuid::new_v4(),
        "close_cleared",
        serde_json::json!({
            "order_list_id": entry.order_list_id,
            "close_order_list_id": close_order_list_id,
            "reason": reason,
            "entry": state_entry_payload(entry),
        }),
    );
    populate_entry_mutation_fields(&mut mutation, entry);
    mutation.order_list_id = close_order_list_id
        .map(ToString::to_string)
        .or_else(|| Some(entry.order_list_id.clone()));
    mutation.ts_event = Some(Utc::now());
    Ok(mutation)
}

fn populate_entry_mutation_fields(
    mutation: &mut StrategyStateMutation,
    entry: &StrategyStateEntry,
) {
    mutation.strategy = Some(entry.strategy.clone());
    mutation.underlying = Some(entry.underlying.clone());
    mutation.trade_date = parse_trade_date(&entry.trade_date);
}

fn uuid_from_nautilus(event_id: UUID4) -> anyhow::Result<Uuid> {
    Ok(Uuid::parse_str(event_id.as_str())?)
}

fn parse_trade_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn state_entry_draft_payload(draft: &StrategyStateEntryDraft) -> serde_json::Value {
    serde_json::json!({
        "trade_date": draft.trade_date,
        "underlying": draft.underlying,
        "strategy": draft.strategy,
        "order_list_id": draft.order_list_id,
        "short_symbol": draft.short_symbol,
        "long_symbol": draft.long_symbol,
        "short_call_symbol": draft.short_call_symbol,
        "long_call_symbol": draft.long_call_symbol,
        "quantity": draft.quantity,
        "credit": draft.credit,
        "debit": draft.debit,
        "score": draft.score,
        "parent_order_id": draft.parent_order_id,
        "submitted_at_utc": draft.submitted_at_utc,
    })
}

fn state_entry_payload(entry: &StrategyStateEntry) -> serde_json::Value {
    serde_json::json!({
        "trade_date": entry.trade_date,
        "underlying": entry.underlying,
        "strategy": entry.strategy,
        "order_list_id": entry.order_list_id,
        "short_symbol": entry.short_symbol,
        "long_symbol": entry.long_symbol,
        "short_call_symbol": entry.short_call_symbol,
        "long_call_symbol": entry.long_call_symbol,
        "quantity": entry.quantity,
        "credit": entry.credit,
        "debit": entry.debit,
        "score": entry.score,
        "parent_order_id": entry.parent_order_id,
        "submitted_at_utc": entry.submitted_at_utc,
        "close_order_list_id": entry.close_order_list_id,
        "close_parent_order_id": entry.close_parent_order_id,
        "close_reason": entry.close_reason,
        "close_attempts": entry.close_attempts,
        "last_close_submitted_at_utc": entry.last_close_submitted_at_utc,
        "submitted": entry.submitted,
        "canceled": entry.canceled,
        "closed": entry.closed,
        "recorded_at_utc": entry.recorded_at_utc,
        "closed_at_utc": entry.closed_at_utc,
    })
}

fn age_secs_from_rfc3339(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| {
            Utc::now()
                .signed_duration_since(timestamp.with_timezone(&Utc))
                .to_std()
                .ok()
        })
        .map(|duration| duration.as_secs())
}

fn scan_report_payload(report: &OptionsScanReport) -> serde_json::Value {
    serde_json::json!({
        "underlying": report.underlying,
        "strategy": report.strategy,
        "outcome": match report.outcome {
            OptionsScanOutcome::Candidate => "candidate",
            OptionsScanOutcome::NoCandidate => "no_candidate",
        },
        "candidate_count": report.candidate_count,
        "reason": report.reason,
        "contracts": report.contract_count,
        "snapshots": report.snapshot_count,
        "scoreable": report.scoreable_count,
        "rejections": report.rejection_counts,
    })
}

fn selected_entry_payload(entry: &SelectedOptionsEntry) -> serde_json::Value {
    let descriptor = entry.descriptor();
    serde_json::json!({
        "strategy": descriptor.strategy,
        "underlying": descriptor.underlying,
        "candidate_type": descriptor.candidate_type,
        "symbols": descriptor.symbols,
        "score": descriptor.score,
        "premium_kind": descriptor.premium_kind.as_str(),
        "premium": descriptor.premium,
    })
}
