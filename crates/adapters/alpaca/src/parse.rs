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

use chrono::{DateTime, Datelike, TimeZone, Utc};
use chrono_tz::America::New_York;
use nautilus_core::Params;
use nautilus_model::{
    enums::{AssetClass, OptionKind},
    identifiers::{InstrumentId, OptionSeriesId, Symbol, Venue},
    instruments::OptionContract,
    types::{Currency, Price, Quantity},
};
use serde_json::json;
use ustr::Ustr;

use crate::{
    common::consts::{ALPACA_OPEN_INTEREST_INFO_KEY, ALPACA_VENUE},
    http::{
        error::{Error, Result},
        models::{AlpacaOptionContract, AlpacaOptionType},
    },
};

const ALPACA_EQUITY_OPTION_EXPIRATION_HOUR_ET: u32 = 16;
const ALPACA_EQUITY_OPTION_EXPIRATION_MINUTE_ET: u32 = 15;

/// Parsed Alpaca OCC-style option symbol fields.
#[derive(Clone, Debug, PartialEq)]
pub struct AlpacaOptionSymbolParts {
    /// Original symbol string.
    pub symbol: String,
    /// Symbol without an optional `O:` prefix.
    pub canonical_symbol: String,
    /// Underlying root symbol.
    pub underlying_symbol: String,
    /// Contract expiration date as `YYYY-MM-DD`.
    pub expiration_date: String,
    /// Contract option side.
    pub option_type: AlpacaOptionType,
    /// Strike price decoded from OCC thousandths.
    pub strike: f64,
}

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
    let expiration_ns = parse_option_expiration_ns(&contract.expiration_date)?;
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

/// Converts an Alpaca option expiration value to the internal expiration timestamp.
///
/// Alpaca option contracts currently expose expiration as a date (`YYYY-MM-DD`). Internally
/// Nautilus option-chain subscriptions use an exact nanosecond timestamp. If Alpaca returns an
/// RFC3339 timestamp, that timestamp is preserved. Date-only values are normalized to the final
/// regular US options close in `America/New_York` so ETF/index underlyings with 4:15 PM ET closes
/// do not expire early.
///
/// # Errors
///
/// Returns an error if the date is invalid or cannot be represented as a positive UNIX timestamp.
pub fn parse_option_expiration_ns(value: &str) -> Result<u64> {
    if let Ok(expiration) = DateTime::parse_from_rfc3339(value) {
        return timestamp_nanos(value, expiration.with_timezone(&Utc));
    }

    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|e| Error::Parse(format!("invalid expiration date `{value}`: {e}")))?;
    let expiration = New_York
        .with_ymd_and_hms(
            date.year(),
            date.month(),
            date.day(),
            ALPACA_EQUITY_OPTION_EXPIRATION_HOUR_ET,
            ALPACA_EQUITY_OPTION_EXPIRATION_MINUTE_ET,
            0,
        )
        .single()
        .ok_or_else(|| {
            Error::Parse(format!(
                "invalid expiration time `{value}` for America/New_York"
            ))
        })?
        .with_timezone(&Utc);
    timestamp_nanos(value, expiration)
}

fn timestamp_nanos(value: &str, expiration: DateTime<Utc>) -> Result<u64> {
    let timestamp = expiration.timestamp_nanos_opt().ok_or_else(|| {
        Error::Parse(format!(
            "expiration date `{value}` produced out-of-range timestamp",
        ))
    })?;

    if timestamp < 0 {
        return Err(Error::Parse(format!(
            "expiration date `{value}` produced negative timestamp",
        )));
    }

    Ok(timestamp as u64)
}

/// Removes the optional `O:` prefix from an Alpaca option symbol.
#[must_use]
pub fn canonical_alpaca_option_symbol(symbol: &str) -> String {
    symbol
        .trim()
        .strip_prefix("O:")
        .unwrap_or(symbol.trim())
        .to_string()
}

/// Parses an Alpaca OCC-style option symbol.
///
/// # Errors
///
/// Returns an error when the symbol is missing an underlying root, expiration, side, or strike.
pub fn parse_alpaca_option_symbol(symbol: &str) -> Result<AlpacaOptionSymbolParts> {
    let symbol = symbol.trim();
    let canonical_symbol = canonical_alpaca_option_symbol(symbol);
    let first_digit = canonical_symbol
        .find(|ch: char| ch.is_ascii_digit())
        .ok_or_else(|| {
            Error::Parse(format!(
                "invalid Alpaca option symbol `{symbol}`: missing expiration"
            ))
        })?;

    let (underlying_symbol, option_details) = canonical_symbol.split_at(first_digit);
    let underlying_symbol = underlying_symbol.to_string();
    if underlying_symbol.is_empty() {
        return Err(Error::Parse(format!(
            "invalid Alpaca option symbol `{symbol}`: missing underlying"
        )));
    }

    if option_details.len() < 15 {
        return Err(Error::Parse(format!(
            "invalid Alpaca option symbol `{symbol}`: expected YYMMDD, option side, and strike"
        )));
    }

    let expiration = &option_details[..6];
    let side = option_details.as_bytes()[6] as char;
    let strike_raw = &option_details[7..];
    if !expiration.chars().all(|ch| ch.is_ascii_digit())
        || strike_raw.is_empty()
        || !strike_raw.chars().all(|ch| ch.is_ascii_digit())
    {
        return Err(Error::Parse(format!(
            "invalid Alpaca option symbol `{symbol}`: malformed expiration or strike"
        )));
    }

    let option_type = match side {
        'C' => AlpacaOptionType::Call,
        'P' => AlpacaOptionType::Put,
        _ => {
            return Err(Error::Parse(format!(
                "invalid Alpaca option symbol `{symbol}`: expected C or P option side"
            )));
        }
    };
    let strike = strike_raw.parse::<u64>().map_err(|error| {
        Error::Parse(format!(
            "invalid Alpaca option symbol `{symbol}` strike `{strike_raw}`: {error}"
        ))
    })? as f64
        / 1_000.0;
    let expiration_date = format!(
        "20{}-{}-{}",
        &expiration[..2],
        &expiration[2..4],
        &expiration[4..6]
    );

    Ok(AlpacaOptionSymbolParts {
        symbol: symbol.to_string(),
        canonical_symbol,
        underlying_symbol,
        expiration_date,
        option_type,
        strike,
    })
}

/// Parses an Alpaca option instrument ID.
///
/// # Errors
///
/// Returns an error when the venue is not Alpaca or the symbol is malformed.
pub fn parse_alpaca_option_instrument_id(
    instrument_id: InstrumentId,
) -> Result<AlpacaOptionSymbolParts> {
    if instrument_id.venue != Venue::new(ALPACA_VENUE) {
        return Err(Error::Parse(format!(
            "expected Alpaca venue {}, got {}",
            ALPACA_VENUE, instrument_id.venue
        )));
    }
    parse_alpaca_option_symbol(instrument_id.symbol.as_str())
}

/// Creates an Alpaca option [`OptionSeriesId`] from an underlying and Alpaca expiration date.
///
/// # Errors
///
/// Returns an error if the Alpaca expiration date cannot be converted to an internal timestamp.
pub fn parse_option_series_id(
    underlying: &str,
    settlement_currency: &str,
    expiration_date: &str,
) -> Result<OptionSeriesId> {
    Ok(OptionSeriesId::new(
        Venue::new(ALPACA_VENUE),
        Ustr::from(underlying),
        Ustr::from(settlement_currency),
        parse_option_expiration_ns(expiration_date)?.into(),
    ))
}

fn parse_option_kind(value: &str) -> Result<OptionKind> {
    match value.to_ascii_lowercase().as_str() {
        "call" => Ok(OptionKind::Call),
        "put" => Ok(OptionKind::Put),
        other => Err(Error::Parse(format!("unsupported option type `{other}`"))),
    }
}
