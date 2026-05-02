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

//! Parsers for Alpaca account WebSocket messages.

use serde::Deserialize;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use crate::websocket::messages::{
    AlpacaAuthorization, AlpacaListening, AlpacaTradeUpdate, AlpacaWsMessage, TRADE_UPDATES_STREAM,
};

/// Parses a raw WebSocket frame into an Alpaca account stream message.
///
/// Alpaca paper trade updates can arrive as binary frames containing UTF-8 JSON, so text and
/// binary frames are intentionally handled through the same JSON parser.
///
/// # Errors
///
/// Returns an error when a text/binary frame cannot be decoded or when a known stream has an
/// unexpected payload shape.
pub fn parse_alpaca_ws_message(message: &Message) -> anyhow::Result<Option<AlpacaWsMessage>> {
    match message {
        Message::Text(text) => parse_text(text.as_str()).map(Some),
        Message::Binary(bytes) => {
            let text = std::str::from_utf8(bytes.as_ref())?;
            parse_text(text).map(Some)
        }
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(None),
        Message::Close(_) => Ok(None),
    }
}

fn parse_text(text: &str) -> anyhow::Result<AlpacaWsMessage> {
    if text == nautilus_network::RECONNECTED {
        return Ok(AlpacaWsMessage::Reconnected);
    }

    let envelope: AlpacaWsEnvelope = serde_json::from_str(text)?;
    let stream = envelope.stream.as_deref().or(envelope.action.as_deref());

    match stream {
        Some("authorization") => {
            let auth = serde_json::from_value::<AlpacaAuthorization>(envelope.data)?;
            Ok(AlpacaWsMessage::Authorization(auth))
        }
        Some("listening") => {
            let listening = serde_json::from_value::<AlpacaListening>(envelope.data)?;
            Ok(AlpacaWsMessage::Listening(listening))
        }
        Some(TRADE_UPDATES_STREAM) => {
            let update = serde_json::from_value::<AlpacaTradeUpdate>(envelope.data)?;
            Ok(AlpacaWsMessage::TradeUpdate(Box::new(update)))
        }
        Some("error") => Ok(AlpacaWsMessage::Error(envelope.data.to_string())),
        Some(other) => Ok(AlpacaWsMessage::Error(format!(
            "unsupported Alpaca WebSocket stream: {other}",
        ))),
        None => Ok(AlpacaWsMessage::Error(
            "Alpaca WebSocket message missing stream".to_string(),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct AlpacaWsEnvelope {
    stream: Option<String>,
    action: Option<String>,
    #[serde(default)]
    data: Value,
}

#[cfg(test)]
mod tests {
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    #[test]
    fn parses_authorization_message() {
        let raw = Message::Text(
            r#"{"stream":"authorization","data":{"action":"authenticate","status":"authorized"}}"#
                .into(),
        );

        let parsed = parse_alpaca_ws_message(&raw).unwrap().unwrap();

        match parsed {
            AlpacaWsMessage::Authorization(auth) => {
                assert_eq!(auth.status.as_deref(), Some("authorized"));
            }
            other => panic!("expected authorization, got {other:?}"),
        }
    }

    #[test]
    fn parses_binary_trade_update_message() {
        let raw = Message::Binary(
            r#"{
                "stream":"trade_updates",
                "data":{
                    "event":"fill",
                    "execution_id":"exec-parent",
                    "price":"0.50",
                    "qty":"1",
                    "timestamp":"2026-05-01T14:30:00Z",
                    "order":{
                        "id":"parent-order",
                        "client_order_id":"nautilus-1",
                        "created_at":"2026-05-01T14:29:59Z",
                        "updated_at":"2026-05-01T14:30:00Z",
                        "submitted_at":"2026-05-01T14:29:59Z",
                        "asset_class":"us_option",
                        "qty":"1",
                        "filled_qty":"1",
                        "type":"limit",
                        "time_in_force":"day",
                        "limit_price":"0.50",
                        "status":"filled",
                        "legs":[
                            {
                                "id":"short-leg-order",
                                "symbol":"SPY260508P00500000",
                                "qty":"1",
                                "filled_qty":"1",
                                "type":"limit",
                                "side":"sell",
                                "time_in_force":"day",
                                "limit_price":"0.25",
                                "status":"filled",
                                "submitted_at":"2026-05-01T14:29:59Z",
                                "updated_at":"2026-05-01T14:30:00Z"
                            },
                            {
                                "id":"long-leg-order",
                                "symbol":"SPY260508P00495000",
                                "qty":"1",
                                "filled_qty":"1",
                                "type":"limit",
                                "side":"buy",
                                "time_in_force":"day",
                                "limit_price":"0.10",
                                "status":"filled",
                                "submitted_at":"2026-05-01T14:29:59Z",
                                "updated_at":"2026-05-01T14:30:00Z"
                            }
                        ]
                    },
                    "legs":[
                        {
                            "execution_id":"exec-short",
                            "price":"0.25",
                            "qty":"1",
                            "order_id":"short-leg-order",
                            "symbol":"SPY260508P00500000",
                            "timestamp":"2026-05-01T14:30:00Z"
                        },
                        {
                            "execution_id":"exec-long",
                            "price":"0.10",
                            "qty":"1",
                            "order_id":"long-leg-order",
                            "symbol":"SPY260508P00495000",
                            "timestamp":"2026-05-01T14:30:00Z"
                        }
                    ]
                }
            }"#
            .as_bytes()
            .to_vec()
            .into(),
        );

        let parsed = parse_alpaca_ws_message(&raw).unwrap().unwrap();

        match parsed {
            AlpacaWsMessage::TradeUpdate(update) => {
                assert_eq!(update.event, "fill");
                assert_eq!(update.legs.as_ref().unwrap().len(), 2);
                assert_eq!(update.order.legs.as_ref().unwrap().len(), 2);
            }
            other => panic!("expected trade update, got {other:?}"),
        }
    }
}
