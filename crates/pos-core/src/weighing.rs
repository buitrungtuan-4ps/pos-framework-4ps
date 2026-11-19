// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A weighed line's weight ([ADR-0223](../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md)).
//!
//! An item sold by weight is priced per kilogram and its line quantity is its net weight in grams,
//! thousandths of a kilogram, so the line total is the price times that quantity, rounded once as
//! every line is. These are the fixed rules: a weighed item's line names where its weight came
//! from, an item sold by the piece takes no weight, a scale's tare comes off its reading and is
//! never edited otherwise, and a line weighs at least the store's least weight and at most what a
//! weight label can hold. Pure, like the rest of the domain: the edge reads the scale and the
//! setting, and asks this module what they make.

use pos_proto::Quantity;
use pos_proto::weighing::{MAX_GRAMS, WeightSource};

/// Why a weight cannot make a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WeighError {
    /// An item sold by weight, added with no weight: from no label, scale or hand.
    Required,
    /// A weight given for an item sold by the piece.
    NotSoldByWeight,
    /// Less than the store's least weight, after the tare.
    BelowMinimum {
        /// The least a line may weigh, in grams.
        min_grams: u32,
    },
    /// More than [`MAX_GRAMS`].
    AboveMaximum,
}

impl core::fmt::Display for WeighError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Required => write!(formatter, "an item sold by weight is added with its weight"),
            Self::NotSoldByWeight => write!(formatter, "the item is not sold by weight"),
            Self::BelowMinimum { min_grams } => {
                write!(formatter, "a weighed line weighs at least {min_grams} g")
            }
            Self::AboveMaximum => write!(formatter, "a weighed line weighs at most {MAX_GRAMS} g"),
        }
    }
}

impl core::error::Error for WeighError {}

/// Whether a line of an item may carry `source`: a weighed item's line names one, and any other
/// item's line none.
///
/// # Errors
///
/// [`WeighError::Required`] for a weighed item with no source, [`WeighError::NotSoldByWeight`] for
/// a source on an item sold by the piece.
pub const fn check_source(
    sold_by_weight: bool,
    source: Option<WeightSource>,
) -> Result<(), WeighError> {
    match (sold_by_weight, source.is_some()) {
        (true, false) => Err(WeighError::Required),
        (false, true) => Err(WeighError::NotSoldByWeight),
        _ => Ok(()),
    }
}

/// The line quantity `grams` makes once `tare_grams` is taken off: the net grams, as thousandths
/// of a kilogram. A label's weight and a typed one are net already, and pass no tare.
///
/// # Errors
///
/// [`WeighError::BelowMinimum`] if the net weight is under `min_grams` (or is nothing),
/// [`WeighError::AboveMaximum`] if it is over [`MAX_GRAMS`].
pub fn net_quantity(
    grams: u32,
    tare_grams: Option<u32>,
    min_grams: u32,
) -> Result<Quantity, WeighError> {
    let net = grams.saturating_sub(tare_grams.unwrap_or(0));
    if net == 0 || net < min_grams {
        return Err(WeighError::BelowMinimum {
            min_grams: min_grams.max(1),
        });
    }
    if net > MAX_GRAMS {
        return Err(WeighError::AboveMaximum);
    }
    Ok(Quantity::from_milli(i64::from(net)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_weighed_item_names_its_source_and_any_other_names_none() {
        assert_eq!(check_source(true, Some(WeightSource::Scale)), Ok(()));
        assert_eq!(check_source(false, None), Ok(()));
        assert_eq!(check_source(true, None), Err(WeighError::Required));
        assert_eq!(
            check_source(false, Some(WeightSource::Label)),
            Err(WeighError::NotSoldByWeight)
        );
    }

    #[test]
    fn the_tare_comes_off_and_the_net_is_held_to_its_bounds() {
        assert_eq!(
            net_quantity(425, Some(50), 1),
            Ok(Quantity::from_milli(375))
        );
        assert_eq!(net_quantity(375, None, 1), Ok(Quantity::from_milli(375)));
        assert_eq!(
            net_quantity(40, Some(50), 1),
            Err(WeighError::BelowMinimum { min_grams: 1 })
        );
        assert_eq!(
            net_quantity(49, None, 50),
            Err(WeighError::BelowMinimum { min_grams: 50 })
        );
        assert_eq!(net_quantity(50, None, 50), Ok(Quantity::from_milli(50)));
        assert_eq!(
            net_quantity(MAX_GRAMS + 1, None, 1),
            Err(WeighError::AboveMaximum)
        );
    }
}
