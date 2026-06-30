use thiserror::Error;

use crate::{
    enums::OrderSide,
    identifiers::{InstrumentId, Symbol, Venue},
};

pub const GENERIC_SPREAD_ID_SEPARATOR: &str = "___";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpreadLeg {
    pub instrument_id: InstrumentId,
    pub ratio: i64,
}

impl SpreadLeg {
    #[must_use]
    pub const fn new(instrument_id: InstrumentId, ratio: i64) -> Self {
        Self {
            instrument_id,
            ratio,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GenericSpreadError {
    #[error("generic spread must contain at least two legs")]
    TooFewLegs,
    #[error("generic spread leg ratio cannot be zero for {instrument_id}")]
    ZeroRatio { instrument_id: InstrumentId },
    #[error("generic spread legs must share the same venue: expected {expected}, was {actual}")]
    VenueMismatch { expected: Venue, actual: Venue },
    #[error("invalid generic spread leg `{component}`")]
    InvalidLeg { component: String },
    #[error("invalid generic spread leg `{component}`: empty symbol")]
    EmptySymbol { component: String },
    #[error("invalid generic spread leg symbol `{component}`: {reason}")]
    InvalidSymbol { component: String, reason: String },
    #[error("invalid generic spread leg ratio `{component}`: {reason}")]
    InvalidRatio { component: String, reason: String },
    #[error("generic spread order is missing side")]
    MissingOrderSide,
}

#[must_use]
pub fn is_generic_spread_id(instrument_id: InstrumentId) -> bool {
    instrument_id
        .symbol
        .as_str()
        .contains(GENERIC_SPREAD_ID_SEPARATOR)
}

/// Creates a generic spread [`InstrumentId`] from signed spread legs.
///
/// Positive leg ratios are bought when buying the spread. Negative leg ratios
/// are sold when buying the spread.
///
/// # Errors
///
/// Returns an error when fewer than two legs are supplied, any ratio is zero,
/// or the leg venues do not match.
pub fn generic_spread_id(legs: &[SpreadLeg]) -> Result<InstrumentId, GenericSpreadError> {
    if legs.len() < 2 {
        return Err(GenericSpreadError::TooFewLegs);
    }

    let first_venue = legs[0].instrument_id.venue;
    for leg in legs {
        if leg.ratio == 0 {
            return Err(GenericSpreadError::ZeroRatio {
                instrument_id: leg.instrument_id,
            });
        }
        if leg.instrument_id.venue != first_venue {
            return Err(GenericSpreadError::VenueMismatch {
                expected: first_venue,
                actual: leg.instrument_id.venue,
            });
        }
    }

    let mut sorted_legs = legs.to_vec();
    sorted_legs.sort_by_key(|leg| leg.instrument_id.symbol);

    let symbol = sorted_legs
        .iter()
        .map(|leg| {
            if leg.ratio > 0 {
                format!("({}){}", leg.ratio, leg.instrument_id.symbol)
            } else {
                format!(
                    "(({})){}",
                    leg.ratio.unsigned_abs(),
                    leg.instrument_id.symbol
                )
            }
        })
        .collect::<Vec<_>>()
        .join(GENERIC_SPREAD_ID_SEPARATOR);

    let symbol = Symbol::new_checked(symbol).map_err(|e| GenericSpreadError::InvalidSymbol {
        component: legs
            .iter()
            .map(|leg| leg.instrument_id.to_string())
            .collect::<Vec<_>>()
            .join(","),
        reason: e.to_string(),
    })?;

    Ok(InstrumentId::new(symbol, first_venue))
}

/// Parses a generic spread [`InstrumentId`] into signed spread legs.
///
/// # Errors
///
/// Returns an error when the symbol is not valid generic spread syntax.
pub fn generic_spread_legs(
    instrument_id: InstrumentId,
) -> Result<Vec<SpreadLeg>, GenericSpreadError> {
    let symbol = instrument_id.symbol.as_str();
    let components = symbol.split(GENERIC_SPREAD_ID_SEPARATOR);
    let mut legs = Vec::new();

    for component in components {
        legs.push(parse_generic_spread_leg(component, instrument_id.venue)?);
    }

    if legs.len() < 2 {
        return Err(GenericSpreadError::TooFewLegs);
    }

    legs.sort_by_key(|leg| leg.instrument_id.symbol);
    Ok(legs)
}

#[must_use]
pub fn generic_spread_total_leg_quantity(instrument_id: InstrumentId) -> Option<u64> {
    generic_spread_legs(instrument_id)
        .ok()
        .map(|legs| legs.iter().map(|leg| leg.ratio.unsigned_abs()).sum())
}

/// Maps a parent spread order side and signed leg ratio into the child leg side.
///
/// Positive ratios follow the parent side; negative ratios trade the opposite
/// side.
///
/// # Errors
///
/// Returns an error for [`OrderSide::NoOrderSide`] or a zero ratio.
pub fn spread_leg_order_side(
    spread_side: OrderSide,
    ratio: i64,
) -> Result<OrderSide, GenericSpreadError> {
    if ratio == 0 {
        return Err(GenericSpreadError::InvalidRatio {
            component: "0".to_string(),
            reason: "ratio cannot be zero".to_string(),
        });
    }

    match (spread_side, ratio.is_positive()) {
        (OrderSide::Buy, true) | (OrderSide::Sell, false) => Ok(OrderSide::Buy),
        (OrderSide::Buy, false) | (OrderSide::Sell, true) => Ok(OrderSide::Sell),
        (OrderSide::NoOrderSide, _) => Err(GenericSpreadError::MissingOrderSide),
    }
}

fn parse_generic_spread_leg(
    component: &str,
    venue: Venue,
) -> Result<SpreadLeg, GenericSpreadError> {
    let (ratio, symbol) = if let Some(rest) = component.strip_prefix("((") {
        let (ratio, symbol) =
            rest.split_once("))")
                .ok_or_else(|| GenericSpreadError::InvalidLeg {
                    component: component.to_string(),
                })?;
        (-parse_positive_ratio(ratio, component)?, symbol)
    } else {
        let rest = component
            .strip_prefix('(')
            .ok_or_else(|| GenericSpreadError::InvalidLeg {
                component: component.to_string(),
            })?;
        let (ratio, symbol) =
            rest.split_once(')')
                .ok_or_else(|| GenericSpreadError::InvalidLeg {
                    component: component.to_string(),
                })?;
        (parse_positive_ratio(ratio, component)?, symbol)
    };

    if symbol.is_empty() {
        return Err(GenericSpreadError::EmptySymbol {
            component: component.to_string(),
        });
    }

    let symbol = Symbol::new_checked(symbol).map_err(|e| GenericSpreadError::InvalidSymbol {
        component: component.to_string(),
        reason: e.to_string(),
    })?;

    Ok(SpreadLeg {
        instrument_id: InstrumentId::new(symbol, venue),
        ratio,
    })
}

fn parse_positive_ratio(value: &str, component: &str) -> Result<i64, GenericSpreadError> {
    let ratio = value
        .parse::<i64>()
        .map_err(|e| GenericSpreadError::InvalidRatio {
            component: component.to_string(),
            reason: e.to_string(),
        })?;
    if ratio <= 0 {
        return Err(GenericSpreadError::InvalidRatio {
            component: component.to_string(),
            reason: "ratio must be positive".to_string(),
        });
    }

    Ok(ratio)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn id(value: &str) -> InstrumentId {
        InstrumentId::from(value)
    }

    #[test]
    fn generic_spread_id_sorts_and_formats_signed_legs() {
        let spread = generic_spread_id(&[
            SpreadLeg::new(id("SPY260508P00500000.ALPACA"), -1),
            SpreadLeg::new(id("SPY260508P00495000.ALPACA"), 1),
        ])
        .unwrap();

        assert_eq!(
            spread.to_string(),
            "(1)SPY260508P00495000___((1))SPY260508P00500000.ALPACA"
        );
    }

    #[test]
    fn generic_spread_legs_parses_and_sorts_components() {
        let legs =
            generic_spread_legs(id("((1))SPY260508P00500000___(1)SPY260508P00495000.ALPACA"))
                .unwrap();

        assert_eq!(
            legs,
            vec![
                SpreadLeg::new(id("SPY260508P00495000.ALPACA"), 1),
                SpreadLeg::new(id("SPY260508P00500000.ALPACA"), -1),
            ]
        );
    }

    #[test]
    fn generic_spread_id_supports_four_leg_iron_condor() {
        let spread = generic_spread_id(&[
            SpreadLeg::new(id("SPY260508P00495000.ALPACA"), 1),
            SpreadLeg::new(id("SPY260508P00500000.ALPACA"), -1),
            SpreadLeg::new(id("SPY260508C00520000.ALPACA"), -1),
            SpreadLeg::new(id("SPY260508C00525000.ALPACA"), 1),
        ])
        .unwrap();

        assert_eq!(
            spread.to_string(),
            "((1))SPY260508C00520000___(1)SPY260508C00525000___(1)SPY260508P00495000___((1))SPY260508P00500000.ALPACA"
        );
        assert_eq!(generic_spread_legs(spread).unwrap().len(), 4);
    }

    #[rstest]
    #[case(OrderSide::Buy, 1, OrderSide::Buy)]
    #[case(OrderSide::Buy, -1, OrderSide::Sell)]
    #[case(OrderSide::Sell, 1, OrderSide::Sell)]
    #[case(OrderSide::Sell, -1, OrderSide::Buy)]
    fn spread_leg_order_side_maps_parent_side_and_ratio(
        #[case] spread_side: OrderSide,
        #[case] ratio: i64,
        #[case] expected: OrderSide,
    ) {
        assert_eq!(spread_leg_order_side(spread_side, ratio).unwrap(), expected);
    }
}
