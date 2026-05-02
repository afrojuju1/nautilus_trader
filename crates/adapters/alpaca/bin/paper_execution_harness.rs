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

//! Paper execution lifecycle harness for Alpaca MLeg option orders.

use std::{env, process, str::FromStr, time::Duration};

use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{AlpacaOrder, ListActivitiesRequest, ListOrdersRequest},
    },
    orders::build_put_credit_spread_open_order,
};
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 3 {
        eprintln!(
            "usage: alpaca-paper-execution-harness <SHORT_PUT_SYMBOL> <LONG_PUT_SYMBOL> <CREDIT_LIMIT> [QTY]"
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
    let poll_attempts = env_parse("ALPACA_EXECUTION_POLL_ATTEMPTS", 1_u64);
    let post_cancel_poll_attempts = env_parse("ALPACA_EXECUTION_POST_CANCEL_POLL_ATTEMPTS", 3_u64);
    let poll_secs = env_parse("ALPACA_EXECUTION_POLL_SECS", 2_u64);

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
    let submitted = match client.submit_mleg_order(&payload).await {
        Ok(order) => order,
        Err(Error::HttpStatus { status, body, .. }) => {
            println!("submit_rejected: http_status={status} body={body}");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    print_order("submitted", &submitted);

    let order_id = submitted
        .id
        .clone()
        .ok_or_else(|| Error::Validation("submitted order did not include id".to_string()))?;
    let mut latest = submitted;
    for attempt in 1..=poll_attempts {
        sleep(Duration::from_secs(poll_secs)).await;
        latest = client.order_by_id(&order_id, true).await?;
        print_order(&format!("poll_{attempt}"), &latest);
        print_matching_activities(&client, &order_id).await?;
        if latest.is_terminal() {
            print_account_surface(&client).await?;
            return Ok(());
        }
    }

    if !latest.is_terminal() {
        client.cancel_order(&order_id).await?;
        println!("cancel_requested: id={order_id}");

        for attempt in 1..=post_cancel_poll_attempts {
            sleep(Duration::from_secs(poll_secs)).await;
            latest = client.order_by_id(&order_id, true).await?;
            print_order(&format!("post_cancel_poll_{attempt}"), &latest);
            if latest.is_terminal() {
                break;
            }
        }
    }

    print_matching_activities(&client, &order_id).await?;
    print_account_surface(&client).await?;

    Ok(())
}

async fn print_matching_activities(
    client: &AlpacaHttpClient,
    order_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let activities = client
        .account_activities(&ListActivitiesRequest::option_reconciliation())
        .await?;
    let matching = activities
        .iter()
        .filter(|activity| activity.order_id.as_deref() == Some(order_id))
        .count();
    println!(
        "activities: reconciliation_page={} matching_order_id={}",
        activities.len(),
        matching,
    );
    Ok(())
}

async fn print_account_surface(
    client: &AlpacaHttpClient,
) -> Result<(), Box<dyn std::error::Error>> {
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    println!(
        "post_lifecycle_state: positions={} open_orders={}",
        positions.len(),
        open_orders.len(),
    );
    Ok(())
}

fn print_order(label: &str, order: &AlpacaOrder) {
    println!(
        "{label}: id={} status={} filled_qty={} order_class={} qty={} limit_price={} legs={}",
        order.id.as_deref().unwrap_or("unknown"),
        order.status.as_deref().unwrap_or("unknown"),
        order.filled_qty.as_deref().unwrap_or("unknown"),
        order.order_class.as_deref().unwrap_or("unknown"),
        order.qty.as_deref().unwrap_or("unknown"),
        order.limit_price.as_deref().unwrap_or("unknown"),
        order.legs.as_ref().map_or(0, Vec::len),
    );
}

fn env_parse<T>(name: &str, default: T) -> T
where
    T: FromStr,
{
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default)
}
