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

//! Native Nautilus runner for the Alpaca `index_put_credit_entry` Phase 3 slice.
//!
//! The runner scans configured underlyings, applies broker/state admission gates, selects one
//! put-credit candidate, and either records a dry-run decision or submits a real Nautilus
//! `SubmitOrderList` through the Alpaca execution client. Submission is disabled by default.

use std::{
    cell::RefCell,
    env, fs,
    path::{Path, PathBuf},
    rc::Rc,
    str::FromStr,
    time::Duration,
};

use chrono::{NaiveTime, Utc};
use chrono_tz::Tz;
use nautilus_alpaca::{
    AlpacaExecutionClient,
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    execution::check_put_credit_entry_admission,
    http::{client::AlpacaHttpClient, error::Error, models::ListOrdersRequest},
    strategy::{PutCreditScannerConfig, SpreadCandidate, scan_put_credit_underlying},
    submit::{MlegSubmitLeg, MlegSubmitOrderListRequest, build_mleg_submit_order_list},
};
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{ExecutionEvent, execution::SubmitOrderList},
};
use nautilus_core::{UUID4, time::get_atomic_clock_realtime};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType, OrderSide},
    events::OrderEventAny,
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, OrderListId, StrategyId, TraderId, Venue,
    },
    types::{Price, Quantity},
};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, timeout},
};

const DEFAULT_EVENT_TIMEOUT_SECS: u64 = 20;
const STRATEGY_FAMILY: &str = "INDEX-PUT-CREDIT-ENTRY";

#[derive(Debug)]
struct RunnerConfig {
    underlyings: Vec<String>,
    max_iterations: u64,
    interval_secs: u64,
    quantity: u64,
    submit_enabled: bool,
    cancel_after_accept: bool,
    ignore_entry_window: bool,
    entry_start: NaiveTime,
    entry_end: NaiveTime,
    entry_timezone: Tz,
    state_path: PathBuf,
    scanner: PutCreditScannerConfig,
}

#[derive(Clone, Debug)]
struct SelectedEntry {
    underlying: String,
    candidate: SpreadCandidate,
}

#[derive(Clone, Debug)]
struct SubmitOutcome {
    accepted: usize,
    rejected: usize,
    parent_order_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StrategyState {
    entries: Vec<StrategyStateEntry>,
}

impl StrategyState {
    fn has_submitted_underlying(&self, trade_date: &str, underlying: &str) -> bool {
        self.entries.iter().any(|entry| {
            entry.submitted && entry.trade_date == trade_date && entry.underlying == underlying
        })
    }

    fn record_submission(
        &mut self,
        trade_date: String,
        underlying: String,
        order_list_id: String,
        candidate: &SpreadCandidate,
        parent_order_id: Option<String>,
    ) {
        self.entries.push(StrategyStateEntry {
            trade_date,
            underlying,
            order_list_id,
            short_symbol: candidate.short.symbol.clone(),
            long_symbol: candidate.long.symbol.clone(),
            credit: candidate.credit,
            score: candidate.score,
            parent_order_id,
            submitted: true,
            recorded_at_utc: Utc::now().to_rfc3339(),
        });
    }
}

impl Default for StrategyState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StrategyStateEntry {
    trade_date: String,
    underlying: String,
    order_list_id: String,
    short_symbol: String,
    long_symbol: String,
    credit: f64,
    score: f64,
    parent_order_id: Option<String>,
    submitted: bool,
    recorded_at_utc: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = RunnerConfig::from_env()?;
    let mut state = load_state(&config.state_path)?;
    let trade_date = market_trade_date(&config);

    println!(
        "index_put_credit_entry: underlyings={} submit_enabled={} quantity={} state_path={}",
        config.underlyings.join(","),
        config.submit_enabled,
        config.quantity,
        config.state_path.display(),
    );

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    let http_client = AlpacaHttpClient::from_data_config(&data_config)?;

    for iteration in 1..=config.max_iterations {
        println!("strategy_iteration={iteration} trade_date={trade_date}");

        if !config.ignore_entry_window && !inside_entry_window(&config) {
            println!(
                "decision: skipped reason=outside_entry_window window={}-{} timezone={}",
                config.entry_start, config.entry_end, config.entry_timezone
            );
        } else {
            let selected =
                select_entry(&http_client, &data_config, &config, &state, &trade_date).await?;
            match selected {
                Some(entry) if config.submit_enabled => {
                    let order_list_id = order_list_id(&trade_date, &entry.underlying);
                    println!(
                        "decision: submit underlying={} short={} long={} credit={:.2} ror={:.1}% score={:.1} order_list_id={}",
                        entry.underlying,
                        entry.candidate.short.symbol,
                        entry.candidate.long.symbol,
                        entry.candidate.credit,
                        entry.candidate.return_on_risk * 100.0,
                        entry.candidate.score,
                        order_list_id,
                    );
                    let outcome =
                        submit_entry(&entry, &order_list_id, config.quantity, &config).await?;
                    if outcome.accepted > 0 {
                        state.record_submission(
                            trade_date.clone(),
                            entry.underlying,
                            order_list_id,
                            &entry.candidate,
                            outcome.parent_order_id,
                        );
                        save_state(&config.state_path, &state)?;
                    }
                    println!(
                        "submit_result: accepted={} rejected={}",
                        outcome.accepted, outcome.rejected
                    );
                }
                Some(entry) => {
                    println!(
                        "decision: dry_run underlying={} short={} long={} credit={:.2} ror={:.1}% score={:.1} reason=submission_disabled",
                        entry.underlying,
                        entry.candidate.short.symbol,
                        entry.candidate.long.symbol,
                        entry.candidate.credit,
                        entry.candidate.return_on_risk * 100.0,
                        entry.candidate.score,
                    );
                }
                None => println!("decision: no_entry"),
            }
        }

        if iteration < config.max_iterations {
            sleep(Duration::from_secs(config.interval_secs)).await;
        }
    }

    Ok(())
}

async fn select_entry(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &RunnerConfig,
    state: &StrategyState,
    trade_date: &str,
) -> anyhow::Result<Option<SelectedEntry>> {
    let account = client.account().await?;
    let positions = client.positions().await?;
    let open_orders = client.orders(&ListOrdersRequest::open_nested()).await?;
    let mut selected: Option<SelectedEntry> = None;

    for underlying in &config.underlyings {
        if state.has_submitted_underlying(trade_date, underlying) {
            println!("{underlying}: admission_rejected reason=daily_duplicate_state");
            continue;
        }

        let result =
            scan_put_credit_underlying(client, data_config, &config.scanner, underlying).await?;
        let Some(best) = result.candidates.first() else {
            println!(
                "{underlying}: no_candidate contracts={} snapshots={} scoreable={}",
                result.contract_count, result.snapshot_count, result.scoreable_count,
            );
            continue;
        };

        let admission = check_put_credit_entry_admission(
            &account,
            &positions,
            &open_orders,
            &best.short.symbol,
            &best.long.symbol,
        );
        if !admission.allowed {
            println!(
                "{underlying}: admission_rejected short={} long={} reasons={}",
                best.short.symbol,
                best.long.symbol,
                admission.reasons.join(" | "),
            );
            continue;
        }

        println!(
            "{underlying}: candidate short={} long={} credit={:.2} ror={:.1}% score={:.1}",
            best.short.symbol,
            best.long.symbol,
            best.credit,
            best.return_on_risk * 100.0,
            best.score,
        );

        if selected
            .as_ref()
            .is_none_or(|current| best.score > current.candidate.score)
        {
            selected = Some(SelectedEntry {
                underlying: underlying.clone(),
                candidate: best.clone(),
            });
        }
    }

    Ok(selected)
}

async fn submit_entry(
    entry: &SelectedEntry,
    order_list_id: &str,
    quantity: u64,
    config: &RunnerConfig,
) -> anyhow::Result<SubmitOutcome> {
    let exec_config = exec_config_from_env();
    let (tx, mut rx) = mpsc::unbounded_channel();
    replace_exec_event_sender(tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let trader_id = TraderId::from("TRADER-001");
    let client_id = ClientId::from(ALPACA_CLIENT_ID);
    let account_id = AccountId::from("ALPACA-001");
    let strategy_id = StrategyId::from(STRATEGY_FAMILY);
    let core = ExecutionClientCore::new(
        trader_id,
        client_id,
        Venue::new(ALPACA_VENUE),
        OmsType::Netting,
        account_id,
        AccountType::Margin,
        None,
        cache,
    );
    let mut client = AlpacaExecutionClient::new(core, exec_config.clone())?;
    client.start()?;
    client.connect().await?;

    let cmd = build_submit_order_list(
        entry,
        order_list_id,
        quantity,
        trader_id,
        Some(client_id),
        strategy_id,
    )?;
    client.submit_order_list(cmd)?;

    let (accepted, rejected) = collect_execution_events(&mut rx, 2).await;
    let parent_order_id = lookup_parent_order(&exec_config, order_list_id).await?;
    if config.cancel_after_accept && accepted > 0 {
        cancel_parent_order(&exec_config, parent_order_id.as_deref()).await?;
    }

    client.disconnect().await?;
    client.stop()?;

    Ok(SubmitOutcome {
        accepted,
        rejected,
        parent_order_id,
    })
}

fn build_submit_order_list(
    entry: &SelectedEntry,
    order_list_id: &str,
    quantity: u64,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    if quantity == 0 {
        anyhow::bail!("quantity must be positive");
    }

    let order_list_id = OrderListId::from(order_list_id);
    let short_client_id = ClientOrderId::from(format!("{order_list_id}-short").as_str());
    let long_client_id = ClientOrderId::from(format!("{order_list_id}-long").as_str());
    let quantity = Quantity::new(quantity as f64, 0);
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: vec![
            MlegSubmitLeg {
                client_order_id: short_client_id,
                instrument_id: alpaca_instrument_id(&entry.candidate.short.symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(entry.candidate.short.bid, 2),
                reduce_only: false,
            },
            MlegSubmitLeg {
                client_order_id: long_client_id,
                instrument_id: alpaca_instrument_id(&entry.candidate.long.symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(entry.candidate.long.ask, 2),
                reduce_only: false,
            },
        ],
        ts_init: get_atomic_clock_realtime().get_time_ns(),
    })
}

async fn collect_execution_events(
    rx: &mut mpsc::UnboundedReceiver<ExecutionEvent>,
    leg_count: usize,
) -> (usize, usize) {
    let mut accepted = 0;
    let mut rejected = 0;
    let deadline = Instant::now()
        + Duration::from_secs(env_parse(
            "ALPACA_INDEX_PUT_CREDIT_EVENT_TIMEOUT_SECS",
            DEFAULT_EVENT_TIMEOUT_SECS,
        ));

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(Some(event)) = timeout(remaining, rx.recv()).await else {
            break;
        };

        if let ExecutionEvent::Order(order_event) = event {
            print_order_event(&order_event);
            match order_event {
                OrderEventAny::Accepted(_) => accepted += 1,
                OrderEventAny::Rejected(_) => rejected += 1,
                _ => {}
            }
            if accepted + rejected >= leg_count {
                break;
            }
        }
    }

    (accepted, rejected)
}

fn print_order_event(order_event: &OrderEventAny) {
    let event_type = order_event.event_type();
    let event = order_event.clone().into_boxed();
    println!(
        "execution_event: order type={event_type:?} client_order_id={} instrument_id={} venue_order_id={} reason={}",
        event.client_order_id(),
        event.instrument_id(),
        event
            .venue_order_id()
            .map_or_else(|| "None".to_string(), |value| value.to_string()),
        event
            .reason()
            .map_or_else(|| "None".to_string(), |value| value.to_string()),
    );
}

async fn lookup_parent_order(
    config: &AlpacaExecClientConfig,
    order_list_id: &str,
) -> anyhow::Result<Option<String>> {
    let client = AlpacaHttpClient::from_exec_config(config)?;
    match client.order_by_client_order_id(order_list_id, true).await {
        Ok(order) => Ok(order.id),
        Err(Error::HttpStatus { status, .. }) if status == 404 => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn cancel_parent_order(
    config: &AlpacaExecClientConfig,
    parent_order_id: Option<&str>,
) -> anyhow::Result<()> {
    let Some(parent_order_id) = parent_order_id else {
        println!("cleanup: skipped reason=no_parent_order_id");
        return Ok(());
    };

    let client = AlpacaHttpClient::from_exec_config(config)?;
    client.cancel_order(parent_order_id).await?;
    println!("cleanup: cancel_requested parent_order_id={parent_order_id}");
    Ok(())
}

fn alpaca_instrument_id(symbol: &str) -> anyhow::Result<InstrumentId> {
    Ok(InstrumentId::from_str(&format!("{symbol}.{ALPACA_VENUE}"))?)
}

fn order_list_id(trade_date: &str, underlying: &str) -> String {
    format!(
        "index-put-credit-{trade_date}-{underlying}-{}",
        UUID4::new()
    )
}

fn inside_entry_window(config: &RunnerConfig) -> bool {
    let now = Utc::now().with_timezone(&config.entry_timezone).time();
    config.entry_start <= now && now <= config.entry_end
}

fn market_trade_date(config: &RunnerConfig) -> String {
    Utc::now()
        .with_timezone(&config.entry_timezone)
        .date_naive()
        .to_string()
}

fn load_state(path: &Path) -> anyhow::Result<StrategyState> {
    if !path.exists() {
        return Ok(StrategyState::default());
    }
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn save_state(path: &Path, state: &StrategyState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

impl RunnerConfig {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            underlyings: underlyings_from_env(),
            max_iterations: env_parse("ALPACA_INDEX_PUT_CREDIT_MAX_ITERATIONS", 1_u64),
            interval_secs: env_parse("ALPACA_INDEX_PUT_CREDIT_INTERVAL_SECS", 300_u64),
            quantity: env_parse("ALPACA_INDEX_PUT_CREDIT_QTY", 1_u64),
            submit_enabled: env_bool("ALPACA_INDEX_PUT_CREDIT_SUBMIT", false),
            cancel_after_accept: env_bool("ALPACA_INDEX_PUT_CREDIT_CANCEL_AFTER_ACCEPT", false),
            ignore_entry_window: env_bool("ALPACA_INDEX_PUT_CREDIT_IGNORE_WINDOW", false),
            entry_start: parse_time_env("ALPACA_INDEX_PUT_CREDIT_ENTRY_START", "09:45")?,
            entry_end: parse_time_env("ALPACA_INDEX_PUT_CREDIT_ENTRY_END", "14:30")?,
            entry_timezone: env::var("ALPACA_INDEX_PUT_CREDIT_ENTRY_TZ")
                .unwrap_or_else(|_| "America/New_York".to_string())
                .parse::<Tz>()?,
            state_path: env::var("ALPACA_INDEX_PUT_CREDIT_STATE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_state_path()),
            scanner: scanner_config_from_env(),
        })
    }
}

fn underlyings_from_env() -> Vec<String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if !args.is_empty() {
        return split_underlyings(args);
    }
    env::var("ALPACA_INDEX_PUT_CREDIT_UNDERLYINGS")
        .ok()
        .map(|value| split_underlyings([value]))
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| {
            ["SPY", "QQQ", "IWM", "DIA", "GLD"]
                .into_iter()
                .map(ToString::to_string)
                .collect()
        })
}

fn split_underlyings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn scanner_config_from_env() -> PutCreditScannerConfig {
    PutCreditScannerConfig {
        min_dte: env_parse("ALPACA_PUT_CREDIT_MIN_DTE", 5_i64),
        max_dte: env_parse("ALPACA_PUT_CREDIT_MAX_DTE", 10_i64),
        short_delta_min: env_parse("ALPACA_PUT_CREDIT_SHORT_DELTA_MIN", 0.18_f64),
        short_delta_max: env_parse("ALPACA_PUT_CREDIT_SHORT_DELTA_MAX", 0.28_f64),
        widths: env::var("ALPACA_PUT_CREDIT_WIDTHS")
            .ok()
            .and_then(|value| parse_csv_f64(&value))
            .unwrap_or_else(|| vec![2.0, 3.0, 5.0]),
        min_open_interest: env_parse("ALPACA_PUT_CREDIT_MIN_OPEN_INTEREST", 200_u64),
        max_leg_spread_pct: env_parse("ALPACA_PUT_CREDIT_MAX_LEG_SPREAD_PCT", 0.15_f64),
        min_return_on_risk: env_parse("ALPACA_PUT_CREDIT_MIN_RETURN_ON_RISK", 0.13_f64),
    }
}

fn parse_csv_f64(value: &str) -> Option<Vec<f64>> {
    let values = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!values.is_empty()).then_some(values)
}

fn parse_time_env(name: &str, default: &str) -> anyhow::Result<NaiveTime> {
    Ok(NaiveTime::parse_from_str(
        &env::var(name).unwrap_or_else(|_| default.to_string()),
        "%H:%M",
    )?)
}

fn default_state_path() -> PathBuf {
    if let Some(value) = env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(value)
            .join("nautilus_trader")
            .join("alpaca_index_put_credit_entry_state.json");
    }
    if let Some(value) = env::var_os("HOME") {
        return PathBuf::from(value)
            .join(".local")
            .join("state")
            .join("nautilus_trader")
            .join("alpaca_index_put_credit_entry_state.json");
    }
    PathBuf::from("alpaca_index_put_credit_entry_state.json")
}

fn exec_config_from_env() -> AlpacaExecClientConfig {
    let mut config = AlpacaExecClientConfig::default();
    config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    config.trade_updates_ws_url = env::var("ALPACA_TRADE_UPDATES_WS_URL").ok();
    config.external_order_filtering = false;
    config
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
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
