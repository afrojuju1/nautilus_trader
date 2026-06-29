//! Transitional operator CLI for Alpaca options runtime operations.

use std::env;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nautilus_alpaca::operator::run_legacy_args(env::args().skip(1).collect()).await
}
