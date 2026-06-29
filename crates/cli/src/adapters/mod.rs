#[cfg(feature = "alpaca")]
mod alpaca;

use crate::opt::{AdapterCommand, AdaptersOpt};

/// Executes adapter-specific operational commands.
///
/// # Errors
///
/// Returns an error if the requested adapter command fails.
pub(crate) async fn run_adapters_command(opt: AdaptersOpt) -> anyhow::Result<()> {
    match opt.command {
        AdapterCommand::Alpaca(alpaca_opt) => alpaca::run_alpaca_command(alpaca_opt).await?,
    }
    Ok(())
}
