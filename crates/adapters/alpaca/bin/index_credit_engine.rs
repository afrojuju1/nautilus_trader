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

//! Native Nautilus entrypoint for the Alpaca index-credit account engine.

use std::env;

use nautilus_alpaca::index_credit::IndexCreditConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("usage: alpaca-index-credit-engine [--check-config] [UNDERLYING[,UNDERLYING]...]");
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--check-config") {
        let config = IndexCreditConfig::from_runtime_env()?;
        println!(
            "alpaca_index_credit_config: underlyings={} strategies={} submit_enabled={} manage_enabled={} close_enabled={} kill_switch={} quantity={} max_iterations={} interval_secs={} state_path={}",
            config.underlyings.join(","),
            config.enabled_strategy_names().join(","),
            config.submit_enabled,
            config.manage_enabled,
            config.close_enabled,
            config.kill_switch,
            config.quantity,
            config.max_iterations,
            config.interval_secs,
            config.state_path.display(),
        );
        return Ok(());
    }
    nautilus_alpaca::index_credit_engine::run_index_credit_engine().await
}
