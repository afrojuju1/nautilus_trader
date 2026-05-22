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

//! Syncs Postgres-backed strategy state to fleet local state files.

use std::env;

use nautilus_alpaca::{
    fleet::load_fleet_config_from_env,
    runtime::{StrategyState, save_strategy_state_atomic},
    storage::{STORAGE_SCHEMA_DEFAULT, StorageRepository, load_strategy_state_record},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let database_url = env::var("ALPACA_STORAGE_DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("ALPACA_STORAGE_DATABASE_URL is required"))?;
    let schema =
        env::var("ALPACA_STORAGE_SCHEMA").unwrap_or_else(|_| STORAGE_SCHEMA_DEFAULT.to_string());
    let storage = StorageRepository::connect_with_schema(&database_url, &schema).await?;
    let fleet =
        load_fleet_config_from_env()?.ok_or_else(|| anyhow::anyhow!("fleet config is required"))?;

    let mut synced = 0_usize;
    let mut missing_records = 0_usize;
    let mut missing_paths = 0_usize;
    for account in fleet.enabled_accounts() {
        let Some(path) = fleet.state_path(account) else {
            missing_paths += 1;
            println!(
                "sync_state: skipped account_id={} reason=missing_state_path",
                account.id
            );
            continue;
        };
        let Some(state) = load_strategy_state_record(&storage, &account.id).await? else {
            missing_records += 1;
            println!(
                "sync_state: skipped account_id={} reason=missing_storage_record path={}",
                account.id,
                path.display(),
            );
            continue;
        };
        let active = active_entry_count(&state);
        save_strategy_state_atomic(&path, &state)?;
        synced += 1;
        println!(
            "sync_state: account_id={} entries={} active={} path={}",
            account.id,
            state.entries.len(),
            active,
            path.display(),
        );
    }

    println!(
        "sync_state: complete synced={} missing_records={} missing_paths={}",
        synced, missing_records, missing_paths,
    );
    Ok(())
}

fn active_entry_count(state: &StrategyState) -> usize {
    state
        .entries
        .iter()
        .filter(|entry| entry.is_active())
        .count()
}
