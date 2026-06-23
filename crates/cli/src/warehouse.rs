use std::path::PathBuf;

use nautilus_persistence::warehouse::clickhouse::{
    ClickHouseConnectOptions, DEFAULT_MIGRATIONS_DIR, run_migrations,
};

use crate::opt::{WarehouseCommand, WarehouseOpt};

/// Executes market-data warehouse commands.
///
/// # Errors
///
/// Returns an error if the warehouse command fails.
pub(crate) async fn run_warehouse_command(opt: WarehouseOpt) -> anyhow::Result<()> {
    match opt.command {
        WarehouseCommand::Migrate(config) => {
            let connect_options = ClickHouseConnectOptions::from_env_with_overrides(
                config.url,
                config.username,
                config.password,
                config.database,
            );
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
    }

    Ok(())
}
