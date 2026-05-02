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

//! Operator status command for the supervised Alpaca account engine.

use std::{collections::BTreeSet, env, path::PathBuf, process::Command};

use chrono::{DateTime, Duration, Utc};
use nautilus_alpaca::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{AlpacaAccount, AlpacaActivity, AlpacaOrder, AlpacaPosition, ListOrdersRequest},
    },
    runtime::{StrategyState, load_strategy_state, read_operator_events},
};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug)]
struct OperatorConfig {
    service_name: String,
    state_path: PathBuf,
    log_path: PathBuf,
    lock_path: PathBuf,
    stale_order_secs: i64,
    kill_switch: bool,
    submit_enabled: bool,
    manage_enabled: bool,
    close_enabled: bool,
    json_output: bool,
}

#[derive(Debug, Serialize)]
struct OperatorStatus {
    checked_at_utc: String,
    engine_state: EngineState,
    service: ServiceStatus,
    account: AccountStatus,
    orders: OrdersStatus,
    positions: PositionsStatus,
    strategy_state: StrategyStateStatus,
    last_scan: Option<Value>,
    last_decision: Option<Value>,
    last_broker_event: Option<Value>,
    alerts: Vec<OperatorAlert>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngineState {
    Idle,
    Trading,
    Blocked,
    Broken,
}

#[derive(Debug, Serialize)]
struct ServiceStatus {
    name: String,
    active: Option<bool>,
    active_state: Option<String>,
    lock_file: String,
    lock_file_exists: bool,
    log_file: String,
    log_file_exists: bool,
    kill_switch: bool,
    submit_enabled: bool,
    manage_enabled: bool,
    close_enabled: bool,
}

#[derive(Debug, Serialize)]
struct AccountStatus {
    status: String,
    currency: String,
    trading_blocked: bool,
    transfers_blocked: bool,
    account_blocked: bool,
    trade_suspended_by_user: bool,
    buying_power: String,
    options_buying_power: String,
    portfolio_value: String,
    cash: String,
}

#[derive(Debug, Serialize)]
struct OrdersStatus {
    open: usize,
    open_mleg: usize,
    nested_legs: usize,
    stale_working: usize,
    recent_rejected: usize,
}

#[derive(Debug, Serialize)]
struct PositionsStatus {
    total: usize,
    options: usize,
    equities: usize,
    unmanaged: usize,
}

#[derive(Debug, Serialize)]
struct StrategyStateStatus {
    path: String,
    exists: bool,
    entries: usize,
    active_entries: usize,
    closed_entries: usize,
    canceled_entries: usize,
    last_recorded_at_utc: Option<String>,
}

#[derive(Debug, Serialize)]
struct OperatorAlert {
    severity: AlertSeverity,
    code: &'static str,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = OperatorConfig::from_env();
    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();

    let client = AlpacaHttpClient::from_data_config(&data_config)?;
    let account = client.account().await?;
    let positions = client.positions().await?;
    let open_orders = client.orders(&open_orders_request()).await?;
    let recent_orders = client.orders(&recent_orders_request()).await?;
    let activities = latest_activities(&client).await.unwrap_or_default();
    let state = load_strategy_state(&config.state_path)?;
    let events = read_operator_events(&config.log_path);

    let status = build_status(
        &config,
        &account,
        &positions,
        &open_orders,
        &recent_orders,
        &activities,
        &state,
        &events,
    );

    if config.json_output {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_human_status(&status);
    }

    if matches!(status.engine_state, EngineState::Broken) {
        std::process::exit(1);
    }

    Ok(())
}

impl OperatorConfig {
    fn from_env() -> Self {
        let state_home = env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".local/state"));
        let default_state_dir = state_home.join("nautilus_trader");
        let log_dir = env::var("NAUTILUS_ALPACA_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_state_dir.join("logs"));
        let lock_dir = env::var("NAUTILUS_ALPACA_LOCK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_state_dir.join("locks"));

        Self {
            service_name: env::var("NAUTILUS_ALPACA_SERVICE")
                .unwrap_or_else(|_| "alpaca-index-credit.service".to_string()),
            state_path: env::var("ALPACA_INDEX_PUT_CREDIT_STATE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    default_state_dir.join("alpaca_index_put_credit_entry_state.json")
                }),
            log_path: log_dir.join("alpaca-index-credit.log"),
            lock_path: lock_dir.join("alpaca-index-credit.lock"),
            stale_order_secs: env_u64("ALPACA_INDEX_CREDIT_STALE_ENTRY_SECS", 900) as i64,
            kill_switch: env_bool("ALPACA_INDEX_CREDIT_KILL_SWITCH", false),
            submit_enabled: env_bool("ALPACA_INDEX_PUT_CREDIT_SUBMIT", false),
            manage_enabled: env_bool("ALPACA_INDEX_CREDIT_MANAGE", false),
            close_enabled: env_bool("ALPACA_INDEX_CREDIT_CLOSE", false),
            json_output: env::args().any(|arg| arg == "--json"),
        }
    }
}

fn build_status(
    config: &OperatorConfig,
    account: &AlpacaAccount,
    positions: &[AlpacaPosition],
    open_orders: &[AlpacaOrder],
    recent_orders: &[AlpacaOrder],
    activities: &[AlpacaActivity],
    state: &StrategyState,
    events: &[Value],
) -> OperatorStatus {
    let now = Utc::now();
    let active_state_symbols = active_strategy_symbols(state);
    let unmanaged_positions = positions
        .iter()
        .filter(|position| {
            position
                .symbol
                .as_ref()
                .is_none_or(|symbol| !active_state_symbols.contains(symbol))
        })
        .count();
    let stale_working_orders = open_orders
        .iter()
        .filter(|order| order_is_stale(order, now, config.stale_order_secs))
        .count();
    let recent_rejected = recent_orders
        .iter()
        .filter(|order| order.status.as_deref() == Some("rejected"))
        .count();

    let account_status = AccountStatus {
        status: account
            .status
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        currency: account
            .currency
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        trading_blocked: account.trading_blocked.unwrap_or(false),
        transfers_blocked: account.transfers_blocked.unwrap_or(false),
        account_blocked: account.account_blocked.unwrap_or(false),
        trade_suspended_by_user: account.trade_suspended_by_user.unwrap_or(false),
        buying_power: account
            .buying_power
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        options_buying_power: account
            .options_buying_power
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        portfolio_value: account
            .portfolio_value
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        cash: account
            .cash
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
    };

    let orders_status = OrdersStatus {
        open: open_orders.len(),
        open_mleg: open_orders
            .iter()
            .filter(|order| order.order_class.as_deref() == Some("mleg"))
            .count(),
        nested_legs: open_orders
            .iter()
            .map(|order| order.legs.as_ref().map_or(0, Vec::len))
            .sum(),
        stale_working: stale_working_orders,
        recent_rejected,
    };

    let positions_status = PositionsStatus {
        total: positions.len(),
        options: positions
            .iter()
            .filter(|position| asset_class_is(position, "us_option"))
            .count(),
        equities: positions
            .iter()
            .filter(|position| asset_class_is(position, "us_equity"))
            .count(),
        unmanaged: unmanaged_positions,
    };

    let strategy_state = StrategyStateStatus {
        path: config.state_path.display().to_string(),
        exists: config.state_path.exists(),
        entries: state.entries.len(),
        active_entries: state
            .entries
            .iter()
            .filter(|entry| entry.is_active())
            .count(),
        closed_entries: state.entries.iter().filter(|entry| entry.closed).count(),
        canceled_entries: state.entries.iter().filter(|entry| entry.canceled).count(),
        last_recorded_at_utc: state
            .entries
            .iter()
            .filter_map(|entry| non_empty(&entry.recorded_at_utc))
            .max(),
    };

    let service = ServiceStatus {
        name: config.service_name.clone(),
        active: service_active(&config.service_name),
        active_state: service_active_state(&config.service_name),
        lock_file: config.lock_path.display().to_string(),
        lock_file_exists: config.lock_path.exists(),
        log_file: config.log_path.display().to_string(),
        log_file_exists: config.log_path.exists(),
        kill_switch: config.kill_switch,
        submit_enabled: config.submit_enabled,
        manage_enabled: config.manage_enabled,
        close_enabled: config.close_enabled,
    };

    let last_scan = latest_event(events, "strategy_iteration");
    let last_decision = latest_event(events, "decision");
    let last_broker_event = latest_broker_event(recent_orders, activities);

    let mut alerts = build_alerts(
        config,
        &service,
        &account_status,
        &orders_status,
        &positions_status,
        &strategy_state,
        open_orders,
        events,
    );
    alerts.sort_by_key(|alert| match alert.severity {
        AlertSeverity::Critical => 0,
        AlertSeverity::Warning => 1,
        AlertSeverity::Info => 2,
    });

    let engine_state = classify_engine_state(
        &alerts,
        config,
        &orders_status,
        &positions_status,
        &account_status,
    );

    OperatorStatus {
        checked_at_utc: now.to_rfc3339(),
        engine_state,
        service,
        account: account_status,
        orders: orders_status,
        positions: positions_status,
        strategy_state,
        last_scan,
        last_decision,
        last_broker_event,
        alerts,
    }
}

fn build_alerts(
    config: &OperatorConfig,
    service: &ServiceStatus,
    account: &AccountStatus,
    orders: &OrdersStatus,
    positions: &PositionsStatus,
    strategy_state: &StrategyStateStatus,
    open_orders: &[AlpacaOrder],
    events: &[Value],
) -> Vec<OperatorAlert> {
    let mut alerts = Vec::new();

    if service.active == Some(false) {
        alerts.push(alert(
            AlertSeverity::Critical,
            "service_inactive",
            format!("{} is not active", service.name),
        ));
    }
    if account.status != "ACTIVE" {
        alerts.push(alert(
            AlertSeverity::Critical,
            "account_not_active",
            format!("Alpaca account status is {}", account.status),
        ));
    }
    if account.trading_blocked || account.account_blocked || account.trade_suspended_by_user {
        alerts.push(alert(
            AlertSeverity::Critical,
            "account_trading_blocked",
            "Alpaca account is blocked or trading is suspended".to_string(),
        ));
    }
    if config.kill_switch {
        alerts.push(alert(
            AlertSeverity::Info,
            "kill_switch_active",
            "new entries are blocked by ALPACA_INDEX_CREDIT_KILL_SWITCH=true".to_string(),
        ));
    }
    if !config.submit_enabled {
        alerts.push(alert(
            AlertSeverity::Info,
            "submission_disabled",
            "entry submission is disabled".to_string(),
        ));
    }
    if orders.recent_rejected > 0 {
        alerts.push(alert(
            AlertSeverity::Warning,
            "recent_rejected_orders",
            format!(
                "{} recent Alpaca orders are rejected",
                orders.recent_rejected
            ),
        ));
    }
    if orders.stale_working > 0 {
        alerts.push(alert(
            AlertSeverity::Warning,
            "stale_working_orders",
            format!(
                "{} open orders are older than {} seconds",
                orders.stale_working, config.stale_order_secs
            ),
        ));
    }
    if positions.unmanaged > 0 {
        alerts.push(alert(
            AlertSeverity::Critical,
            "unmanaged_positions",
            format!(
                "{} open positions are not represented in strategy state",
                positions.unmanaged
            ),
        ));
    }
    if strategy_state.active_entries > 0 && open_orders.is_empty() && positions.total == 0 {
        alerts.push(alert(
            AlertSeverity::Warning,
            "state_broker_mismatch",
            "strategy state has active entries but broker has no open orders or positions"
                .to_string(),
        ));
    }
    if !strategy_state.exists && (orders.open > 0 || positions.total > 0) {
        alerts.push(alert(
            AlertSeverity::Critical,
            "missing_strategy_state",
            "broker exposure exists but the local strategy state file is missing".to_string(),
        ));
    }
    if recent_event_count(events, "runner_start", 3600) > 1 {
        alerts.push(alert(
            AlertSeverity::Warning,
            "service_restart_recent",
            "the runner emitted multiple start events in the last hour".to_string(),
        ));
    }
    if recent_event_count(events, "websocket_disconnect", 3600) > 0 {
        alerts.push(alert(
            AlertSeverity::Critical,
            "websocket_disconnect",
            "a trade-update websocket disconnect event was emitted in the last hour".to_string(),
        ));
    }
    if recent_event_count(events, "reconciliation_error", 3600) > 0 {
        alerts.push(alert(
            AlertSeverity::Critical,
            "reconciliation_error",
            "a broker reconciliation repair error was emitted in the last hour".to_string(),
        ));
    }

    alerts
}

fn classify_engine_state(
    alerts: &[OperatorAlert],
    config: &OperatorConfig,
    orders: &OrdersStatus,
    positions: &PositionsStatus,
    account: &AccountStatus,
) -> EngineState {
    if alerts
        .iter()
        .any(|alert| alert.severity == AlertSeverity::Critical)
    {
        EngineState::Broken
    } else if config.kill_switch
        || !config.submit_enabled
        || account.trading_blocked
        || account.trade_suspended_by_user
    {
        EngineState::Blocked
    } else if orders.open > 0 || positions.total > 0 {
        EngineState::Trading
    } else {
        EngineState::Idle
    }
}

fn print_human_status(status: &OperatorStatus) {
    println!(
        "engine: state={:?} checked_at_utc={}",
        status.engine_state, status.checked_at_utc
    );
    println!(
        "service: name={} active={} kill_switch={} submit={} manage={} close={} lock={} log={}",
        status.service.name,
        status.service.active_state.as_deref().unwrap_or("unknown"),
        status.service.kill_switch,
        status.service.submit_enabled,
        status.service.manage_enabled,
        status.service.close_enabled,
        status.service.lock_file,
        status.service.log_file,
    );
    println!(
        "account: status={} trading_blocked={} account_blocked={} suspended={} buying_power={} options_buying_power={} portfolio_value={} cash={}",
        status.account.status,
        status.account.trading_blocked,
        status.account.account_blocked,
        status.account.trade_suspended_by_user,
        status.account.buying_power,
        status.account.options_buying_power,
        status.account.portfolio_value,
        status.account.cash,
    );
    println!(
        "orders: open={} mleg={} nested_legs={} stale_working={} recent_rejected={}",
        status.orders.open,
        status.orders.open_mleg,
        status.orders.nested_legs,
        status.orders.stale_working,
        status.orders.recent_rejected,
    );
    println!(
        "positions: total={} options={} equities={} unmanaged={}",
        status.positions.total,
        status.positions.options,
        status.positions.equities,
        status.positions.unmanaged,
    );
    println!(
        "strategy_state: exists={} entries={} active={} closed={} canceled={} path={}",
        status.strategy_state.exists,
        status.strategy_state.entries,
        status.strategy_state.active_entries,
        status.strategy_state.closed_entries,
        status.strategy_state.canceled_entries,
        status.strategy_state.path,
    );
    println!(
        "last_scan: {}",
        status
            .last_scan
            .as_ref()
            .map_or_else(|| "none".to_string(), compact_json)
    );
    println!(
        "last_decision: {}",
        status
            .last_decision
            .as_ref()
            .map_or_else(|| "none".to_string(), compact_json)
    );
    println!(
        "last_broker_event: {}",
        status
            .last_broker_event
            .as_ref()
            .map_or_else(|| "none".to_string(), compact_json)
    );
    println!("alerts: count={}", status.alerts.len());
    for alert in &status.alerts {
        println!(
            "alert: severity={:?} code={} message={}",
            alert.severity, alert.code, alert.message
        );
    }
}

fn open_orders_request() -> ListOrdersRequest {
    let mut request = ListOrdersRequest::open_nested();
    request.limit = env_u64("ALPACA_OPERATOR_OPEN_ORDER_LIMIT", 100) as usize;
    request
}

fn recent_orders_request() -> ListOrdersRequest {
    ListOrdersRequest {
        status: "all".to_string(),
        limit: env_u64("ALPACA_OPERATOR_RECENT_ORDER_LIMIT", 100) as usize,
        direction: Some("desc".to_string()),
        nested: true,
        ..ListOrdersRequest::default()
    }
}

async fn latest_activities(client: &AlpacaHttpClient) -> anyhow::Result<Vec<AlpacaActivity>> {
    let request = nautilus_alpaca::http::models::ListActivitiesRequest {
        page_size: env_u64("ALPACA_OPERATOR_ACTIVITY_LIMIT", 20) as usize,
        ..nautilus_alpaca::http::models::ListActivitiesRequest::option_reconciliation()
    };
    Ok(client.account_activities(&request).await?)
}

fn latest_event(events: &[Value], event_type: &str) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .cloned()
}

fn recent_event_count(events: &[Value], event_type: &str, lookback_secs: i64) -> usize {
    let cutoff = Utc::now() - Duration::seconds(lookback_secs);
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .filter(|event| {
            event
                .get("ts_utc")
                .and_then(Value::as_str)
                .and_then(parse_utc)
                .is_some_and(|ts| ts >= cutoff)
        })
        .count()
}

fn latest_broker_event(orders: &[AlpacaOrder], activities: &[AlpacaActivity]) -> Option<Value> {
    if let Some(activity) = activities.first() {
        return Some(json!({
            "source": "account_activity",
            "activity_type": activity.activity_type,
            "symbol": activity.symbol,
            "order_id": activity.order_id,
            "transaction_time": activity.transaction_time,
        }));
    }

    orders.first().map(|order| {
        json!({
            "source": "order",
            "id": order.id,
            "client_order_id": order.client_order_id,
            "status": order.status,
            "symbol": order.symbol,
            "updated_at": order.updated_at,
            "submitted_at": order.submitted_at,
        })
    })
}

fn active_strategy_symbols(state: &StrategyState) -> BTreeSet<String> {
    let mut symbols = BTreeSet::new();
    for entry in state.entries.iter().filter(|entry| entry.is_active()) {
        if !entry.short_symbol.is_empty() {
            symbols.insert(entry.short_symbol.clone());
        }
        if !entry.long_symbol.is_empty() {
            symbols.insert(entry.long_symbol.clone());
        }
    }
    symbols
}

fn order_is_stale(order: &AlpacaOrder, now: DateTime<Utc>, stale_order_secs: i64) -> bool {
    if !order.is_working() {
        return false;
    }
    let submitted_at = order
        .submitted_at
        .as_deref()
        .or(order.created_at.as_deref())
        .and_then(parse_utc);
    submitted_at
        .is_some_and(|ts| now.signed_duration_since(ts) > Duration::seconds(stale_order_secs))
}

fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| datetime.with_timezone(&Utc))
        .ok()
}

fn service_active(service_name: &str) -> Option<bool> {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", service_name])
        .status()
        .ok()
        .map(|status| status.success())
}

fn service_active_state(service_name: &str) -> Option<String> {
    Command::new("systemctl")
        .args(["--user", "is-active", service_name])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn asset_class_is(position: &AlpacaPosition, expected: &str) -> bool {
    position
        .asset_class
        .as_deref()
        .is_some_and(|value| value == expected)
}

fn alert(severity: AlertSeverity, code: &'static str, message: String) -> OperatorAlert {
    OperatorAlert {
        severity,
        code,
        message,
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn non_empty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "unprintable".to_string())
}
