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

//! Alpaca instrument provider primitives.

use nautilus_model::instruments::OptionContract;

use crate::{
    http::{
        client::AlpacaHttpClient,
        error::Result,
        models::{AlpacaOptionContract, AlpacaOptionType, ListOptionContractsRequest},
    },
    parse::parse_option_contract,
};

/// Loads Alpaca option contracts from the Trading API.
#[derive(Clone, Debug)]
pub struct AlpacaOptionContractProvider {
    client: AlpacaHttpClient,
}

impl AlpacaOptionContractProvider {
    /// Creates a new option contract provider.
    #[must_use]
    pub fn new(client: AlpacaHttpClient) -> Self {
        Self { client }
    }

    /// Loads active option contracts for an underlying and expiration window.
    ///
    /// # Errors
    ///
    /// Returns an error if Alpaca rejects the request or the response cannot be decoded.
    pub async fn load_active_contracts(
        &self,
        underlying_symbol: impl Into<String>,
        min_expiration: impl Into<String>,
        max_expiration: impl Into<String>,
        option_type: Option<AlpacaOptionType>,
    ) -> Result<Vec<AlpacaOptionContract>> {
        let mut request = ListOptionContractsRequest::active([underlying_symbol.into()]);
        request.expiration_date_gte = Some(min_expiration.into());
        request.expiration_date_lte = Some(max_expiration.into());
        request.option_type = option_type;

        Ok(self
            .client
            .list_option_contracts(&request)
            .await?
            .option_contracts)
    }

    /// Loads active option contracts and converts them to Nautilus instruments.
    ///
    /// # Errors
    ///
    /// Returns an error if Alpaca rejects the request, the response cannot be decoded, or a
    /// contract cannot be converted into a Nautilus [`OptionContract`].
    pub async fn load_active_instruments(
        &self,
        underlying_symbol: impl Into<String>,
        min_expiration: impl Into<String>,
        max_expiration: impl Into<String>,
        option_type: Option<AlpacaOptionType>,
    ) -> Result<Vec<OptionContract>> {
        self.load_active_contracts(
            underlying_symbol,
            min_expiration,
            max_expiration,
            option_type,
        )
        .await?
        .iter()
        .map(parse_option_contract)
        .collect()
    }
}
