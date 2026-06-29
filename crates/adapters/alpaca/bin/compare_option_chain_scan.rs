//! Compare Alpaca REST-fed scanner output with Nautilus option-chain scanner output.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
};

use anyhow::{Context, bail};
use chrono::{NaiveDate, Utc};
use nautilus_alpaca::{
    candidate_scan_actor::{candidate_scan_config_from_runtime, scan_option_chain_candidates},
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        models::{
            AlpacaAccount, AlpacaOptionContract, AlpacaOptionQuote, AlpacaOptionSnapshot,
            AlpacaOptionType, OptionSnapshotsRequest, StockSnapshotsRequest,
        },
    },
    options_runtime::{
        AlpacaOptionsRuntimeConfig, OptionsCandidateSet, OptionsScanOutcome, OptionsScanReport,
        SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry, SelectedNakedOptionEntry,
        SelectedOptionsEntry,
    },
    parse::{parse_option_contract, parse_option_series_id},
    providers::AlpacaOptionContractProvider,
    runtime::{
        credit_spread_strategy_name, debit_spread_strategy_name, naked_option_strategy_name,
    },
    strategy::{
        scan_credit_spread_snapshot_at, scan_debit_spread_snapshot_at,
        scan_iron_condor_snapshots_at, scan_naked_option_snapshot_at,
    },
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{
        QuoteTick,
        greeks::OptionGreekValues,
        option_chain::{OptionChainSlice, OptionGreeks, OptionStrikeData},
    },
    enums::{GreeksConvention, OptionKind},
    instruments::OptionContract,
    types::{Price, Quantity},
};
use nautilus_trading::options::candidates::{
    CreditSpreadKind, DebitSpreadKind, NakedOptionCapitalContext, NakedOptionKind,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct Args {
    underlying: String,
    expiry: NaiveDate,
    pretty: bool,
}

#[derive(Clone, Debug, Default)]
struct RestOptionChainSnapshot {
    call_contracts: Vec<AlpacaOptionContract>,
    call_snapshots: BTreeMap<String, AlpacaOptionSnapshot>,
    put_contracts: Vec<AlpacaOptionContract>,
    put_snapshots: BTreeMap<String, AlpacaOptionSnapshot>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AlpacaOptionsRuntimeConfig::from_runtime_env()?;
    let args = Args::from_env(&config)?;
    let data_config = AlpacaDataClientConfig::default();
    let client = AlpacaHttpClient::from_data_config(&data_config)?;
    let now = Utc::now();
    let ts = unix_nanos(now)?;
    let scan_date = now.date_naive();
    let trade_date = scan_date.format("%Y-%m-%d").to_string();

    let chain =
        load_rest_option_chain_snapshot(&client, &data_config, &args.underlying, args.expiry)
            .await?;
    let underlying_price = load_underlying_price(&client, &data_config, &args.underlying).await?;
    let options_buying_power = load_options_buying_power_if_needed(&client, &config).await?;

    let rest = rest_scan_candidates(
        &config,
        &chain,
        &args.underlying,
        scan_date,
        underlying_price,
        options_buying_power,
        &trade_date,
    );
    let option_chain_slice =
        option_chain_slice_from_rest(&chain, &args.underlying, args.expiry, underlying_price, ts)?;
    let option_chain_config = candidate_scan_config_from_runtime(&config, options_buying_power);
    let option_chain =
        scan_option_chain_candidates(&option_chain_slice, &option_chain_config, &trade_date);

    let report = json!({
        "type": "alpaca_option_chain_scan_comparison",
        "checked_at_utc": now.to_rfc3339(),
        "underlying": args.underlying,
        "expiration_date": args.expiry.to_string(),
        "scan_date": scan_date.to_string(),
        "option_feed": data_config.option_feed.as_str(),
        "stock_feed": data_config.stock_feed.as_str(),
        "enabled_strategies": config.enabled_strategy_family_names(),
        "inputs": {
            "rest_call_contracts": chain.call_contracts.len(),
            "rest_call_snapshots": chain.call_snapshots.len(),
            "rest_put_contracts": chain.put_contracts.len(),
            "rest_put_snapshots": chain.put_snapshots.len(),
            "option_chain_calls": option_chain_slice.call_count(),
            "option_chain_puts": option_chain_slice.put_count(),
            "option_chain_strikes": option_chain_slice.strike_count(),
            "atm_strike": option_chain_slice.atm_strike.map(|price| price.as_f64()),
            "underlying_price": underlying_price,
            "options_buying_power": options_buying_power,
        },
        "rest": candidate_payload(&rest),
        "option_chain": candidate_payload(&option_chain),
        "comparison": comparison_payload(&rest, &option_chain),
    });

    if args.pretty {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", serde_json::to_string(&report)?);
    }

    Ok(())
}

impl Args {
    fn from_env(config: &AlpacaOptionsRuntimeConfig) -> anyhow::Result<Self> {
        let mut pretty = false;
        let mut values = Vec::new();
        for arg in env::args().skip(1) {
            match arg.as_str() {
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                "--pretty" => pretty = true,
                value if value.starts_with('-') => bail!("unknown argument `{value}`"),
                value => values.push(value.to_string()),
            }
        }

        let underlying = values
            .first()
            .cloned()
            .or_else(|| env::var("ALPACA_COMPARE_UNDERLYING").ok())
            .or_else(|| config.underlyings.first().cloned())
            .context("underlying required: pass UNDERLYING or set ALPACA_COMPARE_UNDERLYING")?;
        let expiry = values
            .get(1)
            .cloned()
            .or_else(|| env::var("ALPACA_COMPARE_EXPIRY").ok())
            .context("expiry required: pass EXPIRY or set ALPACA_COMPARE_EXPIRY")?;
        if values.len() > 2 {
            bail!("too many positional arguments: expected UNDERLYING EXPIRY");
        }

        Ok(Self {
            underlying: underlying.to_ascii_uppercase(),
            expiry: NaiveDate::parse_from_str(&expiry, "%Y-%m-%d")
                .with_context(|| format!("invalid expiry `{expiry}`, expected YYYY-MM-DD"))?,
            pretty,
        })
    }
}

fn print_usage() {
    println!(
        "usage: alpaca-compare-option-chain-scan [--pretty] UNDERLYING EXPIRY\n\
         example: alpaca-compare-option-chain-scan --pretty SPY 2026-07-02"
    );
}

async fn load_rest_option_chain_snapshot(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    underlying: &str,
    expiry: NaiveDate,
) -> anyhow::Result<RestOptionChainSnapshot> {
    let provider = AlpacaOptionContractProvider::new(client.clone());
    let expiry = expiry.to_string();
    let (call_contracts, call_snapshots) = load_side(
        client,
        data_config,
        &provider,
        underlying,
        &expiry,
        AlpacaOptionType::Call,
    )
    .await?;
    let (put_contracts, put_snapshots) = load_side(
        client,
        data_config,
        &provider,
        underlying,
        &expiry,
        AlpacaOptionType::Put,
    )
    .await?;

    Ok(RestOptionChainSnapshot {
        call_contracts,
        call_snapshots,
        put_contracts,
        put_snapshots,
    })
}

async fn load_side(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    provider: &AlpacaOptionContractProvider,
    underlying: &str,
    expiry: &str,
    option_type: AlpacaOptionType,
) -> anyhow::Result<(
    Vec<AlpacaOptionContract>,
    BTreeMap<String, AlpacaOptionSnapshot>,
)> {
    let contracts = provider
        .load_active_contracts(
            underlying.to_string(),
            expiry.to_string(),
            expiry.to_string(),
            Some(option_type),
        )
        .await?;
    let symbols = contracts
        .iter()
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();
    if symbols.is_empty() {
        return Ok((contracts, BTreeMap::new()));
    }

    let mut request = OptionSnapshotsRequest::for_symbols(symbols);
    request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&request).await?.snapshots;
    Ok((contracts, snapshots))
}

async fn load_underlying_price(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    underlying: &str,
) -> anyhow::Result<f64> {
    let mut request = StockSnapshotsRequest::for_symbols([underlying.to_string()]);
    request.feed = Some(data_config.stock_feed.as_str().to_string());
    let snapshots = client.stock_snapshots(&request).await?.snapshots;
    snapshots
        .get(underlying)
        .or_else(|| {
            snapshots
                .iter()
                .find(|(symbol, _)| symbol.eq_ignore_ascii_case(underlying))
                .map(|(_, snapshot)| snapshot)
        })
        .and_then(|snapshot| snapshot.latest_price())
        .with_context(|| format!("stock snapshot missing latest price for {underlying}"))
}

async fn load_options_buying_power_if_needed(
    client: &AlpacaHttpClient,
    config: &AlpacaOptionsRuntimeConfig,
) -> anyhow::Result<Option<f64>> {
    if config.naked_kinds.is_empty() {
        return Ok(None);
    }

    let account = client.account().await?;
    Ok(account_options_buying_power(&account))
}

fn account_options_buying_power(account: &AlpacaAccount) -> Option<f64> {
    account
        .options_buying_power
        .as_deref()
        .or(account.buying_power.as_deref())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn rest_scan_candidates(
    config: &AlpacaOptionsRuntimeConfig,
    chain: &RestOptionChainSnapshot,
    underlying: &str,
    scan_date: NaiveDate,
    underlying_price: f64,
    options_buying_power: Option<f64>,
    trade_date: &str,
) -> OptionsCandidateSet {
    let mut candidates = OptionsCandidateSet::new(trade_date);

    for kind in &config.spread_kinds {
        let (contracts, snapshots) = credit_side(chain, *kind);
        let result = scan_credit_spread_snapshot_at(
            underlying,
            contracts,
            snapshots,
            &config.scanner,
            *kind,
            scan_date,
        );
        candidates.push_scan(scan_report(
            underlying,
            credit_spread_strategy_name(*kind),
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::Credit(SelectedEntry {
                underlying: underlying.to_string(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }
    }

    if config.iron_condor_enabled {
        let result = scan_iron_condor_snapshots_at(
            underlying,
            &chain.put_contracts,
            &chain.put_snapshots,
            &chain.call_contracts,
            &chain.call_snapshots,
            &config.iron_condor_scanner,
            scan_date,
        );
        candidates.push_scan(scan_report(
            underlying,
            "iron_condor",
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::IronCondor(
                SelectedIronCondorEntry {
                    underlying: underlying.to_string(),
                    candidate: best.clone(),
                },
            ));
        }
    }

    for kind in &config.debit_kinds {
        let (contracts, snapshots) = debit_side(chain, *kind);
        let result = scan_debit_spread_snapshot_at(
            underlying,
            contracts,
            snapshots,
            &config.debit_scanner,
            *kind,
            scan_date,
        );
        candidates.push_scan(scan_report(
            underlying,
            debit_spread_strategy_name(*kind),
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::Debit(SelectedDebitEntry {
                underlying: underlying.to_string(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }
    }

    for kind in &config.naked_kinds {
        let (contracts, snapshots) = naked_side(chain, *kind);
        let result = scan_naked_option_snapshot_at(
            underlying,
            contracts,
            snapshots,
            config.naked_scanner_for(*kind),
            *kind,
            underlying_price,
            Some(NakedOptionCapitalContext {
                options_buying_power,
                quantity: config.quantity,
            }),
            scan_date,
        );
        candidates.push_scan(scan_report(
            underlying,
            naked_option_strategy_name(*kind),
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::NakedOption(
                SelectedNakedOptionEntry {
                    underlying: underlying.to_string(),
                    kind: *kind,
                    candidate: best.clone(),
                },
            ));
        }
    }

    candidates
}

fn credit_side(
    chain: &RestOptionChainSnapshot,
    kind: CreditSpreadKind,
) -> (
    &[AlpacaOptionContract],
    &BTreeMap<String, AlpacaOptionSnapshot>,
) {
    match kind {
        CreditSpreadKind::Put => (&chain.put_contracts, &chain.put_snapshots),
        CreditSpreadKind::Call => (&chain.call_contracts, &chain.call_snapshots),
    }
}

fn debit_side(
    chain: &RestOptionChainSnapshot,
    kind: DebitSpreadKind,
) -> (
    &[AlpacaOptionContract],
    &BTreeMap<String, AlpacaOptionSnapshot>,
) {
    match kind {
        DebitSpreadKind::Put => (&chain.put_contracts, &chain.put_snapshots),
        DebitSpreadKind::Call => (&chain.call_contracts, &chain.call_snapshots),
    }
}

fn naked_side(
    chain: &RestOptionChainSnapshot,
    kind: NakedOptionKind,
) -> (
    &[AlpacaOptionContract],
    &BTreeMap<String, AlpacaOptionSnapshot>,
) {
    if kind.is_put() {
        (&chain.put_contracts, &chain.put_snapshots)
    } else {
        (&chain.call_contracts, &chain.call_snapshots)
    }
}

fn scan_report(
    underlying: &str,
    strategy: &'static str,
    candidate_count: usize,
    contract_count: usize,
    snapshot_count: usize,
    scoreable_count: usize,
    rejection_counts: BTreeMap<String, usize>,
) -> OptionsScanReport {
    OptionsScanReport::new(
        underlying,
        strategy,
        candidate_count,
        contract_count,
        snapshot_count,
        scoreable_count,
        rejection_counts,
    )
}

fn option_chain_slice_from_rest(
    chain: &RestOptionChainSnapshot,
    underlying: &str,
    expiry: NaiveDate,
    underlying_price: f64,
    ts: UnixNanos,
) -> anyhow::Result<OptionChainSlice> {
    let series_id = parse_option_series_id(underlying, "USD", &expiry.to_string())
        .map_err(|e| anyhow::anyhow!("invalid option series for {underlying} {expiry}: {e}"))?;
    let mut slice = OptionChainSlice {
        series_id,
        atm_strike: None,
        calls: BTreeMap::new(),
        puts: BTreeMap::new(),
        ts_event: ts,
        ts_init: ts,
    };

    insert_side(
        &mut slice,
        &chain.call_contracts,
        &chain.call_snapshots,
        underlying_price,
        ts,
    )?;
    insert_side(
        &mut slice,
        &chain.put_contracts,
        &chain.put_snapshots,
        underlying_price,
        ts,
    )?;
    slice.atm_strike = nearest_strike(&slice, underlying_price);

    Ok(slice)
}

fn insert_side(
    slice: &mut OptionChainSlice,
    contracts: &[AlpacaOptionContract],
    snapshots: &BTreeMap<String, AlpacaOptionSnapshot>,
    underlying_price: f64,
    ts: UnixNanos,
) -> anyhow::Result<()> {
    for contract in contracts {
        let option = parse_option_contract(contract)
            .with_context(|| format!("failed to parse option contract {}", contract.symbol))?;
        let Some(snapshot) = snapshot_for_symbol(snapshots, &contract.symbol) else {
            continue;
        };
        let Some(data) = option_strike_data(&option, contract, snapshot, underlying_price, ts)?
        else {
            continue;
        };

        match option.option_kind {
            OptionKind::Call => {
                slice.calls.insert(option.strike_price, data);
            }
            OptionKind::Put => {
                slice.puts.insert(option.strike_price, data);
            }
        }
    }

    Ok(())
}

fn option_strike_data(
    option: &OptionContract,
    contract: &AlpacaOptionContract,
    snapshot: &AlpacaOptionSnapshot,
    underlying_price: f64,
    ts: UnixNanos,
) -> anyhow::Result<Option<OptionStrikeData>> {
    let Some(quote) = quote_tick(option, snapshot.latest_quote.as_ref(), ts)? else {
        return Ok(None);
    };
    let greeks = option_greeks(option.id, contract, snapshot, underlying_price, ts);
    Ok(Some(OptionStrikeData { quote, greeks }))
}

fn quote_tick(
    option: &OptionContract,
    quote: Option<&AlpacaOptionQuote>,
    ts: UnixNanos,
) -> anyhow::Result<Option<QuoteTick>> {
    let Some(quote) = quote else {
        return Ok(None);
    };
    if !quote.is_valid() {
        return Ok(None);
    }

    let bid = quote.bid_price.context("valid quote missing bid price")?;
    let ask = quote.ask_price.context("valid quote missing ask price")?;
    let bid_size = quote.bid_size.context("valid quote missing bid size")?;
    let ask_size = quote.ask_size.context("valid quote missing ask size")?;
    let bid_price = Price::new(bid, option.price_precision);
    let ask_price = Price::new(ask, option.price_precision);
    let bid_size = Quantity::from(bid_size);
    let ask_size = Quantity::from(ask_size);

    Ok(Some(QuoteTick::new_checked(
        option.id, bid_price, ask_price, bid_size, ask_size, ts, ts,
    )?))
}

fn option_greeks(
    instrument_id: nautilus_model::identifiers::InstrumentId,
    contract: &AlpacaOptionContract,
    snapshot: &AlpacaOptionSnapshot,
    underlying_price: f64,
    ts: UnixNanos,
) -> Option<OptionGreeks> {
    let greeks = snapshot.greeks.as_ref()?;
    if !greeks.has_any() {
        return None;
    }

    Some(OptionGreeks {
        instrument_id,
        convention: GreeksConvention::BlackScholes,
        greeks: OptionGreekValues {
            delta: greeks.delta.unwrap_or_default(),
            gamma: greeks.gamma.unwrap_or_default(),
            vega: greeks.vega.unwrap_or_default(),
            theta: greeks.theta.unwrap_or_default(),
            rho: greeks.rho.unwrap_or_default(),
        },
        mark_iv: snapshot.implied_volatility,
        bid_iv: None,
        ask_iv: None,
        underlying_price: Some(underlying_price),
        open_interest: contract
            .open_interest
            .as_deref()
            .and_then(|value| value.parse::<f64>().ok()),
        ts_event: ts,
        ts_init: ts,
    })
}

fn snapshot_for_symbol<'a>(
    snapshots: &'a BTreeMap<String, AlpacaOptionSnapshot>,
    symbol: &str,
) -> Option<&'a AlpacaOptionSnapshot> {
    let canonical = symbol.strip_prefix("O:").unwrap_or(symbol);
    snapshots
        .get(symbol)
        .or_else(|| snapshots.get(canonical))
        .or_else(|| {
            snapshots
                .iter()
                .find(|(key, _)| normalize_symbol(key) == normalize_symbol(symbol))
                .map(|(_, snapshot)| snapshot)
        })
}

fn nearest_strike(slice: &OptionChainSlice, underlying_price: f64) -> Option<Price> {
    if !underlying_price.is_finite() || underlying_price <= 0.0 {
        return None;
    }

    slice.strikes().into_iter().min_by(|left, right| {
        (left.as_f64() - underlying_price)
            .abs()
            .total_cmp(&(right.as_f64() - underlying_price).abs())
    })
}

fn candidate_payload(candidates: &OptionsCandidateSet) -> Value {
    json!({
        "trade_date": candidates.trade_date,
        "scans": candidates.scans.iter().map(scan_payload).collect::<Vec<_>>(),
        "ranked_entries": candidates.ranked_entries().iter().map(selected_payload).collect::<Vec<_>>(),
        "selected": candidates.selected_entry().map(selected_payload),
    })
}

fn scan_payload(report: &OptionsScanReport) -> Value {
    json!({
        "underlying": report.underlying,
        "strategy": report.strategy,
        "outcome": match report.outcome {
            OptionsScanOutcome::Candidate => "candidate",
            OptionsScanOutcome::NoCandidate => "no_candidate",
        },
        "candidate_count": report.candidate_count,
        "reason": report.reason,
        "contracts": report.contract_count,
        "snapshots": report.snapshot_count,
        "scoreable": report.scoreable_count,
        "rejections": report.rejection_counts,
    })
}

fn selected_payload(entry: &SelectedOptionsEntry) -> Value {
    let descriptor = entry.descriptor();
    let normalized_symbols = descriptor
        .symbols
        .iter()
        .map(|symbol| normalize_symbol(symbol))
        .collect::<Vec<_>>();

    json!({
        "strategy": descriptor.strategy,
        "underlying": descriptor.underlying,
        "candidate_type": descriptor.candidate_type,
        "symbols": descriptor.symbols,
        "normalized_symbols": normalized_symbols,
        "score": descriptor.score,
        "premium_kind": descriptor.premium_kind.as_str(),
        "premium": descriptor.premium,
    })
}

fn comparison_payload(rest: &OptionsCandidateSet, option_chain: &OptionsCandidateSet) -> Value {
    let scan_mismatches = scan_mismatches(rest, option_chain);
    let rest_selected = rest.selected_entry().map(selected_key);
    let option_chain_selected = option_chain.selected_entry().map(selected_key);
    let selected_score_delta = match (rest.selected_entry(), option_chain.selected_entry()) {
        (Some(rest), Some(option_chain)) => Some(rest.score() - option_chain.score()),
        _ => None,
    };

    json!({
        "scan_counts_match": scan_mismatches.is_empty(),
        "scan_mismatches": scan_mismatches,
        "selected_match": rest_selected == option_chain_selected,
        "rest_selected_key": rest_selected,
        "option_chain_selected_key": option_chain_selected,
        "selected_score_delta": selected_score_delta,
    })
}

fn scan_mismatches(rest: &OptionsCandidateSet, option_chain: &OptionsCandidateSet) -> Vec<Value> {
    let rest_scans = rest
        .scans
        .iter()
        .map(|scan| (scan.strategy, scan))
        .collect::<BTreeMap<_, _>>();
    let option_chain_scans = option_chain
        .scans
        .iter()
        .map(|scan| (scan.strategy, scan))
        .collect::<BTreeMap<_, _>>();
    let strategies = rest_scans
        .keys()
        .chain(option_chain_scans.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut mismatches = Vec::new();

    for strategy in strategies {
        match (rest_scans.get(strategy), option_chain_scans.get(strategy)) {
            (Some(rest), Some(option_chain)) => {
                let mut fields = Vec::new();
                if rest.candidate_count != option_chain.candidate_count {
                    fields.push("candidate_count");
                }
                if rest.contract_count != option_chain.contract_count {
                    fields.push("contract_count");
                }
                if rest.snapshot_count != option_chain.snapshot_count {
                    fields.push("snapshot_count");
                }
                if rest.scoreable_count != option_chain.scoreable_count {
                    fields.push("scoreable_count");
                }
                if rest.rejection_counts != option_chain.rejection_counts {
                    fields.push("rejection_counts");
                }
                if !fields.is_empty() {
                    mismatches.push(json!({
                        "strategy": strategy,
                        "fields": fields,
                        "rest": scan_payload(rest),
                        "option_chain": scan_payload(option_chain),
                    }));
                }
            }
            (Some(rest), None) => mismatches.push(json!({
                "strategy": strategy,
                "fields": ["missing_option_chain_scan"],
                "rest": scan_payload(rest),
                "option_chain": null,
            })),
            (None, Some(option_chain)) => mismatches.push(json!({
                "strategy": strategy,
                "fields": ["missing_rest_scan"],
                "rest": null,
                "option_chain": scan_payload(option_chain),
            })),
            (None, None) => {}
        }
    }

    mismatches
}

fn selected_key(entry: &SelectedOptionsEntry) -> String {
    let descriptor = entry.descriptor();
    let symbols = descriptor
        .symbols
        .iter()
        .map(|symbol| normalize_symbol(symbol))
        .collect::<Vec<_>>()
        .join("|");
    format!("{}:{symbols}", descriptor.strategy)
}

fn normalize_symbol(symbol: &str) -> String {
    let symbol = symbol.strip_suffix(".ALPACA").unwrap_or(symbol);
    symbol
        .strip_prefix("O:")
        .unwrap_or(symbol)
        .to_ascii_uppercase()
}

fn unix_nanos(value: chrono::DateTime<Utc>) -> anyhow::Result<UnixNanos> {
    value
        .timestamp_nanos_opt()
        .and_then(|timestamp| u64::try_from(timestamp).ok())
        .map(UnixNanos::from)
        .context("current time was outside supported UnixNanos range")
}
