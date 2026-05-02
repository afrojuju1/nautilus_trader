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

//! Factory functions for creating Alpaca clients.

use std::any::Any;

use nautilus_common::{
    cache::CacheView,
    clients::ExecutionClient,
    factories::{ClientConfig, ExecutionClientFactory},
};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType},
    identifiers::{AccountId, ClientId, TraderId, Venue},
};

use crate::{
    common::consts::{ALPACA_CLIENT_ID, ALPACA_VENUE},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    execution::AlpacaExecutionClient,
};

impl ClientConfig for AlpacaDataClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ClientConfig for AlpacaExecClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Factory for creating Alpaca execution clients.
#[derive(Debug, Clone)]
pub struct AlpacaExecutionClientFactory {
    trader_id: TraderId,
    account_id: AccountId,
}

impl AlpacaExecutionClientFactory {
    /// Creates a new [`AlpacaExecutionClientFactory`] instance.
    #[must_use]
    pub const fn new(trader_id: TraderId, account_id: AccountId) -> Self {
        Self {
            trader_id,
            account_id,
        }
    }
}

impl ExecutionClientFactory for AlpacaExecutionClientFactory {
    fn create(
        &self,
        name: &str,
        config: &dyn ClientConfig,
        cache: CacheView,
    ) -> anyhow::Result<Box<dyn ExecutionClient>> {
        let alpaca_config = config
            .as_any()
            .downcast_ref::<AlpacaExecClientConfig>()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid config type for AlpacaExecutionClientFactory. Expected AlpacaExecClientConfig, was {config:?}",
                )
            })?
            .clone();

        let core = ExecutionClientCore::new(
            self.trader_id,
            ClientId::from(name),
            Venue::new(ALPACA_VENUE),
            OmsType::Netting,
            self.account_id,
            AccountType::Margin,
            None,
            cache,
        );

        Ok(Box::new(AlpacaExecutionClient::new(core, alpaca_config)?))
    }

    fn name(&self) -> &'static str {
        ALPACA_CLIENT_ID
    }

    fn config_type(&self) -> &'static str {
        "AlpacaExecClientConfig"
    }
}

#[cfg(test)]
mod tests {
    use nautilus_common::factories::ClientConfig;

    use super::*;

    #[test]
    fn alpaca_exec_client_config_implements_client_config() {
        let config = AlpacaExecClientConfig::default();
        let boxed_config: Box<dyn ClientConfig> = Box::new(config);

        assert!(
            boxed_config
                .as_any()
                .downcast_ref::<AlpacaExecClientConfig>()
                .is_some()
        );
    }
}
