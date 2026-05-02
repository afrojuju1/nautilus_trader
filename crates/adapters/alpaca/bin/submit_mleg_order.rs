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

//! Smoke utility for submitting a paper Alpaca MLeg payload, then canceling if accepted.

use std::{env, process};

use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{AlpacaOrder, ListOrdersRequest},
    },
    orders::build_put_credit_spread_open_order,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 3 {
        eprintln!(
            "usage: alpaca-submit-mleg-order <SHORT_PUT_SYMBOL> <LONG_PUT_SYMBOL> <CREDIT_LIMIT> [QTY]"
        );
        process::exit(2);
    }

    let short_put_symbol = &args[0];
    let long_put_symbol = &args[1];
    let credit_limit = args[2].parse::<f64>()?;
    let quantity = args
        .get(3)
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(1);
    let payload = build_put_credit_spread_open_order(
        short_put_symbol,
        long_put_symbol,
        credit_limit,
        quantity,
    )?;

    let mut config = AlpacaDataClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&config)?;
    match client.submit_mleg_order(&payload).await {
        Ok(order) => {
            print_order("submitted", &order);
            if !is_terminal_order(&order) {
                let order_id = order.id.as_deref().ok_or_else(|| {
                    Error::Validation("accepted order did not include id".to_string())
                })?;
                client.cancel_order(order_id).await?;
                println!("cancel_requested: id={order_id}");
            }
        }
        Err(Error::HttpStatus { status, body, .. }) => {
            println!("submit_rejected: http_status={status} body={body}");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }

    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    println!("open_orders_after_submit_smoke: {}", open_orders.len());

    Ok(())
}

fn print_order(label: &str, order: &AlpacaOrder) {
    println!(
        "{label}: id={} status={} order_class={} qty={} limit_price={}",
        order.id.as_deref().unwrap_or("unknown"),
        order.status.as_deref().unwrap_or("unknown"),
        order.order_class.as_deref().unwrap_or("unknown"),
        order.qty.as_deref().unwrap_or("unknown"),
        order.limit_price.as_deref().unwrap_or("unknown"),
    );
}

fn is_terminal_order(order: &AlpacaOrder) -> bool {
    matches!(
        order.status.as_deref(),
        Some("filled" | "canceled" | "expired" | "rejected")
    )
}
