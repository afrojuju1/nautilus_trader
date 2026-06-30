//! Operator command implementations for the Alpaca adapter.

use std::{env, sync::OnceLock};

mod candidate_alerts;
mod check_account_orders;
mod fleet_status;
mod historical_replay;
mod load_option_contracts;
mod load_option_snapshots;
mod operator_status;
mod performance_report;
mod reconciliation_probe;
mod spread_reconciliation_preview;
mod sync_strategy_state;
mod watch_trade_updates;

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
    /// Loads option contracts through the Alpaca data API.
    OptionContracts,
    /// Loads option snapshots through the Alpaca data API.
    OptionSnapshots,
    /// Probes REST-backed order/fill/position reconciliation.
    Reconciliation,
    /// Probes trade-update WebSocket authorization and listening.
    TradeUpdates,
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
        AlpacaOperatorCommand::OptionContracts => load_option_contracts::run().await,
        AlpacaOperatorCommand::OptionSnapshots => load_option_snapshots::run().await,
        AlpacaOperatorCommand::Reconciliation => reconciliation_probe::run().await,
        AlpacaOperatorCommand::TradeUpdates => watch_trade_updates::run().await,
    }
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
