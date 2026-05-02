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

use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Tz;
use nautilus_alpaca::{
    AlpacaExecutionClient,
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    execution::check_put_credit_entry_admission,
    http::{
        client::AlpacaHttpClient,
        error::Error,
        models::{AlpacaOrder, ListOrdersRequest, OptionSnapshotsRequest},
    },
    strategy::{
        CreditSpreadKind, PutCreditScannerConfig, SpreadCandidate, scan_call_credit_underlying,
        scan_put_credit_underlying,
    },
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
use serde_json::{Map, Value, json};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, timeout},
};

const DEFAULT_EVENT_TIMEOUT_SECS: u64 = 20;
const STRATEGY_FAMILY: &str = "INDEX-PUT-CREDIT-ENTRY";

#[derive(Debug)]
struct RunnerConfig {
    underlyings: Vec<String>,
    spread_kinds: Vec<CreditSpreadKind>,
    max_iterations: u64,
    interval_secs: u64,
    quantity: u64,
    submit_enabled: bool,
    manage_enabled: bool,
    kill_switch: bool,
    force_flatten: bool,
    cancel_after_accept: bool,
    stale_entry_secs: u64,
    close_enabled: bool,
    profit_target_close_fraction: f64,
    stop_loss_close_multiple: f64,
    max_hold_secs: u64,
    expiration_exit_days: i64,
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
    kind: CreditSpreadKind,
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
            entry.submitted
                && !entry.closed
                && !entry.canceled
                && entry.trade_date == trade_date
                && entry.underlying == underlying
        })
    }

    fn record_submission(
        &mut self,
        trade_date: String,
        underlying: String,
        kind: CreditSpreadKind,
        quantity: u64,
        order_list_id: String,
        candidate: &SpreadCandidate,
        parent_order_id: Option<String>,
    ) {
        self.entries.push(StrategyStateEntry {
            trade_date,
            underlying,
            strategy: strategy_name(kind).to_string(),
            order_list_id,
            short_symbol: candidate.short.symbol.clone(),
            long_symbol: candidate.long.symbol.clone(),
            quantity,
            credit: candidate.credit,
            score: candidate.score,
            parent_order_id,
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: Utc::now().to_rfc3339(),
            closed_at_utc: None,
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
    #[serde(default = "default_strategy_name")]
    strategy: String,
    order_list_id: String,
    short_symbol: String,
    long_symbol: String,
    #[serde(default = "default_quantity")]
    quantity: u64,
    credit: f64,
    score: f64,
    parent_order_id: Option<String>,
    #[serde(default)]
    close_order_list_id: Option<String>,
    #[serde(default)]
    close_parent_order_id: Option<String>,
    #[serde(default)]
    close_reason: Option<String>,
    submitted: bool,
    #[serde(default)]
    canceled: bool,
    #[serde(default)]
    closed: bool,
    recorded_at_utc: String,
    #[serde(default)]
    closed_at_utc: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = RunnerConfig::from_env()?;
    let mut state = load_state(&config.state_path)?;

    println!(
        "index_credit_entry: underlyings={} strategies={} submit_enabled={} manage_enabled={} close_enabled={} kill_switch={} quantity={} state_path={}",
        config.underlyings.join(","),
        config
            .spread_kinds
            .iter()
            .map(|kind| strategy_name(*kind))
            .collect::<Vec<_>>()
            .join(","),
        config.submit_enabled,
        config.manage_enabled,
        config.close_enabled,
        config.kill_switch,
        config.quantity,
        config.state_path.display(),
    );
    emit_operator_event(
        "runner_start",
        json!({
            "underlyings": &config.underlyings,
            "strategies": config.spread_kinds.iter().map(|kind| strategy_name(*kind)).collect::<Vec<_>>(),
            "submit_enabled": config.submit_enabled,
            "manage_enabled": config.manage_enabled,
            "close_enabled": config.close_enabled,
            "kill_switch": config.kill_switch,
            "quantity": config.quantity,
            "state_path": config.state_path.display().to_string(),
        }),
    );

    let mut data_config = AlpacaDataClientConfig::default();
    data_config.trading_base_url = env::var("ALPACA_TRADING_BASE_URL").ok();
    data_config.data_base_url = env::var("ALPACA_DATA_BASE_URL").ok();
    let http_client = AlpacaHttpClient::from_data_config(&data_config)?;

    let mut iteration = 1_u64;
    loop {
        let trade_date = market_trade_date(&config);
        println!("strategy_iteration={iteration} trade_date={trade_date}");
        emit_operator_event(
            "strategy_iteration",
            json!({
                "iteration": iteration,
                "trade_date": trade_date,
            }),
        );

        if manage_existing_entries(&http_client, &data_config, &config, &mut state).await? {
            save_state(&config.state_path, &state)?;
        }

        if config.kill_switch {
            println!("decision: skipped reason=kill_switch_enabled");
            emit_operator_event(
                "decision",
                json!({
                    "action": "skipped",
                    "reason": "kill_switch_enabled",
                    "trade_date": trade_date,
                }),
            );
        } else if !config.ignore_entry_window && !inside_entry_window(&config) {
            println!(
                "decision: skipped reason=outside_entry_window window={}-{} timezone={}",
                config.entry_start, config.entry_end, config.entry_timezone
            );
            emit_operator_event(
                "decision",
                json!({
                    "action": "skipped",
                    "reason": "outside_entry_window",
                    "window_start": config.entry_start.to_string(),
                    "window_end": config.entry_end.to_string(),
                    "timezone": config.entry_timezone.to_string(),
                    "trade_date": trade_date,
                }),
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
                    emit_operator_event(
                        "decision",
                        json!({
                            "action": "submit",
                            "underlying": &entry.underlying,
                            "strategy": strategy_name(entry.kind),
                            "short_symbol": &entry.candidate.short.symbol,
                            "long_symbol": &entry.candidate.long.symbol,
                            "credit": entry.candidate.credit,
                            "return_on_risk": entry.candidate.return_on_risk,
                            "score": entry.candidate.score,
                            "order_list_id": &order_list_id,
                            "trade_date": trade_date,
                        }),
                    );
                    let outcome =
                        submit_entry(&entry, &order_list_id, config.quantity, &config).await?;
                    if outcome.accepted > 0 {
                        state.record_submission(
                            trade_date.clone(),
                            entry.underlying,
                            entry.kind,
                            config.quantity,
                            order_list_id,
                            &entry.candidate,
                            outcome.parent_order_id.clone(),
                        );
                        save_state(&config.state_path, &state)?;
                    }
                    println!(
                        "submit_result: accepted={} rejected={}",
                        outcome.accepted, outcome.rejected
                    );
                    emit_operator_event(
                        "submit_result",
                        json!({
                            "accepted": outcome.accepted,
                            "rejected": outcome.rejected,
                            "parent_order_id": outcome.parent_order_id,
                        }),
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
                    emit_operator_event(
                        "decision",
                        json!({
                            "action": "dry_run",
                            "reason": "submission_disabled",
                            "underlying": &entry.underlying,
                            "strategy": strategy_name(entry.kind),
                            "short_symbol": &entry.candidate.short.symbol,
                            "long_symbol": &entry.candidate.long.symbol,
                            "credit": entry.candidate.credit,
                            "return_on_risk": entry.candidate.return_on_risk,
                            "score": entry.candidate.score,
                            "trade_date": trade_date,
                        }),
                    );
                }
                None => {
                    println!("decision: no_entry");
                    emit_operator_event(
                        "decision",
                        json!({
                            "action": "no_entry",
                            "trade_date": trade_date,
                        }),
                    );
                }
            }
        }

        if config.max_iterations != 0 && iteration >= config.max_iterations {
            break;
        }

        iteration = iteration.saturating_add(1);
        sleep(Duration::from_secs(config.interval_secs)).await;
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

        for kind in &config.spread_kinds {
            let result = match kind {
                CreditSpreadKind::Put => {
                    scan_put_credit_underlying(client, data_config, &config.scanner, underlying)
                        .await?
                }
                CreditSpreadKind::Call => {
                    scan_call_credit_underlying(client, data_config, &config.scanner, underlying)
                        .await?
                }
            };
            let Some(best) = result.candidates.first() else {
                println!(
                    "{underlying}: no_candidate strategy={} contracts={} snapshots={} scoreable={}",
                    strategy_name(*kind),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
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
                    "{underlying}: admission_rejected strategy={} short={} long={} reasons={}",
                    strategy_name(*kind),
                    best.short.symbol,
                    best.long.symbol,
                    admission.reasons.join(" | "),
                );
                continue;
            }

            println!(
                "{underlying}: candidate strategy={} short={} long={} credit={:.2} ror={:.1}% score={:.1}",
                strategy_name(*kind),
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
                    kind: *kind,
                    candidate: best.clone(),
                });
            }
        }
    }

    Ok(selected)
}

async fn manage_existing_entries(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    config: &RunnerConfig,
    state: &mut StrategyState,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for entry in state
        .entries
        .iter_mut()
        .filter(|entry| entry.submitted && !entry.closed && !entry.canceled)
    {
        if let Some(close_order_list_id) = entry.close_order_list_id.as_deref() {
            if let Some(order) = lookup_parent_order_snapshot(client, close_order_list_id).await? {
                println!(
                    "manage: close_order order_list_id={} status={}",
                    close_order_list_id,
                    order.status.as_deref().unwrap_or("unknown"),
                );
                if order.status.as_deref() == Some("filled") {
                    entry.closed = true;
                    entry.close_parent_order_id = order.id;
                    entry.closed_at_utc = Some(Utc::now().to_rfc3339());
                    changed = true;
                }
            }
            continue;
        }

        let Some(entry_order) = lookup_parent_order_snapshot(client, &entry.order_list_id).await?
        else {
            println!(
                "manage: entry_order_missing order_list_id={}",
                entry.order_list_id
            );
            continue;
        };

        if entry_order.is_working() {
            let stale = config.stale_entry_secs > 0
                && order_age_secs(&entry_order).is_some_and(|age| age >= config.stale_entry_secs);
            if stale {
                println!(
                    "manage: stale_entry order_list_id={} status={} manage_enabled={}",
                    entry.order_list_id,
                    entry_order.status.as_deref().unwrap_or("unknown"),
                    config.manage_enabled,
                );
                if config.manage_enabled {
                    cancel_parent_order_by_id(client, entry_order.id.as_deref()).await?;
                    entry.canceled = true;
                    changed = true;
                }
            }
            continue;
        }

        if matches!(
            entry_order.status.as_deref(),
            Some("canceled" | "expired" | "rejected")
        ) {
            entry.canceled = true;
            changed = true;
            println!(
                "manage: entry_terminal order_list_id={} status={}",
                entry.order_list_id,
                entry_order.status.as_deref().unwrap_or("unknown"),
            );
            continue;
        }

        let Some(close_quote) = close_quote(client, data_config, entry).await? else {
            println!(
                "manage: close_quote_unavailable short={} long={}",
                entry.short_symbol, entry.long_symbol
            );
            continue;
        };
        let close_reason = close_reason(config, entry, close_quote.debit);
        println!(
            "manage: position underlying={} strategy={} close_debit={:.2} entry_credit={:.2} reason={}",
            entry.underlying,
            entry.strategy,
            close_quote.debit,
            entry.credit,
            close_reason.as_deref().unwrap_or("none"),
        );

        let Some(close_reason) = close_reason else {
            continue;
        };
        if !(config.manage_enabled && config.close_enabled) {
            continue;
        }

        let close_order_list_id = close_order_list_id(entry);
        let outcome = submit_close_entry(entry, &close_quote, &close_order_list_id, config).await?;
        if outcome.accepted > 0 {
            entry.close_order_list_id = Some(close_order_list_id);
            entry.close_parent_order_id = outcome.parent_order_id;
            entry.close_reason = Some(close_reason);
            changed = true;
        }
    }

    Ok(changed)
}

#[derive(Clone, Copy, Debug)]
struct CloseQuote {
    short_ask: f64,
    long_bid: f64,
    debit: f64,
}

async fn close_quote(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    entry: &StrategyStateEntry,
) -> anyhow::Result<Option<CloseQuote>> {
    let mut request = OptionSnapshotsRequest::for_symbols([
        entry.short_symbol.clone(),
        entry.long_symbol.clone(),
    ]);
    request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&request).await?.snapshots;
    let short_quote = snapshots
        .get(&entry.short_symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref());
    let long_quote = snapshots
        .get(&entry.long_symbol)
        .and_then(|snapshot| snapshot.latest_quote.as_ref());
    let (Some(short_ask), Some(long_bid)) = (
        short_quote.and_then(|quote| quote.ask_price),
        long_quote.and_then(|quote| quote.bid_price),
    ) else {
        return Ok(None);
    };
    let debit = short_ask - long_bid;
    if debit <= 0.0 {
        return Ok(None);
    }
    Ok(Some(CloseQuote {
        short_ask,
        long_bid,
        debit,
    }))
}

fn close_reason(
    config: &RunnerConfig,
    entry: &StrategyStateEntry,
    close_debit: f64,
) -> Option<String> {
    if config.force_flatten {
        return Some("manual_flatten".to_string());
    }
    if close_debit <= entry.credit * config.profit_target_close_fraction {
        return Some("profit_target".to_string());
    }
    if close_debit >= entry.credit * config.stop_loss_close_multiple {
        return Some("stop_loss".to_string());
    }
    if config.max_hold_secs > 0
        && recorded_age_secs(entry).is_some_and(|age| age >= config.max_hold_secs)
    {
        return Some("max_hold".to_string());
    }
    if config.expiration_exit_days >= 0
        && days_to_expiration(&entry.short_symbol)
            .is_some_and(|days| days <= config.expiration_exit_days)
    {
        return Some("expiration_risk".to_string());
    }
    None
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

async fn submit_close_entry(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    _config: &RunnerConfig,
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

    let cmd = build_close_submit_order_list(
        entry,
        quote,
        order_list_id,
        trader_id,
        Some(client_id),
        strategy_id,
    )?;
    client.submit_order_list(cmd)?;

    let (accepted, rejected) = collect_execution_events(&mut rx, 2).await;
    let parent_order_id = lookup_parent_order(&exec_config, order_list_id).await?;

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

fn build_close_submit_order_list(
    entry: &StrategyStateEntry,
    quote: &CloseQuote,
    order_list_id: &str,
    trader_id: TraderId,
    client_id: Option<ClientId>,
    strategy_id: StrategyId,
) -> anyhow::Result<SubmitOrderList> {
    let order_list_id = OrderListId::from(order_list_id);
    let short_client_id = ClientOrderId::from(format!("{order_list_id}-short-close").as_str());
    let long_client_id = ClientOrderId::from(format!("{order_list_id}-long-close").as_str());
    let quantity = Quantity::new(entry.quantity as f64, 0);
    build_mleg_submit_order_list(MlegSubmitOrderListRequest {
        trader_id,
        client_id,
        strategy_id,
        order_list_id,
        legs: vec![
            MlegSubmitLeg {
                client_order_id: short_client_id,
                instrument_id: alpaca_instrument_id(&entry.short_symbol)?,
                order_side: OrderSide::Buy,
                quantity,
                limit_price: Price::new(quote.short_ask, 2),
                reduce_only: true,
            },
            MlegSubmitLeg {
                client_order_id: long_client_id,
                instrument_id: alpaca_instrument_id(&entry.long_symbol)?,
                order_side: OrderSide::Sell,
                quantity,
                limit_price: Price::new(quote.long_bid, 2),
                reduce_only: true,
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

async fn lookup_parent_order_snapshot(
    client: &AlpacaHttpClient,
    order_list_id: &str,
) -> anyhow::Result<Option<AlpacaOrder>> {
    match client.order_by_client_order_id(order_list_id, true).await {
        Ok(order) => Ok(Some(order)),
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

async fn cancel_parent_order_by_id(
    client: &AlpacaHttpClient,
    parent_order_id: Option<&str>,
) -> anyhow::Result<()> {
    let Some(parent_order_id) = parent_order_id else {
        println!("cleanup: skipped reason=no_parent_order_id");
        return Ok(());
    };
    client.cancel_order(parent_order_id).await?;
    println!("cleanup: cancel_requested parent_order_id={parent_order_id}");
    Ok(())
}

fn alpaca_instrument_id(symbol: &str) -> anyhow::Result<InstrumentId> {
    Ok(InstrumentId::from_str(&format!("{symbol}.{ALPACA_VENUE}"))?)
}

fn order_list_id(trade_date: &str, underlying: &str) -> String {
    format!(
        "index-credit-entry-{trade_date}-{underlying}-{}",
        UUID4::new()
    )
}

fn close_order_list_id(entry: &StrategyStateEntry) -> String {
    format!(
        "index-credit-close-{}-{}-{}",
        entry.trade_date,
        entry.underlying,
        UUID4::new()
    )
}

fn strategy_name(kind: CreditSpreadKind) -> &'static str {
    match kind {
        CreditSpreadKind::Put => "index_put_credit_entry",
        CreditSpreadKind::Call => "index_call_credit_entry",
    }
}

fn default_strategy_name() -> String {
    strategy_name(CreditSpreadKind::Put).to_string()
}

fn default_quantity() -> u64 {
    1
}

fn order_age_secs(order: &AlpacaOrder) -> Option<u64> {
    order
        .submitted_at
        .as_deref()
        .or(order.created_at.as_deref())
        .and_then(age_secs_from_rfc3339)
}

fn recorded_age_secs(entry: &StrategyStateEntry) -> Option<u64> {
    age_secs_from_rfc3339(&entry.recorded_at_utc)
}

fn days_to_expiration(symbol: &str) -> Option<i64> {
    let chars = symbol.as_bytes();
    for index in 0..chars.len().saturating_sub(6) {
        let date_slice = &chars[index..index + 6];
        let put_call = chars.get(index + 6).copied();
        if date_slice.iter().all(u8::is_ascii_digit) && matches!(put_call, Some(b'P' | b'C')) {
            let value = std::str::from_utf8(date_slice).ok()?;
            let year = 2000 + value[0..2].parse::<i32>().ok()?;
            let month = value[2..4].parse::<u32>().ok()?;
            let day = value[4..6].parse::<u32>().ok()?;
            let expiration = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
            return Some(
                expiration
                    .signed_duration_since(Utc::now().date_naive())
                    .num_days(),
            );
        }
    }
    None
}

fn age_secs_from_rfc3339(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| {
            Utc::now()
                .signed_duration_since(timestamp.with_timezone(&Utc))
                .to_std()
                .ok()
        })
        .map(|duration| duration.as_secs())
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

fn emit_operator_event(event_type: &str, payload: Value) {
    let mut event = Map::new();
    event.insert("ts_utc".to_string(), Value::String(Utc::now().to_rfc3339()));
    event.insert("type".to_string(), Value::String(event_type.to_string()));
    if let Value::Object(fields) = payload {
        event.extend(fields);
    }
    if let Ok(line) = serde_json::to_string(&Value::Object(event)) {
        println!("operator_event={line}");
    }
}

impl RunnerConfig {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            underlyings: underlyings_from_env(),
            spread_kinds: spread_kinds_from_env()?,
            max_iterations: env_parse("ALPACA_INDEX_PUT_CREDIT_MAX_ITERATIONS", 1_u64),
            interval_secs: env_parse("ALPACA_INDEX_PUT_CREDIT_INTERVAL_SECS", 300_u64),
            quantity: env_parse("ALPACA_INDEX_PUT_CREDIT_QTY", 1_u64),
            submit_enabled: env_bool("ALPACA_INDEX_PUT_CREDIT_SUBMIT", false),
            manage_enabled: env_bool("ALPACA_INDEX_CREDIT_MANAGE", false),
            kill_switch: env_bool("ALPACA_INDEX_CREDIT_KILL_SWITCH", false),
            force_flatten: env_bool("ALPACA_INDEX_CREDIT_FORCE_FLATTEN", false),
            cancel_after_accept: env_bool("ALPACA_INDEX_PUT_CREDIT_CANCEL_AFTER_ACCEPT", false),
            stale_entry_secs: env_parse("ALPACA_INDEX_CREDIT_STALE_ENTRY_SECS", 900_u64),
            close_enabled: env_bool("ALPACA_INDEX_CREDIT_CLOSE", false),
            profit_target_close_fraction: env_parse(
                "ALPACA_INDEX_CREDIT_PROFIT_TARGET_CLOSE_FRACTION",
                0.50_f64,
            ),
            stop_loss_close_multiple: env_parse(
                "ALPACA_INDEX_CREDIT_STOP_LOSS_CLOSE_MULTIPLE",
                2.0_f64,
            ),
            max_hold_secs: env_parse("ALPACA_INDEX_CREDIT_MAX_HOLD_SECS", 0_u64),
            expiration_exit_days: env_parse("ALPACA_INDEX_CREDIT_EXPIRATION_EXIT_DAYS", 1_i64),
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

fn spread_kinds_from_env() -> anyhow::Result<Vec<CreditSpreadKind>> {
    let value = env::var("ALPACA_INDEX_CREDIT_STRATEGIES")
        .unwrap_or_else(|_| "put".to_string())
        .to_ascii_lowercase();
    let mut kinds = Vec::new();
    for raw in value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        match raw {
            "put" | "put_credit" | "index_put_credit_entry" => {
                kinds.push(CreditSpreadKind::Put);
            }
            "call" | "call_credit" | "index_call_credit_entry" => {
                kinds.push(CreditSpreadKind::Call);
            }
            "both" | "all" => {
                kinds.push(CreditSpreadKind::Put);
                kinds.push(CreditSpreadKind::Call);
            }
            other => anyhow::bail!("unsupported ALPACA_INDEX_CREDIT_STRATEGIES value {other}"),
        }
    }
    if kinds.is_empty() {
        kinds.push(CreditSpreadKind::Put);
    }
    kinds.sort_by_key(|kind| match kind {
        CreditSpreadKind::Put => 0,
        CreditSpreadKind::Call => 1,
    });
    kinds.dedup();
    Ok(kinds)
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
