use nautilus_alpaca::operator::{self, AlpacaOperatorCommand};

use crate::opt::{AlpacaAdapterCommand, AlpacaAdapterOpt, AlpacaAlertsCommand, ForwardedArgs};

/// Executes Alpaca adapter operational commands.
///
/// # Errors
///
/// Returns an error if the requested Alpaca command fails.
pub(crate) async fn run_alpaca_command(opt: AlpacaAdapterOpt) -> anyhow::Result<()> {
    match opt.command {
        AlpacaAdapterCommand::Status(args) => {
            run_operator_command(AlpacaOperatorCommand::Status, args).await?
        }
        AlpacaAdapterCommand::Account(args) => {
            run_operator_command(AlpacaOperatorCommand::Account, args).await?
        }
        AlpacaAdapterCommand::Fleet(args) => {
            run_operator_command(AlpacaOperatorCommand::Fleet, args).await?
        }
        AlpacaAdapterCommand::Alerts(alerts) => match alerts.command {
            AlpacaAlertsCommand::Candidates(args) => {
                run_operator_command(AlpacaOperatorCommand::CandidateAlerts, args).await?
            }
        },
        AlpacaAdapterCommand::Performance(args) => {
            run_operator_command(AlpacaOperatorCommand::Performance, args).await?
        }
        AlpacaAdapterCommand::Replay(args) => {
            run_operator_command(AlpacaOperatorCommand::Replay, args).await?
        }
        AlpacaAdapterCommand::SyncState(args) => {
            run_operator_command(AlpacaOperatorCommand::SyncState, args).await?
        }
        AlpacaAdapterCommand::OptionContracts(args) => {
            run_operator_command(AlpacaOperatorCommand::OptionContracts, args).await?
        }
        AlpacaAdapterCommand::OptionSnapshots(args) => {
            run_operator_command(AlpacaOperatorCommand::OptionSnapshots, args).await?
        }
        AlpacaAdapterCommand::Reconciliation(args) => {
            run_operator_command(AlpacaOperatorCommand::Reconciliation, args).await?
        }
        AlpacaAdapterCommand::TradeUpdates(args) => {
            run_operator_command(AlpacaOperatorCommand::TradeUpdates, args).await?
        }
    }
    Ok(())
}

async fn run_operator_command(
    command: AlpacaOperatorCommand,
    args: ForwardedArgs,
) -> anyhow::Result<()> {
    operator::run_command(command, args.args).await
}
