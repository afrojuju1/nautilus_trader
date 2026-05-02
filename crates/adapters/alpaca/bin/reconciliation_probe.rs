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

//! Smoke utility for Alpaca REST-backed execution reconciliation.

use std::env;

use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    execution::{
        fill_reports_from_alpaca_activities, order_status_reports_from_alpaca,
        position_status_reports_from_alpaca_positions,
    },
    http::{
        client::AlpacaHttpClient,
        models::{ListActivitiesRequest, ListOrdersRequest},
    },
};
use nautilus_core::{UnixNanos, datetime::unix_nanos_to_iso8601, time::get_atomic_clock_realtime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let lookback_mins = env::args()
        .nth(1)
        .map(|value| value.parse::<u64>())
        .transpose()?
        .or_else(|| {
            env::var("ALPACA_RECONCILIATION_LOOKBACK_MINS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(240);

    let mut config = AlpacaDataClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&config)?;
    let ts_now = get_atomic_clock_realtime().get_time_ns();
    let start = ts_now
        .as_u64()
        .saturating_sub(lookback_mins * 60 * 1_000_000_000);
    let start = UnixNanos::from(start);

    let orders = client
        .orders(&ListOrdersRequest {
            status: "all".to_string(),
            nested: true,
            limit: 500,
            after: Some(unix_nanos_to_iso8601(start)),
            ..Default::default()
        })
        .await?;
    let mut order_reports = Vec::new();
    for order in orders {
        order_reports.extend(order_status_reports_from_alpaca(
            &order,
            "ALPACA-001",
            ts_now,
        )?);
    }

    let mut activities_request = ListActivitiesRequest::option_reconciliation();
    activities_request.direction = Some("asc".to_string());
    activities_request.after = Some(unix_nanos_to_iso8601(start));
    let activities = client.account_activities_all(&activities_request).await?;
    let fill_reports = fill_reports_from_alpaca_activities(&activities, "ALPACA-001", ts_now)?;

    let positions = client.positions().await?;
    let position_reports =
        position_status_reports_from_alpaca_positions(&positions, "ALPACA-001", ts_now)?;

    println!(
        "mass_status: lookback_mins={} orders={} fill_orders={} positions={}",
        lookback_mins,
        order_reports.len(),
        fill_reports.len(),
        position_reports.len(),
    );

    Ok(())
}
