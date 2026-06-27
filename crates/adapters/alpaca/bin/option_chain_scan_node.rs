//! Read-only Nautilus option-chain scan node for Alpaca candidate logic.
//!
//! This runs a real `BacktestNode`: catalog quote/Greeks data flows through the Nautilus data
//! engine, `OptionChainManager` assembles `OptionChainSlice` events, and
//! `OptionChainCandidateScanActor` ranks candidates without submitting orders.

use std::{collections::BTreeSet, env, path::Path};

use anyhow::{Context, anyhow, bail};
use chrono::NaiveDate;
use nautilus_alpaca::{
    candidate_engine::{CreditSpreadKind, DebitSpreadKind, NakedOptionKind},
    candidate_scan_actor::{
        OptionChainCandidateScanActor, OptionChainCandidateScanActorConfig,
        OptionChainCandidateScanConfig,
    },
};
use nautilus_backtest::{
    config::{BacktestDataConfig, BacktestRunConfig, BacktestVenueConfig, NautilusDataType},
    node::BacktestNode,
};
use nautilus_model::{
    data::option_chain::StrikeRange,
    enums::{AccountType, BookType, OmsType},
    identifiers::{ActorId, InstrumentId, OptionSeriesId, Venue},
    instruments::{Instrument, InstrumentAny},
    types::Price,
};
use nautilus_persistence::backend::catalog::ParquetDataCatalog;
use ustr::Ustr;

const DEFAULT_SNAPSHOT_INTERVAL_MS: u64 = 5_000;

#[derive(Debug)]
struct Args {
    catalog_path: String,
    underlying: String,
    venue: Venue,
    expiry: Option<String>,
    snapshot_interval_ms: Option<u64>,
    chunk_size: Option<usize>,
    strategies: Vec<String>,
}

#[derive(Clone, Debug)]
struct SeriesSelection {
    series_id: OptionSeriesId,
    instrument_ids: Vec<InstrumentId>,
    strikes: Vec<Price>,
}

fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let args = Args::from_env()?;
    let catalog = ParquetDataCatalog::new(Path::new(&args.catalog_path), None, None, None, None);
    let instruments = catalog.query_instruments(None)?;
    let selection = select_series(
        &instruments,
        args.venue,
        &args.underlying,
        args.expiry.as_deref(),
    )?;
    let strike_range = strike_range_from_env(&selection.strikes)?;
    let scan_config = scan_config_from_values(&args.strategies)?;

    println!(
        "option_chain_scan_node: catalog={} series={} instruments={} strikes={} snapshot_interval_ms={:?}",
        args.catalog_path,
        selection.series_id,
        selection.instrument_ids.len(),
        selection.strikes.len(),
        args.snapshot_interval_ms,
    );

    let quote_data = BacktestDataConfig::builder()
        .data_type(NautilusDataType::QuoteTick)
        .catalog_path(args.catalog_path.clone())
        .instrument_ids(selection.instrument_ids.clone())
        .build()?;
    let greeks_data = BacktestDataConfig::builder()
        .data_type(NautilusDataType::OptionGreeks)
        .catalog_path(args.catalog_path.clone())
        .instrument_ids(selection.instrument_ids.clone())
        .build()?;

    let run_config = BacktestRunConfig::builder()
        .id("alpaca-option-chain-scan-node".to_string())
        .venues(vec![venue_config(
            selection.series_id.venue,
            selection.series_id.settlement_currency,
        )])
        .data(vec![quote_data, greeks_data])
        .maybe_chunk_size(args.chunk_size)
        .build()?;
    let config_id = run_config.id().to_string();

    let mut node = BacktestNode::new(vec![run_config])?;
    node.build()?;

    let actor = OptionChainCandidateScanActor::new(OptionChainCandidateScanActorConfig {
        actor_id: Some(ActorId::from("ALPACA-OPTION-CHAIN-SCAN-NODE")),
        series: vec![selection.series_id],
        strike_range,
        snapshot_interval_ms: args.snapshot_interval_ms,
        client_id: None,
        bootstrap_instruments: false,
        scan: scan_config,
    });

    let engine = node
        .get_engine_mut(&config_id)
        .ok_or_else(|| anyhow!("backtest engine not built for run {config_id}"))?;
    engine.add_actor(actor)?;

    node.run()?;
    println!("option_chain_scan_node: complete");

    Ok(())
}

impl Args {
    fn from_env() -> anyhow::Result<Self> {
        let positional = env::args().skip(1).collect::<Vec<_>>();
        let catalog_path = positional
            .first()
            .cloned()
            .or_else(|| env::var("ALPACA_OPTION_CHAIN_CATALOG").ok())
            .context(
                "catalog path required: pass CATALOG_PATH or set ALPACA_OPTION_CHAIN_CATALOG",
            )?;
        let underlying = positional
            .get(1)
            .cloned()
            .or_else(|| env::var("ALPACA_OPTION_CHAIN_UNDERLYING").ok())
            .context(
                "underlying required: pass UNDERLYING or set ALPACA_OPTION_CHAIN_UNDERLYING",
            )?;
        let venue = positional
            .get(2)
            .cloned()
            .or_else(|| env::var("ALPACA_OPTION_CHAIN_VENUE").ok())
            .unwrap_or_else(|| "ALPACA".to_string());
        let expiry = env::var("ALPACA_OPTION_CHAIN_EXPIRY").ok();
        let snapshot_interval_ms = optional_u64_env(
            "ALPACA_OPTION_CHAIN_SNAPSHOT_INTERVAL_MS",
            Some(DEFAULT_SNAPSHOT_INTERVAL_MS),
        )?;
        let chunk_size = optional_usize_env("ALPACA_OPTION_CHAIN_CHUNK_SIZE")?;
        let strategies = env::var("ALPACA_OPTION_CHAIN_STRATEGY_FAMILIES")
            .ok()
            .map(split_values)
            .unwrap_or_default();

        Ok(Self {
            catalog_path,
            underlying,
            venue: Venue::new(&venue),
            expiry,
            snapshot_interval_ms,
            chunk_size,
            strategies,
        })
    }
}

fn select_series(
    instruments: &[InstrumentAny],
    venue: Venue,
    underlying: &str,
    expiry: Option<&str>,
) -> anyhow::Result<SeriesSelection> {
    let requested_expiry = expiry.map(parse_expiry_date).transpose()?;
    let mut eligible = instruments
        .iter()
        .filter(|instrument| instrument.venue() == venue)
        .filter(|instrument| {
            instrument
                .underlying()
                .is_some_and(|value| value.as_str().eq_ignore_ascii_case(underlying))
        })
        .filter(|instrument| instrument.expiration_ns().is_some())
        .filter(|instrument| instrument.strike_price().is_some())
        .filter(|instrument| {
            requested_expiry.is_none_or(|expiry| {
                instrument
                    .expiration_ns()
                    .is_some_and(|expiration| expiration.to_datetime_utc().date_naive() == expiry)
            })
        })
        .collect::<Vec<_>>();

    if eligible.is_empty() {
        bail!(
            "no option instruments found in catalog for venue={} underlying={} expiry={:?}",
            venue,
            underlying,
            expiry,
        );
    }

    eligible.sort_by_key(|instrument| {
        (
            instrument.expiration_ns().expect("filtered"),
            instrument.settlement_currency().code,
        )
    });

    let selected_expiration = eligible[0].expiration_ns().expect("filtered");
    let selected_settlement = eligible[0].settlement_currency().code;
    let selected_underlying = eligible[0].underlying().expect("filtered");
    let selected = eligible
        .into_iter()
        .filter(|instrument| instrument.expiration_ns() == Some(selected_expiration))
        .filter(|instrument| instrument.settlement_currency().code == selected_settlement)
        .collect::<Vec<_>>();

    let strikes = selected
        .iter()
        .filter_map(|instrument| instrument.strike_price())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let instrument_ids = selected
        .iter()
        .map(|instrument| instrument.id())
        .collect::<Vec<_>>();

    Ok(SeriesSelection {
        series_id: OptionSeriesId::new(
            venue,
            selected_underlying,
            selected_settlement,
            selected_expiration,
        ),
        instrument_ids,
        strikes,
    })
}

fn venue_config(venue: Venue, settlement_currency: Ustr) -> BacktestVenueConfig {
    BacktestVenueConfig::builder()
        .name(Ustr::from(venue.as_str()))
        .oms_type(OmsType::Netting)
        .account_type(AccountType::Margin)
        .book_type(BookType::L1_MBP)
        .starting_balances(vec![format!("100000 {}", settlement_currency)])
        .build()
        .expect("static venue config should be valid")
}

fn strike_range_from_env(series_strikes: &[Price]) -> anyhow::Result<StrikeRange> {
    if let Some(raw) = optional_raw_env("ALPACA_OPTION_CHAIN_FIXED_STRIKES") {
        let strikes = split_values(raw)
            .into_iter()
            .map(|value| Ok(Price::from(value.as_str())))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if strikes.is_empty() {
            bail!("ALPACA_OPTION_CHAIN_FIXED_STRIKES did not contain any strikes");
        }
        return Ok(StrikeRange::Fixed(strikes));
    }

    if series_strikes.is_empty() {
        bail!("selected option series has no strikes");
    }

    Ok(StrikeRange::Fixed(series_strikes.to_vec()))
}

fn scan_config_from_values(values: &[String]) -> anyhow::Result<OptionChainCandidateScanConfig> {
    let mut config = OptionChainCandidateScanConfig::default();
    if values.is_empty() {
        apply_scan_env_overrides(&mut config)?;
        return Ok(config);
    }

    config.spread_kinds.clear();
    config.iron_condor_enabled = false;
    config.debit_kinds.clear();
    config.naked_kinds.clear();

    for raw in values.iter().map(|value| value.to_ascii_lowercase()) {
        match raw.as_str() {
            "put" | "put_credit" => config.spread_kinds.push(CreditSpreadKind::Put),
            "call" | "call_credit" => config.spread_kinds.push(CreditSpreadKind::Call),
            "credit" | "both" => {
                config.spread_kinds.push(CreditSpreadKind::Put);
                config.spread_kinds.push(CreditSpreadKind::Call);
            }
            "iron_condor" | "condor" => config.iron_condor_enabled = true,
            "call_debit" => config.debit_kinds.push(DebitSpreadKind::Call),
            "put_debit" => config.debit_kinds.push(DebitSpreadKind::Put),
            "debit" | "directional" => {
                config.debit_kinds.push(DebitSpreadKind::Call);
                config.debit_kinds.push(DebitSpreadKind::Put);
            }
            "naked_call" | "short_call" => config.naked_kinds.push(NakedOptionKind::Call),
            "naked_put" | "short_put" => config.naked_kinds.push(NakedOptionKind::Put),
            "naked_call_1_3dte" | "short_call_1_3dte" => {
                config.naked_kinds.push(NakedOptionKind::CallOneToThreeDte);
            }
            "naked_put_1_3dte" | "short_put_1_3dte" => {
                config.naked_kinds.push(NakedOptionKind::PutOneToThreeDte);
            }
            "naked_1_3dte" => {
                config.naked_kinds.push(NakedOptionKind::CallOneToThreeDte);
                config.naked_kinds.push(NakedOptionKind::PutOneToThreeDte);
            }
            "naked" | "undefined_risk" => {
                config.naked_kinds.push(NakedOptionKind::Call);
                config.naked_kinds.push(NakedOptionKind::Put);
            }
            other => bail!("unsupported ALPACA_OPTION_CHAIN_STRATEGY_FAMILIES value {other}"),
        }
    }

    config.spread_kinds.sort_by_key(|kind| match kind {
        CreditSpreadKind::Put => 0,
        CreditSpreadKind::Call => 1,
    });
    config.spread_kinds.dedup();
    config.debit_kinds.sort_by_key(|kind| match kind {
        DebitSpreadKind::Call => 0,
        DebitSpreadKind::Put => 1,
    });
    config.debit_kinds.dedup();
    config.naked_kinds.sort_by_key(|kind| match kind {
        NakedOptionKind::Call => 0,
        NakedOptionKind::Put => 1,
        NakedOptionKind::CallOneToThreeDte => 2,
        NakedOptionKind::PutOneToThreeDte => 3,
    });
    config.naked_kinds.dedup();

    apply_scan_env_overrides(&mut config)?;
    Ok(config)
}

fn apply_scan_env_overrides(config: &mut OptionChainCandidateScanConfig) -> anyhow::Result<()> {
    if let Some(value) = optional_f64_env("ALPACA_OPTION_CHAIN_OPTIONS_BUYING_POWER")? {
        config.options_buying_power = Some(value);
    }
    if let Some(value) = optional_u64_env("ALPACA_OPTION_CHAIN_QUANTITY", None)? {
        config.quantity = value;
    }
    Ok(())
}

fn parse_expiry_date(value: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("invalid expiry date {value:?}, expected YYYY-MM-DD"))
}

fn split_values(raw: String) -> Vec<String> {
    raw.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn optional_usize_env(name: &str) -> anyhow::Result<Option<usize>> {
    let Some(value) = optional_raw_env(name) else {
        return Ok(None);
    };
    value
        .parse::<usize>()
        .map(Some)
        .with_context(|| format!("invalid {name}={value:?}, expected unsigned integer"))
}

fn optional_u64_env(name: &str, default: Option<u64>) -> anyhow::Result<Option<u64>> {
    let Some(value) = optional_raw_env(name) else {
        return Ok(default);
    };
    if matches!(value.as_str(), "none" | "raw" | "off") {
        return Ok(None);
    }
    value
        .parse::<u64>()
        .map(Some)
        .with_context(|| format!("invalid {name}={value:?}, expected unsigned integer or raw"))
}

fn optional_f64_env(name: &str) -> anyhow::Result<Option<f64>> {
    let Some(value) = optional_raw_env(name) else {
        return Ok(None);
    };
    value
        .parse::<f64>()
        .map(Some)
        .with_context(|| format!("invalid {name}={value:?}, expected number"))
}

fn optional_raw_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
