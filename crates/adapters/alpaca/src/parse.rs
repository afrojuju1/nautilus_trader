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

//! Conversion helpers from Alpaca wire models to Nautilus model objects.

use nautilus_core::Params;
use nautilus_model::{
    enums::{AssetClass, OptionKind},
    identifiers::{InstrumentId, Symbol},
    instruments::OptionContract,
    types::{Currency, Price, Quantity},
};
use serde_json::json;
use time::{Date, macros::format_description};

use crate::{
    common::consts::{ALPACA_OPEN_INTEREST_INFO_KEY, ALPACA_VENUE},
    http::{
        error::{Error, Result},
        models::AlpacaOptionContract,
    },
};

/// Converts an Alpaca option contract into a Nautilus [`OptionContract`].
///
/// # Errors
///
/// Returns an error if required contract fields are invalid.
pub fn parse_option_contract(contract: &AlpacaOptionContract) -> Result<OptionContract> {
    let instrument_id =
        InstrumentId::from(format!("{}.{}", contract.symbol, ALPACA_VENUE).as_str());
    let raw_symbol = Symbol::from(contract.symbol.as_str());
    let option_kind = parse_option_kind(&contract.option_type)?;
    let strike_price = Price::from(contract.strike_price.as_str());
    let expiration_ns = parse_expiration_date(&contract.expiration_date)?;
    let multiplier = Quantity::from(contract.size.as_deref().unwrap_or("100"));
    let info = contract.open_interest.as_ref().and_then(|value| {
        value.parse::<f64>().ok().map(|open_interest| {
            let mut params = Params::new();
            params.insert(
                ALPACA_OPEN_INTEREST_INFO_KEY.to_string(),
                json!(open_interest),
            );
            params
        })
    });

    OptionContract::new_checked(
        instrument_id,
        raw_symbol,
        AssetClass::Equity,
        None,
        contract.underlying_symbol.as_str().into(),
        option_kind,
        strike_price,
        Currency::USD(),
        0.into(),
        expiration_ns.into(),
        2,
        Price::from("0.01"),
        multiplier,
        Quantity::from(1),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        info,
        0.into(),
        0.into(),
    )
    .map_err(|e| Error::Parse(format!("invalid option contract {}: {e}", contract.symbol)))
}

fn parse_option_kind(value: &str) -> Result<OptionKind> {
    match value.to_ascii_lowercase().as_str() {
        "call" => Ok(OptionKind::Call),
        "put" => Ok(OptionKind::Put),
        other => Err(Error::Parse(format!("unsupported option type `{other}`"))),
    }
}

fn parse_expiration_date(value: &str) -> Result<u64> {
    let format = format_description!("[year]-[month]-[day]");
    let date = Date::parse(value, &format)
        .map_err(|e| Error::Parse(format!("invalid expiration date `{value}`: {e}")))?;
    let timestamp = date
        .with_hms(0, 0, 0)
        .map_err(|e| Error::Parse(format!("invalid expiration time `{value}`: {e}")))?
        .assume_utc()
        .unix_timestamp_nanos();

    if timestamp < 0 {
        return Err(Error::Parse(format!(
            "expiration date `{value}` produced negative timestamp",
        )));
    }

    Ok(timestamp as u64)
}
