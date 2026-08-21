// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Quantity in thousandths, for the things a whole number cannot express.
//!
//! Most order lines have a whole-number quantity, and `i64` would do. Three cases
//! in the specification do not:
//!
//! * **Half-and-half products.** The `SPLIT_ITEM` modifier makes one line out of two
//!   halves (`pos-spec.md` §3), and the bill of materials has to be computed per
//!   fraction — half a base recipe, not a whole one.
//! * **Weighed items.** 0.375 kg at a price per kilogram.
//! * **Recipe amounts.** A bill of materials measured in grams and millilitres,
//!   where a modifier adds "50 g of dough" to a base recipe (`pos-spec.md` §8).
//!
//! Thousandths give three decimal places exactly, with no float anywhere. One gram
//! resolution on a kilogram, one millilitre on a litre.

use serde::{Deserialize, Serialize};

use crate::money::{Money, MoneyError, Rounding};

/// A quantity, counted in thousandths of a unit.
///
/// The wire form is the integer count of thousandths, so 1.5 is `1500`. Naming it
/// `milli` rather than `value` keeps the scale visible at every use site.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Quantity {
    /// Thousandths of a unit.
    pub milli: i64,
}

impl Quantity {
    /// Thousandths of a unit per whole unit.
    pub const SCALE: i64 = 1_000;

    /// Zero.
    pub const ZERO: Self = Self { milli: 0 };
    /// One whole unit.
    pub const ONE: Self = Self { milli: Self::SCALE };
    /// One half — the fraction a `SPLIT_ITEM` line uses for each of its halves.
    pub const HALF: Self = Self {
        milli: Self::SCALE / 2,
    };

    /// A quantity from thousandths.
    #[must_use]
    pub const fn from_milli(milli: i64) -> Self {
        Self { milli }
    }

    /// The quantity in thousandths, the inverse of [`Self::from_milli`].
    ///
    /// An adapter posting consumption to an external ledger needs the raw value to put on the wire.
    #[must_use]
    pub const fn as_milli(self) -> i64 {
        self.milli
    }

    /// A whole number of units.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if scaling to thousandths leaves `i64`.
    pub fn from_whole(units: i64) -> Result<Self, MoneyError> {
        units
            .checked_mul(Self::SCALE)
            .map(Self::from_milli)
            .ok_or(MoneyError::Overflow)
    }

    /// Whether the quantity is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.milli == 0
    }

    /// Adds two quantities.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the sum leaves `i64`.
    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.milli
            .checked_add(other.milli)
            .map(Self::from_milli)
            .ok_or(MoneyError::Overflow)
    }

    /// Subtracts one quantity from another.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the difference leaves `i64`.
    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.milli
            .checked_sub(other.milli)
            .map(Self::from_milli)
            .ok_or(MoneyError::Overflow)
    }

    /// Scales a quantity by another, treating `factor` as a fraction of one unit.
    ///
    /// This is the bill-of-materials calculation: 50 g of an ingredient for half a
    /// pizza is `Quantity::from_milli(50_000).checked_scale(Quantity::HALF, …)`.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the product leaves `i64`.
    pub fn checked_scale(self, factor: Self, mode: Rounding) -> Result<Self, MoneyError> {
        let numerator = i128::from(self.milli)
            .checked_mul(i128::from(factor.milli))
            .ok_or(MoneyError::Overflow)?;
        crate::money::div_round(numerator, i128::from(Self::SCALE), mode).map(Self::from_milli)
    }
}

impl Money {
    /// Multiplies a unit price by a fractional quantity.
    ///
    /// A price per kilogram times 0.375 kg, or a half-and-half line times
    /// [`Quantity::HALF`].
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the product leaves `i64`.
    pub fn mul_quantity(self, quantity: Quantity, mode: Rounding) -> Result<Self, MoneyError> {
        let numerator = i128::from(self.amount_minor)
            .checked_mul(i128::from(quantity.milli))
            .ok_or(MoneyError::Overflow)?;
        let amount = crate::money::div_round(numerator, i128::from(Quantity::SCALE), mode)?;
        Ok(Self::new(self.currency_code, amount))
    }
}

#[cfg(test)]
mod tests {
    use super::Quantity;
    use crate::money::{CurrencyCode, Money, Rounding};

    fn vnd(amount: i64) -> Money {
        Money::new(CurrencyCode::VND, amount)
    }

    #[test]
    fn a_half_of_a_price_is_half_the_price() {
        assert_eq!(
            vnd(199_000)
                .mul_quantity(Quantity::HALF, Rounding::HalfUp)
                .expect("scales"),
            vnd(99_500)
        );
    }

    #[test]
    fn a_weighed_item_prices_by_fraction() {
        // 120,000 đồng per kilogram, 0.375 kg.
        assert_eq!(
            vnd(120_000)
                .mul_quantity(Quantity::from_milli(375), Rounding::HalfUp)
                .expect("scales"),
            vnd(45_000)
        );
    }

    #[test]
    fn scaling_by_one_is_the_identity() {
        assert_eq!(
            vnd(150_000)
                .mul_quantity(Quantity::ONE, Rounding::HalfUp)
                .expect("scales"),
            vnd(150_000)
        );
    }

    #[test]
    fn a_recipe_amount_halves_for_a_split_item() {
        // 50 g of dough for a whole pizza is 25 g for each half.
        let dough = Quantity::from_milli(50_000);
        assert_eq!(
            dough
                .checked_scale(Quantity::HALF, Rounding::HalfUp)
                .expect("scales"),
            Quantity::from_milli(25_000)
        );
    }

    #[test]
    fn rounding_mode_is_honoured_when_the_price_does_not_divide() {
        let odd = vnd(999);
        assert_eq!(
            odd.mul_quantity(Quantity::HALF, Rounding::Floor)
                .expect("floor"),
            vnd(499)
        );
        assert_eq!(
            odd.mul_quantity(Quantity::HALF, Rounding::HalfUp)
                .expect("half up"),
            vnd(500)
        );
    }

    #[test]
    fn serialises_as_thousandths() {
        let json = serde_json::to_string(&Quantity::HALF).expect("serialise");
        assert_eq!(json, r#"{"milli":500}"#);
    }
}
