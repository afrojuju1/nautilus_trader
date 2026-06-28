//! Alpaca option market-data WebSocket message models.

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AlpacaOptionMarketDataMessage {
    Quote(AlpacaOptionStreamQuote),
    Trade(AlpacaOptionStreamTrade),
    Subscription(AlpacaOptionStreamSubscription),
    Success(AlpacaOptionStreamSuccess),
    Error(AlpacaOptionStreamError),
    Reconnected,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamQuote {
    #[serde(rename = "S")]
    pub symbol: String,
    #[serde(rename = "t")]
    pub timestamp: String,
    #[serde(default, rename = "bx")]
    pub bid_exchange: Option<String>,
    #[serde(rename = "bp")]
    pub bid_price: f64,
    #[serde(rename = "bs")]
    pub bid_size: u64,
    #[serde(default, rename = "ax")]
    pub ask_exchange: Option<String>,
    #[serde(rename = "ap")]
    pub ask_price: f64,
    #[serde(rename = "as")]
    pub ask_size: u64,
    #[serde(default, rename = "c")]
    pub conditions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamTrade {
    #[serde(rename = "S")]
    pub symbol: String,
    #[serde(rename = "t")]
    pub timestamp: String,
    #[serde(rename = "p")]
    pub price: f64,
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(default, rename = "x")]
    pub exchange: Option<String>,
    #[serde(default, rename = "c")]
    pub conditions: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamSubscription {
    #[serde(default)]
    pub quotes: Vec<String>,
    #[serde(default)]
    pub trades: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamSuccess {
    #[serde(default)]
    pub msg: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamError {
    #[serde(default)]
    pub code: Option<u16>,
    #[serde(default, alias = "message")]
    pub msg: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlpacaOptionMarketDataSubscriptionRequest {
    action: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    quotes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    trades: Vec<String>,
}

impl AlpacaOptionMarketDataSubscriptionRequest {
    pub(crate) fn subscribe(quotes: Vec<String>, trades: Vec<String>) -> Self {
        Self {
            action: "subscribe",
            quotes,
            trades,
        }
    }

    pub(crate) fn unsubscribe(quotes: Vec<String>, trades: Vec<String>) -> Self {
        Self {
            action: "unsubscribe",
            quotes,
            trades,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.quotes.is_empty() && self.trades.is_empty()
    }

    pub(crate) fn to_msgpack_bytes(&self) -> anyhow::Result<Vec<u8>> {
        rmp_serde::to_vec_named(self)
            .with_context(|| format!("failed to encode Alpaca option {} request", self.action))
    }
}

pub(crate) fn parse_option_market_data_message(
    message: &Message,
) -> anyhow::Result<Vec<AlpacaOptionMarketDataMessage>> {
    match message {
        Message::Binary(bytes) => {
            let value: Value = rmp_serde::from_slice(bytes.as_ref())
                .context("failed to decode Alpaca option market-data MsgPack frame")?;
            parse_option_market_data_value(value)
        }
        Message::Text(text) => {
            if text.as_str() == nautilus_network::RECONNECTED {
                return Ok(vec![AlpacaOptionMarketDataMessage::Reconnected]);
            }
            let value: Value = serde_json::from_str(text)
                .context("failed to decode Alpaca option market-data text frame")?;
            parse_option_market_data_value(value)
        }
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) | Message::Close(_) => Ok(vec![]),
    }
}

fn parse_option_market_data_value(
    value: Value,
) -> anyhow::Result<Vec<AlpacaOptionMarketDataMessage>> {
    match value {
        Value::Array(values) => values
            .into_iter()
            .map(parse_option_market_data_entry)
            .collect::<anyhow::Result<Vec<_>>>(),
        value => Ok(vec![parse_option_market_data_entry(value)?]),
    }
}

fn parse_option_market_data_entry(value: Value) -> anyhow::Result<AlpacaOptionMarketDataMessage> {
    let message_type = value
        .get("T")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Alpaca option market-data message missing T field"))?;

    match message_type {
        "q" => Ok(AlpacaOptionMarketDataMessage::Quote(
            serde_json::from_value(value).context("failed to parse Alpaca option quote message")?,
        )),
        "t" => Ok(AlpacaOptionMarketDataMessage::Trade(
            serde_json::from_value(value).context("failed to parse Alpaca option trade message")?,
        )),
        "subscription" => Ok(AlpacaOptionMarketDataMessage::Subscription(
            serde_json::from_value(value)
                .context("failed to parse Alpaca option subscription message")?,
        )),
        "success" => Ok(AlpacaOptionMarketDataMessage::Success(
            serde_json::from_value(value)
                .context("failed to parse Alpaca option success message")?,
        )),
        "error" => Ok(AlpacaOptionMarketDataMessage::Error(
            serde_json::from_value(value).context("failed to parse Alpaca option error message")?,
        )),
        other => Err(anyhow!(
            "unsupported Alpaca option market-data message type {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_msgpack_quote_and_trade_batch() {
        let raw = json!([
            {
                "T": "q",
                "S": "SPY260619P00450000",
                "t": "2026-05-08T14:30:00.123456789Z",
                "bx": "P",
                "bp": 1.20,
                "bs": 4,
                "ax": "Q",
                "ap": 1.30,
                "as": 6,
                "c": ["R"]
            },
            {
                "T": "t",
                "S": "SPY260619P00450000",
                "t": "2026-05-08T14:30:01.123456789Z",
                "p": 1.25,
                "s": 2,
                "x": "Q",
                "c": ["I"]
            }
        ]);
        let bytes = rmp_serde::to_vec_named(&raw).expect("encode fixture");

        let parsed =
            parse_option_market_data_message(&Message::Binary(bytes.into())).expect("parse");

        assert_eq!(parsed.len(), 2);
        match &parsed[0] {
            AlpacaOptionMarketDataMessage::Quote(quote) => {
                assert_eq!(quote.symbol, "SPY260619P00450000");
                assert_eq!(quote.bid_price, 1.20);
                assert_eq!(quote.ask_size, 6);
            }
            other => panic!("expected quote, got {other:?}"),
        }
        match &parsed[1] {
            AlpacaOptionMarketDataMessage::Trade(trade) => {
                assert_eq!(trade.symbol, "SPY260619P00450000");
                assert_eq!(trade.price, 1.25);
                assert_eq!(trade.size, 2);
            }
            other => panic!("expected trade, got {other:?}"),
        }
    }

    #[test]
    fn encodes_subscribe_request_as_msgpack() {
        let request = AlpacaOptionMarketDataSubscriptionRequest::subscribe(
            vec!["SPY260619P00450000".to_string()],
            vec!["SPY260619C00450000".to_string()],
        );
        let bytes = request.to_msgpack_bytes().expect("encode");
        let decoded: Value = rmp_serde::from_slice(&bytes).expect("decode");

        assert_eq!(decoded["action"], "subscribe");
        assert_eq!(decoded["quotes"][0], "SPY260619P00450000");
        assert_eq!(decoded["trades"][0], "SPY260619C00450000");
    }
}
