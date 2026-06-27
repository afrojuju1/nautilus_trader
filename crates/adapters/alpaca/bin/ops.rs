//! Unified operator CLI for Alpaca options runtime operations.

use std::{env, sync::OnceLock};

#[path = "candidate_alerts.rs"]
mod candidate_alerts;
#[path = "check_account_orders.rs"]
mod check_account_orders;
#[path = "fleet_status.rs"]
mod fleet_status;
#[path = "historical_replay.rs"]
mod historical_replay;
#[path = "operator_status.rs"]
mod operator_status;
#[path = "performance_report.rs"]
mod performance_report;
#[path = "sync_strategy_state.rs"]
mod sync_strategy_state;

static ARG_OFFSET: OnceLock<usize> = OnceLock::new();

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (command, arg_offset) = command()?;
    match command {
        Command::Status => {
            set_arg_offset(arg_offset)?;
            operator_status::run().await
        }
        Command::Account => {
            set_arg_offset(arg_offset)?;
            check_account_orders::run().await
        }
        Command::Fleet => {
            set_arg_offset(arg_offset)?;
            fleet_status::run().await
        }
        Command::CandidateAlerts => {
            set_arg_offset(arg_offset)?;
            candidate_alerts::run().await
        }
        Command::Performance => {
            set_arg_offset(arg_offset)?;
            performance_report::run().await
        }
        Command::Replay => {
            set_arg_offset(arg_offset)?;
            historical_replay::run().await
        }
        Command::SyncState => {
            set_arg_offset(arg_offset)?;
            sync_strategy_state::run().await
        }
        Command::Help => {
            print_usage();
            Ok(())
        }
    }
}

pub(crate) fn ops_args() -> Vec<String> {
    let offset = ARG_OFFSET.get().copied().unwrap_or(1);
    env::args().skip(offset).collect()
}

fn set_arg_offset(offset: usize) -> anyhow::Result<()> {
    ARG_OFFSET
        .set(offset)
        .map_err(|_| anyhow::anyhow!("operator arg offset was already initialized"))
}

fn command() -> anyhow::Result<(Command, usize)> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let Some(first) = args.first().map(String::as_str) else {
        return Ok((Command::Status, 1));
    };
    match first {
        "status" => Ok((Command::Status, 2)),
        "account" | "check-account" | "check-account-orders" => Ok((Command::Account, 2)),
        "fleet" => Ok((Command::Fleet, 2)),
        "alerts" if args.get(1).is_some_and(|value| value == "candidates") => {
            Ok((Command::CandidateAlerts, 3))
        }
        "candidate-alerts" => Ok((Command::CandidateAlerts, 2)),
        "performance" => Ok((Command::Performance, 2)),
        "replay" | "historical-replay" => Ok((Command::Replay, 2)),
        "sync-state" => Ok((Command::SyncState, 2)),
        "--help" | "-h" | "help" => Ok((Command::Help, 2)),
        other => anyhow::bail!("unsupported alpaca-ops command `{other}`"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Status,
    Account,
    Fleet,
    CandidateAlerts,
    Performance,
    Replay,
    SyncState,
    Help,
}

fn print_usage() {
    println!(
        "usage: alpaca-ops <command> [args]\n\
         commands:\n\
           status [--json]\n\
           account\n\
           fleet [--json] [--include-disabled] [--registry PATH]\n\
           alerts candidates [--send|--dry-run] [--date YYYY-MM-DD] [--lookback-minutes N]\n\
           performance [--json] [--send-discord] [--since YYYY-MM-DD] [--until YYYY-MM-DD]\n\
           replay [--json] [--include-records] [--since YYYY-MM-DD] [--until YYYY-MM-DD] [--max-candidates N] [--max-rank N] [--lookahead-minutes N] [--timeframe 1Min]\n\
           sync-state"
    );
}
