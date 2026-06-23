//! Nautilus-native entry strategy for Alpaca option candidates.

use std::{any::Any, collections::BTreeSet, fmt::Debug, str::FromStr, sync::Arc};

use chrono::{NaiveTime, Utc};
use chrono_tz::Tz;
use nautilus_common::{actor::DataActor, factories::OrderFactory};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    data::{CustomData, CustomDataTrait, DataType, HasTsInit},
    enums::{OrderSide, TimeInForce},
    identifiers::{ClientId, ClientOrderId, InstrumentId, OrderListId},
    orders::OrderAny,
    types::{Price, Quantity},
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{OrderApi, Strategy, StrategyConfig, StrategyCore},
};

use crate::{
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    options_runtime::{
        OptionsEngineConfig, OptionsOpportunitySet, OptionsScanOutcome, OptionsScanReport,
        SelectedOptionsEntry,
    },
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
    /// Whether live submission is globally enabled.
    pub submit_enabled: bool,
    /// Strategy names that are intentionally scanned but not submitted.
    pub dry_run_strategy_names: BTreeSet<String>,
    /// Whether new entries are blocked.
    pub kill_switch: bool,
    /// Whether the entry window should be ignored.
    pub ignore_entry_window: bool,
    /// Entry window start.
    pub entry_start: NaiveTime,
    /// Entry window end.
    pub entry_end: NaiveTime,
    /// Entry window timezone.
    pub entry_timezone: Tz,
}

impl AlpacaOptionsEntryStrategyConfig {
    /// Builds a config from a base strategy config and contract quantity.
    #[must_use]
    pub fn new(base: StrategyConfig, quantity: u64) -> Self {
        Self {
            base,
            quantity,
            client_id: Some(ClientId::from(ALPACA_CLIENT_ID)),
            submit_enabled: true,
            dry_run_strategy_names: BTreeSet::new(),
            kill_switch: false,
            ignore_entry_window: true,
            entry_start: NaiveTime::MIN,
            entry_end: NaiveTime::from_hms_opt(23, 59, 59).expect("valid terminal day time"),
            entry_timezone: chrono_tz::UTC,
        }
    }

    /// Builds a strategy config from the account-engine runtime config.
    #[must_use]
    pub fn from_engine_config(base: StrategyConfig, engine: &OptionsEngineConfig) -> Self {
        Self {
            base,
            quantity: engine.quantity,
            client_id: Some(ClientId::from(ALPACA_CLIENT_ID)),
            submit_enabled: engine.submit_enabled,
            dry_run_strategy_names: engine
                .dry_run_strategy_names()
                .into_iter()
                .map(ToString::to_string)
                .collect(),
            kill_switch: engine.kill_switch,
            ignore_entry_window: engine.ignore_entry_window,
            entry_start: engine.entry_start,
            entry_end: engine.entry_end,
            entry_timezone: engine.entry_timezone,
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

/// Nautilus strategy responsible for converting selected option opportunities into orders.
#[derive(Debug)]
pub struct AlpacaOptionsEntryStrategy {
    core: StrategyCore,
    config: AlpacaOptionsEntryStrategyConfig,
    submitted_underlying_keys: BTreeSet<String>,
}

impl AlpacaOptionsEntryStrategy {
    /// Creates a new [`AlpacaOptionsEntryStrategy`].
    #[must_use]
    pub fn new(config: AlpacaOptionsEntryStrategyConfig) -> Self {
        Self {
            core: StrategyCore::new(config.base.clone()),
            config,
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

        if let Some(reason) = self.entry_block_reason() {
            log::info!(
                "Skipping Alpaca options entry: reason={} underlying={} strategy={}",
                reason,
                entry.underlying(),
                entry.strategy_name()
            );
            return Ok(None);
        }
        if !self.selected_submit_enabled(&entry) {
            log::info!(
                "Dry-run Alpaca options entry: underlying={} strategy={} symbols={} score={:.1}",
                entry.underlying(),
                entry.strategy_name(),
                entry.option_symbols().join(","),
                entry.score()
            );
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
        match self.submit_selected_entry(entry, &order_list_id) {
            Ok(submission) => Ok(Some(submission)),
            Err(error) => {
                self.submitted_underlying_keys.remove(&underlying_key);
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
        let mut order_api = self.order();
        let mut orders = build_selected_entry_orders(
            &mut order_api,
            &entry,
            order_list_id,
            self.config.quantity,
        )?;
        let order_count = orders.len();
        if order_count == 1 {
            self.submit_order(orders.remove(0), None, self.config.client_id, None)?;
        } else {
            apply_order_list_id(&mut orders, order_list_id);
            self.submit_order_list(orders, None, self.config.client_id, None)?;
        }

        Ok(AlpacaOptionsEntrySubmission {
            entry,
            order_list_id: order_list_id.to_string(),
            order_count,
        })
    }

    fn selected_submit_enabled(&self, entry: &SelectedOptionsEntry) -> bool {
        self.config.submit_enabled
            && !self
                .config
                .dry_run_strategy_names
                .contains(entry.strategy_name())
    }

    fn entry_block_reason(&self) -> Option<&'static str> {
        if self.config.kill_switch {
            return Some("kill_switch_enabled");
        }
        if !self.config.ignore_entry_window && !self.inside_entry_window() {
            return Some("outside_entry_window");
        }
        None
    }

    fn inside_entry_window(&self) -> bool {
        let now = Utc::now().with_timezone(&self.config.entry_timezone).time();
        self.config.entry_start <= now && now <= self.config.entry_end
    }
}

nautilus_strategy!(AlpacaOptionsEntryStrategy);

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
