//! Alpaca option market-data WebSocket message models.

use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as DeError, SeqAccess, Visitor},
};
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "T")]
enum AlpacaOptionMarketDataWireMessage {
    #[serde(rename = "q")]
    Quote(AlpacaOptionStreamQuote),
    #[serde(rename = "t")]
    Trade(AlpacaOptionStreamTrade),
    #[serde(rename = "subscription")]
    Subscription(AlpacaOptionStreamSubscription),
    #[serde(rename = "success")]
    Success(AlpacaOptionStreamSuccess),
    #[serde(rename = "error")]
    Error(AlpacaOptionStreamError),
}

impl From<AlpacaOptionMarketDataWireMessage> for AlpacaOptionMarketDataMessage {
    fn from(message: AlpacaOptionMarketDataWireMessage) -> Self {
        match message {
            AlpacaOptionMarketDataWireMessage::Quote(message) => Self::Quote(message),
            AlpacaOptionMarketDataWireMessage::Trade(message) => Self::Trade(message),
            AlpacaOptionMarketDataWireMessage::Subscription(message) => Self::Subscription(message),
            AlpacaOptionMarketDataWireMessage::Success(message) => Self::Success(message),
            AlpacaOptionMarketDataWireMessage::Error(message) => Self::Error(message),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamQuote {
    #[serde(rename = "S", deserialize_with = "deserialize_msgpack_string")]
    pub symbol: String,
    #[serde(
        rename = "t",
        deserialize_with = "deserialize_msgpack_timestamp_string"
    )]
    pub timestamp: String,
    #[serde(
        default,
        rename = "bx",
        deserialize_with = "deserialize_optional_msgpack_string"
    )]
    pub bid_exchange: Option<String>,
    #[serde(rename = "bp")]
    pub bid_price: f64,
    #[serde(rename = "bs")]
    pub bid_size: u64,
    #[serde(
        default,
        rename = "ax",
        deserialize_with = "deserialize_optional_msgpack_string"
    )]
    pub ask_exchange: Option<String>,
    #[serde(rename = "ap")]
    pub ask_price: f64,
    #[serde(rename = "as")]
    pub ask_size: u64,
    #[serde(
        default,
        rename = "c",
        deserialize_with = "deserialize_msgpack_condition_vec"
    )]
    pub conditions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamTrade {
    #[serde(rename = "S", deserialize_with = "deserialize_msgpack_string")]
    pub symbol: String,
    #[serde(
        rename = "t",
        deserialize_with = "deserialize_msgpack_timestamp_string"
    )]
    pub timestamp: String,
    #[serde(rename = "p")]
    pub price: f64,
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(
        default,
        rename = "x",
        deserialize_with = "deserialize_optional_msgpack_string"
    )]
    pub exchange: Option<String>,
    #[serde(
        default,
        rename = "c",
        deserialize_with = "deserialize_msgpack_condition_vec"
    )]
    pub conditions: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamSubscription {
    #[serde(default, deserialize_with = "deserialize_msgpack_string_vec")]
    pub quotes: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_msgpack_string_vec")]
    pub trades: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamSuccess {
    #[serde(default, deserialize_with = "deserialize_optional_msgpack_string")]
    pub msg: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct AlpacaOptionStreamError {
    #[serde(default)]
    pub code: Option<u16>,
    #[serde(
        default,
        alias = "message",
        deserialize_with = "deserialize_optional_msgpack_string"
    )]
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
        Message::Binary(bytes) => parse_option_market_data_msgpack(bytes.as_ref()),
        Message::Text(text) => {
            if text.as_str() == nautilus_network::RECONNECTED {
                return Ok(vec![AlpacaOptionMarketDataMessage::Reconnected]);
            }
            parse_option_market_data_json(text.as_str())
        }
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) | Message::Close(_) => Ok(vec![]),
    }
}

fn parse_option_market_data_msgpack(
    bytes: &[u8],
) -> anyhow::Result<Vec<AlpacaOptionMarketDataMessage>> {
    rmp_serde::from_slice::<Vec<AlpacaOptionMarketDataWireMessage>>(bytes)
        .map(messages_from_wire)
        .or_else(|batch_error| {
            rmp_serde::from_slice::<AlpacaOptionMarketDataWireMessage>(bytes)
                .map(|message| vec![message.into()])
                .with_context(|| {
                    format!(
                        "failed to decode Alpaca option market-data MsgPack frame: {batch_error}"
                    )
                })
        })
}

fn parse_option_market_data_json(text: &str) -> anyhow::Result<Vec<AlpacaOptionMarketDataMessage>> {
    serde_json::from_str::<Vec<AlpacaOptionMarketDataWireMessage>>(text)
        .map(messages_from_wire)
        .or_else(|batch_error| {
            serde_json::from_str::<AlpacaOptionMarketDataWireMessage>(text)
                .map(|message| vec![message.into()])
                .with_context(|| {
                    format!("failed to decode Alpaca option market-data text frame: {batch_error}")
                })
        })
}

fn messages_from_wire(
    messages: Vec<AlpacaOptionMarketDataWireMessage>,
) -> Vec<AlpacaOptionMarketDataMessage> {
    messages.into_iter().map(Into::into).collect()
}

#[derive(Deserialize)]
struct MsgpackString(#[serde(deserialize_with = "deserialize_msgpack_string")] String);

#[derive(Deserialize)]
#[serde(rename = "_ExtStruct")]
struct MsgpackExt((i8, MsgpackBytes));

#[derive(Deserialize)]
struct MsgpackBytes(#[serde(deserialize_with = "deserialize_msgpack_bytes")] Vec<u8>);

fn deserialize_msgpack_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct MsgpackStringVisitor;

    impl<'de> Visitor<'de> for MsgpackStringVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a UTF-8 string or byte string")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value.to_string())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value)
        }

        fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            std::str::from_utf8(value)
                .map(str::to_string)
                .map_err(E::custom)
        }

        fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            String::from_utf8(value).map_err(E::custom)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut value = Vec::new();
            while let Some(byte) = seq.next_element::<u8>()? {
                value.push(byte);
            }
            String::from_utf8(value).map_err(A::Error::custom)
        }

        fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }
    }

    deserializer.deserialize_any(MsgpackStringVisitor)
}

fn deserialize_msgpack_timestamp_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct MsgpackTimestampVisitor;

    impl<'de> Visitor<'de> for MsgpackTimestampVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an RFC3339 timestamp string or MessagePack timestamp extension")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value.to_string())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value)
        }

        fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            std::str::from_utf8(value)
                .map(str::to_string)
                .map_err(E::custom)
        }

        fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            String::from_utf8(value).map_err(E::custom)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut value = Vec::new();
            while let Some(byte) = seq.next_element::<u8>()? {
                value.push(byte);
            }
            String::from_utf8(value).map_err(A::Error::custom)
        }

        fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            let MsgpackExt((tag, bytes)) = MsgpackExt::deserialize(deserializer)?;
            msgpack_timestamp_ext_to_rfc3339(tag, &bytes.0).map_err(D::Error::custom)
        }
    }

    deserializer.deserialize_any(MsgpackTimestampVisitor)
}

fn deserialize_msgpack_bytes<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct MsgpackBytesVisitor;

    impl<'de> Visitor<'de> for MsgpackBytesVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a MessagePack byte array")
        }

        fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value.to_vec())
        }

        fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut value = Vec::new();
            while let Some(byte) = seq.next_element::<u8>()? {
                value.push(byte);
            }
            Ok(value)
        }
    }

    deserializer.deserialize_any(MsgpackBytesVisitor)
}

fn msgpack_timestamp_ext_to_rfc3339(tag: i8, bytes: &[u8]) -> Result<String, String> {
    if tag != -1 {
        return Err(format!("unsupported MessagePack extension tag {tag}"));
    }

    let (seconds, nanos) = match bytes.len() {
        4 => {
            let seconds = u32::from_be_bytes(
                bytes
                    .try_into()
                    .map_err(|_| "invalid 32-bit timestamp payload".to_string())?,
            );
            (i64::from(seconds), 0)
        }
        8 => {
            let value = u64::from_be_bytes(
                bytes
                    .try_into()
                    .map_err(|_| "invalid 64-bit timestamp payload".to_string())?,
            );
            let nanos = (value >> 34) as u32;
            let seconds = value & ((1_u64 << 34) - 1);
            (
                i64::try_from(seconds).map_err(|error| error.to_string())?,
                nanos,
            )
        }
        12 => {
            let nanos = u32::from_be_bytes(
                bytes[..4]
                    .try_into()
                    .map_err(|_| "invalid 96-bit timestamp nanos".to_string())?,
            );
            let seconds = i64::from_be_bytes(
                bytes[4..]
                    .try_into()
                    .map_err(|_| "invalid 96-bit timestamp seconds".to_string())?,
            );
            (seconds, nanos)
        }
        len => {
            return Err(format!(
                "invalid MessagePack timestamp payload length {len}"
            ));
        }
    };

    let timestamp = DateTime::<Utc>::from_timestamp(seconds, nanos)
        .ok_or_else(|| format!("invalid MessagePack timestamp seconds={seconds} nanos={nanos}"))?;
    Ok(timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true))
}

fn deserialize_optional_msgpack_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<MsgpackString>::deserialize(deserializer)?.map(|value| value.0))
}

fn deserialize_msgpack_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<MsgpackString>::deserialize(deserializer)
        .map(|values| values.into_iter().map(|value| value.0).collect())
}

#[derive(Deserialize)]
struct MsgpackCondition(#[serde(deserialize_with = "deserialize_msgpack_condition")] String);

fn deserialize_msgpack_condition_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct MsgpackConditionsVisitor;

    impl<'de> Visitor<'de> for MsgpackConditionsVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a condition code or list of condition codes")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(vec![value.to_string()])
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(vec![value])
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(vec![value.to_string()])
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(vec![value.to_string()])
        }

        fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            condition_bytes_to_string(value, E::custom).map(|value| vec![value])
        }

        fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            condition_bytes_to_string(&value, E::custom).map(|value| vec![value])
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<MsgpackCondition>()? {
                values.push(value.0);
            }
            Ok(values)
        }

        fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            MsgpackCondition::deserialize(deserializer).map(|value| vec![value.0])
        }
    }

    deserializer.deserialize_any(MsgpackConditionsVisitor)
}

fn deserialize_msgpack_condition<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct MsgpackConditionVisitor;

    impl<'de> Visitor<'de> for MsgpackConditionVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a condition code string, byte string, or numeric code")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value.to_string())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value.to_string())
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value.to_string())
        }

        fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            condition_bytes_to_string(value, E::custom)
        }

        fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            condition_bytes_to_string(&value, E::custom)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<i16>()? {
                values.push(value);
            }

            if values.len() == 1 && !(0..=u8::MAX as i16).contains(&values[0]) {
                return Ok(values[0].to_string());
            }

            if values
                .iter()
                .all(|value| (0..=u8::MAX as i16).contains(value))
            {
                let bytes = values
                    .into_iter()
                    .map(|value| value as u8)
                    .collect::<Vec<_>>();
                return condition_bytes_to_string(&bytes, A::Error::custom);
            }

            Ok(values
                .into_iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(","))
        }

        fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }
    }

    deserializer.deserialize_any(MsgpackConditionVisitor)
}

fn condition_bytes_to_string<E>(
    value: &[u8],
    error: impl FnOnce(std::str::Utf8Error) -> E,
) -> Result<String, E> {
    std::str::from_utf8(value)
        .map(str::to_string)
        .map_err(error)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[derive(Serialize)]
    #[serde(untagged)]
    enum RawOptionMarketDataMessage<'a> {
        Quote(RawOptionStreamQuote<'a>),
        Trade(RawOptionStreamTrade<'a>),
    }

    #[derive(Serialize)]
    struct RawOptionStreamQuote<'a> {
        #[serde(rename = "T")]
        message_type: &'a str,
        #[serde(rename = "S")]
        symbol: &'a [u8],
        #[serde(rename = "t")]
        timestamp: &'a [u8],
        #[serde(rename = "bx")]
        bid_exchange: &'a [u8],
        #[serde(rename = "bp")]
        bid_price: f64,
        #[serde(rename = "bs")]
        bid_size: u64,
        #[serde(rename = "ax")]
        ask_exchange: &'a [u8],
        #[serde(rename = "ap")]
        ask_price: f64,
        #[serde(rename = "as")]
        ask_size: u64,
        #[serde(rename = "c")]
        conditions: Vec<&'a [u8]>,
    }

    #[derive(Serialize)]
    struct RawOptionStreamTrade<'a> {
        #[serde(rename = "T")]
        message_type: &'a str,
        #[serde(rename = "S")]
        symbol: &'a [u8],
        #[serde(rename = "t")]
        timestamp: &'a [u8],
        #[serde(rename = "p")]
        price: f64,
        #[serde(rename = "s")]
        size: u64,
        #[serde(rename = "x")]
        exchange: &'a [u8],
        #[serde(rename = "c")]
        conditions: Vec<&'a [u8]>,
    }

    #[derive(Serialize)]
    struct RawOptionStreamQuoteWithNumericConditions<'a> {
        #[serde(rename = "T")]
        message_type: &'a str,
        #[serde(rename = "S")]
        symbol: &'a [u8],
        #[serde(rename = "t")]
        timestamp: &'a [u8],
        #[serde(rename = "bx")]
        bid_exchange: &'a [u8],
        #[serde(rename = "bp")]
        bid_price: f64,
        #[serde(rename = "bs")]
        bid_size: u64,
        #[serde(rename = "ax")]
        ask_exchange: &'a [u8],
        #[serde(rename = "ap")]
        ask_price: f64,
        #[serde(rename = "as")]
        ask_size: u64,
        #[serde(rename = "c")]
        conditions: Vec<Vec<i8>>,
    }

    struct RawMsgpackBytes<'a>(&'a [u8]);

    impl Serialize for RawMsgpackBytes<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            serializer.serialize_bytes(self.0)
        }
    }

    #[derive(Serialize)]
    #[serde(rename = "_ExtStruct")]
    struct RawMsgpackExt<'a>((i8, RawMsgpackBytes<'a>));

    #[derive(Serialize)]
    struct RawOptionStreamQuoteWithTimestampExt<'a> {
        #[serde(rename = "T")]
        message_type: &'a str,
        #[serde(rename = "S")]
        symbol: &'a [u8],
        #[serde(rename = "t")]
        timestamp: RawMsgpackExt<'a>,
        #[serde(rename = "bx")]
        bid_exchange: &'a [u8],
        #[serde(rename = "bp")]
        bid_price: f64,
        #[serde(rename = "bs")]
        bid_size: u64,
        #[serde(rename = "ax")]
        ask_exchange: &'a [u8],
        #[serde(rename = "ap")]
        ask_price: f64,
        #[serde(rename = "as")]
        ask_size: u64,
        #[serde(rename = "c")]
        conditions: &'a str,
    }

    #[test]
    fn parses_msgpack_quote_and_trade_batch() {
        let raw = vec![
            RawOptionMarketDataMessage::Quote(RawOptionStreamQuote {
                message_type: "q",
                symbol: b"SPY260619P00450000",
                timestamp: b"2026-05-08T14:30:00.123456789Z",
                bid_exchange: b"P",
                bid_price: 1.20,
                bid_size: 4,
                ask_exchange: b"Q",
                ask_price: 1.30,
                ask_size: 6,
                conditions: vec![b"R"],
            }),
            RawOptionMarketDataMessage::Trade(RawOptionStreamTrade {
                message_type: "t",
                symbol: b"SPY260619P00450000",
                timestamp: b"2026-05-08T14:30:01.123456789Z",
                price: 1.25,
                size: 2,
                exchange: b"Q",
                conditions: vec![b"I"],
            }),
        ];
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
    fn parses_msgpack_subscription_ack() {
        let raw = AlpacaOptionMarketDataWireMessage::Subscription(AlpacaOptionStreamSubscription {
            quotes: vec!["SPY260619P00450000".to_string()],
            trades: vec![],
        });
        let bytes = rmp_serde::to_vec_named(&raw).expect("encode fixture");

        let parsed =
            parse_option_market_data_message(&Message::Binary(bytes.into())).expect("parse");

        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            AlpacaOptionMarketDataMessage::Subscription(subscription) => {
                assert_eq!(subscription.quotes, vec!["SPY260619P00450000"]);
                assert!(subscription.trades.is_empty());
            }
            other => panic!("expected subscription, got {other:?}"),
        }
    }

    #[test]
    fn parses_msgpack_quote_with_signed_numeric_condition() {
        let raw = RawOptionStreamQuoteWithNumericConditions {
            message_type: "q",
            symbol: b"SPY260619P00450000",
            timestamp: b"2026-05-08T14:30:00.123456789Z",
            bid_exchange: b"P",
            bid_price: 1.20,
            bid_size: 4,
            ask_exchange: b"Q",
            ask_price: 1.30,
            ask_size: 6,
            conditions: vec![vec![-1]],
        };
        let bytes = rmp_serde::to_vec_named(&vec![raw]).expect("encode fixture");

        let parsed =
            parse_option_market_data_message(&Message::Binary(bytes.into())).expect("parse");

        match parsed.first().expect("parsed message") {
            AlpacaOptionMarketDataMessage::Quote(quote) => {
                assert_eq!(quote.symbol, "SPY260619P00450000");
                assert_eq!(quote.conditions, vec!["-1"]);
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn parses_msgpack_quote_with_timestamp_extension_and_scalar_condition() {
        let seconds = 1_782_753_164_u64;
        let nanos = 729_945_661_u64;
        let timestamp = ((nanos << 34) | seconds).to_be_bytes();
        let raw = RawOptionStreamQuoteWithTimestampExt {
            message_type: "q",
            symbol: b"SPY260706P00739000",
            timestamp: RawMsgpackExt((-1, RawMsgpackBytes(&timestamp))),
            bid_exchange: b"C",
            bid_price: 4.81,
            bid_size: 157,
            ask_exchange: b"I",
            ask_price: 4.84,
            ask_size: 140,
            conditions: " ",
        };
        let bytes = rmp_serde::to_vec_named(&vec![raw]).expect("encode fixture");

        let parsed =
            parse_option_market_data_message(&Message::Binary(bytes.into())).expect("parse");

        match parsed.first().expect("parsed message") {
            AlpacaOptionMarketDataMessage::Quote(quote) => {
                assert_eq!(quote.symbol, "SPY260706P00739000");
                assert_eq!(quote.timestamp, "2026-06-29T17:12:44.729945661Z");
                assert_eq!(quote.conditions, vec![" "]);
            }
            other => panic!("expected quote, got {other:?}"),
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
