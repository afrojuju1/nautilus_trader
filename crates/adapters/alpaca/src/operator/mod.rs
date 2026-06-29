//! Operator command implementations for the Alpaca adapter.

use std::{env, sync::OnceLock};

mod candidate_alerts;
mod check_account_orders;
mod fleet_status;
mod historical_replay;
mod operator_status;
mod performance_report;
mod sync_strategy_state;

static OPERATOR_ARGS: OnceLock<Vec<String>> = OnceLock::new();

/// Alpaca adapter operator commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlpacaOperatorCommand {
    /// Prints supervised runtime status.
    Status,
    /// Checks the broker account, positions, and open orders.
    Account,
    /// Summarizes the configured Alpaca account fleet.
    Fleet,
    /// Emits candidate alerts from the operational store.
    CandidateAlerts,
    /// Builds performance reports from broker fills and operational ledgers.
    Performance,
    /// Replays candidate evidence against historical Alpaca option bars.
    Replay,
    /// Syncs operational strategy state to configured local state files.
    SyncState,
}

/// Runs an Alpaca adapter operator command with command-local arguments.
///
/// # Errors
///
/// Returns an error if the command fails or if the process has already initialized command args.
pub async fn run_command(command: AlpacaOperatorCommand, args: Vec<String>) -> anyhow::Result<()> {
    set_args(args)?;
    match command {
        AlpacaOperatorCommand::Status => operator_status::run().await,
        AlpacaOperatorCommand::Account => check_account_orders::run().await,
        AlpacaOperatorCommand::Fleet => fleet_status::run().await,
        AlpacaOperatorCommand::CandidateAlerts => candidate_alerts::run().await,
        AlpacaOperatorCommand::Performance => performance_report::run().await,
        AlpacaOperatorCommand::Replay => historical_replay::run().await,
        AlpacaOperatorCommand::SyncState => sync_strategy_state::run().await,
    }
}

/// Parses and runs the transitional `alpaca-ops` command-line shape.
///
/// # Errors
///
/// Returns an error if the command is unsupported or the selected command fails.
pub async fn run_legacy_args(args: Vec<String>) -> anyhow::Result<()> {
    let Some(first) = args.first().map(String::as_str) else {
        return run_command(AlpacaOperatorCommand::Status, Vec::new()).await;
    };
    let (command, offset) = match first {
        "status" => (AlpacaOperatorCommand::Status, 1),
        "account" | "check-account" | "check-account-orders" => (AlpacaOperatorCommand::Account, 1),
        "fleet" => (AlpacaOperatorCommand::Fleet, 1),
        "alerts" if args.get(1).is_some_and(|value| value == "candidates") => {
            (AlpacaOperatorCommand::CandidateAlerts, 2)
        }
        "candidate-alerts" => (AlpacaOperatorCommand::CandidateAlerts, 1),
        "performance" => (AlpacaOperatorCommand::Performance, 1),
        "replay" | "historical-replay" => (AlpacaOperatorCommand::Replay, 1),
        "sync-state" => (AlpacaOperatorCommand::SyncState, 1),
        "--help" | "-h" | "help" => {
            print_legacy_usage();
            return Ok(());
        }
        other => anyhow::bail!("unsupported alpaca-ops command `{other}`"),
    };
    run_command(command, args.into_iter().skip(offset).collect()).await
}

pub(crate) fn args() -> Vec<String> {
    OPERATOR_ARGS
        .get()
        .cloned()
        .unwrap_or_else(|| env::args().skip(1).collect())
}

fn set_args(args: Vec<String>) -> anyhow::Result<()> {
    OPERATOR_ARGS
        .set(args)
        .map_err(|_| anyhow::anyhow!("operator args were already initialized"))
}

fn print_legacy_usage() {
    println!(
        "usage: alpaca-ops <command> [args]\n\
         commands:\n\
           status [--json]\n\
           account\n\
           fleet [--json] [--include-disabled] [--registry PATH]\n\
           alerts candidates [--send|--dry-run] [--date YYYY-MM-DD] [--lookback-minutes N]\n\
           performance [--json] [--send-discord] [--since YYYY-MM-DD] [--until YYYY-MM-DD] [--no-track-candidate-outcomes]\n\
           replay [--json] [--include-records] [--since YYYY-MM-DD] [--until YYYY-MM-DD] [--max-candidates N] [--max-rank N] [--lookahead-minutes N] [--timeframe 1Min]\n\
           sync-state"
    );
}
