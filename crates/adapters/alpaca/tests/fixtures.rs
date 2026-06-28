use std::{
    str::FromStr,
    sync::{Mutex, MutexGuard},
};

use nautilus_alpaca::{
    common::{
        consts::{
            ENV_ALPACA_API_KEY, ENV_ALPACA_API_SECRET, ENV_ALPACA_SECRET_KEY, ENV_APCA_API_KEY_ID,
            ENV_APCA_API_SECRET_KEY,
        },
        credentials::AlpacaCredential,
        urls::{
            DATA_BASE_URL, LIVE_TRADE_UPDATES_WS_URL, LIVE_TRADING_BASE_URL, OPTION_WS_URL,
            PAPER_TRADE_UPDATES_WS_URL, PAPER_TRADING_BASE_URL,
        },
    },
    config::{
        AlpacaDataClientConfig, AlpacaEnvironment, AlpacaExecClientConfig, AlpacaOptionFeed,
        AlpacaStockFeed,
    },
    execution::{
        option_underlying_symbol, order_status_from_alpaca, order_status_reports_from_alpaca,
    },
    http::models::{
        AlpacaOptionContract, AlpacaOrder, OptionSnapshotsRequest, OptionSnapshotsResponse,
    },
    orders::{
        AlpacaOrderSide, AlpacaPositionIntent, MlegOrderPayload,
        build_call_debit_spread_open_order, build_iron_condor_close_order,
        build_iron_condor_open_order, build_put_credit_spread_close_order,
        build_put_credit_spread_open_order,
    },
    parse::parse_option_contract,
};
#[cfg(feature = "live")]
use nautilus_alpaca::{
    execution::{
        fill_reports_from_alpaca_activities, fill_reports_from_trade_update,
        position_status_reports_from_alpaca_positions,
    },
    http::models::{AlpacaActivity, AlpacaPosition},
    websocket::messages::{AlpacaTradeUpdate, AlpacaTradeUpdateLeg},
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    enums::{OptionKind, OrderStatus},
    identifiers::{InstrumentId, VenueOrderId},
    types::{Price, Quantity},
};
#[cfg(feature = "live")]
use nautilus_model::{
    enums::{OrderSide, PositionSideSpecified},
    identifiers::TradeId,
};
use serde_json::{Value, json};

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
    assert_eq!(
        data.resolved_option_market_data_ws_url(),
        format!("{OPTION_WS_URL}/indicative")
    );
    assert_eq!(data.resolved_trading_base_url(), PAPER_TRADING_BASE_URL);
    assert_eq!(data.stock_feed, AlpacaStockFeed::Iex);
    assert_eq!(data.option_feed, AlpacaOptionFeed::Indicative);

    let data_override = AlpacaDataClientConfig {
        environment: AlpacaEnvironment::Live,
        data_base_url: Some("http://localhost:9101".to_string()),
        option_market_data_ws_url: Some("ws://localhost:9103/v1beta1/opra".to_string()),
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
    assert_eq!(
        data_override.resolved_option_market_data_ws_url(),
        "ws://localhost:9103/v1beta1/opra",
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
fn option_symbology_and_contract_parsing_use_alpaca_venue_contracts() {
    assert_eq!(option_underlying_symbol("SPY260619P00450000"), "SPY");
    assert_eq!(option_underlying_symbol("QQQ260619C00350000"), "QQQ");

    let contract = option_contract("SPY260619P00450000", "put", "450.00");
    let instrument = parse_option_contract(&contract).expect("valid option contract");

    assert_eq!(
        instrument.id,
        InstrumentId::from_str("SPY260619P00450000.ALPACA").expect("instrument id"),
    );
    assert_eq!(instrument.raw_symbol.as_str(), "SPY260619P00450000");
    assert_eq!(instrument.underlying.as_str(), "SPY");
    assert_eq!(instrument.option_kind, OptionKind::Put);
    assert_eq!(instrument.strike_price, Price::from("450.00"));
    assert_eq!(instrument.multiplier, Quantity::from("100"));

    let invalid_contract = AlpacaOptionContract {
        option_type: "warrant".to_string(),
        ..option_contract("SPY260619P00450000", "put", "450.00")
    };
    assert!(parse_option_contract(&invalid_contract).is_err());
}

#[test]
fn option_snapshot_fixtures_parse_aliases_quotes_greeks_and_page_tokens() {
    let response: OptionSnapshotsResponse = serde_json::from_value(json!({
        "snapshots": {
            "SPY260619P00450000": {
                "latestQuote": {
                    "bp": "1.20",
                    "ap": "1.30",
                    "bs": "4",
                    "as": "6",
                    "t": "2026-05-08T14:30:00Z"
                },
                "latestTrade": {
                    "p": "1.24",
                    "s": "2",
                    "t": "2026-05-08T14:29:55Z"
                },
                "minuteBar": {
                    "c": "1.25",
                    "v": "10",
                    "t": "2026-05-08T14:29:00Z"
                },
                "greeks": {
                    "d": "-0.24",
                    "g": "0.015"
                },
                "iv": "0.42"
            }
        },
        "next_page_token": "page-2"
    }))
    .expect("option snapshot response");

    let snapshot = response
        .snapshots
        .get("SPY260619P00450000")
        .expect("snapshot");
    let quote = snapshot.latest_quote.as_ref().expect("latest quote");

    assert!(snapshot.has_valid_quote());
    assert_eq!(quote.midpoint(), Some(1.25));
    assert!(snapshot.has_greek_inputs());
    assert_eq!(snapshot.implied_volatility, Some(0.42));
    assert_eq!(response.next_token().as_deref(), Some("page-2"));

    let request = OptionSnapshotsRequest::for_symbols(["SPY260619P00450000", "SPY260619P00445000"]);
    let paged = request.with_symbols_and_page(["QQQ260619C00350000"], Some("next".to_string()));

    assert_eq!(
        request.symbols,
        vec![
            "SPY260619P00450000".to_string(),
            "SPY260619P00445000".to_string(),
        ],
    );
    assert_eq!(paged.symbols, vec!["QQQ260619C00350000".to_string()]);
    assert_eq!(paged.page_token.as_deref(), Some("next"));
}

#[test]
fn order_status_reports_cover_partial_and_terminal_statuses() {
    assert_eq!(
        order_status_from_alpaca(Some("accepted_for_bidding")).expect("accepted status"),
        OrderStatus::Accepted,
    );
    assert_eq!(
        order_status_from_alpaca(Some("replaced")).expect("replaced status"),
        OrderStatus::PendingUpdate,
    );
    assert_eq!(
        order_status_from_alpaca(Some("done_for_day")).expect("expired status"),
        OrderStatus::Expired,
    );
    assert_eq!(
        order_status_from_alpaca(Some("stopped")).expect("rejected status"),
        OrderStatus::Rejected,
    );

    for (status, expected) in [
        ("partially_filled", OrderStatus::PartiallyFilled),
        ("canceled", OrderStatus::Canceled),
        ("expired", OrderStatus::Expired),
        ("rejected", OrderStatus::Rejected),
        ("replaced", OrderStatus::PendingUpdate),
    ] {
        let order = order_fixture(
            "order-status",
            "SPY260619P00450000",
            "buy",
            status,
            "2",
            if status == "partially_filled" {
                "1"
            } else {
                "0"
            },
        );
        let reports = order_status_reports_from_alpaca(&order, "ALPACA-001", UnixNanos::from(1))
            .expect("order status report");

        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].venue_order_id,
            VenueOrderId::from("order-status")
        );
        assert_eq!(reports[0].order_status, expected);
        assert_eq!(
            reports[0].instrument_id,
            InstrumentId::from_str("SPY260619P00450000.ALPACA").expect("instrument id"),
        );
    }
}

#[cfg(feature = "live")]
#[test]
fn fill_and_position_fixtures_map_trade_activity_partial_fills_and_open_positions() {
    let partial_update = AlpacaTradeUpdate {
        event: "partial_fill".to_string(),
        order: order_fixture(
            "parent-order",
            "SPY260619P00450000",
            "sell",
            "partially_filled",
            "2",
            "0.5",
        ),
        execution_id: Some("partial-exec".to_string()),
        price: Some("0.42".to_string()),
        qty: Some("0.5".to_string()),
        position_qty: None,
        timestamp: Some("2026-05-08T14:31:00Z".to_string()),
        legs: None,
    };
    let partial_reports =
        fill_reports_from_trade_update(&partial_update, "ALPACA-001", UnixNanos::from(1))
            .expect("partial fill report");

    assert_eq!(partial_reports.len(), 1);
    assert_eq!(
        partial_reports[0].instrument_id,
        InstrumentId::from_str("SPY260619P00450000.ALPACA").expect("instrument id"),
    );
    assert_eq!(
        partial_reports[0].venue_order_id,
        VenueOrderId::from("parent-order"),
    );
    assert_eq!(partial_reports[0].trade_id, TradeId::new("partial-exec"));
    assert_eq!(partial_reports[0].order_side, OrderSide::Sell);
    assert_eq!(partial_reports[0].last_qty, Quantity::from("0.5"));
    assert_eq!(partial_reports[0].last_px, Price::from("0.42"));

    let activities = vec![
        AlpacaActivity {
            activity_type: Some("FILL".to_string()),
            id: Some("20260508143100000::fill-1".to_string()),
            cum_qty: Some("1".to_string()),
            leaves_qty: Some("0".to_string()),
            price: Some("0.72".to_string()),
            qty: Some("1".to_string()),
            side: Some("buy".to_string()),
            symbol: Some("SPY260619P00450000".to_string()),
            transaction_time: Some("2026-05-08T14:31:00Z".to_string()),
            order_id: Some("close-leg".to_string()),
            activity_subtype: None,
            date: None,
            net_amount: None,
            cusip: None,
            per_share_amount: None,
        },
        AlpacaActivity {
            activity_type: Some("OPEXP".to_string()),
            id: Some("expiry-1".to_string()),
            cum_qty: None,
            leaves_qty: None,
            price: None,
            qty: Some("1".to_string()),
            side: None,
            symbol: Some("SPY260619P00450000".to_string()),
            transaction_time: None,
            order_id: None,
            activity_subtype: None,
            date: Some("2026-06-19".to_string()),
            net_amount: None,
            cusip: None,
            per_share_amount: None,
        },
    ];
    let activity_reports =
        fill_reports_from_alpaca_activities(&activities, "ALPACA-001", UnixNanos::from(1))
            .expect("activity fill reports");

    assert_eq!(activity_reports.len(), 1);
    assert_eq!(
        activity_reports[0].venue_order_id,
        VenueOrderId::from("close-leg"),
    );
    assert_eq!(
        activity_reports[0].trade_id,
        TradeId::new("20260508143100000::fill-1"),
    );
    assert_eq!(activity_reports[0].order_side, OrderSide::Buy);

    let positions = vec![AlpacaPosition {
        asset_id: Some("asset-1".to_string()),
        symbol: Some("SPY260619P00450000".to_string()),
        exchange: Some("OPRA".to_string()),
        asset_class: Some("us_option".to_string()),
        qty: Some("1".to_string()),
        side: Some("short".to_string()),
        market_value: None,
        cost_basis: None,
        current_price: None,
        unrealized_pl: None,
        unrealized_plpc: None,
        avg_entry_price: Some("0.40".to_string()),
    }];
    let position_reports =
        position_status_reports_from_alpaca_positions(&positions, "ALPACA-001", UnixNanos::from(1))
            .expect("position reports");

    assert_eq!(position_reports.len(), 1);
    assert_eq!(
        position_reports[0].instrument_id,
        InstrumentId::from_str("SPY260619P00450000.ALPACA").expect("instrument id"),
    );
    assert_eq!(
        position_reports[0].position_side,
        PositionSideSpecified::Short,
    );
}

#[cfg(feature = "live")]
#[test]
fn mleg_trade_update_fixtures_map_per_leg_fills() {
    let mut parent = empty_order();
    parent.id = Some("parent-order".to_string());
    parent.client_order_id = Some("nautilus-parent".to_string());
    parent.created_at = Some("2026-05-08T14:30:00Z".to_string());
    parent.updated_at = Some("2026-05-08T14:31:00Z".to_string());
    parent.order_type = Some("limit".to_string());
    parent.time_in_force = Some("day".to_string());
    parent.status = Some("filled".to_string());
    parent.limit_price = Some("-0.40".to_string());
    parent.legs = Some(vec![
        order_fixture(
            "short-leg",
            "SPY260619P00450000",
            "sell",
            "filled",
            "1",
            "1",
        ),
        order_fixture("long-leg", "SPY260619P00445000", "buy", "filled", "1", "1"),
    ]);

    let update = AlpacaTradeUpdate {
        event: "fill".to_string(),
        order: parent,
        execution_id: Some("parent-exec".to_string()),
        price: None,
        qty: None,
        position_qty: None,
        timestamp: Some("2026-05-08T14:31:00Z".to_string()),
        legs: Some(vec![
            AlpacaTradeUpdateLeg {
                execution_id: Some("short-exec".to_string()),
                price: Some("0.55".to_string()),
                qty: Some("1".to_string()),
                position_qty: None,
                order_id: Some("short-leg".to_string()),
                symbol: Some("SPY260619P00450000".to_string()),
                timestamp: Some("2026-05-08T14:31:00Z".to_string()),
                side: None,
            },
            AlpacaTradeUpdateLeg {
                execution_id: Some("long-exec".to_string()),
                price: Some("0.15".to_string()),
                qty: Some("1".to_string()),
                position_qty: None,
                order_id: Some("long-leg".to_string()),
                symbol: Some("SPY260619P00445000".to_string()),
                timestamp: Some("2026-05-08T14:31:00Z".to_string()),
                side: None,
            },
        ]),
    };
    let reports = fill_reports_from_trade_update(&update, "ALPACA-001", UnixNanos::from(1))
        .expect("mleg fill reports");

    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].venue_order_id, VenueOrderId::from("short-leg"));
    assert_eq!(reports[0].trade_id, TradeId::new("short-exec"));
    assert_eq!(reports[0].order_side, OrderSide::Sell);
    assert_eq!(reports[1].venue_order_id, VenueOrderId::from("long-leg"));
    assert_eq!(reports[1].trade_id, TradeId::new("long-exec"));
    assert_eq!(reports[1].order_side, OrderSide::Buy);
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

fn option_contract(symbol: &str, option_type: &str, strike_price: &str) -> AlpacaOptionContract {
    AlpacaOptionContract {
        id: format!("{symbol}-id"),
        symbol: symbol.to_string(),
        name: Some(format!("{symbol} option")),
        status: Some("active".to_string()),
        tradable: Some(true),
        expiration_date: "2026-06-19".to_string(),
        root_symbol: Some(option_underlying_symbol(symbol)),
        underlying_symbol: option_underlying_symbol(symbol),
        underlying_asset_id: Some("underlying-asset".to_string()),
        option_type: option_type.to_string(),
        style: Some("american".to_string()),
        strike_price: strike_price.to_string(),
        size: Some("100".to_string()),
        open_interest: Some("1234".to_string()),
        open_interest_date: Some("2026-05-07".to_string()),
        close_price: Some("1.25".to_string()),
        close_price_date: Some("2026-05-07".to_string()),
        ppind: Some(true),
    }
}

fn order_fixture(
    id: &str,
    symbol: &str,
    side: &str,
    status: &str,
    qty: &str,
    filled_qty: &str,
) -> AlpacaOrder {
    let mut order = empty_order();
    order.id = Some(id.to_string());
    order.client_order_id = Some(format!("client-{id}"));
    order.created_at = Some("2026-05-08T14:30:00Z".to_string());
    order.updated_at = Some("2026-05-08T14:31:00Z".to_string());
    order.submitted_at = Some("2026-05-08T14:30:05Z".to_string());
    order.filled_at = (status == "filled").then(|| "2026-05-08T14:31:00Z".to_string());
    order.expired_at = (status == "expired").then(|| "2026-05-08T14:31:00Z".to_string());
    order.canceled_at = (status == "canceled").then(|| "2026-05-08T14:31:00Z".to_string());
    order.failed_at = (status == "rejected").then(|| "2026-05-08T14:31:00Z".to_string());
    order.symbol = Some(symbol.to_string());
    order.qty = Some(qty.to_string());
    order.filled_qty = Some(filled_qty.to_string());
    order.order_type = Some("limit".to_string());
    order.side = Some(side.to_string());
    order.time_in_force = Some("day".to_string());
    order.limit_price = Some("0.42".to_string());
    order.status = Some(status.to_string());
    order
}

fn empty_order() -> AlpacaOrder {
    AlpacaOrder {
        id: None,
        client_order_id: None,
        created_at: None,
        updated_at: None,
        submitted_at: None,
        filled_at: None,
        expired_at: None,
        canceled_at: None,
        failed_at: None,
        asset_id: None,
        symbol: None,
        asset_class: None,
        qty: None,
        filled_qty: None,
        filled_avg_price: None,
        order_type: None,
        side: None,
        time_in_force: None,
        limit_price: None,
        status: None,
        order_class: None,
        legs: None,
    }
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
