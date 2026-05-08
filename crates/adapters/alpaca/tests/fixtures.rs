use std::sync::{Mutex, MutexGuard};

use nautilus_alpaca::{
    common::{
        consts::{
            ENV_ALPACA_API_KEY, ENV_ALPACA_API_SECRET, ENV_ALPACA_SECRET_KEY, ENV_APCA_API_KEY_ID,
            ENV_APCA_API_SECRET_KEY,
        },
        credentials::AlpacaCredential,
        urls::{
            DATA_BASE_URL, LIVE_TRADE_UPDATES_WS_URL, LIVE_TRADING_BASE_URL,
            PAPER_TRADE_UPDATES_WS_URL, PAPER_TRADING_BASE_URL,
        },
    },
    config::{
        AlpacaDataClientConfig, AlpacaEnvironment, AlpacaExecClientConfig, AlpacaOptionFeed,
        AlpacaStockFeed,
    },
    orders::{
        AlpacaOrderSide, AlpacaPositionIntent, MlegOrderPayload,
        build_call_debit_spread_open_order, build_iron_condor_close_order,
        build_iron_condor_open_order, build_put_credit_spread_close_order,
        build_put_credit_spread_open_order,
    },
};
use serde_json::Value;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const CREDENTIAL_ENV_VARS: [&str; 5] = [
    ENV_APCA_API_KEY_ID,
    ENV_ALPACA_API_KEY,
    ENV_APCA_API_SECRET_KEY,
    ENV_ALPACA_SECRET_KEY,
    ENV_ALPACA_API_SECRET,
];

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved_values: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn cleared() -> Self {
        let lock = ENV_LOCK.lock().expect("env lock poisoned");
        let guard = Self {
            _lock: lock,
            saved_values: CREDENTIAL_ENV_VARS
                .iter()
                .map(|name| (*name, std::env::var(name).ok()))
                .collect(),
        };

        for name in CREDENTIAL_ENV_VARS {
            remove_env(name);
        }

        guard
    }

    fn set(&self, name: &'static str, value: &str) {
        set_env(name, value);
    }

    fn remove(&self, name: &'static str) {
        remove_env(name);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved_values {
            match value {
                Some(value) => set_env(name, value),
                None => remove_env(name),
            }
        }
    }
}

fn set_env(name: &str, value: &str) {
    // SAFETY: These fixture tests serialize all mutations of the Alpaca credential environment
    // variables through ENV_LOCK and restore the original values before releasing the lock.
    unsafe {
        std::env::set_var(name, value);
    }
}

fn remove_env(name: &str) {
    // SAFETY: These fixture tests serialize all mutations of the Alpaca credential environment
    // variables through ENV_LOCK and restore the original values before releasing the lock.
    unsafe {
        std::env::remove_var(name);
    }
}

#[test]
fn credentials_resolve_primary_and_fallback_environment_names() {
    let env = EnvGuard::cleared();

    env.set(ENV_ALPACA_API_KEY, "fallback-key");
    env.set(ENV_ALPACA_SECRET_KEY, "fallback-secret");
    env.set(ENV_ALPACA_API_SECRET, "alternate-secret");
    env.set(ENV_APCA_API_KEY_ID, "primary-key");
    env.set(ENV_APCA_API_SECRET_KEY, "primary-secret");

    let primary = AlpacaCredential::resolve(None, None).expect("primary env credentials");
    assert_eq!(primary.api_key(), "primary-key");
    assert_eq!(primary.api_secret(), "primary-secret");

    env.remove(ENV_APCA_API_KEY_ID);
    env.remove(ENV_APCA_API_SECRET_KEY);

    let fallback = AlpacaCredential::resolve(None, None).expect("fallback env credentials");
    assert_eq!(fallback.api_key(), "fallback-key");
    assert_eq!(fallback.api_secret(), "fallback-secret");
}

#[test]
fn configs_resolve_default_and_override_endpoints() {
    let data = AlpacaDataClientConfig::default();
    assert_eq!(data.resolved_data_base_url(), DATA_BASE_URL);
    assert_eq!(data.resolved_trading_base_url(), PAPER_TRADING_BASE_URL);
    assert_eq!(data.stock_feed, AlpacaStockFeed::Iex);
    assert_eq!(data.option_feed, AlpacaOptionFeed::Indicative);

    let data_override = AlpacaDataClientConfig {
        environment: AlpacaEnvironment::Live,
        data_base_url: Some("http://localhost:9101".to_string()),
        trading_base_url: Some("http://localhost:9102".to_string()),
        stock_feed: AlpacaStockFeed::Sip,
        option_feed: AlpacaOptionFeed::Opra,
        ..Default::default()
    };
    assert_eq!(
        data_override.resolved_data_base_url(),
        "http://localhost:9101"
    );
    assert_eq!(
        data_override.resolved_trading_base_url(),
        "http://localhost:9102",
    );
    assert_eq!(data_override.stock_feed.as_str(), "sip");
    assert_eq!(data_override.option_feed.as_str(), "opra");

    let exec_paper = AlpacaExecClientConfig::default();
    assert_eq!(
        exec_paper.resolved_trading_base_url(),
        PAPER_TRADING_BASE_URL
    );
    assert_eq!(
        exec_paper.resolved_trade_updates_ws_url(),
        PAPER_TRADE_UPDATES_WS_URL,
    );

    let exec_live = AlpacaExecClientConfig {
        environment: AlpacaEnvironment::Live,
        ..Default::default()
    };
    assert_eq!(exec_live.resolved_trading_base_url(), LIVE_TRADING_BASE_URL);
    assert_eq!(
        exec_live.resolved_trade_updates_ws_url(),
        LIVE_TRADE_UPDATES_WS_URL,
    );
}

#[test]
fn configs_detect_explicit_credentials_without_environment() {
    let _env = EnvGuard::cleared();

    let data = AlpacaDataClientConfig {
        api_key: Some("explicit-key".to_string()),
        api_secret: Some("explicit-secret".to_string()),
        ..Default::default()
    };
    let exec = AlpacaExecClientConfig {
        api_key: Some("explicit-key".to_string()),
        api_secret: Some("explicit-secret".to_string()),
        ..Default::default()
    };

    assert!(data.has_api_credentials());
    assert!(exec.has_api_credentials());
    assert!(!AlpacaDataClientConfig::default().has_api_credentials());
    assert!(!AlpacaExecClientConfig::default().has_api_credentials());
}

#[test]
fn spread_builders_serialize_signed_net_premium_contracts() {
    let open_credit = order_value(
        build_put_credit_spread_open_order("SPY260619P00450000", "SPY260619P00445000", 0.40, 1)
            .expect("put credit open order"),
    );
    assert_eq!(open_credit["limit_price"].as_str(), Some("-0.40"));
    assert_leg(
        &open_credit,
        0,
        "SPY260619P00450000",
        AlpacaOrderSide::Sell,
        AlpacaPositionIntent::SellToOpen,
    );
    assert_leg(
        &open_credit,
        1,
        "SPY260619P00445000",
        AlpacaOrderSide::Buy,
        AlpacaPositionIntent::BuyToOpen,
    );

    let close_credit = order_value(
        build_put_credit_spread_close_order("SPY260619P00450000", "SPY260619P00445000", 0.20, 1)
            .expect("put credit close order"),
    );
    assert_eq!(close_credit["limit_price"].as_str(), Some("0.20"));
    assert_leg(
        &close_credit,
        0,
        "SPY260619P00450000",
        AlpacaOrderSide::Buy,
        AlpacaPositionIntent::BuyToClose,
    );
    assert_leg(
        &close_credit,
        1,
        "SPY260619P00445000",
        AlpacaOrderSide::Sell,
        AlpacaPositionIntent::SellToClose,
    );

    let open_debit = order_value(
        build_call_debit_spread_open_order("SPY260619C00450000", "SPY260619C00455000", 1.25, 1)
            .expect("call debit open order"),
    );
    assert_eq!(open_debit["limit_price"].as_str(), Some("1.25"));
    assert_leg(
        &open_debit,
        0,
        "SPY260619C00450000",
        AlpacaOrderSide::Buy,
        AlpacaPositionIntent::BuyToOpen,
    );
    assert_leg(
        &open_debit,
        1,
        "SPY260619C00455000",
        AlpacaOrderSide::Sell,
        AlpacaPositionIntent::SellToOpen,
    );
}

#[test]
fn iron_condor_builders_serialize_four_leg_open_and_close_contracts() {
    let open = order_value(
        build_iron_condor_open_order(
            "SPY260619P00450000",
            "SPY260619P00445000",
            "SPY260619C00460000",
            "SPY260619C00465000",
            0.95,
            1,
        )
        .expect("iron condor open order"),
    );
    assert_eq!(open["limit_price"].as_str(), Some("-0.95"));
    assert_leg(
        &open,
        0,
        "SPY260619P00450000",
        AlpacaOrderSide::Sell,
        AlpacaPositionIntent::SellToOpen,
    );
    assert_leg(
        &open,
        1,
        "SPY260619P00445000",
        AlpacaOrderSide::Buy,
        AlpacaPositionIntent::BuyToOpen,
    );
    assert_leg(
        &open,
        2,
        "SPY260619C00460000",
        AlpacaOrderSide::Sell,
        AlpacaPositionIntent::SellToOpen,
    );
    assert_leg(
        &open,
        3,
        "SPY260619C00465000",
        AlpacaOrderSide::Buy,
        AlpacaPositionIntent::BuyToOpen,
    );

    let close = order_value(
        build_iron_condor_close_order(
            "SPY260619P00450000",
            "SPY260619P00445000",
            "SPY260619C00460000",
            "SPY260619C00465000",
            0.35,
            1,
        )
        .expect("iron condor close order"),
    );
    assert_eq!(close["limit_price"].as_str(), Some("0.35"));
    assert_leg(
        &close,
        0,
        "SPY260619P00450000",
        AlpacaOrderSide::Buy,
        AlpacaPositionIntent::BuyToClose,
    );
    assert_leg(
        &close,
        1,
        "SPY260619P00445000",
        AlpacaOrderSide::Sell,
        AlpacaPositionIntent::SellToClose,
    );
    assert_leg(
        &close,
        2,
        "SPY260619C00460000",
        AlpacaOrderSide::Buy,
        AlpacaPositionIntent::BuyToClose,
    );
    assert_leg(
        &close,
        3,
        "SPY260619C00465000",
        AlpacaOrderSide::Sell,
        AlpacaPositionIntent::SellToClose,
    );
}

fn order_value(payload: MlegOrderPayload) -> Value {
    serde_json::to_value(payload).expect("order payload serializes")
}

fn assert_leg(
    payload: &Value,
    leg_index: usize,
    symbol: &str,
    side: AlpacaOrderSide,
    position_intent: AlpacaPositionIntent,
) {
    let leg = &payload["legs"][leg_index];

    assert_eq!(leg["symbol"].as_str(), Some(symbol));
    assert_eq!(leg["ratio_qty"].as_str(), Some("1"));
    assert_eq!(leg["side"].as_str(), Some(serialized_side(side)));
    assert_eq!(
        leg["position_intent"].as_str(),
        Some(serialized_position_intent(position_intent)),
    );
}

const fn serialized_side(side: AlpacaOrderSide) -> &'static str {
    match side {
        AlpacaOrderSide::Buy => "buy",
        AlpacaOrderSide::Sell => "sell",
    }
}

const fn serialized_position_intent(position_intent: AlpacaPositionIntent) -> &'static str {
    match position_intent {
        AlpacaPositionIntent::BuyToOpen => "buy_to_open",
        AlpacaPositionIntent::BuyToClose => "buy_to_close",
        AlpacaPositionIntent::SellToOpen => "sell_to_open",
        AlpacaPositionIntent::SellToClose => "sell_to_close",
    }
}
