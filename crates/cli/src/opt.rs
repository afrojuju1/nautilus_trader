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

use std::path::PathBuf;

use clap::Parser;

/// Command-line interface for NautilusTrader.
#[derive(Debug, Parser)]
#[clap(version, about, author)]
pub struct NautilusCli {
    #[clap(subcommand)]
    pub command: Commands,
}

/// Available top-level commands for the NautilusTrader CLI.
#[derive(Parser, Debug)]
pub enum Commands {
    Database(DatabaseOpt),
    Warehouse(WarehouseOpt),
    Ops(OpsOpt),
    #[cfg(feature = "alpaca")]
    Adapters(AdaptersOpt),
    #[cfg(feature = "defi")]
    Blockchain(BlockchainOpt),
}

/// Database management options and subcommands.
#[derive(Parser, Debug)]
#[command(about = "Postgres database operations", long_about = None)]
pub struct DatabaseOpt {
    #[clap(subcommand)]
    pub command: DatabaseCommand,
}

/// Configuration parameters for database connection and operations.
#[derive(Parser, Debug, Clone)]
pub struct DatabaseConfig {
    /// Hostname or IP address of the database server.
    #[arg(long)]
    pub host: Option<String>,
    /// Port number of the database server.
    #[arg(long)]
    pub port: Option<u16>,
    /// Username for connecting to the database.
    #[arg(long)]
    pub username: Option<String>,
    /// Name of the database.
    #[arg(long)]
    pub database: Option<String>,
    /// Password for connecting to the database.
    #[arg(long)]
    pub password: Option<String>,
    /// Directory path to the schema files.
    #[arg(long)]
    pub schema: Option<String>,
}

/// Available database management commands.
#[derive(Parser, Debug, Clone)]
#[command(about = "Postgres database operations", long_about = None)]
pub enum DatabaseCommand {
    /// Initializes a new Postgres database with the latest schema.
    Init(DatabaseConfig),
    /// Drops roles, privileges and deletes all data from the database.
    Drop(DatabaseConfig),
}

/// Market-data warehouse options and subcommands.
#[derive(Parser, Debug)]
#[command(about = "Market-data warehouse operations", long_about = None)]
pub struct WarehouseOpt {
    #[clap(subcommand)]
    pub command: WarehouseCommand,
}

/// Configuration parameters for ClickHouse warehouse operations.
#[derive(Parser, Debug, Clone)]
pub struct ClickHouseConfig {
    /// ClickHouse HTTP URL, including protocol and port.
    #[arg(long)]
    pub url: Option<String>,
    /// Username for connecting to ClickHouse.
    #[arg(long)]
    pub username: Option<String>,
    /// Password for connecting to ClickHouse.
    #[arg(long)]
    pub password: Option<String>,
    /// ClickHouse database selected for market-data warehouse operations.
    #[arg(long)]
    pub database: Option<String>,
    /// Directory path to ClickHouse migration SQL files.
    #[arg(long)]
    pub migrations_dir: Option<PathBuf>,
}

/// Available warehouse management commands.
#[derive(Parser, Debug, Clone)]
#[command(about = "Market-data warehouse operations", long_about = None)]
pub enum WarehouseCommand {
    /// Applies pending ClickHouse warehouse migrations.
    Migrate(ClickHouseConfig),
    /// Checks ClickHouse warehouse connectivity.
    Health(ClickHouseConfig),
    /// Writes and reads back a tiny QuoteTick batch.
    QuoteSmoke(ClickHouseSmokeConfig),
    /// Backfills QuoteTick data from a Nautilus catalog into ClickHouse.
    BackfillQuotes(ClickHouseBackfillQuotesConfig),
    /// Validates catalog QuoteTick data against ClickHouse rows.
    ValidateQuotes(ClickHouseValidateQuotesConfig),
}

/// Source-neutral trading operations.
#[derive(Parser, Debug)]
#[command(about = "Source-neutral trading operations", long_about = None)]
pub struct OpsOpt {
    #[clap(subcommand)]
    pub command: OpsCommand,
}

/// Available source-neutral trading operations.
#[derive(Parser, Debug, Clone)]
#[command(about = "Source-neutral trading operations", long_about = None)]
pub enum OpsCommand {
    /// Checks operational Postgres connectivity and optional strategy-state metadata.
    Status(OpsStatusConfig),
}

/// Configuration parameters for source-neutral operational-store status.
#[derive(Parser, Debug, Clone)]
pub struct OpsStatusConfig {
    /// Operational Postgres database URL. Defaults to NAUTILUS_OPERATIONAL_DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,
    /// Operational Postgres schema. Defaults to NAUTILUS_OPERATIONAL_SCHEMA or trading_ops.
    #[arg(long)]
    pub schema: Option<String>,
    /// Optional account ID for strategy-state metadata.
    #[arg(long)]
    pub account_id: Option<String>,
    /// Prints JSON output.
    #[arg(long)]
    pub json: bool,
}

#[cfg(feature = "alpaca")]
/// Adapter-specific operational commands.
#[derive(Parser, Debug)]
#[command(about = "Adapter-specific operational commands", long_about = None)]
pub struct AdaptersOpt {
    #[clap(subcommand)]
    pub command: AdapterCommand,
}

#[cfg(feature = "alpaca")]
/// Available adapter-specific command groups.
#[derive(Parser, Debug)]
#[command(about = "Adapter-specific operational commands", long_about = None)]
pub enum AdapterCommand {
    Alpaca(AlpacaAdapterOpt),
}

#[cfg(feature = "alpaca")]
/// Alpaca adapter operational commands.
#[derive(Parser, Debug)]
#[command(about = "Alpaca adapter operations", long_about = None)]
pub struct AlpacaAdapterOpt {
    #[clap(subcommand)]
    pub command: AlpacaAdapterCommand,
}

#[cfg(feature = "alpaca")]
/// Available Alpaca adapter commands.
#[derive(Parser, Debug, Clone)]
#[command(about = "Alpaca adapter operations", long_about = None)]
pub enum AlpacaAdapterCommand {
    /// Prints supervised Alpaca runtime status.
    Status(ForwardedArgs),
    /// Checks Alpaca account, positions, and open orders.
    Account(ForwardedArgs),
    /// Summarizes configured Alpaca accounts.
    Fleet(ForwardedArgs),
    /// Emits Alpaca candidate alerts.
    Alerts(AlpacaAlertsOpt),
    /// Builds Alpaca-backed performance reports.
    Performance(ForwardedArgs),
    /// Replays Alpaca candidate evidence against historical option bars.
    Replay(ForwardedArgs),
    /// Syncs Alpaca operational strategy state to configured local files.
    SyncState(ForwardedArgs),
}

#[cfg(feature = "alpaca")]
/// Alpaca alert commands.
#[derive(Parser, Debug, Clone)]
#[command(about = "Alpaca alert operations", long_about = None)]
pub struct AlpacaAlertsOpt {
    #[clap(subcommand)]
    pub command: AlpacaAlertsCommand,
}

#[cfg(feature = "alpaca")]
/// Available Alpaca alert commands.
#[derive(Parser, Debug, Clone)]
pub enum AlpacaAlertsCommand {
    /// Emits candidate alerts from the operational store.
    Candidates(ForwardedArgs),
}

#[cfg(feature = "alpaca")]
/// Arguments forwarded to adapter-owned command implementations.
#[derive(Parser, Debug, Clone)]
pub struct ForwardedArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// Configuration parameters for the warehouse QuoteTick smoke command.
#[derive(Parser, Debug, Clone)]
pub struct ClickHouseSmokeConfig {
    #[clap(flatten)]
    pub clickhouse: ClickHouseConfig,
    /// Source label recorded on the smoke row.
    #[arg(long, default_value = "smoke")]
    pub source: String,
}

/// Configuration parameters for catalog-backed QuoteTick warehouse backfills.
#[derive(Parser, Debug, Clone)]
pub struct ClickHouseBackfillQuotesConfig {
    #[clap(flatten)]
    pub clickhouse: ClickHouseConfig,
    /// Nautilus catalog path or URI to read from.
    #[arg(long)]
    pub catalog_uri: String,
    /// Instrument ID to backfill. May be passed more than once.
    #[arg(long = "instrument-id")]
    pub instrument_ids: Vec<String>,
    /// Inclusive start timestamp in Unix nanoseconds.
    #[arg(long)]
    pub start_ns: Option<u64>,
    /// Inclusive end timestamp in Unix nanoseconds.
    #[arg(long)]
    pub end_ns: Option<u64>,
    /// Source label recorded on inserted rows.
    #[arg(long, default_value = "catalog-backfill")]
    pub source: String,
    /// Rows per ClickHouse insert batch.
    #[arg(long, default_value_t = 5000)]
    pub batch_size: usize,
}

/// Configuration parameters for catalog versus ClickHouse QuoteTick validation.
#[derive(Parser, Debug, Clone)]
pub struct ClickHouseValidateQuotesConfig {
    #[clap(flatten)]
    pub clickhouse: ClickHouseConfig,
    /// Nautilus catalog path or URI to validate against.
    #[arg(long)]
    pub catalog_uri: String,
    /// Instrument ID to validate. May be passed more than once.
    #[arg(long = "instrument-id")]
    pub instrument_ids: Vec<String>,
    /// Inclusive start timestamp in Unix nanoseconds. Required for ClickHouse validation.
    #[arg(long)]
    pub start_ns: Option<u64>,
    /// Inclusive end timestamp in Unix nanoseconds. Required for ClickHouse validation.
    #[arg(long)]
    pub end_ns: Option<u64>,
    /// ClickHouse source label to validate.
    #[arg(long, default_value = "catalog-backfill")]
    pub source: String,
    /// Effective market-data read source: catalog or clickhouse. Defaults to environment or catalog.
    #[arg(long)]
    pub read_source: Option<String>,
}

#[cfg(feature = "defi")]
/// Blockchain management options and subcommands.
#[derive(Parser, Debug)]
#[command(about = "Blockchain operations", long_about = None)]
pub struct BlockchainOpt {
    #[clap(subcommand)]
    pub command: BlockchainCommand,
}

#[cfg(feature = "defi")]
/// Available blockchain management commands.
#[derive(Parser, Debug, Clone)]
#[command(about = "Blockchain operations", long_about = None)]
pub enum BlockchainCommand {
    /// Syncs blockchain blocks.
    SyncBlocks {
        /// The blockchain chain name (case-insensitive). Examples: ethereum, arbitrum, base, polygon, bsc
        #[arg(long)]
        chain: String,
        /// Starting block number to sync from (optional)
        #[arg(long)]
        from_block: Option<u64>,
        /// Ending block number to sync to (optional, defaults to current chain head)
        #[arg(long)]
        to_block: Option<u64>,
        /// Database configuration options
        #[clap(flatten)]
        database: DatabaseConfig,
    },
    /// Sync DEX pools.
    SyncDex {
        /// The blockchain chain name (case-insensitive). Supported chains are listed below.
        #[arg(long)]
        chain: String,
        /// The DEX name (case-insensitive). Supported DEX names are listed below.
        #[arg(long)]
        dex: String,
        /// RPC HTTP URL for blockchain calls (optional, falls back to `RPC_HTTP_URL` env var)
        #[arg(long)]
        rpc_url: Option<String>,
        /// Reset sync progress and start from the beginning, ignoring last synced block
        #[arg(long)]
        reset: bool,
        /// Maximum number of Multicall calls per RPC request (optional, defaults to 200)
        #[arg(long)]
        multicall_calls_per_rpc_request: Option<u32>,
        /// Database configuration options
        #[clap(flatten)]
        database: DatabaseConfig,
    },
    /// Analyze a specific DEX pool.
    AnalyzePool {
        /// The blockchain chain name (case-insensitive). Supported chains are listed below.
        #[arg(long)]
        chain: String,
        /// The DEX name (case-insensitive). Supported DEX names are listed below.
        #[arg(long)]
        dex: String,
        /// The pool contract address
        #[arg(long)]
        address: String,
        /// Starting block number to sync from (optional)
        #[arg(long)]
        from_block: Option<u64>,
        /// Ending block number to sync to (optional, defaults to current chain head)
        #[arg(long)]
        to_block: Option<u64>,
        /// RPC HTTP URL for blockchain calls (optional, falls back to RPC_HTTP_URL env var)
        #[expect(
            clippy::doc_markdown,
            reason = "clap renders doc comments as plain help text"
        )]
        #[arg(long)]
        rpc_url: Option<String>,
        /// Reset sync progress and start from the beginning, ignoring last synced block
        #[arg(long)]
        reset: bool,
        /// Return needs_bootstrap for pools without a valid snapshot before the target block
        #[expect(
            clippy::doc_markdown,
            reason = "clap renders doc comments as plain help text"
        )]
        #[arg(long)]
        require_existing_snapshot: bool,
        /// Checkpoint block numbers to snapshot in one pass (comma-separated, each at or below to-block)
        #[arg(long, value_delimiter = ',')]
        checkpoint_blocks: Vec<u64>,
        /// Skip on-chain validation and persist replay-derived snapshots without the multicall compare
        #[arg(long)]
        skip_validation: bool,
        /// Maximum number of Multicall calls per RPC request (optional, defaults to 200)
        #[arg(long)]
        multicall_calls_per_rpc_request: Option<u32>,
        /// Database configuration options
        #[clap(flatten)]
        database: DatabaseConfig,
    },
    /// Analyze several DEX pools in one runtime.
    AnalyzePools {
        /// The blockchain chain name (case-insensitive). Supported chains are listed below.
        #[arg(long)]
        chain: String,
        /// The DEX name (case-insensitive). Supported DEX names are listed below.
        #[arg(long)]
        dex: String,
        /// Pool contract address. Can be repeated.
        #[arg(long = "address")]
        addresses: Vec<String>,
        /// File containing one pool contract address per line. Empty lines and comment lines are ignored.
        #[arg(long)]
        addresses_file: Option<String>,
        /// Starting block number to sync from (optional)
        #[arg(long)]
        from_block: Option<u64>,
        /// Ending block number to sync to (optional, defaults to current chain head)
        #[arg(long)]
        to_block: Option<u64>,
        /// RPC HTTP URL for blockchain calls (optional, falls back to RPC_HTTP_URL env var)
        #[expect(
            clippy::doc_markdown,
            reason = "clap renders doc comments as plain help text"
        )]
        #[arg(long)]
        rpc_url: Option<String>,
        /// Reset sync progress and start from the beginning, ignoring last synced block
        #[arg(long)]
        reset: bool,
        /// Return needs_bootstrap for pools without a valid snapshot before the target block
        #[expect(
            clippy::doc_markdown,
            reason = "clap renders doc comments as plain help text"
        )]
        #[arg(long)]
        require_existing_snapshot: bool,
        /// Checkpoint block numbers to snapshot in one pass (comma-separated, each at or below to-block)
        #[arg(long, value_delimiter = ',')]
        checkpoint_blocks: Vec<u64>,
        /// Skip on-chain validation and persist replay-derived snapshots without the multicall compare
        #[arg(long)]
        skip_validation: bool,
        /// Maximum number of pools to analyze concurrently (optional, defaults to 4)
        #[arg(long)]
        concurrency: Option<usize>,
        /// Maximum number of Multicall calls per RPC request (optional, defaults to 200)
        #[arg(long)]
        multicall_calls_per_rpc_request: Option<u32>,
        /// Database configuration options
        #[clap(flatten)]
        database: DatabaseConfig,
    },
}

#[cfg(all(test, feature = "defi"))]
mod tests {
    use clap::Parser;
    use rstest::rstest;

    use super::*;

    #[rstest]
    fn analyze_pools_cli_parses_repeated_addresses_file_and_shared_options() {
        let cli = NautilusCli::try_parse_from([
            "nautilus",
            "blockchain",
            "analyze-pools",
            "--chain",
            "ethereum",
            "--dex",
            "UniswapV3",
            "--address",
            "0x1111111111111111111111111111111111111111",
            "--address",
            "0x2222222222222222222222222222222222222222",
            "--addresses-file",
            "/tmp/pools.txt",
            "--from-block",
            "100",
            "--to-block",
            "200",
            "--rpc-url",
            "http://localhost:8545",
            "--reset",
            "--require-existing-snapshot",
            "--multicall-calls-per-rpc-request",
            "25",
            "--host",
            "localhost",
            "--port",
            "5433",
            "--username",
            "postgres",
            "--database",
            "nautilus",
            "--password",
            "secret",
        ])
        .unwrap();

        match cli.command {
            Commands::Blockchain(BlockchainOpt {
                command:
                    BlockchainCommand::AnalyzePools {
                        chain,
                        dex,
                        addresses,
                        addresses_file,
                        from_block,
                        to_block,
                        rpc_url,
                        reset,
                        require_existing_snapshot,
                        checkpoint_blocks,
                        skip_validation,
                        concurrency,
                        multicall_calls_per_rpc_request,
                        database,
                    },
            }) => {
                assert_eq!(chain, "ethereum");
                assert_eq!(dex, "UniswapV3");
                assert_eq!(
                    addresses,
                    vec![
                        "0x1111111111111111111111111111111111111111".to_string(),
                        "0x2222222222222222222222222222222222222222".to_string(),
                    ]
                );
                assert_eq!(addresses_file.as_deref(), Some("/tmp/pools.txt"));
                assert_eq!(from_block, Some(100));
                assert_eq!(to_block, Some(200));
                assert_eq!(rpc_url.as_deref(), Some("http://localhost:8545"));
                assert!(reset);
                assert!(require_existing_snapshot);
                assert!(checkpoint_blocks.is_empty());
                assert!(!skip_validation);
                assert_eq!(concurrency, None);
                assert_eq!(multicall_calls_per_rpc_request, Some(25));
                assert_eq!(database.host.as_deref(), Some("localhost"));
                assert_eq!(database.port, Some(5433));
                assert_eq!(database.username.as_deref(), Some("postgres"));
                assert_eq!(database.database.as_deref(), Some("nautilus"));
                assert_eq!(database.password.as_deref(), Some("secret"));
                assert_eq!(database.schema, None);
            }
            _ => panic!("Expected analyze-pools blockchain command"),
        }
    }

    #[rstest]
    #[case("analyze-pool")]
    #[case("analyze-pools")]
    fn blockchain_analysis_help_lists_capabilities_as_plain_text(#[case] subcommand: &str) {
        let mut command = crate::cli_command();
        let help = command
            .find_subcommand_mut("blockchain")
            .and_then(|command| command.find_subcommand_mut(subcommand))
            .map(|command| command.render_long_help().to_string())
            .unwrap();

        // Snapshot-capable DEXes are listed; the registered-but-unsupported SushiSwapV2 is not.
        assert!(help.contains("UniswapV3"));
        assert!(help.contains("PancakeSwapV3"));
        assert!(help.contains("AerodromeSlipstream"));
        assert!(!help.contains("SushiSwapV2"));
        assert!(help.contains("RPC_HTTP_URL"));
        assert!(help.contains("needs_bootstrap"));
        // Help is rendered as plain text, so doc-markdown backticks must not survive.
        assert!(!help.contains("`UniswapV3`"));
        assert!(!help.contains("`PancakeSwapV3`"));
        assert!(!help.contains("`RPC_HTTP_URL`"));
        assert!(!help.contains("`needs_bootstrap`"));
    }

    #[rstest]
    fn blockchain_sync_dex_help_lists_discoverable_dexes() {
        let mut command = crate::cli_command();
        let help = command
            .find_subcommand_mut("blockchain")
            .and_then(|command| command.find_subcommand_mut("sync-dex"))
            .map(|command| command.render_long_help().to_string())
            .unwrap();

        // sync-dex receives the discovery block, not the snapshot block.
        assert!(help.contains("Discoverable DEXes"));
        assert!(!help.contains("Snapshot-capable"));
        // UniswapV2 is discovery-only, so it appears here but never in the snapshot listing.
        assert!(help.contains("UniswapV2"));
    }
}
