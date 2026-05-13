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

//! Native Nautilus entrypoint for the Alpaca options-engine account engine.

use std::env;

use nautilus_alpaca::options_runtime::OptionsEngineConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("usage: alpaca-options-engine [--check-config] [UNDERLYING[,UNDERLYING]...]");
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--check-config") {
        let config = OptionsEngineConfig::from_runtime_env()?;
        println!(
            "alpaca_options_engine_config: underlyings={} strategies={} dry_run_strategies={} submit_enabled={} manage_enabled={} close_enabled={} kill_switch={} quantity={} max_active_entries={} max_daily_submits={} max_open_orders={} max_active_entries_per_underlying={} max_active_entries_per_sector={} fleet_account={} fleet_policy_blocks={} stale_close_secs={} close_regular_hours_only={} close_window={}-{} close_price_cushion={:.2} max_close_attempts={} close_reprice_cooldown_secs={} max_iterations={} interval_secs={} state_path={} candidate_ledger_enabled={} candidate_ledger_max_candidates={}",
            config.underlyings.join(","),
            config.enabled_strategy_names().join(","),
            config.dry_run_strategy_names().join(","),
            config.submit_enabled,
            config.manage_enabled,
            config.close_enabled,
            config.kill_switch,
            config.quantity,
            format_limit(config.max_active_entries),
            format_limit(config.max_daily_submits),
            format_limit(config.max_open_orders),
            format_limit(config.max_active_entries_per_underlying),
            format_limit(config.max_active_entries_per_sector),
            config.fleet_account_id.as_deref().unwrap_or("none"),
            if config.fleet_policy_blocks.is_empty() {
                "none".to_string()
            } else {
                config.fleet_policy_blocks.join(",")
            },
            config.stale_close_secs,
            config.close_regular_hours_only,
            config.close_start,
            config.close_end,
            config.close_price_cushion,
            config.max_close_attempts,
            config.close_reprice_cooldown_secs,
            config.max_iterations,
            config.interval_secs,
            config.state_path.display(),
            config.candidate_ledger_enabled,
            config.candidate_ledger_max_candidates,
        );
        return Ok(());
    }
    nautilus_alpaca::options_engine::run_options_engine().await
}

fn format_limit(limit: Option<usize>) -> String {
    limit.map_or_else(|| "unlimited".to_string(), |value| value.to_string())
}
