//! Read-only Nautilus option-chain scan node for Alpaca candidate logic.
//!
//! This runs a real `BacktestNode`: catalog quote/Greeks data flows through the Nautilus data
//! engine, `OptionChainManager` assembles `OptionChainSlice` events, and
//! `OptionChainCandidateScanActor` ranks candidates without submitting orders.

use std::{collections::BTreeSet, env, path::Path, str::FromStr};

use anyhow::{Context, anyhow, bail};
use chrono::{NaiveDate, Utc};
use nautilus_alpaca::candidate_scan_actor::{
    OptionChainCandidateScanActor, OptionChainCandidateScanActorConfig,
    OptionChainCandidateScanConfig,
};
use nautilus_alpaca::options_runtime::{
    AlpacaOptionsStrategyFamily, AlpacaOptionsStrategyMode, AlpacaOptionsStrategyProfile,
    AlpacaOptionsStrategyRiskOverrides, AlpacaOptionsStrategyScannerConfig,
};
use nautilus_backtest::{
    config::{BacktestDataConfig, BacktestRunConfig, BacktestVenueConfig, NautilusDataType},
    node::BacktestNode,
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::option_chain::StrikeRange,
    enums::{AccountType, BookType, OmsType},
    identifiers::{ActorId, InstrumentId, OptionSeriesId, Venue},
    instruments::{Instrument, InstrumentAny},
    types::Price,
};
use nautilus_persistence::backend::catalog::ParquetDataCatalog;
use nautilus_trading::options::universe::{
    OptionDteWindow, OptionUniverseContract, OptionUniverseIntent, OptionUniverseResolution,
    OptionUniverseStrategyFamily, resolve_option_universe,
};
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
    reason: String,
}

fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let args = Args::from_env()?;
    let catalog = ParquetDataCatalog::new(Path::new(&args.catalog_path), None, None, None, None);
    let instruments = catalog.query_instruments(None)?;
    let scan_config = scan_config_from_values(&args.strategies, &args.underlying)?;
    let selection = select_series(
        &instruments,
        args.venue,
        &args.underlying,
        args.expiry.as_deref(),
        &scan_config,
    )?;
    let strike_range = strike_range_from_env(&selection.strikes)?;

    println!(
        "option_chain_scan_node: catalog={} series={} instruments={} strikes={} selection_reason={} snapshot_interval_ms={:?}",
        args.catalog_path,
        selection.series_id,
        selection.instrument_ids.len(),
        selection.strikes.len(),
        selection.reason,
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
        ..Default::default()
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
    scan_config: &OptionChainCandidateScanConfig,
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

    let (selected_series_id, reason) = if requested_expiry.is_some() {
        let selected_expiration = eligible[0].expiration_ns().expect("filtered");
        let selected_settlement = eligible[0].settlement_currency().code;
        let selected_underlying = eligible[0].underlying().expect("filtered");
        (
            OptionSeriesId::new(
                venue,
                selected_underlying,
                selected_settlement,
                selected_expiration,
            ),
            "explicit_expiry_filter".to_string(),
        )
    } else {
        select_series_with_neutral_universe(underlying, scan_config, &eligible)?
    };
    let selected = eligible
        .into_iter()
        .filter(|instrument| instrument.expiration_ns() == Some(selected_series_id.expiration_ns))
        .filter(|instrument| {
            instrument.settlement_currency().code == selected_series_id.settlement_currency
        })
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
        series_id: selected_series_id,
        instrument_ids,
        strikes,
        reason,
    })
}

fn select_series_with_neutral_universe(
    underlying: &str,
    scan_config: &OptionChainCandidateScanConfig,
    instruments: &[&InstrumentAny],
) -> anyhow::Result<(OptionSeriesId, String)> {
    let intents = diagnostic_universe_intents(underlying, scan_config);
    if intents.is_empty() {
        bail!("no diagnostic universe intents were enabled for underlying={underlying}");
    }
    let contracts = instruments
        .iter()
        .filter_map(|instrument| option_universe_contract_from_instrument(instrument))
        .collect::<Vec<_>>();
    let evaluation_time = unix_nanos(Utc::now())?;
    let resolution = resolve_option_universe(&intents, &contracts, &[], evaluation_time);
    let Some(selected) = resolution.selected.first() else {
        bail!(
            "no option series matched neutral diagnostic universe intent for underlying={} skipped={}",
            underlying,
            skipped_universe_summary(&resolution),
        );
    };
    Ok((
        selected.coverage.series_id,
        format!(
            "neutral_universe:{}:{}dte",
            selected.reason.as_str(),
            selected.coverage.dte,
        ),
    ))
}

fn diagnostic_universe_intents(
    underlying: &str,
    config: &OptionChainCandidateScanConfig,
) -> Vec<OptionUniverseIntent> {
    config
        .strategy_profiles
        .iter()
        .map(|profile| {
            OptionUniverseIntent::from_family(
                profile.id.clone(),
                underlying.to_string(),
                universe_family_from_profile(profile.family),
                dte_window_from_profile(profile),
            )
        })
        .collect()
}

fn skipped_universe_summary(resolution: &OptionUniverseResolution) -> String {
    if resolution.skipped.is_empty() {
        return "none".to_string();
    }
    resolution
        .skipped
        .iter()
        .map(|skipped| {
            format!(
                "{}:{}:{}",
                skipped.profile_id,
                skipped.underlying,
                skipped.reason.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn option_universe_contract_from_instrument(
    instrument: &InstrumentAny,
) -> Option<OptionUniverseContract> {
    let underlying = instrument.underlying()?;
    let option_kind = instrument.option_kind()?;
    let expiration_ns = instrument.expiration_ns()?;
    if instrument.strike_price().is_none() {
        return None;
    }

    let series_id = OptionSeriesId::new(
        instrument.venue(),
        underlying,
        instrument.settlement_currency().code,
        expiration_ns,
    );
    Some(OptionUniverseContract::new(
        instrument.id(),
        series_id,
        option_kind,
    ))
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

fn scan_config_from_values(
    values: &[String],
    underlying: &str,
) -> anyhow::Result<OptionChainCandidateScanConfig> {
    let mut config = OptionChainCandidateScanConfig::default();
    let families = diagnostic_strategy_families(values)?;
    apply_scan_env_overrides(&mut config)?;
    config.strategy_profiles = diagnostic_profiles_from_families(&families, underlying, &config);
    Ok(config)
}

fn diagnostic_strategy_families(
    values: &[String],
) -> anyhow::Result<Vec<AlpacaOptionsStrategyFamily>> {
    let mut families = Vec::new();
    if values.is_empty() {
        families.push(AlpacaOptionsStrategyFamily::PutCredit);
    }
    for raw in values.iter().map(|value| value.to_ascii_lowercase()) {
        match raw.as_str() {
            "credit" | "both" => {
                families.push(AlpacaOptionsStrategyFamily::PutCredit);
                families.push(AlpacaOptionsStrategyFamily::CallCredit);
            }
            "debit" | "directional" => {
                families.push(AlpacaOptionsStrategyFamily::CallDebit);
                families.push(AlpacaOptionsStrategyFamily::PutDebit);
            }
            "naked_1_3dte" => {
                families.push(AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte);
                families.push(AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte);
            }
            "naked" | "undefined_risk" => {
                families.push(AlpacaOptionsStrategyFamily::NakedCall);
                families.push(AlpacaOptionsStrategyFamily::NakedPut);
            }
            value => families.push(AlpacaOptionsStrategyFamily::from_str(value).map_err(|_| {
                anyhow!("unsupported ALPACA_OPTION_CHAIN_STRATEGY_FAMILIES value {value}")
            })?),
        }
    }
    families.sort();
    families.dedup();
    Ok(families)
}

fn diagnostic_profiles_from_families(
    families: &[AlpacaOptionsStrategyFamily],
    underlying: &str,
    config: &OptionChainCandidateScanConfig,
) -> Vec<AlpacaOptionsStrategyProfile> {
    families
        .iter()
        .copied()
        .map(|family| AlpacaOptionsStrategyProfile {
            id: format!("diagnostic_{}", family.as_str()),
            family,
            mode: AlpacaOptionsStrategyMode::Live,
            underlyings: vec![underlying.to_string()],
            quantity: config.quantity.max(1),
            scanner: scanner_config_for_family(family, config),
            risk: AlpacaOptionsStrategyRiskOverrides::default(),
        })
        .collect()
}

fn scanner_config_for_family(
    family: AlpacaOptionsStrategyFamily,
    config: &OptionChainCandidateScanConfig,
) -> AlpacaOptionsStrategyScannerConfig {
    match family {
        AlpacaOptionsStrategyFamily::PutCredit | AlpacaOptionsStrategyFamily::CallCredit => {
            AlpacaOptionsStrategyScannerConfig::Credit(config.credit_scanner.clone())
        }
        AlpacaOptionsStrategyFamily::IronCondor => {
            AlpacaOptionsStrategyScannerConfig::IronCondor(config.iron_condor_scanner.clone())
        }
        AlpacaOptionsStrategyFamily::PutDebit | AlpacaOptionsStrategyFamily::CallDebit => {
            AlpacaOptionsStrategyScannerConfig::Debit(config.debit_scanner.clone())
        }
        AlpacaOptionsStrategyFamily::NakedPut | AlpacaOptionsStrategyFamily::NakedCall => {
            AlpacaOptionsStrategyScannerConfig::Naked(config.naked_scanner.clone())
        }
        AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte
        | AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte => {
            AlpacaOptionsStrategyScannerConfig::Naked(config.naked_1_3dte_scanner.clone())
        }
    }
}

fn universe_family_from_profile(
    family: AlpacaOptionsStrategyFamily,
) -> OptionUniverseStrategyFamily {
    match family {
        AlpacaOptionsStrategyFamily::PutCredit => OptionUniverseStrategyFamily::PutCredit,
        AlpacaOptionsStrategyFamily::CallCredit => OptionUniverseStrategyFamily::CallCredit,
        AlpacaOptionsStrategyFamily::IronCondor => OptionUniverseStrategyFamily::IronCondor,
        AlpacaOptionsStrategyFamily::PutDebit => OptionUniverseStrategyFamily::PutDebit,
        AlpacaOptionsStrategyFamily::CallDebit => OptionUniverseStrategyFamily::CallDebit,
        AlpacaOptionsStrategyFamily::NakedPut => OptionUniverseStrategyFamily::NakedPut,
        AlpacaOptionsStrategyFamily::NakedCall => OptionUniverseStrategyFamily::NakedCall,
        AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte => {
            OptionUniverseStrategyFamily::NakedPutOneToThreeDte
        }
        AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte => {
            OptionUniverseStrategyFamily::NakedCallOneToThreeDte
        }
    }
}

fn dte_window_from_profile(profile: &AlpacaOptionsStrategyProfile) -> OptionDteWindow {
    match &profile.scanner {
        AlpacaOptionsStrategyScannerConfig::Credit(scanner) => {
            OptionDteWindow::new(scanner.min_dte, scanner.max_dte)
        }
        AlpacaOptionsStrategyScannerConfig::IronCondor(scanner) => {
            OptionDteWindow::new(scanner.credit.min_dte, scanner.credit.max_dte)
        }
        AlpacaOptionsStrategyScannerConfig::Debit(scanner) => {
            OptionDteWindow::new(scanner.min_dte, scanner.max_dte)
        }
        AlpacaOptionsStrategyScannerConfig::Naked(scanner) => {
            OptionDteWindow::new(scanner.min_dte, scanner.max_dte)
        }
    }
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

fn unix_nanos(value: chrono::DateTime<Utc>) -> anyhow::Result<UnixNanos> {
    value
        .timestamp_nanos_opt()
        .and_then(|timestamp| u64::try_from(timestamp).ok())
        .map(UnixNanos::from)
        .context("current time was outside supported UnixNanos range")
}
