use std::path::PathBuf;

use nautilus_persistence::warehouse::clickhouse::{
    ClickHouseConnectOptions, DEFAULT_MIGRATIONS_DIR, check_health, run_migrations,
    run_quote_tick_smoke,
};

use crate::opt::{ClickHouseConfig, WarehouseCommand, WarehouseOpt};

/// Executes market-data warehouse commands.
///
/// # Errors
///
/// Returns an error if the warehouse command fails.
pub(crate) async fn run_warehouse_command(opt: WarehouseOpt) -> anyhow::Result<()> {
    match opt.command {
        WarehouseCommand::Migrate(config) => {
            let connect_options = connect_options_from_config(&config);
            log::info!(
                "Connecting to ClickHouse at {}",
                connect_options.connection_string_masked()
            );
            let migrations_dir = config
                .migrations_dir
                .unwrap_or_else(|| PathBuf::from(DEFAULT_MIGRATIONS_DIR));
            let report = run_migrations(&connect_options, &migrations_dir).await?;
            println!(
                "ClickHouse migrations complete: applied={} skipped={}",
                report.applied_count(),
                report.skipped_count()
            );
            for migration in report.applied {
                println!(
                    "applied {} {} statements={} checksum={}",
                    migration.version,
                    migration.description,
                    migration.statements,
                    migration.checksum
                );
            }
            for migration in report.skipped {
                println!(
                    "skipped {} {} statements={} checksum={}",
                    migration.version,
                    migration.description,
                    migration.statements,
                    migration.checksum
                );
            }
        }
        WarehouseCommand::Health(config) => {
            let connect_options = connect_options_from_config(&config);
            check_health(&connect_options).await?;
            println!("ClickHouse warehouse health ok");
        }
        WarehouseCommand::QuoteSmoke(config) => {
            let connect_options = connect_options_from_config(&config.clickhouse);
            let report = run_quote_tick_smoke(&connect_options, &config.source).await?;
            println!(
                "ClickHouse QuoteTick smoke complete: ingest_run_id={} written={} count={}",
                report.ingest_run_id, report.written, report.count
            );
        }
    }

    Ok(())
}

fn connect_options_from_config(config: &ClickHouseConfig) -> ClickHouseConnectOptions {
    ClickHouseConnectOptions::from_env_with_overrides(
        config.url.clone(),
        config.username.clone(),
        config.password.clone(),
        config.database.clone(),
    )
}
