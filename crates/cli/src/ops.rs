use std::env;

use nautilus_infrastructure::sql::operational::{
    OPERATIONAL_SCHEMA_DEFAULT, OperationalRepository, load_strategy_state_metadata,
};
use serde_json::json;

use crate::opt::{OpsCommand, OpsOpt, OpsStatusConfig};

/// Executes source-neutral trading operations.
///
/// # Errors
///
/// Returns an error if the requested operation fails.
pub(crate) async fn run_ops_command(opt: OpsOpt) -> anyhow::Result<()> {
    match opt.command {
        OpsCommand::Status(config) => run_status(config).await?,
    }
    Ok(())
}

async fn run_status(config: OpsStatusConfig) -> anyhow::Result<()> {
    let database_url = match config.database_url {
        Some(value) => value,
        None => env::var("NAUTILUS_OPERATIONAL_DATABASE_URL").map_err(|_| {
            anyhow::anyhow!(
                "NAUTILUS_OPERATIONAL_DATABASE_URL is required unless --database-url is provided"
            )
        })?,
    };
    let schema = config
        .schema
        .or_else(|| env::var("NAUTILUS_OPERATIONAL_SCHEMA").ok())
        .unwrap_or_else(|| OPERATIONAL_SCHEMA_DEFAULT.to_string());

    let storage =
        OperationalRepository::connect_read_only_with_schema(&database_url, &schema).await?;
    let migration_status = storage.migration_status().await?;
    let strategy_state_metadata = match config.account_id.as_deref() {
        Some(account_id) => load_strategy_state_metadata(&storage, account_id).await?,
        None => None,
    };

    if config.json {
        let strategy_state = strategy_state_metadata.as_ref().map(|metadata| {
            json!({
                "version": metadata.version,
                "writer_id": metadata.writer_id,
                "run_id": metadata.run_id,
                "last_event_id": metadata.last_event_id,
            })
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "operational_store": {
                    "schema": schema,
                    "applied_migrations": migration_status.applied_count,
                    "latest_migration_version": migration_status.latest_version,
                    "dirty_migration_version": migration_status.dirty_version,
                },
                "strategy_state": {
                    "account_id": config.account_id,
                    "metadata": strategy_state,
                },
            }))?
        );
    } else {
        println!(
            "ops_status storage=postgres schema={} applied_migrations={} latest_migration_version={:?} dirty_migration_version={:?}",
            schema,
            migration_status.applied_count,
            migration_status.latest_version,
            migration_status.dirty_version,
        );
        if let Some(account_id) = config.account_id {
            match strategy_state_metadata {
                Some(metadata) => {
                    println!(
                        "strategy_state account_id={} version={} writer_id={:?} run_id={:?} last_event_id={:?}",
                        account_id,
                        metadata.version,
                        metadata.writer_id,
                        metadata.run_id,
                        metadata.last_event_id,
                    );
                }
                None => println!("strategy_state account_id={account_id} missing=true"),
            }
        }
    }

    Ok(())
}
