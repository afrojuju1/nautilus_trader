//! Nautilus-native entry strategy for Alpaca option candidates.

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    str::FromStr,
    sync::Arc,
};

use chrono::Utc;
use nautilus_common::{actor::DataActor, cache::CacheApi, factories::OrderFactory};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    data::{CustomData, CustomDataTrait, DataType, HasTsInit},
    enums::{OrderSide, TimeInForce},
    events::{OrderAccepted, OrderDenied, OrderRejected},
    identifiers::{ClientId, ClientOrderId, InstrumentId, OrderListId},
    instruments::Instrument,
    orders::{Order, OrderAny},
    types::{Price, Quantity},
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{OrderApi, Strategy, StrategyConfig, StrategyCore},
};

use crate::{
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    options_entry_admission::{
        EntryAdmissionConfig, EntryAdmissionSnapshot, EntryGateDecision, SubmissionBlock,
        UNCOVERED_OPTION_PERMISSION_REJECTION_REASON, entry_gate_decision,
        is_uncovered_option_permission_rejection, selected_submit_enabled,
        submission_block_for_selected,
    },
    options_runtime::{
        OptionsEngineConfig, OptionsOpportunitySet, OptionsScanOutcome, OptionsScanReport,
        SelectedOptionsEntry,
    },
    runtime::StrategyState,
};

/// Custom data type published by option-chain scanner actors for entry strategies.
#[derive(Clone, Debug)]
pub struct OptionsOpportunityData {
    /// Ranked option opportunities discovered by the scanner.
    pub opportunities: OptionsOpportunitySet,
    ts_event: UnixNanos,
    ts_init: UnixNanos,
}

impl OptionsOpportunityData {
    const TYPE_NAME: &'static str = "AlpacaOptionsOpportunityData";

    /// Creates a new custom data payload from an opportunity set.
    #[must_use]
    pub const fn new(
        opportunities: OptionsOpportunitySet,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Self {
        Self {
            opportunities,
            ts_event,
            ts_init,
        }
    }

    /// Returns the Nautilus custom data type used for routing opportunity payloads.
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

impl HasTsInit for OptionsOpportunityData {
    fn ts_init(&self) -> UnixNanos {
        self.ts_init
    }
}

impl CustomDataTrait for OptionsOpportunityData {
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
            "trade_date": self.opportunities.trade_date,
            "ts_event": self.ts_event.as_u64(),
            "ts_init": self.ts_init.as_u64(),
            "scans": self
                .opportunities
                .scans
                .iter()
                .map(scan_report_payload)
                .collect::<Vec<_>>(),
            "ranked_entries": self.opportunities.ranked_entries().len(),
            "selected": self
                .opportunities
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
                && self.opportunities.trade_date == other.opportunities.trade_date
                && self.opportunities.ranked_entries().len()
                    == other.opportunities.ranked_entries().len()
        })
    }

    fn type_name_static() -> &'static str
    where
        Self: Sized,
    {
        Self::TYPE_NAME
    }
}

/// Configuration for [`AlpacaOptionsEntryStrategy`].
#[derive(Clone, Debug)]
pub struct AlpacaOptionsEntryStrategyConfig {
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
}

impl AlpacaOptionsEntryStrategyConfig {
    /// Builds a config from a base strategy config and contract quantity.
    #[must_use]
    pub fn new(base: StrategyConfig, quantity: u64) -> Self {
        Self {
            base,
            quantity,
            client_id: Some(ClientId::from(ALPACA_CLIENT_ID)),
            admission: EntryAdmissionConfig::default(),
            initial_state: StrategyState::default(),
        }
    }

    /// Builds a strategy config from the account-engine runtime config.
    #[must_use]
    pub fn from_engine_config(base: StrategyConfig, engine: &OptionsEngineConfig) -> Self {
        Self {
            base,
            quantity: engine.quantity,
            client_id: Some(ClientId::from(ALPACA_CLIENT_ID)),
            admission: EntryAdmissionConfig::from_engine_config(engine),
            initial_state: StrategyState::default(),
        }
    }
}

/// Submission result for one selected options entry.
#[derive(Clone, Debug)]
pub struct AlpacaOptionsEntrySubmission {
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
    quantity: u64,
    order_count: usize,
    accepted: usize,
    rejected: usize,
    recorded: bool,
    rejection_reasons: Vec<String>,
}

/// Nautilus strategy responsible for converting selected option opportunities into orders.
#[derive(Debug)]
pub struct AlpacaOptionsEntryStrategy {
    core: StrategyCore,
    config: AlpacaOptionsEntryStrategyConfig,
    state: StrategyState,
    pending_submissions: BTreeMap<String, PendingEntrySubmission>,
    pending_client_order_ids: BTreeMap<String, String>,
    submitted_underlying_keys: BTreeSet<String>,
}

impl AlpacaOptionsEntryStrategy {
    /// Creates a new [`AlpacaOptionsEntryStrategy`].
    #[must_use]
    pub fn new(config: AlpacaOptionsEntryStrategyConfig) -> Self {
        Self {
            core: StrategyCore::new(config.base.clone()),
            state: config.initial_state.clone(),
            config,
            pending_submissions: BTreeMap::new(),
            pending_client_order_ids: BTreeMap::new(),
            submitted_underlying_keys: BTreeSet::new(),
        }
    }

    /// Handles opportunity data received from scanner actors.
    ///
    /// # Errors
    ///
    /// Returns an error if order construction or Nautilus strategy submission fails.
    ///
    /// # Panics
    ///
    /// Panics if submission is enabled and the strategy has not been registered with a Nautilus
    /// runtime.
    pub fn submit_opportunity_data(
        &mut self,
        data: &OptionsOpportunityData,
    ) -> anyhow::Result<Option<AlpacaOptionsEntrySubmission>> {
        let Some(entry) = data.opportunities.selected_entry().cloned() else {
            return Ok(None);
        };

        match entry_gate_decision(&self.config.admission, Utc::now()) {
            EntryGateDecision::Continue => {}
            EntryGateDecision::KillSwitch => {
                log::info!(
                    "Skipping Alpaca options entry: reason=kill_switch_enabled underlying={} strategy={}",
                    entry.underlying(),
                    entry.strategy_name()
                );
                return Ok(None);
            }
            EntryGateDecision::OutsideEntryWindow => {
                log::info!(
                    "Skipping Alpaca options entry: reason=outside_entry_window underlying={} strategy={}",
                    entry.underlying(),
                    entry.strategy_name()
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
            return Ok(None);
        }

        let snapshot = self.entry_admission_snapshot(&entry)?;
        if let Some(block) = submission_block_for_selected(
            &self.config.admission,
            &self.state,
            &entry,
            &data.opportunities.trade_date,
            &snapshot,
        ) {
            self.log_entry_block(&data.opportunities.trade_date, &entry, &block);
            return Ok(None);
        }

        let underlying_key = submitted_underlying_key(&data.opportunities.trade_date, &entry);
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
            return Ok(None);
        }

        let order_list_id = entry_order_list_id(&data.opportunities.trade_date, entry.underlying());
        match self.submit_selected_entry_with_trade_date(
            entry,
            &data.opportunities.trade_date,
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

    /// Submits the highest-ranked opportunity, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if order construction or Nautilus strategy submission fails.
    ///
    /// # Panics
    ///
    /// Panics if the strategy has not been registered with a Nautilus runtime.
    pub fn submit_opportunities(
        &mut self,
        opportunities: OptionsOpportunitySet,
        order_list_id: &str,
    ) -> anyhow::Result<Option<AlpacaOptionsEntrySubmission>> {
        let Some(entry) = opportunities.into_selected_entry() else {
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
    ) -> anyhow::Result<AlpacaOptionsEntrySubmission> {
        let orders = self.build_entry_orders(&entry, order_list_id)?;
        let order_count = orders.len();
        self.submit_entry_orders(orders, order_list_id)?;

        Ok(AlpacaOptionsEntrySubmission {
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
    ) -> anyhow::Result<AlpacaOptionsEntrySubmission> {
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

        if let Err(error) = self.submit_entry_orders(orders, order_list_id) {
            self.remove_pending_submission(order_list_id);
            return Err(error);
        }

        Ok(AlpacaOptionsEntrySubmission {
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
        self.pending_submissions.insert(
            order_list_id.clone(),
            PendingEntrySubmission {
                entry,
                trade_date,
                order_list_id,
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
            self.state.record_entry_submission(draft);
        }

        if should_remove {
            self.remove_pending_submission(&order_list_id);
        }
    }

    fn handle_order_rejected(&mut self, client_order_id: ClientOrderId, reason: &str) {
        let client_order_id = client_order_id.to_string();
        let Some(order_list_id) = self.pending_client_order_ids.get(&client_order_id).cloned()
        else {
            return;
        };

        let mut rejected_state = None;
        let mut should_remove = false;
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
                        None,
                    ),
                    close_reason.to_string(),
                ));
                pending.recorded = true;
            }
            should_remove = pending.accepted + pending.rejected >= pending.order_count;
        }

        if let Some((draft, close_reason)) = rejected_state {
            log::info!(
                "Recording terminally rejected Alpaca options entry: order_list_id={} client_order_id={} reason={}",
                order_list_id,
                client_order_id,
                close_reason
            );
            self.state.record_entry_submission(draft);
            if let Some(entry) = self.state.entries.last_mut() {
                entry.mark_canceled();
                entry.close_reason = Some(close_reason);
            }
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
        let open_order_count = cache.orders_open_count(None, None, None, None, None);
        let mut broker_admission_reasons = Vec::new();
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
            if cache.has_orders_open(None, Some(instrument_id), None, None, None) {
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

        for order in cache.orders_open(None, None, None, None, None) {
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

    fn log_entry_block(
        &self,
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
    }
}

nautilus_strategy!(AlpacaOptionsEntryStrategy, {
    fn on_order_accepted(&mut self, event: OrderAccepted) {
        self.handle_order_accepted(event);
    }

    fn on_order_rejected(&mut self, event: OrderRejected) {
        self.handle_order_rejected(event.client_order_id, event.reason.as_str());
    }

    fn on_order_denied(&mut self, event: OrderDenied) {
        self.handle_order_rejected(event.client_order_id, event.reason.as_str());
    }
});

impl DataActor for AlpacaOptionsEntryStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.subscribe_data(OptionsOpportunityData::data_type(), None, None);
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.unsubscribe_data(OptionsOpportunityData::data_type(), None, None);
        Ok(())
    }

    fn on_data(&mut self, data: &CustomData) -> anyhow::Result<()> {
        let Some(opportunities) = data.data.as_any().downcast_ref::<OptionsOpportunityData>()
        else {
            return Ok(());
        };
        self.submit_opportunity_data(opportunities)?;
        Ok(())
    }
}

/// Builds a unique order-list ID for one submitted entry.
#[must_use]
pub fn entry_order_list_id(trade_date: &str, underlying: &str) -> String {
    format!(
        "options-engine-entry-{trade_date}-{underlying}-{}",
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
