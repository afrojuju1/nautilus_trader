use std::path::PathBuf;

use nautilus_persistence::warehouse::clickhouse::{
    ClickHouseConnectOptions, DEFAULT_MIGRATIONS_DIR, MarketDataReadSource,
    QuoteTickCatalogBackfill, QuoteTickReadRequest, backfill_quote_ticks_from_catalog,
    check_health, run_migrations, run_quote_tick_smoke, validate_quote_ticks,
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
        WarehouseCommand::BackfillQuotes(config) => {
            let connect_options = connect_options_from_config(&config.clickhouse);
            let request = QuoteTickCatalogBackfill::from_unix_nanos(
                config.catalog_uri,
                config.instrument_ids,
                config.start_ns,
                config.end_ns,
                config.source,
                config.batch_size,
            )?;
            let report = backfill_quote_ticks_from_catalog(&connect_options, &request).await?;
            println!(
                "ClickHouse QuoteTick backfill complete: ingest_run_id={} catalog_rows={} written_rows={} warehouse_rows={} source={} catalog_uri={}",
                report.ingest_run_id,
                report.catalog_rows,
                report.written_rows,
                report.warehouse_rows,
                report.source,
                report.catalog_uri
            );
            if !report.instrument_ids.is_empty() {
                println!("instruments={}", report.instrument_ids.join(","));
            }
        }
        WarehouseCommand::ValidateQuotes(config) => {
            let connect_options = connect_options_from_config(&config.clickhouse);
            let read_source = match config.read_source.as_deref() {
                Some(value) => value.parse::<MarketDataReadSource>()?,
                None => MarketDataReadSource::from_env()?,
            };
            let request = QuoteTickReadRequest::from_unix_nanos(
                config.catalog_uri,
                config.instrument_ids,
                config.start_ns,
                config.end_ns,
                config.source,
                read_source,
            )?;
            let report = validate_quote_ticks(&connect_options, &request).await?;
            println!(
                "ClickHouse QuoteTick validation complete: read_source={} catalog_rows={} clickhouse_rows={} selected_rows={} first_ts_init={:?} last_ts_init={:?} checksum={} source={} catalog_uri={}",
                report.read_source,
                report.catalog.rows,
                report.clickhouse.rows,
                report.selected.rows,
                report.catalog.first_ts_init,
                report.catalog.last_ts_init,
                report.catalog.checksum,
                report.source,
                report.catalog_uri
            );
            if !report.instrument_ids.is_empty() {
                println!("instruments={}", report.instrument_ids.join(","));
            }
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
