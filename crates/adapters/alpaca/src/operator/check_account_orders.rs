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

//! Smoke utility for polling Alpaca account, positions, and open orders.

use std::env;

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaPosition, ListOrdersRequest},
    },
    runtime_env::load_options_env_file,
};

pub(crate) async fn run() -> anyhow::Result<()> {
    load_options_env_file()?;
    let mut config = AlpacaDataClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&config)?;
    let account = client.account().await?;
    let positions = client.positions().await?;
    let mut orders_request = ListOrdersRequest::open_nested();
    orders_request.limit = env::var("ALPACA_ORDERS_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);
    let orders = client.orders(&orders_request).await?;

    let option_positions = positions
        .iter()
        .filter(|position| asset_class_is(position, "us_option"))
        .count();
    let equity_positions = positions
        .iter()
        .filter(|position| asset_class_is(position, "us_equity"))
        .count();
    let mleg_orders = orders
        .iter()
        .filter(|order| order.order_class.as_deref() == Some("mleg"))
        .count();
    let nested_legs = orders
        .iter()
        .map(|order| order.legs.as_ref().map_or(0, Vec::len))
        .sum::<usize>();

    println!(
        "account: status={} currency={} trading_blocked={} transfers_blocked={} account_blocked={} trade_suspended_by_user={} buying_power={} options_buying_power={} portfolio_value={} cash={}",
        account.status.as_deref().unwrap_or("unknown"),
        account.currency.as_deref().unwrap_or("unknown"),
        account.trading_blocked.unwrap_or(false),
        account.transfers_blocked.unwrap_or(false),
        account.account_blocked.unwrap_or(false),
        account.trade_suspended_by_user.unwrap_or(false),
        account.buying_power.as_deref().unwrap_or("unknown"),
        account.options_buying_power.as_deref().unwrap_or("unknown"),
        account.portfolio_value.as_deref().unwrap_or("unknown"),
        account.cash.as_deref().unwrap_or("unknown"),
    );
    println!(
        "positions: total={} options={} equities={}",
        positions.len(),
        option_positions,
        equity_positions,
    );
    println!(
        "orders: open={} mleg={} nested_legs={}",
        orders.len(),
        mleg_orders,
        nested_legs,
    );

    Ok(())
}

fn asset_class_is(position: &AlpacaPosition, expected: &str) -> bool {
    position
        .asset_class
        .as_deref()
        .is_some_and(|value| value == expected)
}
