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

//! Smoke utility for Alpaca account trade-update WebSocket authentication and listening.

use std::{env, process, time::Duration};

use nautilus_alpaca::{
    common::credentials::AlpacaCredential,
    config::AlpacaExecClientConfig,
    websocket::{client::AlpacaTradeUpdatesWebSocketClient, messages::AlpacaWsMessage},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wait_secs = env::args()
        .nth(1)
        .map(|value| value.parse::<u64>())
        .transpose()?
        .or_else(|| {
            env::var("ALPACA_TRADE_UPDATES_WAIT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(15);

    let mut config = AlpacaExecClientConfig::default();
    config.trade_updates_ws_url = env::var("ALPACA_TRADE_UPDATES_WS_URL").ok();

    let credential = AlpacaCredential::resolve(config.api_key.clone(), config.api_secret.clone())
        .ok_or("missing Alpaca credentials")?;
    let mut client =
        AlpacaTradeUpdatesWebSocketClient::new(config.resolved_trade_updates_ws_url(), credential);
    client.connect().await?;
    let Some(mut rx) = client.take_out_rx() else {
        client.disconnect().await;
        return Err("trade updates receiver unavailable".into());
    };

    let mut authorized = false;
    let mut listening = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_secs);

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AlpacaWsMessage::Authorization(auth))) => {
                authorized = auth.status.as_deref() == Some("authorized");
                println!(
                    "authorization: status={}",
                    auth.status.as_deref().unwrap_or("unknown"),
                );
            }
            Ok(Some(AlpacaWsMessage::Listening(listening_msg))) => {
                listening = listening_msg
                    .streams
                    .iter()
                    .any(|stream| stream == "trade_updates");
                println!("listening: streams={:?}", listening_msg.streams);
                if authorized && listening {
                    break;
                }
            }
            Ok(Some(AlpacaWsMessage::TradeUpdate(update))) => {
                println!(
                    "trade_update: event={} order_id={} client_order_id={}",
                    update.event,
                    update.order.id.as_deref().unwrap_or("unknown"),
                    update.order.client_order_id.as_deref().unwrap_or("unknown"),
                );
            }
            Ok(Some(AlpacaWsMessage::Disconnected { reason })) => {
                println!("disconnected: reason={reason}");
            }
            Ok(Some(AlpacaWsMessage::Reconnected)) => {
                println!("reconnected");
            }
            Ok(Some(AlpacaWsMessage::Error(err))) => {
                println!("error: {err}");
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    client.disconnect().await;

    if authorized && listening {
        Ok(())
    } else {
        eprintln!("trade updates stream did not reach authorized+listening within {wait_secs}s");
        process::exit(1);
    }
}
