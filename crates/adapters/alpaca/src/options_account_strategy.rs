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
    actor::{DataActor, DataActorNative},
    cache::CacheApi,
    factories::OrderFactory,
    timer::TimeEvent,
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_infrastructure::sql::operational::StrategyStateMutation;
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
    options::regime::{RegimeContext, insert_regime_context},
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
        is_uncovered_option_permission_rejection, selected_open_orders_enabled,
        submission_block_for_selected,
    },
    options_entry_planner::{AlpacaOptionsEntryPlan, plan_selected_candidate, plan_selected_entry},
    options_lifecycle::OptionLifecycleRiskHandle,
    options_management::{
        AlpacaOptionsManagementConfig, CloseOrderMode, CloseQuote, close_attempts_exhausted,
        close_price_cushion_for_attempt, close_quote_from_ticks, close_reason,
        close_reprice_cooldown_remaining_secs, emit_management_snapshot, management_instrument_ids,
    },
    options_runtime::{
        AlpacaOptionsRuntimeConfig, OptionsCandidateSet, OptionsScanOutcome, OptionsScanReport,
        SelectedOptionsEntry,
    },
    runtime::{StrategyState, StrategyStateEntry, StrategyStateEntryDraft, emit_operator_event},
    spread_plan::{
        OptionSpreadPlan, selected_entry_spread_plan, spread_quote_subscription_params,
        strategy_state_vertical_spread_plan,
    },
    state_persistence::StrategyStatePersistenceHandle,
    strategy_state_entry::selected_entry_state_entry_draft,
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
    /// Regime context applied before candidate selection.
    pub regime_context: Option<RegimeContext>,
    ts_event: UnixNanos,
    ts_init: UnixNanos,
}

impl OptionsCandidateData {
    const TYPE_NAME: &'static str = "OptionsCandidateData";

    /// Creates a new custom data payload from an candidate set.
    #[must_use]
    pub fn new(
        candidates: OptionsCandidateSet,
        regime_context: Option<RegimeContext>,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Self {
        Self {
            candidates,
            regime_context,
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
        let mut payload = serde_json::json!({
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
        });
        insert_regime_context(&mut payload, self.regime_context.as_ref());
        Ok(serde_json::to_string(&payload)?)
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
                && self.regime_context == other.regime_context
        })
    }

    fn type_name_static() -> &'static str
    where
        Self: Sized,
    {
        Self::TYPE_NAME
    }
}

/// Configuration for [`AlpacaOptionsAccountStrategy`].
#[derive(Clone, Debug)]
pub struct AlpacaOptionsAccountStrategyConfig {
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
    /// Async state persistence boundary used when broker orders are enabled.
    pub state_persistence: Option<StrategyStatePersistenceHandle>,
    /// Async candidate-ledger persistence boundary used for scanner and strategy evidence.
    pub candidate_ledger_persistence: Option<CandidateLedgerPersistenceHandle>,
    /// Shared lifecycle-risk state from the account-activity poller.
    pub lifecycle_risk: Option<OptionLifecycleRiskHandle>,
    /// Management settings owned by the live strategy runtime.
    pub management: AlpacaOptionsManagementConfig,
}

impl AlpacaOptionsAccountStrategyConfig {
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
    close_order_mode: String,
    order_count: usize,
    accepted: usize,
    rejected: usize,
    recorded: bool,
}

/// Nautilus strategy responsible for converting selected option candidates into orders.
#[derive(Debug)]
pub struct AlpacaOptionsAccountStrategy {
    core: StrategyCore,
    config: AlpacaOptionsAccountStrategyConfig,
    state: StrategyState,
    pending_submissions: BTreeMap<String, PendingEntrySubmission>,
    pending_client_order_ids: BTreeMap<String, String>,
    pending_close_submissions: BTreeMap<String, PendingCloseSubmission>,
    pending_close_client_order_ids: BTreeMap<String, String>,
    submitted_underlying_keys: BTreeSet<String>,
    recorded_candidate_alert_keys: BTreeSet<String>,
    active_risk_quote_subscriptions: BTreeSet<InstrumentId>,
    candidate_quote_instrument_ids: BTreeSet<InstrumentId>,
    candidate_spread_instrument_ids: BTreeSet<InstrumentId>,
    active_spread_instrument_ids: BTreeSet<InstrumentId>,
}

impl AlpacaOptionsAccountStrategy {
    /// Creates a new [`AlpacaOptionsAccountStrategy`].
    #[must_use]
    pub fn new(config: AlpacaOptionsAccountStrategyConfig) -> Self {
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
            active_risk_quote_subscriptions: BTreeSet::new(),
            candidate_quote_instrument_ids: BTreeSet::new(),
            candidate_spread_instrument_ids: BTreeSet::new(),
            active_spread_instrument_ids: BTreeSet::new(),
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
        self.update_candidate_quote_subscriptions(&data.candidates);
        let Some(entry_plan) = plan_selected_candidate(&data.candidates) else {
            return Ok(None);
        };
        let entry = &entry_plan.entry;
        let regime_context = data.regime_context.as_ref();
        self.emit_selected_spread_quote_snapshot(&data.candidates.trade_date, &entry_plan);

        match entry_gate_decision(&self.config.admission, Utc::now()) {
            EntryGateDecision::Continue => {}
            EntryGateDecision::OutsideEntryWindow => {
                log::info!(
                    "Skipping Alpaca options entry: trade_date={} reason=outside_entry_window underlying={} strategy={}",
                    data.candidates.trade_date,
                    entry_plan.underlying,
                    entry_plan.strategy
                );
                emit_operator_event(
                    "entry_decision",
                    json!({
                        "action": "skipped",
                        "reason": "outside_entry_window",
                        "trade_date": data.candidates.trade_date,
                        "underlying": entry_plan.underlying.as_str(),
                        "strategy": entry_plan.strategy,
                    }),
                );
                self.record_selected_candidate_alert(
                    &data.candidates.trade_date,
                    entry,
                    "skipped",
                    None,
                    Some("outside_entry_window"),
                    None,
                    None,
                    &[],
                    regime_context,
                );
                return Ok(None);
            }
        }

        if let Some(context) = regime_context.filter(|context| context.dry_run_only) {
            log::info!(
                "Regime dry-run Alpaca options entry: trade_date={} underlying={} strategy={} symbols={} label={} codes={}",
                data.candidates.trade_date,
                entry_plan.underlying,
                entry_plan.strategy,
                entry_plan.symbols.join(","),
                context.label.as_str(),
                context.explanation_codes.join(",")
            );
            let mut payload = json!({
                "action": "dry_run",
                "reason": "regime_dry_run_only",
                "trade_date": data.candidates.trade_date,
                "underlying": entry_plan.underlying.as_str(),
                "strategy": entry_plan.strategy,
                "strategy_family": entry_plan.family.as_str(),
                "symbols": entry_plan.symbols.clone(),
                "planned_order_legs": entry_plan_payload_legs(&entry_plan),
                "score": entry_plan.score,
            });
            insert_regime_context(&mut payload, Some(context));
            emit_operator_event("entry_decision", payload);
            self.record_selected_candidate_alert(
                &data.candidates.trade_date,
                entry,
                "dry_run",
                None,
                Some("regime_dry_run_only"),
                None,
                None,
                &context.explanation_codes,
                regime_context,
            );
            self.emit_vertical_spread_order_draft(
                &data.candidates.trade_date,
                &entry_plan,
                "regime_dry_run_only",
            );
            return Ok(None);
        }

        if !selected_open_orders_enabled(&self.config.admission, entry) {
            log::info!(
                "Dry-run Alpaca options entry: underlying={} strategy={} symbols={} score={:.1}",
                entry_plan.underlying,
                entry_plan.strategy,
                entry_plan.symbols.join(","),
                entry_plan.score
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "dry_run",
                    "reason": "open_orders_disabled",
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry_plan.underlying.as_str(),
                    "strategy": entry_plan.strategy,
                    "strategy_family": entry_plan.family.as_str(),
                    "symbols": entry_plan.symbols.clone(),
                    "planned_order_legs": entry_plan_payload_legs(&entry_plan),
                    "score": entry_plan.score,
                }),
            );
            self.record_selected_candidate_alert(
                &data.candidates.trade_date,
                entry,
                "dry_run",
                None,
                Some("open_orders_disabled"),
                None,
                None,
                &[],
                regime_context,
            );
            self.emit_vertical_spread_order_draft(
                &data.candidates.trade_date,
                &entry_plan,
                "open_orders_disabled",
            );
            return Ok(None);
        }

        if let Some(block) = self.lifecycle_submission_block(entry) {
            self.log_entry_block(&data.candidates.trade_date, entry, &block, regime_context);
            return Ok(None);
        }
        if let Some(block) = self.selected_entry_quote_freshness_block(entry) {
            self.log_entry_block(&data.candidates.trade_date, entry, &block, regime_context);
            return Ok(None);
        }

        if self.config.admission.open_orders_enabled && !self.candidate_ledger_persistence_ready() {
            log::error!(
                "Skipping Alpaca options entry: trade_date={} reason=candidate_ledger_unhealthy underlying={} strategy={} symbols={}",
                data.candidates.trade_date,
                entry_plan.underlying,
                entry_plan.strategy,
                entry_plan.symbols.join(",")
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "skipped",
                    "reason": "candidate_ledger_unhealthy",
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry_plan.underlying.as_str(),
                    "strategy": entry_plan.strategy,
                    "strategy_family": entry_plan.family.as_str(),
                    "symbols": entry_plan.symbols.clone(),
                    "planned_order_legs": entry_plan_payload_legs(&entry_plan),
                }),
            );
            return Ok(None);
        }

        if self.config.admission.open_orders_enabled && !self.state_persistence_ready() {
            log::error!(
                "Skipping Alpaca options entry: trade_date={} reason=state_persistence_unhealthy underlying={} strategy={} symbols={}",
                data.candidates.trade_date,
                entry_plan.underlying,
                entry_plan.strategy,
                entry_plan.symbols.join(",")
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "skipped",
                    "reason": "state_persistence_unhealthy",
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry_plan.underlying.as_str(),
                    "strategy": entry_plan.strategy,
                    "strategy_family": entry_plan.family.as_str(),
                    "symbols": entry_plan.symbols.clone(),
                    "planned_order_legs": entry_plan_payload_legs(&entry_plan),
                }),
            );
            self.record_selected_candidate_alert(
                &data.candidates.trade_date,
                entry,
                "skipped",
                None,
                Some("state_persistence_unhealthy"),
                None,
                None,
                &[],
                regime_context,
            );
            return Ok(None);
        }

        let snapshot = self.entry_admission_snapshot(entry)?;
        if let Some(block) = submission_block_for_selected(
            &self.config.admission,
            &self.state,
            entry,
            &data.candidates.trade_date,
            &snapshot,
        ) {
            self.log_entry_block(&data.candidates.trade_date, entry, &block, regime_context);
            return Ok(None);
        }

        let underlying_key = submitted_underlying_key(&data.candidates.trade_date, entry);
        if !self
            .submitted_underlying_keys
            .insert(underlying_key.clone())
        {
            log::info!(
                "Skipping duplicate Alpaca options entry: key={} strategy={} symbols={}",
                underlying_key,
                entry_plan.strategy,
                entry_plan.symbols.join(",")
            );
            emit_operator_event(
                "entry_decision",
                json!({
                    "action": "skipped",
                    "reason": "duplicate_pending_submission",
                    "key": underlying_key,
                    "trade_date": data.candidates.trade_date,
                    "underlying": entry_plan.underlying.as_str(),
                    "strategy": entry_plan.strategy,
                    "strategy_family": entry_plan.family.as_str(),
                    "symbols": entry_plan.symbols.clone(),
                    "planned_order_legs": entry_plan_payload_legs(&entry_plan),
                }),
            );
            self.record_selected_candidate_alert(
                &data.candidates.trade_date,
                entry,
                "skipped",
                None,
                Some("duplicate_pending_submission"),
                None,
                None,
                &[],
                regime_context,
            );
            return Ok(None);
        }

        let order_list_id =
            entry_order_list_id(&data.candidates.trade_date, &entry_plan.underlying);
        match self.submit_entry_plan_with_trade_date(
            entry_plan,
            &data.candidates.trade_date,
            &order_list_id,
            regime_context,
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
        let Some(entry_plan) = plan_selected_candidate(&candidates) else {
            return Ok(None);
        };
        self.submit_entry_plan(entry_plan, order_list_id).map(Some)
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
        let Some(entry_plan) = plan_selected_entry(entry) else {
            anyhow::bail!("selected entry could not be planned");
        };
        self.submit_entry_plan(entry_plan, order_list_id)
    }

    fn submit_entry_plan(
        &mut self,
        entry_plan: AlpacaOptionsEntryPlan,
        order_list_id: &str,
    ) -> anyhow::Result<AlpacaOptionsSubmission> {
        let orders = self.build_entry_orders(&entry_plan.entry, order_list_id)?;
        let order_count = orders.len();
        self.submit_entry_orders(orders, order_list_id)?;

        Ok(AlpacaOptionsSubmission {
            entry: entry_plan.entry,
            order_list_id: order_list_id.to_string(),
            order_count,
        })
    }

    fn submit_entry_plan_with_trade_date(
        &mut self,
        entry_plan: AlpacaOptionsEntryPlan,
        trade_date: &str,
        order_list_id: &str,
        regime_context: Option<&RegimeContext>,
    ) -> anyhow::Result<AlpacaOptionsSubmission> {
        let orders = self.build_entry_orders(&entry_plan.entry, order_list_id)?;
        let client_order_ids = orders
            .iter()
            .map(|order| order.client_order_id().to_string())
            .collect::<Vec<_>>();
        let order_count = orders.len();
        self.record_pending_submission(
            entry_plan.entry.clone(),
            trade_date.to_string(),
            order_list_id.to_string(),
            order_count,
            client_order_ids,
        );
        self.record_selected_candidate_alert(
            trade_date,
            &entry_plan.entry,
            "submitted",
            Some(order_list_id),
            None,
            None,
            None,
            &[],
            regime_context,
        );

        if let Err(error) = self.submit_entry_orders(orders, order_list_id) {
            self.remove_pending_submission(order_list_id);
            return Err(error);
        }

        Ok(AlpacaOptionsSubmission {
            entry: entry_plan.entry,
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
        self.refresh_active_risk_quote_subscriptions();
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

        self.emit_vertical_spread_close_order_draft(&entry);

        let stale_quote_symbols = self.stale_active_risk_quote_symbols(&entry);
        if !stale_quote_symbols.is_empty() {
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "active_risk_quote_stale",
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "symbols": stale_quote_symbols,
                    "limit_secs": self.config.management.active_risk_quote_stale_secs,
                }),
            );
            return Ok(());
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

        if !self.config.management.close_orders_enabled {
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": "close_orders_disabled",
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "trigger": trigger,
                    "close_orders_enabled": self.config.management.close_orders_enabled,
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
        if self.config.management.close_order_mode == CloseOrderMode::OptionSpread {
            self.submit_spread_close_entry(entry, trigger, close_order_list_id)?;
            return Ok(());
        }

        let close_price_cushion = close_price_cushion_for_attempt(&self.config.management, &entry);
        let submit_quote = close_quote.with_price_cushion(close_price_cushion);
        self.submit_close_entry(
            entry,
            submit_quote,
            trigger,
            close_order_list_id,
            close_price_cushion,
        )?;
        Ok(())
    }

    fn manage_working_entry_order(&mut self, entry: &StrategyStateEntry) -> anyhow::Result<()> {
        if !self.config.management.close_orders_enabled
            || self.config.management.stale_entry_secs == 0
        {
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
        if !self.config.management.close_orders_enabled
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
        close_price_cushion: f64,
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
            close_reason.clone(),
            CloseOrderMode::LegacyLegOrderList.as_str().to_string(),
            order_count,
            client_order_ids,
        );
        emit_operator_event(
            "management_action",
            json!({
                "action": "close_submit",
                "underlying": &entry.underlying,
                "strategy": &entry.strategy,
                "order_list_id": &entry.order_list_id,
                "close_order_list_id": &close_order_list_id,
                "close_reason": &close_reason,
                "close_attempt": entry.close_attempts.saturating_add(1),
                "close_order_mode": CloseOrderMode::LegacyLegOrderList.as_str(),
                "close_price_cushion": close_price_cushion,
                "close_reprice_step": self.config.management.close_reprice_step,
                "max_close_price_cushion": self.config.management.max_close_price_cushion,
                "close_debit": quote.debit,
            }),
        );

        if let Err(error) = self.submit_close_orders(orders, &close_order_list_id) {
            self.remove_pending_close_submission(&close_order_list_id);
            return Err(error);
        }
        Ok(())
    }

    fn submit_spread_close_entry(
        &mut self,
        entry: StrategyStateEntry,
        close_reason: String,
        close_order_list_id: String,
    ) -> anyhow::Result<()> {
        let mut draft =
            match self.vertical_spread_close_order_draft(&entry, &close_order_list_id)? {
                Some(draft) => draft,
                None => {
                    emit_operator_event(
                        "management_block",
                        json!({
                            "action": "close_blocked",
                            "reason": "spread_close_order_mode_requires_vertical_entry",
                            "underlying": entry.underlying,
                            "strategy": entry.strategy,
                            "order_list_id": entry.order_list_id,
                            "close_order_list_id": close_order_list_id,
                            "close_reason": close_reason,
                            "close_order_mode": CloseOrderMode::OptionSpread.as_str(),
                            "current_broker_submit_path": "single_option_spread_order",
                            "fallback_broker_submit_path": "legacy_leg_order_list",
                            "submitted": false,
                        }),
                    );
                    return Ok(());
                }
            };
        let Some(order) = draft.order.take() else {
            emit_operator_event(
                "management_block",
                json!({
                    "action": "close_blocked",
                    "reason": draft
                        .pricing
                        .unavailable_reason
                        .unwrap_or("spread_close_order_unavailable"),
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "order_list_id": entry.order_list_id,
                    "close_order_list_id": close_order_list_id,
                    "close_reason": close_reason,
                    "close_order_mode": CloseOrderMode::OptionSpread.as_str(),
                    "spread_instrument_id": draft.plan.instrument_id.to_string(),
                    "spread_symbol": draft.plan.raw_symbol.to_string(),
                    "pricing_source": draft.pricing.pricing_source,
                    "spread_bid": draft.pricing.spread_bid,
                    "spread_ask": draft.pricing.spread_ask,
                    "spread_mid": draft.pricing.spread_mid,
                    "quote_age_secs": draft.pricing.quote_age_secs,
                    "quote_stale_limit_secs": draft.pricing.quote_stale_limit_secs,
                    "close_price_cushion": draft.pricing.close_price_cushion,
                    "submitted": false,
                }),
            );
            return Ok(());
        };
        let client_order_id = order.client_order_id().to_string();
        self.record_pending_close_submission(
            entry.order_list_id.clone(),
            close_order_list_id.clone(),
            close_reason.clone(),
            CloseOrderMode::OptionSpread.as_str().to_string(),
            1,
            vec![client_order_id.clone()],
        );
        emit_operator_event(
            "management_action",
            json!({
                "action": "spread_close_submit",
                "underlying": &entry.underlying,
                "strategy": &entry.strategy,
                "order_list_id": &entry.order_list_id,
                "close_order_list_id": &close_order_list_id,
                "close_reason": &close_reason,
                "close_attempt": entry.close_attempts.saturating_add(1),
                "close_order_mode": CloseOrderMode::OptionSpread.as_str(),
                "spread_instrument_id": draft.plan.instrument_id.to_string(),
                "spread_symbol": draft.plan.raw_symbol.to_string(),
                "legs": draft
                    .plan
                    .legs
                    .iter()
                    .map(|leg| {
                        json!({
                            "symbol": leg.symbol.as_str(),
                            "instrument_id": leg.instrument_id.to_string(),
                            "ratio": leg.ratio,
                        })
                    })
                    .collect::<Vec<_>>(),
                "client_order_id": client_order_id,
                "close_side": "sell",
                "reduce_only": true,
                "pricing_source": draft.pricing.pricing_source,
                "signed_close_limit": draft.pricing.signed_limit_price,
                "close_price_cushion": draft.pricing.close_price_cushion,
                "spread_bid": draft.pricing.spread_bid,
                "spread_ask": draft.pricing.spread_ask,
                "spread_mid": draft.pricing.spread_mid,
                "quote_age_secs": draft.pricing.quote_age_secs,
                "quote_stale_limit_secs": draft.pricing.quote_stale_limit_secs,
            }),
        );

        if let Err(error) = self.submit_close_orders(vec![order], &close_order_list_id) {
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

    fn update_candidate_quote_subscriptions(&mut self, candidates: &OptionsCandidateSet) {
        let candidate_limit = self.config.management.active_risk_candidate_quote_limit;
        self.candidate_quote_instrument_ids =
            candidate_quote_instrument_ids(candidates, candidate_limit);
        self.candidate_spread_instrument_ids =
            self.register_candidate_spread_instruments(candidates, candidate_limit);
        self.refresh_active_risk_quote_subscriptions();
    }

    fn refresh_active_risk_quote_subscriptions(&mut self) {
        let active_quote_instrument_ids = self
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
        self.active_spread_instrument_ids = self.register_active_spread_instruments();

        let desired = active_quote_instrument_ids
            .into_iter()
            .chain(self.candidate_quote_instrument_ids.iter().copied())
            .chain(self.candidate_spread_instrument_ids.iter().copied())
            .chain(self.active_spread_instrument_ids.iter().copied())
            .collect::<BTreeSet<_>>();

        for instrument_id in desired
            .difference(&self.active_risk_quote_subscriptions)
            .copied()
            .collect::<Vec<_>>()
        {
            self.subscribe_quotes(
                instrument_id,
                self.config.client_id,
                self.quote_subscription_params(instrument_id),
            );
        }
        for instrument_id in self
            .active_risk_quote_subscriptions
            .difference(&desired)
            .copied()
            .collect::<Vec<_>>()
        {
            self.unsubscribe_quotes(instrument_id, self.config.client_id, None);
        }
        if desired != self.active_risk_quote_subscriptions {
            emit_operator_event(
                "active_risk_quote_cache",
                json!({
                    "active_entry_quote_symbols": active_entry_quote_symbols(&self.state),
                    "active_entry_spread_symbols_count": self.active_spread_instrument_ids.len(),
                    "candidate_quote_symbols_count": self.candidate_quote_instrument_ids.len(),
                    "candidate_spread_symbols_count": self.candidate_spread_instrument_ids.len(),
                    "candidate_quote_limit": self.config.management.active_risk_candidate_quote_limit,
                    "active_entry_spread_symbols": instrument_symbols(&self.active_spread_instrument_ids),
                    "candidate_quote_symbols": instrument_symbols(&self.candidate_quote_instrument_ids),
                    "candidate_spread_symbols": instrument_symbols(&self.candidate_spread_instrument_ids),
                    "subscribed_symbols": instrument_symbols(&desired),
                    "subscribed_count": desired.len(),
                }),
            );
        }
        self.active_risk_quote_subscriptions = desired;
    }

    fn quote_subscription_params(
        &self,
        instrument_id: InstrumentId,
    ) -> Option<nautilus_core::Params> {
        (self
            .candidate_spread_instrument_ids
            .contains(&instrument_id)
            || self.active_spread_instrument_ids.contains(&instrument_id))
        .then(spread_quote_subscription_params)
    }

    fn register_active_spread_instruments(&mut self) -> BTreeSet<InstrumentId> {
        let ts_init = self.clock().timestamp_ns();
        let plans = self
            .state
            .entries
            .iter()
            .filter(|entry| entry.is_active())
            .filter_map(|entry| match strategy_state_vertical_spread_plan(entry, ts_init) {
                Ok(Some(plan)) => Some(plan),
                Ok(None) => None,
                Err(error) => {
                    log::warn!(
                        "Failed to rebuild active Nautilus spread plan: order_list_id={} underlying={} strategy={} symbols={} error={error:#}",
                        entry.order_list_id,
                        entry.underlying,
                        entry.strategy,
                        entry.symbols().join(","),
                    );
                    None
                }
            })
            .collect::<Vec<_>>();
        self.register_spread_plans(plans)
    }

    fn register_candidate_spread_instruments(
        &mut self,
        candidates: &OptionsCandidateSet,
        limit: usize,
    ) -> BTreeSet<InstrumentId> {
        if limit == 0 {
            return BTreeSet::new();
        }

        let ts_init = self.clock().timestamp_ns();
        let plans = candidates
            .ranked_entries()
            .iter()
            .take(limit)
            .filter_map(|entry| match selected_entry_spread_plan(entry, ts_init) {
                Ok(Some(plan)) => Some(plan),
                Ok(None) => None,
                Err(error) => {
                    log::warn!(
                        "Failed to build Nautilus spread plan: underlying={} strategy={} symbols={} error={error:#}",
                        entry.underlying(),
                        entry.strategy_name(),
                        entry.option_symbols().join(","),
                    );
                    None
                }
            })
            .collect::<Vec<_>>();
        self.register_spread_plans(plans)
    }

    fn register_spread_plans(&mut self, plans: Vec<OptionSpreadPlan>) -> BTreeSet<InstrumentId> {
        if plans.is_empty() {
            return BTreeSet::new();
        }

        let cache_rc = DataActorNative::cache_rc(self);
        let mut cache = cache_rc.borrow_mut();
        let mut instrument_ids = BTreeSet::new();
        for plan in plans {
            if cache.instrument(&plan.instrument_id).is_none()
                && let Err(error) = cache.add_instrument(plan.instrument.clone())
            {
                log::error!(
                    "Failed to cache Nautilus spread instrument {}: {error:#}",
                    plan.instrument_id
                );
                continue;
            }
            instrument_ids.insert(plan.instrument_id);
        }

        instrument_ids
    }

    fn emit_selected_spread_quote_snapshot(
        &self,
        trade_date: &str,
        entry_plan: &AlpacaOptionsEntryPlan,
    ) {
        let entry = &entry_plan.entry;
        let plan = match selected_entry_spread_plan(entry, UnixNanos::default()) {
            Ok(Some(plan)) => plan,
            Ok(None) => return,
            Err(error) => {
                emit_operator_event(
                    "candidate_spread_quote",
                    json!({
                        "action": "unavailable",
                        "reason": "spread_plan_error",
                        "trade_date": trade_date,
                        "underlying": entry_plan.underlying.as_str(),
                        "strategy": entry_plan.strategy,
                        "strategy_family": entry_plan.family.as_str(),
                        "symbols": entry_plan.symbols.clone(),
                        "planned_order_legs": entry_plan_payload_legs(entry_plan),
                        "error": error.to_string(),
                    }),
                );
                return;
            }
        };

        let cache = self.cache();
        let quote = cache.quote(&plan.instrument_id);
        let mut payload = spread_quote_snapshot_payload(trade_date, entry_plan, &plan);
        match quote {
            Some(quote) => {
                let mid = f64::midpoint(quote.bid_price.as_f64(), quote.ask_price.as_f64());
                insert_value_field(&mut payload, "action", json!("observed"));
                insert_value_field(
                    &mut payload,
                    "pricing_source",
                    json!("nautilus_spread_quote"),
                );
                insert_value_field(&mut payload, "spread_bid", json!(quote.bid_price.as_f64()));
                insert_value_field(&mut payload, "spread_ask", json!(quote.ask_price.as_f64()));
                insert_value_field(&mut payload, "spread_mid", json!(mid));
                insert_value_field(
                    &mut payload,
                    "scanner_vs_spread_mid",
                    json!(plan.scanner_premium - mid.abs()),
                );
                insert_value_field(
                    &mut payload,
                    "quote_ts_event",
                    json!(quote.ts_event.as_u64()),
                );
                insert_value_field(&mut payload, "quote_ts_init", json!(quote.ts_init.as_u64()));
            }
            None => {
                insert_value_field(&mut payload, "action", json!("unavailable"));
                insert_value_field(&mut payload, "reason", json!("spread_quote_missing"));
                insert_value_field(&mut payload, "pricing_source", json!("scanner_snapshot"));
            }
        }
        emit_operator_event("candidate_spread_quote", payload);
    }

    fn emit_vertical_spread_order_draft(
        &mut self,
        trade_date: &str,
        entry_plan: &AlpacaOptionsEntryPlan,
        dry_run_reason: &str,
    ) {
        if !entry_plan.is_vertical_spread() {
            return;
        }

        let entry = &entry_plan.entry;
        let order_list_id = entry_order_list_id(trade_date, &entry_plan.underlying);
        match self.vertical_spread_order_draft(entry, &order_list_id) {
            Ok(Some(draft)) => emit_operator_event(
                "entry_spread_order_draft",
                vertical_spread_order_draft_payload(trade_date, entry_plan, dry_run_reason, &draft),
            ),
            Ok(None) => {}
            Err(error) => emit_operator_event(
                "entry_spread_order_draft",
                json!({
                    "action": "unavailable",
                    "reason": "spread_order_draft_error",
                    "dry_run_reason": dry_run_reason,
                    "trade_date": trade_date,
                    "underlying": entry_plan.underlying.as_str(),
                    "strategy": entry_plan.strategy,
                    "strategy_family": entry_plan.family.as_str(),
                    "symbols": entry_plan.symbols.clone(),
                    "planned_order_legs": entry_plan_payload_legs(entry_plan),
                    "error": error.to_string(),
                }),
            ),
        }
    }

    fn vertical_spread_order_draft(
        &mut self,
        entry: &SelectedOptionsEntry,
        order_list_id: &str,
    ) -> anyhow::Result<Option<VerticalSpreadOrderDraft>> {
        let Some(plan) = selected_entry_spread_plan(entry, self.clock().timestamp_ns())? else {
            return Ok(None);
        };
        if !is_vertical_spread_plan(&plan) {
            return Ok(None);
        }

        self.cache_spread_plan(&plan)?;
        let pricing = self.spread_entry_order_pricing(&plan);
        let mut order_api = self.order();
        let order = build_vertical_spread_entry_order(
            &mut order_api,
            &plan,
            order_list_id,
            self.config.quantity,
            pricing.signed_limit_price,
        )?;
        Ok(Some(VerticalSpreadOrderDraft {
            plan,
            order,
            pricing,
        }))
    }

    fn cache_spread_plan(&mut self, plan: &OptionSpreadPlan) -> anyhow::Result<()> {
        let cache_rc = DataActorNative::cache_rc(self);
        let mut cache = cache_rc.borrow_mut();
        if cache.instrument(&plan.instrument_id).is_none() {
            cache.add_instrument(plan.instrument.clone())?;
        }
        Ok(())
    }

    fn spread_entry_order_pricing(&self, plan: &OptionSpreadPlan) -> SpreadEntryOrderPricing {
        let cache = self.cache();
        if let Some(quote) = cache.quote(&plan.instrument_id) {
            let bid = quote.bid_price.as_f64();
            let ask = quote.ask_price.as_f64();
            let mid = f64::midpoint(bid, ask);
            if ask != 0.0 {
                return SpreadEntryOrderPricing {
                    pricing_source: "nautilus_spread_quote",
                    signed_limit_price: ask,
                    spread_bid: Some(bid),
                    spread_ask: Some(ask),
                    spread_mid: Some(mid),
                    quote_ts_event: Some(quote.ts_event),
                    quote_ts_init: Some(quote.ts_init),
                };
            }
        }

        SpreadEntryOrderPricing {
            pricing_source: "scanner_snapshot_fallback",
            signed_limit_price: signed_scanner_spread_price(plan),
            spread_bid: None,
            spread_ask: None,
            spread_mid: None,
            quote_ts_event: None,
            quote_ts_init: None,
        }
    }

    fn emit_vertical_spread_close_order_draft(&mut self, entry: &StrategyStateEntry) {
        let close_order_list_id = close_order_list_id(entry);
        match self.vertical_spread_close_order_draft(entry, &close_order_list_id) {
            Ok(Some(draft)) => emit_operator_event(
                "close_spread_order_draft",
                vertical_spread_close_order_draft_payload(
                    entry,
                    &close_order_list_id,
                    &draft,
                    self.config.management.close_order_mode,
                ),
            ),
            Ok(None) => {}
            Err(error) => emit_operator_event(
                "close_spread_order_draft",
                json!({
                    "action": "unavailable",
                    "reason": "spread_close_order_draft_error",
                    "trade_date": entry.trade_date,
                    "underlying": entry.underlying,
                    "strategy": entry.strategy,
                    "entry_order_list_id": entry.order_list_id,
                    "draft_close_order_list_id": close_order_list_id,
                    "symbols": entry.symbols(),
                    "error": error.to_string(),
                    "current_broker_submit_path": self.config.management.close_order_mode.as_str(),
                    "draft_broker_submit_path": "single_option_spread_order",
                    "submitted": false,
                }),
            ),
        }
    }

    fn vertical_spread_close_order_draft(
        &mut self,
        entry: &StrategyStateEntry,
        close_order_list_id: &str,
    ) -> anyhow::Result<Option<VerticalSpreadCloseOrderDraft>> {
        let Some(plan) = strategy_state_vertical_spread_plan(entry, self.clock().timestamp_ns())?
        else {
            return Ok(None);
        };
        if !is_vertical_spread_plan(&plan) {
            return Ok(None);
        }

        self.cache_spread_plan(&plan)?;
        let close_price_cushion = close_price_cushion_for_attempt(&self.config.management, entry);
        let pricing = self.spread_close_order_pricing(&plan, close_price_cushion);
        let order = if let Some(signed_limit_price) = pricing.signed_limit_price {
            let mut order_api = self.order();
            Some(build_vertical_spread_close_order(
                &mut order_api,
                &plan,
                close_order_list_id,
                entry.quantity,
                signed_limit_price,
            )?)
        } else {
            None
        };
        Ok(Some(VerticalSpreadCloseOrderDraft {
            plan,
            order,
            pricing,
        }))
    }

    fn spread_close_order_pricing(
        &self,
        plan: &OptionSpreadPlan,
        close_price_cushion: f64,
    ) -> SpreadCloseOrderPricing {
        let cache = self.cache();
        let Some(quote) = cache.quote(&plan.instrument_id) else {
            return SpreadCloseOrderPricing {
                pricing_source: "nautilus_spread_quote",
                signed_limit_price: None,
                unavailable_reason: Some("spread_quote_missing"),
                spread_bid: None,
                spread_ask: None,
                spread_mid: None,
                quote_ts_event: None,
                quote_ts_init: None,
                quote_age_secs: None,
                quote_stale_limit_secs: Some(self.config.management.active_risk_quote_stale_secs),
                close_price_cushion,
            };
        };

        let bid = quote.bid_price.as_f64();
        let ask = quote.ask_price.as_f64();
        let mid = f64::midpoint(bid, ask);
        let quote_age = quote_age_secs(quote.ts_event, Utc::now());
        let stale_limit = self.config.management.active_risk_quote_stale_secs;
        let unavailable_reason =
            if stale_limit > 0 && quote_age.is_some_and(|age| age > stale_limit) {
                Some("spread_quote_stale")
            } else if bid == 0.0 {
                Some("spread_quote_zero_bid")
            } else {
                None
            };

        SpreadCloseOrderPricing {
            pricing_source: "nautilus_spread_quote",
            signed_limit_price: unavailable_reason
                .is_none()
                .then_some(bid - close_price_cushion),
            unavailable_reason,
            spread_bid: Some(bid),
            spread_ask: Some(ask),
            spread_mid: Some(mid),
            quote_ts_event: Some(quote.ts_event),
            quote_ts_init: Some(quote.ts_init),
            quote_age_secs: quote_age,
            quote_stale_limit_secs: Some(stale_limit),
            close_price_cushion,
        }
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
        close_order_mode: String,
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
                close_order_mode,
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
                    pending.close_order_mode.clone(),
                ));
                pending.recorded = true;
            }
            should_remove = pending.accepted + pending.rejected >= pending.order_count;
        }

        if let Some((
            entry_order_list_id,
            close_order_list_id,
            parent_order_id,
            close_reason,
            close_order_mode,
        )) = state_update
        {
            let mutation = if let Some(entry) = self.state_entry_mut(&entry_order_list_id) {
                entry.record_close_submission(
                    close_order_list_id.clone(),
                    parent_order_id.clone(),
                    close_reason.clone(),
                    close_order_mode.clone(),
                );
                Some(close_accepted_state_mutation(
                    event,
                    entry,
                    &close_order_list_id,
                    parent_order_id.as_deref(),
                    &close_reason,
                    &close_order_mode,
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
                draft = Some(selected_entry_state_entry_draft(
                    &pending.entry,
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
                    selected_entry_state_entry_draft(
                        &pending.entry,
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
        let open_order_count = open_broker_order_group_count(&cache);
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

    fn selected_entry_quote_freshness_block(
        &self,
        entry: &SelectedOptionsEntry,
    ) -> Option<SubmissionBlock> {
        if self.config.management.active_risk_quote_stale_secs == 0 {
            return None;
        }
        let stale_symbols = entry
            .option_symbols()
            .into_iter()
            .filter(|symbol| {
                alpaca_instrument_id(symbol)
                    .ok()
                    .and_then(|instrument_id| self.cache().quote(&instrument_id))
                    .is_some_and(|quote| {
                        quote_age_secs(quote.ts_event, Utc::now()).is_some_and(|age| {
                            age > self.config.management.active_risk_quote_stale_secs
                        })
                    })
            })
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        (!stale_symbols.is_empty()).then(|| SubmissionBlock {
            reason: "active_risk_quote_stale".to_string(),
            current: None,
            limit: None,
            details: stale_symbols
                .into_iter()
                .map(|symbol| format!("symbol={symbol}"))
                .collect(),
        })
    }

    fn stale_active_risk_quote_symbols(&self, entry: &StrategyStateEntry) -> Vec<String> {
        if self.config.management.active_risk_quote_stale_secs == 0 {
            return Vec::new();
        }
        entry
            .symbols()
            .into_iter()
            .filter(|symbol| {
                alpaca_instrument_id(symbol)
                    .ok()
                    .and_then(|instrument_id| self.cache().quote(&instrument_id))
                    .is_some_and(|quote| {
                        quote_age_secs(quote.ts_event, Utc::now()).is_some_and(|age| {
                            age > self.config.management.active_risk_quote_stale_secs
                        })
                    })
            })
            .map(ToString::to_string)
            .collect()
    }

    fn log_entry_block(
        &mut self,
        trade_date: &str,
        entry: &SelectedOptionsEntry,
        block: &SubmissionBlock,
        regime_context: Option<&RegimeContext>,
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
        let mut payload = json!({
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
        });
        insert_regime_context(&mut payload, regime_context);
        emit_operator_event("entry_decision", payload);
        self.record_selected_candidate_alert(
            trade_date,
            entry,
            "selected_but_blocked",
            None,
            Some(&block.reason),
            block.current,
            block.limit,
            &block.details,
            regime_context,
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
        regime_context: Option<&RegimeContext>,
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
        insert_regime_context(&mut payload, regime_context);
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
            if self.config.admission.open_orders_enabled {
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

nautilus_strategy!(AlpacaOptionsAccountStrategy, {
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

impl DataActor for AlpacaOptionsAccountStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.subscribe_data(OptionsCandidateData::data_type(), None, None);
        self.refresh_active_risk_quote_subscriptions();
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
            .active_risk_quote_subscriptions
            .iter()
            .copied()
            .collect::<Vec<_>>()
        {
            self.unsubscribe_quotes(instrument_id, self.config.client_id, None);
        }
        self.active_risk_quote_subscriptions.clear();
        self.candidate_quote_instrument_ids.clear();
        self.candidate_spread_instrument_ids.clear();
        self.active_spread_instrument_ids.clear();
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

fn candidate_quote_instrument_ids(
    candidates: &OptionsCandidateSet,
    limit: usize,
) -> BTreeSet<InstrumentId> {
    if limit == 0 {
        return BTreeSet::new();
    }
    candidates
        .ranked_entries()
        .iter()
        .take(limit)
        .flat_map(SelectedOptionsEntry::option_symbols)
        .filter_map(|symbol| alpaca_instrument_id(symbol).ok())
        .collect()
}

fn entry_plan_payload_legs(entry_plan: &AlpacaOptionsEntryPlan) -> Vec<Value> {
    entry_plan
        .order_legs
        .iter()
        .map(|leg| {
            json!({
                "symbol": leg.symbol.as_str(),
                "side": leg.side.as_str(),
            })
        })
        .collect()
}

fn spread_quote_snapshot_payload(
    trade_date: &str,
    entry_plan: &AlpacaOptionsEntryPlan,
    plan: &OptionSpreadPlan,
) -> Value {
    json!({
        "trade_date": trade_date,
        "underlying": plan.underlying.as_str(),
        "strategy": plan.strategy.as_str(),
        "strategy_family": entry_plan.family.as_str(),
        "symbols": entry_plan.symbols.clone(),
        "planned_order_legs": entry_plan_payload_legs(entry_plan),
        "spread_instrument_id": plan.instrument_id.to_string(),
        "spread_symbol": plan.raw_symbol.to_string(),
        "legs": plan
            .legs
            .iter()
            .map(|leg| {
                json!({
                    "symbol": leg.symbol.as_str(),
                    "instrument_id": leg.instrument_id.to_string(),
                    "ratio": leg.ratio,
                })
            })
            .collect::<Vec<_>>(),
        "scanner_premium_kind": plan.scanner_premium_kind.as_str(),
        "scanner_premium": plan.scanner_premium,
        "vega_pricing_enabled": false,
    })
}

struct SpreadEntryOrderPricing {
    pricing_source: &'static str,
    signed_limit_price: f64,
    spread_bid: Option<f64>,
    spread_ask: Option<f64>,
    spread_mid: Option<f64>,
    quote_ts_event: Option<UnixNanos>,
    quote_ts_init: Option<UnixNanos>,
}

struct VerticalSpreadOrderDraft {
    plan: OptionSpreadPlan,
    order: OrderAny,
    pricing: SpreadEntryOrderPricing,
}

struct SpreadCloseOrderPricing {
    pricing_source: &'static str,
    signed_limit_price: Option<f64>,
    unavailable_reason: Option<&'static str>,
    spread_bid: Option<f64>,
    spread_ask: Option<f64>,
    spread_mid: Option<f64>,
    quote_ts_event: Option<UnixNanos>,
    quote_ts_init: Option<UnixNanos>,
    quote_age_secs: Option<u64>,
    quote_stale_limit_secs: Option<u64>,
    close_price_cushion: f64,
}

struct VerticalSpreadCloseOrderDraft {
    plan: OptionSpreadPlan,
    order: Option<OrderAny>,
    pricing: SpreadCloseOrderPricing,
}

fn vertical_spread_order_draft_payload(
    trade_date: &str,
    entry_plan: &AlpacaOptionsEntryPlan,
    dry_run_reason: &str,
    draft: &VerticalSpreadOrderDraft,
) -> Value {
    let scanner_signed_price = signed_scanner_spread_price(&draft.plan);
    let mut payload = json!({
        "action": "drafted",
        "dry_run_reason": dry_run_reason,
        "trade_date": trade_date,
        "underlying": draft.plan.underlying.as_str(),
        "strategy": draft.plan.strategy.as_str(),
        "strategy_family": entry_plan.family.as_str(),
        "symbols": entry_plan.symbols.clone(),
        "planned_order_legs": entry_plan_payload_legs(entry_plan),
        "spread_instrument_id": draft.plan.instrument_id.to_string(),
        "spread_symbol": draft.plan.raw_symbol.to_string(),
        "legs": draft
            .plan
            .legs
            .iter()
            .map(|leg| {
                json!({
                    "symbol": leg.symbol.as_str(),
                    "instrument_id": leg.instrument_id.to_string(),
                    "ratio": leg.ratio,
                })
            })
            .collect::<Vec<_>>(),
        "order": {
            "client_order_id": draft.order.client_order_id().to_string(),
            "instrument_id": draft.order.instrument_id().to_string(),
            "side": format!("{:?}", draft.order.order_side()).to_ascii_lowercase(),
            "quantity": draft.order.quantity().as_f64(),
            "signed_limit_price": draft.order.price().map(|price| price.as_f64()),
            "reduce_only": draft.order.is_reduce_only(),
        },
        "pricing_source": draft.pricing.pricing_source,
        "scanner_premium_kind": draft.plan.scanner_premium_kind.as_str(),
        "scanner_premium": draft.plan.scanner_premium,
        "scanner_signed_price": scanner_signed_price,
        "scanner_vs_order_limit": scanner_signed_price - draft.pricing.signed_limit_price,
        "current_broker_submit_path": "legacy_leg_order_list",
        "draft_broker_submit_path": "single_option_spread_order",
        "submitted": false,
        "vega_pricing_enabled": false,
    });
    if let Some(value) = draft.pricing.spread_bid {
        insert_value_field(&mut payload, "spread_bid", json!(value));
    }
    if let Some(value) = draft.pricing.spread_ask {
        insert_value_field(&mut payload, "spread_ask", json!(value));
    }
    if let Some(value) = draft.pricing.spread_mid {
        insert_value_field(&mut payload, "spread_mid", json!(value));
    }
    if let Some(value) = draft.pricing.quote_ts_event {
        insert_value_field(&mut payload, "quote_ts_event", json!(value.as_u64()));
    }
    if let Some(value) = draft.pricing.quote_ts_init {
        insert_value_field(&mut payload, "quote_ts_init", json!(value.as_u64()));
    }
    payload
}

fn vertical_spread_close_order_draft_payload(
    entry: &StrategyStateEntry,
    close_order_list_id: &str,
    draft: &VerticalSpreadCloseOrderDraft,
    close_order_mode: CloseOrderMode,
) -> Value {
    let scanner_signed_entry_price = signed_scanner_spread_price(&draft.plan);
    let action = if draft.order.is_some() {
        "drafted"
    } else {
        "unavailable"
    };
    let order = draft.order.as_ref().map(|order| {
        json!({
            "client_order_id": order.client_order_id().to_string(),
            "instrument_id": order.instrument_id().to_string(),
            "side": format!("{:?}", order.order_side()).to_ascii_lowercase(),
            "quantity": order.quantity().as_f64(),
            "signed_limit_price": order.price().map(|price| price.as_f64()),
            "reduce_only": order.is_reduce_only(),
        })
    });
    let mut payload = json!({
        "action": action,
        "trade_date": entry.trade_date,
        "underlying": draft.plan.underlying.as_str(),
        "strategy": draft.plan.strategy.as_str(),
        "entry_order_list_id": entry.order_list_id,
        "draft_close_order_list_id": close_order_list_id,
        "symbols": entry.symbols(),
        "spread_instrument_id": draft.plan.instrument_id.to_string(),
        "spread_symbol": draft.plan.raw_symbol.to_string(),
        "legs": draft
            .plan
            .legs
            .iter()
            .map(|leg| {
                json!({
                    "symbol": leg.symbol.as_str(),
                    "instrument_id": leg.instrument_id.to_string(),
                    "ratio": leg.ratio,
                })
            })
            .collect::<Vec<_>>(),
        "order": order,
        "pricing_source": draft.pricing.pricing_source,
        "signed_close_limit": draft.pricing.signed_limit_price,
        "close_price_cushion": draft.pricing.close_price_cushion,
        "scanner_premium_kind": draft.plan.scanner_premium_kind.as_str(),
        "scanner_premium": draft.plan.scanner_premium,
        "scanner_signed_entry_price": scanner_signed_entry_price,
        "close_side": "sell",
        "reduce_only": true,
        "current_broker_submit_path": close_order_mode.as_str(),
        "draft_broker_submit_path": "single_option_spread_order",
        "submitted": false,
        "vega_pricing_enabled": false,
    });
    if let Some(reason) = draft.pricing.unavailable_reason {
        insert_value_field(&mut payload, "reason", json!(reason));
    }
    if let Some(value) = draft.pricing.spread_bid {
        insert_value_field(&mut payload, "spread_bid", json!(value));
    }
    if let Some(value) = draft.pricing.spread_ask {
        insert_value_field(&mut payload, "spread_ask", json!(value));
    }
    if let Some(value) = draft.pricing.spread_mid {
        insert_value_field(&mut payload, "spread_mid", json!(value));
    }
    if let Some(value) = draft.pricing.quote_ts_event {
        insert_value_field(&mut payload, "quote_ts_event", json!(value.as_u64()));
    }
    if let Some(value) = draft.pricing.quote_ts_init {
        insert_value_field(&mut payload, "quote_ts_init", json!(value.as_u64()));
    }
    if let Some(value) = draft.pricing.quote_age_secs {
        insert_value_field(&mut payload, "quote_age_secs", json!(value));
    }
    if let Some(value) = draft.pricing.quote_stale_limit_secs {
        insert_value_field(&mut payload, "quote_stale_limit_secs", json!(value));
    }
    payload
}

fn active_entry_quote_symbols(state: &StrategyState) -> Vec<String> {
    state
        .entries
        .iter()
        .filter(|entry| entry.is_active())
        .flat_map(StrategyStateEntry::symbols)
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn instrument_symbols(instrument_ids: &BTreeSet<InstrumentId>) -> Vec<String> {
    instrument_ids
        .iter()
        .map(|instrument_id| instrument_id.symbol.to_string())
        .collect()
}

fn quote_age_secs(ts_event: UnixNanos, now: DateTime<Utc>) -> Option<u64> {
    let now_ns = u64::try_from(now.timestamp_nanos_opt()?).ok()?;
    Some(now_ns.saturating_sub(ts_event.as_u64()) / 1_000_000_000)
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

    /// Creates a Nautilus limit order for one option spread.
    fn option_spread_limit(
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

    fn option_spread_limit(
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

    fn option_spread_limit(
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

fn build_vertical_spread_entry_order(
    orders: &mut impl OptionsEntryOrderCreator,
    plan: &OptionSpreadPlan,
    order_list_id: &str,
    quantity: u64,
    signed_limit_price: f64,
) -> anyhow::Result<OrderAny> {
    if quantity == 0 {
        anyhow::bail!("spread order {order_list_id} quantity must be positive");
    }
    if signed_limit_price == 0.0 {
        anyhow::bail!("spread order {order_list_id} signed limit price must be non-zero");
    }
    if !is_vertical_spread_plan(plan) {
        anyhow::bail!(
            "spread order {order_list_id} only supports two-leg vertical drafts, got {} legs",
            plan.legs.len()
        );
    }

    let mut order = orders.option_spread_limit(
        plan.instrument_id,
        OrderSide::Buy,
        Quantity::new(quantity as f64, 0),
        Price::new(signed_limit_price, 2),
        false,
        labeled_client_order_id(order_list_id, "spread"),
    );
    order.set_order_list_id(OrderListId::from(order_list_id));
    Ok(order)
}

fn build_vertical_spread_close_order(
    orders: &mut impl OptionsEntryOrderCreator,
    plan: &OptionSpreadPlan,
    order_list_id: &str,
    quantity: u64,
    signed_limit_price: f64,
) -> anyhow::Result<OrderAny> {
    if quantity == 0 {
        anyhow::bail!("spread close order {order_list_id} quantity must be positive");
    }
    if signed_limit_price == 0.0 {
        anyhow::bail!("spread close order {order_list_id} signed limit price must be non-zero");
    }
    if !is_vertical_spread_plan(plan) {
        anyhow::bail!(
            "spread close order {order_list_id} only supports two-leg vertical drafts, got {} legs",
            plan.legs.len()
        );
    }

    let mut order = orders.option_spread_limit(
        plan.instrument_id,
        OrderSide::Sell,
        Quantity::new(quantity as f64, 0),
        Price::new(signed_limit_price, 2),
        true,
        labeled_client_order_id(order_list_id, "spread-close"),
    );
    order.set_order_list_id(OrderListId::from(order_list_id));
    Ok(order)
}

fn is_vertical_spread_plan(plan: &OptionSpreadPlan) -> bool {
    plan.legs.len() == 2
}

fn signed_scanner_spread_price(plan: &OptionSpreadPlan) -> f64 {
    match plan.scanner_premium_kind {
        nautilus_trading::options::entries::EntryPremiumKind::Credit => -plan.scanner_premium,
        nautilus_trading::options::entries::EntryPremiumKind::Debit => plan.scanner_premium,
    }
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

fn open_broker_order_group_count(cache: &CacheApi<'_>) -> usize {
    let mut order_group_ids = BTreeSet::new();
    for order in cache
        .orders_open(None, None, None, None, None)
        .into_iter()
        .chain(cache.orders_inflight(None, None, None, None, None))
    {
        if let Some(order_list_id) = order.order_list_id() {
            order_group_ids.insert(format!("list:{order_list_id}"));
        } else {
            order_group_ids.insert(format!("client:{}", order.client_order_id()));
        }
    }
    order_group_ids.len()
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
    close_order_mode: &str,
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
            "close_order_mode": close_order_mode,
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
        "risk_capital_usd": draft.risk_capital_usd,
        "score": draft.score,
        "parent_order_id": draft.parent_order_id,
        "submitted_at_utc": draft.submitted_at_utc,
        "spread_instrument_id": draft.spread_instrument_id,
        "spread_raw_symbol": draft.spread_raw_symbol,
        "spread_legs": draft
            .spread_legs
            .iter()
            .map(|leg| {
                serde_json::json!({
                    "symbol": leg.symbol.as_str(),
                    "instrument_id": leg.instrument_id.as_str(),
                    "ratio": leg.ratio,
                })
            })
            .collect::<Vec<_>>(),
        "entry_pricing_source": draft.entry_pricing_source,
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
        "risk_capital_usd": entry.risk_capital_usd,
        "score": entry.score,
        "parent_order_id": entry.parent_order_id,
        "submitted_at_utc": entry.submitted_at_utc,
        "spread_instrument_id": entry.spread_instrument_id,
        "spread_raw_symbol": entry.spread_raw_symbol,
        "spread_legs": entry
            .spread_legs
            .iter()
            .map(|leg| {
                serde_json::json!({
                    "symbol": leg.symbol.as_str(),
                    "instrument_id": leg.instrument_id.as_str(),
                    "ratio": leg.ratio,
                })
            })
            .collect::<Vec<_>>(),
        "entry_pricing_source": entry.entry_pricing_source,
        "close_order_list_id": entry.close_order_list_id,
        "close_parent_order_id": entry.close_parent_order_id,
        "close_reason": entry.close_reason,
        "close_order_mode": entry.close_order_mode,
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
