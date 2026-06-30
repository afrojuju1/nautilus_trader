//! Live Nautilus option-chain scan and entry node for Alpaca.

use std::{env, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use chrono::{Duration as ChronoDuration, NaiveDate, Utc};
use nautilus_alpaca::{
    account_capabilities::{account_capability_preflight, emit_account_capability_preflight},
    candidate_ledger_persistence::CandidateLedgerPersistenceHandle,
    candidate_scan_actor::{
        OptionChainCandidateScanActor, OptionChainCandidateScanActorConfig,
        candidate_scan_config_from_runtime,
    },
    common::consts::ALPACA_CLIENT_ID,
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    execution::account_entry_admission_reasons,
    factories::{AlpacaDataClientFactory, AlpacaExecutionClientFactory},
    http::{client::AlpacaHttpClient, models::AlpacaAccount},
    options_lifecycle::{
        OptionLifecycleRiskConfig, OptionLifecycleRiskHandle, emit_lifecycle_poll,
        emit_lifecycle_poll_error, lifecycle_events_from_activities,
        option_lifecycle_activity_request,
    },
    options_runtime::AlpacaOptionsRuntimeConfig,
    options_strategy::{AlpacaOptionsStrategy, AlpacaOptionsStrategyConfig},
    parse::parse_option_series_id,
    runtime::{StrategyState, emit_operator_event, save_strategy_state_atomic},
    state_persistence::{StrategyStatePersistenceHandle, start_runtime_lease_heartbeat},
    state_reconciliation::{
        StrategyStateReconciliationRepair, StrategyStateReconciliationReport,
        reconcile_strategy_state,
    },
};
use nautilus_common::enums::Environment;
use nautilus_infrastructure::sql::operational::{
    OperationalRepository, RuntimeLeaseRequest, STRATEGY_STATE_MIGRATION_VERSION,
    StrategyStateMutation, acquire_runtime_lease, persist_strategy_state_mutation,
    release_runtime_lease,
};
use nautilus_live::node::LiveNode;
use nautilus_model::{
    data::option_chain::StrikeRange,
    identifiers::{AccountId, ActorId, ClientId, StrategyId, TraderId},
    types::Price,
};
use nautilus_trading::strategy::StrategyConfig;
use serde_json::json;
use uuid::Uuid;

const DEFAULT_SNAPSHOT_INTERVAL_MS: u64 = 5_000;
const DEFAULT_STRIKES_ABOVE: usize = 10;
const DEFAULT_STRIKES_BELOW: usize = 10;
const DEFAULT_RUNTIME_LEASE_TTL_SECS: u64 = 300;

struct LiveSubmitPersistence {
    state: StrategyStatePersistenceHandle,
    candidate_ledger: CandidateLedgerPersistenceHandle,
    repository: Arc<OperationalRepository>,
    account_id: String,
    writer_id: String,
    run_id: Uuid,
    migration_latest_version: i64,
}

impl LiveSubmitPersistence {
    async fn release(self) {
        if let Err(error) = self.state.flush().await {
            log::error!(
                "Failed to flush Alpaca strategy-state mutations before lease release: account_id={} run_id={} error={error:#}",
                self.account_id,
                self.run_id
            );
            emit_operator_event(
                "strategy_state_persistence_error",
                json!({
                    "reason": "flush_failed",
                    "account_id": self.account_id.clone(),
                    "run_id": self.run_id.to_string(),
                    "error": error.to_string(),
                }),
            );
        }
        if let Err(error) = self.candidate_ledger.flush().await {
            log::error!(
                "Failed to flush Alpaca candidate-ledger evidence before lease release: account_id={} run_id={} error={error:#}",
                self.account_id,
                self.run_id
            );
            emit_operator_event(
                "candidate_ledger_error",
                json!({
                    "reason": "flush_failed",
                    "account_id": self.account_id.clone(),
                    "run_id": self.run_id.to_string(),
                    "error": error.to_string(),
                }),
            );
        }
        match release_runtime_lease(&self.repository, &self.account_id, self.run_id).await {
            Ok(true) => emit_operator_event(
                "runtime_lease_released",
                json!({
                    "account_id": self.account_id,
                    "run_id": self.run_id.to_string(),
                }),
            ),
            Ok(false) => log::warn!(
                "Alpaca runtime lease was not released because ownership was already gone: account_id={} run_id={}",
                self.account_id,
                self.run_id
            ),
            Err(error) => log::warn!(
                "Failed to release Alpaca runtime lease: account_id={} run_id={} error={error:#}",
                self.account_id,
                self.run_id
            ),
        }
    }
}

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
    max_runtime_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let runtime_config = AlpacaOptionsRuntimeConfig::from_runtime_env()?;
    if env::args().skip(1).any(|arg| arg == "--check-config") {
        print_config_check(&runtime_config);
        return Ok(());
    }
    let args = Args::from_env(&runtime_config)?;
    let broker_orders_requested =
        runtime_config.open_orders_enabled || runtime_config.close_orders_enabled;
    let runtime_config = if broker_orders_requested {
        AlpacaOptionsRuntimeConfig::from_runtime_env_with_operational_store()
            .await
            .context("broker orders require operational Postgres readiness")?
    } else {
        runtime_config
    };
    let series_id = parse_option_series_id(&args.underlying, &args.settlement, &args.expiry)
        .map_err(|e| anyhow::anyhow!("invalid Alpaca option series: {e}"))?;
    let client_id = ClientId::from(ALPACA_CLIENT_ID);
    let data_config = AlpacaDataClientConfig {
        snapshot_greeks_poll_secs: args.snapshot_greeks_poll_secs,
        ..Default::default()
    };
    let lifecycle_config = runtime_config.lifecycle_risk_config();
    let lifecycle_risk = OptionLifecycleRiskHandle::new(lifecycle_config.clone());
    let options_buying_power = load_options_buying_power_if_needed(&runtime_config, &data_config)
        .await
        .context("failed to load options buying-power context")?;
    let scan_config = candidate_scan_config_from_runtime(&runtime_config, options_buying_power);
    let mut strategy_state = runtime_config
        .load_strategy_state()
        .await
        .context("failed to load Alpaca options strategy state")?;
    let mut live_submit_persistence = if broker_orders_requested {
        Some(
            prepare_state_persistence(&runtime_config, &args)
                .await
                .context("failed to prepare operational state persistence readiness")?,
        )
    } else {
        None
    };
    let startup_account_admission_reasons = if broker_orders_requested {
        let result = async {
            let persistence = live_submit_persistence
                .as_ref()
                .context("broker-order operational persistence was not prepared")?;
            prepare_live_submit_broker_state(
                &runtime_config,
                &data_config,
                &mut strategy_state,
                persistence,
            )
            .await
            .context("failed to prepare Alpaca broker-order state")
        }
        .await;
        release_live_submit_persistence_on_error(result, &mut live_submit_persistence).await?
    } else {
        Vec::new()
    };
    let lifecycle_daemon_result = if broker_orders_requested {
        start_lifecycle_risk_daemon(&data_config, lifecycle_risk.clone(), lifecycle_config)
            .await
            .context("failed to start Alpaca option lifecycle risk daemon")
    } else {
        Ok(None)
    };
    let _lifecycle_daemon = release_live_submit_persistence_on_error(
        lifecycle_daemon_result,
        &mut live_submit_persistence,
    )
    .await?;
    let strategy_state_entry_count = strategy_state.entries.len();

    log::info!(
        "Starting Alpaca options live node: series={} snapshot_interval_ms={:?} max_runtime_secs={:?} strategies={:?} open_orders_enabled={} close_orders_enabled={} operational_store_required={} strategy_state_entries={}",
        series_id,
        args.snapshot_interval_ms,
        args.max_runtime_secs,
        runtime_config.enabled_strategy_family_names(),
        runtime_config.open_orders_enabled,
        runtime_config.close_orders_enabled,
        broker_orders_requested,
        strategy_state_entry_count,
    );

    let account_id = account_id_from_env();
    let node_result = LiveNode::builder(args.trader_id, Environment::Live)
        .and_then(|builder| {
            builder.with_name(args.node_name).add_data_client(
                Some(ALPACA_CLIENT_ID.to_string()),
                Box::new(AlpacaDataClientFactory::new()),
                Box::new(data_config),
            )
        })
        .and_then(|builder| {
            builder.add_exec_client(
                Some(ALPACA_CLIENT_ID.to_string()),
                Box::new(AlpacaExecutionClientFactory::new(
                    args.trader_id,
                    account_id,
                )),
                Box::new(AlpacaExecClientConfig::default()),
            )
        })
        .and_then(|builder| builder.with_delay_post_stop_secs(5).build())
        .map_err(anyhow::Error::from);
    let mut node =
        release_live_submit_persistence_on_error(node_result, &mut live_submit_persistence).await?;

    let mut actor = OptionChainCandidateScanActor::new(OptionChainCandidateScanActorConfig {
        actor_id: Some(args.actor_id),
        series: vec![series_id],
        strike_range: args.strike_range,
        snapshot_interval_ms: args.snapshot_interval_ms,
        client_id: Some(client_id),
        bootstrap_instruments: true,
        scan: scan_config,
    });
    if let Some(persistence) = &live_submit_persistence {
        actor = actor.with_candidate_ledger_persistence(persistence.candidate_ledger.clone());
    }
    release_live_submit_persistence_on_error(
        node.add_actor(actor).map_err(anyhow::Error::from),
        &mut live_submit_persistence,
    )
    .await?;

    let strategy_config = StrategyConfig {
        strategy_id: Some(StrategyId::from("ALPACA-OPTIONS-ENTRY")),
        order_id_tag: Some("AOE".to_string()),
        ..Default::default()
    };
    let mut entry_config =
        AlpacaOptionsStrategyConfig::from_runtime_config(strategy_config, &runtime_config);
    entry_config.admission.account_admission_reasons = startup_account_admission_reasons;
    entry_config.initial_state = strategy_state;
    entry_config.state_persistence = live_submit_persistence
        .as_ref()
        .map(|persistence| persistence.state.clone());
    entry_config.candidate_ledger_persistence = live_submit_persistence
        .as_ref()
        .map(|persistence| persistence.candidate_ledger.clone());
    entry_config.lifecycle_risk = Some(lifecycle_risk);
    let strategy = AlpacaOptionsStrategy::new(entry_config);
    release_live_submit_persistence_on_error(
        node.add_strategy(strategy).map_err(anyhow::Error::from),
        &mut live_submit_persistence,
    )
    .await?;

    if let Some(max_runtime_secs) = args.max_runtime_secs {
        let handle = node.handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(max_runtime_secs)).await;
            log::info!("Stopping Alpaca option-chain live node after {max_runtime_secs}s");
            handle.stop();
        });
    }

    let run_result = node.run().await;
    if let Some(persistence) = live_submit_persistence {
        persistence.release().await;
    }
    run_result?;
    Ok(())
}

async fn release_live_submit_persistence_on_error<T>(
    result: anyhow::Result<T>,
    persistence: &mut Option<LiveSubmitPersistence>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Some(persistence) = persistence.take() {
                persistence.release().await;
            }
            Err(error)
        }
    }
}

async fn prepare_state_persistence(
    config: &AlpacaOptionsRuntimeConfig,
    args: &Args,
) -> anyhow::Result<LiveSubmitPersistence> {
    let repository = config
        .operational_repository
        .as_ref()
        .context(
            "NAUTILUS_OPERATIONAL_DATABASE_URL is required when ALPACA_OPEN_ORDERS=true or ALPACA_CLOSE_ORDERS=true",
        )?
        .clone();
    let migration_status = repository.migration_status().await?;
    if let Some(dirty_version) = migration_status.dirty_version {
        bail!("Operational Postgres migration is dirty at version {dirty_version}");
    }
    let latest_version = migration_status.latest_version.unwrap_or_default();
    if latest_version < STRATEGY_STATE_MIGRATION_VERSION {
        bail!(
            "Operational Postgres migration {STRATEGY_STATE_MIGRATION_VERSION} is required; latest applied version is {latest_version}"
        );
    }

    let account_id = config.operational_account_id().to_string();
    let run_id = Uuid::new_v4();
    let holder_id = format!("{}:{}", args.node_name, std::process::id());
    let service_name = env::var("NAUTILUS_ALPACA_SERVICE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| args.node_name.clone());
    let ttl = Duration::from_secs(
        optional_u64_env(
            "ALPACA_RUNTIME_LEASE_TTL_SECS",
            Some(DEFAULT_RUNTIME_LEASE_TTL_SECS),
        )?
        .unwrap_or(DEFAULT_RUNTIME_LEASE_TTL_SECS),
    );
    let lease = acquire_runtime_lease(
        &repository,
        &account_id,
        &RuntimeLeaseRequest {
            holder_id: holder_id.clone(),
            run_id,
            service_name: Some(service_name),
            mode: "broker_orders".to_string(),
            ttl,
        },
    )
    .await?;
    if !lease.acquired {
        bail!(
            "Alpaca runtime lease for account {} is held by {} until {}",
            account_id,
            lease.holder_id,
            lease.expires_at
        );
    }
    log::info!(
        "Acquired Alpaca runtime lease: account_id={} holder_id={} run_id={} expires_at={}",
        account_id,
        holder_id,
        lease.run_id,
        lease.expires_at
    );
    let heartbeat_repository = repository.clone();
    let state_persistence = StrategyStatePersistenceHandle::spawn(
        repository,
        account_id.clone(),
        holder_id.clone(),
        run_id,
    );
    let candidate_ledger_persistence =
        CandidateLedgerPersistenceHandle::spawn(heartbeat_repository.clone(), account_id.clone());
    start_runtime_lease_heartbeat(
        heartbeat_repository.clone(),
        account_id.clone(),
        run_id,
        ttl,
        Some(state_persistence.clone()),
        Some(candidate_ledger_persistence.clone()),
    );
    Ok(LiveSubmitPersistence {
        state: state_persistence,
        candidate_ledger: candidate_ledger_persistence,
        repository: heartbeat_repository,
        account_id,
        writer_id: holder_id,
        run_id,
        migration_latest_version: latest_version,
    })
}

async fn prepare_live_submit_broker_state(
    config: &AlpacaOptionsRuntimeConfig,
    data_config: &AlpacaDataClientConfig,
    strategy_state: &mut StrategyState,
    persistence: &LiveSubmitPersistence,
) -> anyhow::Result<Vec<String>> {
    let client = AlpacaHttpClient::from_data_config(data_config)?;
    let account = client.account().await?;
    let account_config = client
        .account_configuration()
        .await
        .context("failed to load Alpaca account configuration")?;
    let capability_preflight = account_capability_preflight(config, &account, &account_config);
    emit_account_capability_preflight(config, &capability_preflight);
    if !capability_preflight.is_ready() {
        emit_operator_event(
            "live_submit_readiness_block",
            json!({
                "reason": "account_capability_preflight",
                "details": capability_preflight.reasons.clone(),
                "required_options_level": capability_preflight.required_options_level,
                "options_trading_level": capability_preflight.options_trading_level,
                "options_approved_level": capability_preflight.options_approved_level,
                "max_options_trading_level": capability_preflight.max_options_trading_level,
            }),
        );
        bail!(
            "broker orders blocked by Alpaca account capability preflight: {}",
            capability_preflight.reasons.join("; ")
        );
    }
    let account_reasons = account_entry_admission_reasons(&account);
    if !account_reasons.is_empty() {
        emit_operator_event(
            "live_submit_readiness_block",
            json!({
                "reason": "account_not_tradable",
                "details": account_reasons.clone(),
            }),
        );
        bail!(
            "broker orders blocked by Alpaca account admission: {}",
            account_reasons.join("; ")
        );
    }

    let report = reconcile_strategy_state(&client, strategy_state).await?;
    if report.changed {
        persist_reconciliation_repairs(config, persistence, &report, strategy_state)
            .await
            .context("failed to persist startup strategy-state reconciliation events")?;
    }
    if report.has_unmanaged_broker_state() {
        emit_unmanaged_broker_state_block(&report);
        bail!(
            "broker orders blocked by unmanaged broker state: positions=[{}] open_orders=[{}] partial_positions=[{}]",
            report.unmanaged_position_symbols.join(","),
            report.unmanaged_open_order_symbols.join(","),
            report.partial_position_symbols.join(","),
        );
    }

    emit_operator_event(
        "live_submit_readiness",
        json!({
            "operational_store_ready": true,
            "lease_held": true,
            "broker_state_reconciled": true,
            "required_migration_version": STRATEGY_STATE_MIGRATION_VERSION,
            "latest_migration_version": persistence.migration_latest_version,
            "state_persistence_healthy": persistence.state.is_healthy(),
            "candidate_ledger_persistence_healthy": persistence.candidate_ledger.is_healthy(),
            "operational_account_id": persistence.account_id,
            "run_id": persistence.run_id.to_string(),
            "reconciliation_events": report.repairs.len(),
            "state_repaired": report.changed,
        }),
    );
    Ok(account_reasons)
}

async fn persist_reconciliation_repairs(
    config: &AlpacaOptionsRuntimeConfig,
    persistence: &LiveSubmitPersistence,
    report: &StrategyStateReconciliationReport,
    strategy_state: &StrategyState,
) -> anyhow::Result<()> {
    if report.repairs.is_empty() {
        anyhow::bail!("strategy-state reconciliation changed state without repair events");
    }

    for repair in &report.repairs {
        let mutation = reconciliation_state_mutation(repair, persistence)?;
        persist_strategy_state_mutation(
            &persistence.repository,
            &persistence.account_id,
            &mutation,
            strategy_state,
        )
        .await
        .with_context(|| {
            format!(
                "failed to persist reconciliation repair {} for order_list_id={}",
                repair.action, repair.order_list_id
            )
        })?;
    }

    save_strategy_state_atomic(&config.state_path, strategy_state)
        .context("failed to update local strategy-state mirror after reconciliation")?;
    Ok(())
}

fn reconciliation_state_mutation(
    repair: &StrategyStateReconciliationRepair,
    persistence: &LiveSubmitPersistence,
) -> anyhow::Result<StrategyStateMutation> {
    let mut mutation = StrategyStateMutation::new(
        Uuid::new_v4(),
        "entry_reconciled",
        json!({
            "action": repair.action,
            "reason": repair.reason,
            "trade_date": repair.trade_date,
            "underlying": repair.underlying,
            "strategy": repair.strategy,
            "order_list_id": repair.order_list_id,
            "close_order_list_id": repair.close_order_list_id,
            "parent_order_id": repair.parent_order_id,
            "symbols": repair.symbols,
            "closed": repair.closed,
            "canceled": repair.canceled,
        }),
    );
    mutation.strategy = Some(repair.strategy.clone());
    mutation.underlying = Some(repair.underlying.clone());
    mutation.trade_date = NaiveDate::parse_from_str(&repair.trade_date, "%Y-%m-%d").ok();
    mutation.order_list_id = Some(repair.order_list_id.clone());
    mutation.venue_order_id = repair.parent_order_id.clone();
    mutation.ts_event = Some(Utc::now());
    mutation.writer_id = Some(persistence.writer_id.clone());
    mutation.run_id = Some(persistence.run_id);
    Ok(mutation)
}

async fn start_lifecycle_risk_daemon(
    data_config: &AlpacaDataClientConfig,
    handle: OptionLifecycleRiskHandle,
    config: OptionLifecycleRiskConfig,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let client = AlpacaHttpClient::from_data_config(data_config)?;
    poll_lifecycle_risk_once(&client, &handle, &config).await;
    if config.poll_secs == 0 {
        return Ok(None);
    }

    Ok(Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(config.poll_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            poll_lifecycle_risk_once(&client, &handle, &config).await;
        }
    })))
}

async fn poll_lifecycle_risk_once(
    client: &AlpacaHttpClient,
    handle: &OptionLifecycleRiskHandle,
    config: &OptionLifecycleRiskConfig,
) {
    let now = Utc::now();
    let after = now - ChronoDuration::hours(clamped_hours(config.activity_lookback_hours));
    let request = option_lifecycle_activity_request(after);
    match client.account_activities_all(&request).await {
        Ok(activities) => {
            let events = lifecycle_events_from_activities(&activities);
            handle.update_events(events.clone(), now);
            emit_lifecycle_poll(&events, &handle.snapshot().account_blocks);
        }
        Err(error) => {
            let error = error.to_string();
            handle.record_poll_error(error.clone(), now);
            emit_lifecycle_poll_error(&error);
        }
    }
}

fn clamped_hours(hours: u64) -> i64 {
    hours.min(i64::MAX as u64) as i64
}

fn emit_unmanaged_broker_state_block(report: &StrategyStateReconciliationReport) {
    emit_operator_event(
        "live_submit_readiness_block",
        json!({
            "reason": "unmanaged_broker_state",
            "unmanaged_position_symbols": report.unmanaged_position_symbols.clone(),
            "unmanaged_open_order_symbols": report.unmanaged_open_order_symbols.clone(),
            "partial_position_symbols": report.partial_position_symbols.clone(),
        }),
    );
}

impl Args {
    fn from_env(config: &AlpacaOptionsRuntimeConfig) -> anyhow::Result<Self> {
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

fn optional_raw_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

async fn load_options_buying_power_if_needed(
    config: &AlpacaOptionsRuntimeConfig,
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
        "usage: alpaca-options-node [--check-config] [UNDERLYING EXPIRY]\n\
         example: ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS=60 \
         alpaca-options-node SPY 2026-07-02"
    );
}

fn print_config_check(config: &AlpacaOptionsRuntimeConfig) {
    println!(
        "alpaca_options_runtime_config: underlyings={} strategy_families={} dry_run_families={} open_orders_enabled={} close_orders_enabled={} close_order_mode={} quantity={} max_active_entries={} max_daily_submits={} max_open_orders={} max_active_entries_per_underlying={} max_active_entries_per_sector={} fleet_account={} fleet_policy_blocks={} stale_close_secs={} close_regular_hours_only={} close_window={}-{} close_price_cushion={:.2} max_close_attempts={} close_reprice_cooldown_secs={} active_risk_candidate_quote_limit={} active_risk_quote_stale_secs={} expiration_exit_days={} lifecycle_poll_secs={} lifecycle_activity_lookback_hours={} lifecycle_activity_block_hours={} expiration_entry_block_days={} max_iterations={} interval_secs={} state_path={} candidate_ledger_enabled={} candidate_ledger_max_candidates={}",
        config.underlyings.join(","),
        config.enabled_strategy_family_names().join(","),
        config.dry_run_strategy_family_names().join(","),
        config.open_orders_enabled,
        config.close_orders_enabled,
        config.close_order_mode.as_str(),
        config.quantity,
        format_limit(config.max_active_entries),
        format_limit(config.max_daily_submits),
        format_limit(config.max_open_orders),
        format_limit(config.max_active_entries_per_underlying),
        format_limit(config.max_active_entries_per_sector),
        config.fleet_account_id.as_deref().unwrap_or("none"),
        if config.fleet_policy_blocks.is_empty() {
            "none".to_string()
        } else {
            config.fleet_policy_blocks.join(",")
        },
        config.stale_close_secs,
        config.close_regular_hours_only,
        config.close_start,
        config.close_end,
        config.close_price_cushion,
        config.max_close_attempts,
        config.close_reprice_cooldown_secs,
        config.active_risk_candidate_quote_limit,
        config.active_risk_quote_stale_secs,
        config.expiration_exit_days,
        config.lifecycle_poll_secs,
        config.lifecycle_activity_lookback_hours,
        config.lifecycle_activity_block_hours,
        config.expiration_entry_block_days,
        config.max_iterations,
        config.interval_secs,
        config.state_path.display(),
        config.candidate_ledger_enabled,
        config.candidate_ledger_max_candidates,
    );
}

fn format_limit(limit: Option<usize>) -> String {
    limit.map_or_else(|| "unlimited".to_string(), |value| value.to_string())
}
