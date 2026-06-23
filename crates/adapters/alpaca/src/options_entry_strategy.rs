//! Nautilus-native entry strategy for Alpaca option candidates.

use std::{fmt::Debug, str::FromStr};

use nautilus_common::{actor::DataActor, factories::OrderFactory};
use nautilus_model::{
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
    options_runtime::{OptionsOpportunitySet, SelectedOptionsEntry},
};

/// Configuration for [`AlpacaOptionsEntryStrategy`].
#[derive(Clone, Debug)]
pub struct AlpacaOptionsEntryStrategyConfig {
    /// Nautilus base strategy configuration.
    pub base: StrategyConfig,
    /// Contract quantity per leg.
    pub quantity: u64,
    /// Execution client ID to route orders to.
    pub client_id: Option<ClientId>,
}

impl AlpacaOptionsEntryStrategyConfig {
    /// Builds a config from a base strategy config and contract quantity.
    #[must_use]
    pub fn new(base: StrategyConfig, quantity: u64) -> Self {
        Self {
            base,
            quantity,
            client_id: Some(ClientId::from(ALPACA_CLIENT_ID)),
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
}

impl AlpacaOptionsEntryStrategy {
    /// Creates a new [`AlpacaOptionsEntryStrategy`].
    #[must_use]
    pub fn new(config: AlpacaOptionsEntryStrategyConfig) -> Self {
        Self {
            core: StrategyCore::new(config.base.clone()),
            config,
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
}

nautilus_strategy!(AlpacaOptionsEntryStrategy);

impl DataActor for AlpacaOptionsEntryStrategy {}

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
