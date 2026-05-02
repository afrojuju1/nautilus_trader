// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  You may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

//! Shared Nautilus `SubmitOrderList` builders for Alpaca multi-leg orders.

use nautilus_common::messages::execution::SubmitOrderList;
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    enums::{OrderSide, OrderType, TimeInForce},
    events::OrderInitialized,
    identifiers::{ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId},
    orders::OrderList,
    types::{Price, Quantity},
};

/// One leg in a multi-leg Nautilus order-list command.
#[derive(Clone, Copy, Debug)]
pub struct MlegSubmitLeg {
    /// Client order ID for this leg.
    pub client_order_id: ClientOrderId,
    /// Instrument ID for this leg.
    pub instrument_id: InstrumentId,
    /// Buy or sell side for this leg.
    pub order_side: OrderSide,
    /// Leg quantity.
    pub quantity: Quantity,
    /// Positive leg limit price.
    pub limit_price: Price,
    /// Whether this leg is reduce-only.
    pub reduce_only: bool,
}

/// Request for building a Nautilus multi-leg `SubmitOrderList`.
#[derive(Clone, Debug)]
pub struct MlegSubmitOrderListRequest {
    /// Trader ID.
    pub trader_id: TraderId,
    /// Optional execution client ID.
    pub client_id: Option<ClientId>,
    /// Strategy ID.
    pub strategy_id: StrategyId,
    /// Order-list ID.
    pub order_list_id: OrderListId,
    /// Multi-leg order legs.
    pub legs: Vec<MlegSubmitLeg>,
    /// Initialization timestamp.
    pub ts_init: UnixNanos,
}

/// Builds a Nautilus `SubmitOrderList` for a broker-native multi-leg order.
///
/// # Errors
///
/// Returns an error when there are fewer than two legs, more than four legs, a zero quantity, or a
/// non-positive leg limit price.
pub fn build_mleg_submit_order_list(
    request: MlegSubmitOrderListRequest,
) -> anyhow::Result<SubmitOrderList> {
    if request.legs.len() < 2 {
        anyhow::bail!("multi-leg SubmitOrderList requires at least two legs");
    }
    if request.legs.len() > 4 {
        anyhow::bail!("multi-leg SubmitOrderList supports at most four legs");
    }

    let client_order_ids = request
        .legs
        .iter()
        .map(|leg| leg.client_order_id)
        .collect::<Vec<_>>();
    let first_instrument_id = request.legs[0].instrument_id;
    let order_inits = request
        .legs
        .iter()
        .map(|leg| {
            if !leg.quantity.is_positive() {
                anyhow::bail!("leg {} quantity must be positive", leg.client_order_id);
            }
            if !leg.limit_price.is_positive() {
                anyhow::bail!("leg {} limit price must be positive", leg.client_order_id);
            }

            let linked_order_ids = client_order_ids
                .iter()
                .copied()
                .filter(|candidate| *candidate != leg.client_order_id)
                .collect::<Vec<_>>();
            Ok(order_init(
                request.trader_id,
                request.strategy_id,
                *leg,
                request.order_list_id,
                linked_order_ids,
                request.ts_init,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let order_list = OrderList::new(
        request.order_list_id,
        first_instrument_id,
        request.strategy_id,
        client_order_ids,
        request.ts_init,
    );

    Ok(SubmitOrderList::new(
        request.trader_id,
        request.client_id,
        request.strategy_id,
        order_list,
        order_inits,
        None,
        None,
        None,
        UUID4::new(),
        request.ts_init,
    ))
}

fn order_init(
    trader_id: TraderId,
    strategy_id: StrategyId,
    leg: MlegSubmitLeg,
    order_list_id: OrderListId,
    linked_order_ids: Vec<ClientOrderId>,
    ts: UnixNanos,
) -> OrderInitialized {
    OrderInitialized::new(
        trader_id,
        strategy_id,
        leg.instrument_id,
        leg.client_order_id,
        leg.order_side,
        OrderType::Limit,
        leg.quantity,
        TimeInForce::Day,
        false,
        leg.reduce_only,
        false,
        false,
        UUID4::new(),
        ts,
        ts,
        Some(leg.limit_price),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(order_list_id),
        Some(linked_order_ids),
        None,
        None,
        None,
        None,
        None,
    )
}
