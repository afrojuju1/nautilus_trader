//! Live Nautilus option-chain scan and entry node for Alpaca.

use std::{env, time::Duration};

use anyhow::{Context, bail};
use nautilus_alpaca::{
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    factories::{AlpacaDataClientFactory, AlpacaExecutionClientFactory},
    http::{client::AlpacaHttpClient, models::AlpacaAccount},
    opportunity_scan_actor::{
        OptionChainOpportunityScanActor, OptionChainOpportunityScanActorConfig,
        option_chain_scan_config_from_engine,
    },
    options_entry_strategy::{AlpacaOptionsEntryStrategy, AlpacaOptionsEntryStrategyConfig},
    options_runtime::OptionsEngineConfig,
};
use nautilus_common::enums::Environment;
use nautilus_live::node::LiveNode;
use nautilus_model::{
    data::option_chain::StrikeRange,
    identifiers::{AccountId, ActorId, ClientId, OptionSeriesId, StrategyId, TraderId},
    types::Price,
};
use nautilus_trading::strategy::StrategyConfig;

const DEFAULT_SNAPSHOT_INTERVAL_MS: u64 = 5_000;
const DEFAULT_STRIKES_ABOVE: usize = 10;
const DEFAULT_STRIKES_BELOW: usize = 10;

#[derive(Debug)]
struct Args {
    trader_id: TraderId,
    node_name: String,
    actor_id: ActorId,
    underlying: String,
    expiry: String,
    settlement: String,
    strike_range: StrikeRange,
    snapshot_interval_ms: Option<u64>,
    snapshot_greeks_poll_secs: Option<u64>,
    entry_submit_enabled: bool,
    max_runtime_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    nautilus_common::logging::ensure_logging_initialized();

    let runtime_config = OptionsEngineConfig::from_runtime_env()?;
    let args = Args::from_env(&runtime_config)?;
    let series_id = OptionSeriesId::from_expiry(
        ALPACA_VENUE,
        &args.underlying,
        &args.settlement,
        &args.expiry,
    )
    .map_err(|e| anyhow::anyhow!("invalid Alpaca option series: {e}"))?;
    let client_id = ClientId::from(ALPACA_CLIENT_ID);
    let data_config = AlpacaDataClientConfig {
        snapshot_greeks_poll_secs: args.snapshot_greeks_poll_secs,
        ..Default::default()
    };
    let options_buying_power = load_options_buying_power_if_needed(&runtime_config, &data_config)
        .await
        .context("failed to load options buying-power context")?;
    let scan_config = option_chain_scan_config_from_engine(&runtime_config, options_buying_power);
    let strategy_state = runtime_config
        .load_strategy_state()
        .await
        .context("failed to load Alpaca options strategy state")?;
    let strategy_state_entry_count = strategy_state.entries.len();

    log::info!(
        "Starting Alpaca options live node: series={} snapshot_interval_ms={:?} max_runtime_secs={:?} strategies={:?} runtime_submit_enabled={} node_entry_submit_enabled={} strategy_state_entries={}",
        series_id,
        args.snapshot_interval_ms,
        args.max_runtime_secs,
        runtime_config.enabled_strategy_names(),
        runtime_config.submit_enabled,
        args.entry_submit_enabled,
        strategy_state_entry_count,
    );

    let account_id = account_id_from_env();
    let mut node = LiveNode::builder(args.trader_id, Environment::Live)?
        .with_name(args.node_name)
        .add_data_client(
            Some(ALPACA_CLIENT_ID.to_string()),
            Box::new(AlpacaDataClientFactory::new()),
            Box::new(data_config),
        )?
        .add_exec_client(
            Some(ALPACA_CLIENT_ID.to_string()),
            Box::new(AlpacaExecutionClientFactory::new(
                args.trader_id,
                account_id,
            )),
            Box::new(AlpacaExecClientConfig::default()),
        )?
        .with_delay_post_stop_secs(5)
        .build()?;

    let actor = OptionChainOpportunityScanActor::new(OptionChainOpportunityScanActorConfig {
        actor_id: Some(args.actor_id),
        series: vec![series_id],
        strike_range: args.strike_range,
        snapshot_interval_ms: args.snapshot_interval_ms,
        client_id: Some(client_id),
        bootstrap_instruments: true,
        scan: scan_config,
    });
    node.add_actor(actor)?;

    let strategy_config = StrategyConfig {
        strategy_id: Some(StrategyId::from("ALPACA-OPTIONS-ENTRY")),
        order_id_tag: Some("AOE".to_string()),
        ..Default::default()
    };
    let mut entry_config =
        AlpacaOptionsEntryStrategyConfig::from_engine_config(strategy_config, &runtime_config);
    entry_config.admission.submit_enabled =
        entry_config.admission.submit_enabled && args.entry_submit_enabled;
    entry_config.initial_state = strategy_state;
    let strategy = AlpacaOptionsEntryStrategy::new(entry_config);
    node.add_strategy(strategy)?;

    if let Some(max_runtime_secs) = args.max_runtime_secs {
        let handle = node.handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(max_runtime_secs)).await;
            log::info!("Stopping Alpaca option-chain live node after {max_runtime_secs}s");
            handle.stop();
        });
    }

    node.run().await?;
    Ok(())
}

impl Args {
    fn from_env(config: &OptionsEngineConfig) -> anyhow::Result<Self> {
        let mut values = Vec::new();
        for arg in env::args().skip(1) {
            match arg.as_str() {
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                value if value.starts_with('-') => bail!("unknown argument `{value}`"),
                value => values.push(value.to_string()),
            }
        }

        if values.len() > 2 {
            bail!("too many positional arguments: expected UNDERLYING EXPIRY");
        }

        let underlying = values
            .first()
            .cloned()
            .or_else(|| env::var("ALPACA_OPTION_CHAIN_UNDERLYING").ok())
            .or_else(|| config.underlyings.first().cloned())
            .context(
                "underlying required: pass UNDERLYING, set ALPACA_OPTION_CHAIN_UNDERLYING, \
                 or configure a runtime universe",
            )?;
        let expiry = values
            .get(1)
            .cloned()
            .or_else(|| env::var("ALPACA_OPTION_CHAIN_EXPIRY").ok())
            .context("expiry required: pass EXPIRY or set ALPACA_OPTION_CHAIN_EXPIRY")?;
        let settlement = env::var("ALPACA_OPTION_CHAIN_SETTLEMENT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "USD".to_string());

        Ok(Self {
            trader_id: TraderId::from(
                env::var("ALPACA_OPTION_CHAIN_TRADER_ID")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "TRADER-001".to_string()),
            ),
            node_name: env::var("ALPACA_OPTION_CHAIN_NODE_NAME")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "ALPACA-OPTIONS-LIVE".to_string()),
            actor_id: ActorId::from(
                env::var("ALPACA_OPTION_CHAIN_ACTOR_ID")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "ALPACA-OPTION-CHAIN-SCAN".to_string()),
            ),
            underlying: underlying.to_ascii_uppercase(),
            expiry,
            settlement: settlement.to_ascii_uppercase(),
            strike_range: strike_range_from_env()?,
            snapshot_interval_ms: optional_u64_env(
                "ALPACA_OPTION_CHAIN_SNAPSHOT_INTERVAL_MS",
                Some(DEFAULT_SNAPSHOT_INTERVAL_MS),
            )?,
            snapshot_greeks_poll_secs: optional_u64_env(
                "ALPACA_OPTION_CHAIN_SNAPSHOT_POLL_SECS",
                None,
            )?,
            entry_submit_enabled: bool_env("ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED", false)?,
            max_runtime_secs: optional_u64_env("ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS", None)?,
        })
    }
}

fn account_id_from_env() -> AccountId {
    AccountId::from(
        env::var("NAUTILUS_ALPACA_ACCOUNT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "ALPACA-001".to_string())
            .as_str(),
    )
}

fn strike_range_from_env() -> anyhow::Result<StrikeRange> {
    if let Ok(raw) = env::var("ALPACA_OPTION_CHAIN_FIXED_STRIKES") {
        let strikes = split_values(raw)
            .into_iter()
            .map(|value| Ok(Price::from(value.as_str())))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if strikes.is_empty() {
            bail!("ALPACA_OPTION_CHAIN_FIXED_STRIKES did not contain any strikes");
        }
        return Ok(StrikeRange::Fixed(strikes));
    }

    Ok(StrikeRange::AtmRelative {
        strikes_above: optional_usize_env("ALPACA_OPTION_CHAIN_STRIKES_ABOVE")?
            .unwrap_or(DEFAULT_STRIKES_ABOVE),
        strikes_below: optional_usize_env("ALPACA_OPTION_CHAIN_STRIKES_BELOW")?
            .unwrap_or(DEFAULT_STRIKES_BELOW),
    })
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

fn bool_env(name: &str, default: bool) -> anyhow::Result<bool> {
    let Some(value) = optional_raw_env(name) else {
        return Ok(default);
    };
    match value.as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("invalid {name}={value:?}, expected true or false"),
    }
}

fn optional_raw_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

async fn load_options_buying_power_if_needed(
    config: &OptionsEngineConfig,
    data_config: &AlpacaDataClientConfig,
) -> anyhow::Result<Option<f64>> {
    if let Some(value) = optional_f64_env("ALPACA_OPTION_CHAIN_OPTIONS_BUYING_POWER")? {
        return Ok(Some(value));
    }
    if config.naked_kinds.is_empty() {
        return Ok(None);
    }

    let client = AlpacaHttpClient::from_data_config(data_config)?;
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

fn split_values(raw: String) -> Vec<String> {
    raw.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn print_usage() {
    println!(
        "usage: alpaca-option-chain-scan-live-node UNDERLYING EXPIRY\n\
         example: ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS=60 \
         alpaca-option-chain-scan-live-node SPY 2026-07-02"
    );
}
