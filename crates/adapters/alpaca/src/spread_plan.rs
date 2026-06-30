//! Nautilus-native option spread planning for Alpaca candidates.

use std::str::FromStr;

use nautilus_core::{Params, UnixNanos};
use nautilus_model::{
    enums::AssetClass,
    identifiers::{InstrumentId, Symbol, Venue},
    instruments::{
        Instrument, InstrumentAny, OptionSpread, SpreadLeg, generic_spread_id, generic_spread_legs,
    },
    types::{Currency, Price, Quantity},
};
use nautilus_trading::options::{
    candidates::ScoredContract,
    entries::{EntryPremiumKind, SelectedOptionsEntry},
};
use serde_json::json;
use ustr::Ustr;

use crate::{
    common::consts::ALPACA_VENUE, parse::parse_option_expiration_ns, runtime::StrategyStateEntry,
};

/// DataEngine param which enables native spread quote aggregation.
pub const AGGREGATE_SPREAD_QUOTES_PARAM: &str = "aggregate_spread_quotes";
/// DataEngine param which disables vega-based spread quote pricing.
pub const DISABLE_VEGA_PRICING_PARAM: &str = "disable_vega_pricing";
/// DataEngine param for the temporary vega-pricing fallback window.
pub const VEGA_PRICING_TIMEOUT_SECONDS_PARAM: &str = "vega_pricing_timeout_seconds";

/// A planned option spread leg using Nautilus generic spread sign conventions.
#[derive(Clone, Debug)]
pub struct OptionSpreadLegPlan {
    /// Alpaca option contract instrument ID.
    pub instrument_id: InstrumentId,
    /// Alpaca option contract symbol.
    pub symbol: String,
    /// Signed spread ratio. Positive legs are bought when buying the spread.
    pub ratio: i64,
}

impl OptionSpreadLegPlan {
    fn from_spread_leg(leg: SpreadLeg) -> Self {
        Self {
            instrument_id: leg.instrument_id,
            symbol: leg.instrument_id.symbol.to_string(),
            ratio: leg.ratio,
        }
    }

    fn to_spread_leg(&self) -> SpreadLeg {
        SpreadLeg::new(self.instrument_id, self.ratio)
    }
}

/// Nautilus-native option spread plan derived from a selected options candidate.
#[derive(Clone, Debug)]
pub struct OptionSpreadPlan {
    /// Nautilus option spread instrument ID.
    pub instrument_id: InstrumentId,
    /// Nautilus generic spread symbol.
    pub raw_symbol: Symbol,
    /// Spread legs in canonical package order.
    pub legs: Vec<OptionSpreadLegPlan>,
    /// Cached spread instrument to register before subscribing or ordering.
    pub instrument: InstrumentAny,
    /// Stable strategy name.
    pub strategy: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Candidate premium kind.
    pub scanner_premium_kind: EntryPremiumKind,
    /// Candidate scanner premium per spread.
    pub scanner_premium: f64,
}

/// Builds a spread-quote subscription params map for Alpaca candidate spreads.
#[must_use]
pub fn spread_quote_subscription_params() -> Params {
    let mut params = Params::new();
    params.insert(AGGREGATE_SPREAD_QUOTES_PARAM.to_string(), json!(true));
    params.insert(DISABLE_VEGA_PRICING_PARAM.to_string(), json!(true));
    params.insert(
        VEGA_PRICING_TIMEOUT_SECONDS_PARAM.to_string(),
        json!(60_u64),
    );
    params
}

/// Parses a Nautilus generic option spread instrument ID into signed Alpaca option legs.
///
/// # Errors
///
/// Returns an error when the spread symbol is not in Nautilus generic spread syntax.
pub fn option_spread_legs_from_instrument_id(
    instrument_id: InstrumentId,
) -> anyhow::Result<Vec<OptionSpreadLegPlan>> {
    generic_spread_legs(instrument_id)
        .map(|legs| {
            legs.into_iter()
                .map(OptionSpreadLegPlan::from_spread_leg)
                .collect()
        })
        .map_err(Into::into)
}

/// Builds a Nautilus-native option spread plan for a selected candidate.
///
/// Returns `Ok(None)` for candidate families which are not option spreads.
///
/// # Errors
///
/// Returns an error if candidate leg expiration metadata cannot be parsed.
pub fn selected_entry_spread_plan(
    entry: &SelectedOptionsEntry,
    ts_init: UnixNanos,
) -> anyhow::Result<Option<OptionSpreadPlan>> {
    let descriptor = entry.descriptor();
    let mut legs = Vec::new();

    match entry {
        SelectedOptionsEntry::Credit(entry) => {
            push_leg(&mut legs, &entry.candidate.long, 1);
            push_leg(&mut legs, &entry.candidate.short, -1);
        }
        SelectedOptionsEntry::IronCondor(entry) => {
            push_leg(&mut legs, &entry.candidate.put.long, 1);
            push_leg(&mut legs, &entry.candidate.put.short, -1);
            push_leg(&mut legs, &entry.candidate.call.long, 1);
            push_leg(&mut legs, &entry.candidate.call.short, -1);
        }
        SelectedOptionsEntry::Debit(entry) => {
            push_leg(&mut legs, &entry.candidate.long, 1);
            push_leg(&mut legs, &entry.candidate.short, -1);
        }
        SelectedOptionsEntry::NakedOption(_) => return Ok(None),
    }

    option_spread_plan(
        legs,
        ts_init,
        descriptor.underlying,
        descriptor.strategy.to_string(),
        descriptor.premium_kind,
        descriptor.premium,
    )
    .map(Some)
}

/// Rebuilds a Nautilus-native spread plan from persisted strategy state when possible.
///
/// Returns `Ok(None)` for naked options or entries without enough persisted spread legs.
///
/// # Errors
///
/// Returns an error if persisted leg expiration metadata cannot be parsed.
pub fn strategy_state_spread_plan(
    entry: &StrategyStateEntry,
    ts_init: UnixNanos,
) -> anyhow::Result<Option<OptionSpreadPlan>> {
    if let Some(plan) = strategy_state_persisted_spread_plan(entry, ts_init)? {
        return Ok(Some(plan));
    }

    if entry.is_naked_option()
        || entry.short_symbol.trim().is_empty()
        || entry.long_symbol.trim().is_empty()
    {
        return Ok(None);
    }

    let mut legs = vec![
        OptionSpreadLegPlan {
            instrument_id: alpaca_option_instrument_id(&entry.long_symbol),
            symbol: entry.long_symbol.clone(),
            ratio: 1,
        },
        OptionSpreadLegPlan {
            instrument_id: alpaca_option_instrument_id(&entry.short_symbol),
            symbol: entry.short_symbol.clone(),
            ratio: -1,
        },
    ];
    if let (Some(long_call_symbol), Some(short_call_symbol)) = (
        entry
            .long_call_symbol
            .as_ref()
            .filter(|symbol| !symbol.trim().is_empty()),
        entry
            .short_call_symbol
            .as_ref()
            .filter(|symbol| !symbol.trim().is_empty()),
    ) {
        legs.push(OptionSpreadLegPlan {
            instrument_id: alpaca_option_instrument_id(long_call_symbol),
            symbol: long_call_symbol.clone(),
            ratio: 1,
        });
        legs.push(OptionSpreadLegPlan {
            instrument_id: alpaca_option_instrument_id(short_call_symbol),
            symbol: short_call_symbol.clone(),
            ratio: -1,
        });
    }
    let (scanner_premium_kind, scanner_premium) = entry.entry_debit().map_or_else(
        || (EntryPremiumKind::Credit, entry.credit.abs()),
        |debit| (EntryPremiumKind::Debit, debit),
    );

    option_spread_plan(
        legs,
        ts_init,
        entry.underlying.clone(),
        entry.strategy.clone(),
        scanner_premium_kind,
        scanner_premium,
    )
    .map(Some)
}

fn strategy_state_persisted_spread_plan(
    entry: &StrategyStateEntry,
    ts_init: UnixNanos,
) -> anyhow::Result<Option<OptionSpreadPlan>> {
    let legs = if entry.spread_legs.is_empty() {
        entry
            .spread_instrument_id
            .as_deref()
            .map(|instrument_id| InstrumentId::from_str(instrument_id))
            .transpose()?
            .map(option_spread_legs_from_instrument_id)
            .transpose()?
    } else {
        Some(
            entry
                .spread_legs
                .iter()
                .map(|leg| {
                    Ok(OptionSpreadLegPlan {
                        instrument_id: InstrumentId::from_str(&leg.instrument_id).map_err(
                            |error| {
                                anyhow::anyhow!(
                                    "invalid persisted spread leg instrument_id `{}`: {error}",
                                    leg.instrument_id
                                )
                            },
                        )?,
                        symbol: leg.symbol.clone(),
                        ratio: leg.ratio,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
        )
    };
    let Some(legs) = legs.filter(|legs| legs.len() >= 2) else {
        return Ok(None);
    };

    let (scanner_premium_kind, scanner_premium) = entry.entry_debit().map_or_else(
        || (EntryPremiumKind::Credit, entry.credit.abs()),
        |debit| (EntryPremiumKind::Debit, debit),
    );
    option_spread_plan(
        legs,
        ts_init,
        entry.underlying.clone(),
        entry.strategy.clone(),
        scanner_premium_kind,
        scanner_premium,
    )
    .map(Some)
}

fn push_leg(legs: &mut Vec<OptionSpreadLegPlan>, contract: &ScoredContract, ratio: i64) {
    legs.push(OptionSpreadLegPlan {
        instrument_id: alpaca_option_instrument_id(&contract.symbol),
        symbol: contract.symbol.clone(),
        ratio,
    });
}

fn alpaca_option_instrument_id(symbol: &str) -> InstrumentId {
    InstrumentId::new(Symbol::new(symbol), Venue::new(ALPACA_VENUE))
}

fn option_spread_plan(
    legs: Vec<OptionSpreadLegPlan>,
    ts_init: UnixNanos,
    underlying: String,
    strategy: String,
    scanner_premium_kind: EntryPremiumKind,
    scanner_premium: f64,
) -> anyhow::Result<OptionSpreadPlan> {
    let spread_legs = legs
        .iter()
        .map(OptionSpreadLegPlan::to_spread_leg)
        .collect::<Vec<_>>();
    let instrument_id = generic_spread_id(&spread_legs)?;
    let legs = option_spread_legs_from_instrument_id(instrument_id)?;
    let expiration_ns = spread_expiration_ns(&legs)?;
    let raw_symbol = instrument_id.symbol;
    let instrument = OptionSpread::new(
        instrument_id,
        raw_symbol,
        AssetClass::Equity,
        None,
        Ustr::from(underlying.as_str()),
        Ustr::from(strategy.as_str()),
        0.into(),
        expiration_ns.into(),
        Currency::USD(),
        2,
        Price::from("0.01"),
        Quantity::from(100),
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
        None,
        ts_init,
        ts_init,
    )
    .into_any();

    Ok(OptionSpreadPlan {
        instrument_id,
        raw_symbol,
        legs,
        instrument,
        strategy,
        underlying,
        scanner_premium_kind,
        scanner_premium,
    })
}

fn spread_expiration_ns(legs: &[OptionSpreadLegPlan]) -> anyhow::Result<u64> {
    let mut expiration_ns = 0_u64;
    for leg in legs {
        let parsed = parse_expiration_from_alpaca_symbol(&leg.symbol)?;
        expiration_ns = expiration_ns.max(parsed);
    }
    Ok(expiration_ns)
}

fn parse_expiration_from_alpaca_symbol(symbol: &str) -> anyhow::Result<u64> {
    let trimmed = symbol.trim();
    let compact = trimmed.strip_prefix("O:").unwrap_or(trimmed);
    let date_start = compact
        .len()
        .checked_sub(15)
        .ok_or_else(|| anyhow::anyhow!("invalid Alpaca option symbol `{symbol}`"))?;
    if date_start == 0 {
        anyhow::bail!("invalid Alpaca option symbol `{symbol}`: missing underlying");
    }
    let date_end = date_start + 6;
    let cp_index = date_end;
    let expiration = compact
        .get(date_start..date_end)
        .filter(|value| value.as_bytes().iter().all(u8::is_ascii_digit))
        .ok_or_else(|| anyhow::anyhow!("invalid Alpaca option expiration in `{symbol}`"))?;
    let cp = compact
        .as_bytes()
        .get(cp_index)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("invalid Alpaca option symbol `{symbol}`"))?;
    if !matches!(cp, b'C' | b'P') {
        anyhow::bail!("invalid Alpaca option right in `{symbol}`");
    }
    compact
        .get(cp_index + 1..)
        .filter(|value| value.as_bytes().iter().all(u8::is_ascii_digit))
        .ok_or_else(|| anyhow::anyhow!("invalid Alpaca option strike in `{symbol}`"))?;
    let yy = &expiration[0..2];
    let mm = &expiration[2..4];
    let dd = &expiration[4..6];
    let expiration = format!("20{yy}-{mm}-{dd}");
    parse_option_expiration_ns(&expiration)
        .map_err(|e| anyhow::anyhow!("invalid expiration in Alpaca option symbol `{symbol}`: {e}"))
}
