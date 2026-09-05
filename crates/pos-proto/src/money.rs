// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Money as an integer in the currency's minor unit, and the arithmetic allowed on
//! it.
//!
//! Floating point is banned at every layer, which the workspace enforces two ways:
//! `clippy::float_arithmetic` bans the operations and `disallowed-types` in
//! `clippy.toml` bans `f32` and `f64` outright. This module is what makes that ban
//! liveable — every calculation a bill needs is here, in integers.
//!
//! # The one rounding primitive
//!
//! Every operation that can lose a fraction of a minor unit funnels through
//! [`div_round`], and every one of them takes an explicit [`Rounding`]. There is no
//! default, because "which way does it round" is a decision somebody has to make
//! rather than inherit.
//!
//! # Why splits add up
//!
//! [`Money::allocate`] and [`Money::split_into`] guarantee that the parts sum
//! **exactly** to the original — the third data-correctness law in
//! `docs/pos-spec.md` §14.3. They achieve it by flooring every share and giving the
//! entire residual to the last part, so exactness is structural rather than lucky.
//!
//! That guarantee only survives if callers **allocate once and store the result**.
//! A bill-level discount is apportioned across lines when it is applied, the
//! per-line amounts are snapshotted, and a later split *partitions those stored
//! integers* rather than recomputing percentages. Partitioning a set of integers
//! preserves their sum trivially; recomputing a percentage at split time loses a
//! đồng eventually, always.

use core::fmt;
use core::num::NonZeroI64;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An ISO 4217 currency code: exactly three uppercase ASCII letters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CurrencyCode([u8; 3]);

impl CurrencyCode {
    /// Vietnamese đồng. The minor unit is the đồng itself — there are no subunits
    /// in practice, so `amount_minor` is a whole đồng.
    pub const VND: Self = Self(*b"VND");
    /// Japanese yen, likewise a zero-decimal currency.
    pub const JPY: Self = Self(*b"JPY");
    /// United States dollar, whose minor unit is the cent.
    pub const USD: Self = Self(*b"USD");
    /// Indian rupee, whose minor unit is the paisa — two decimal places, so `100` is one rupee.
    ///
    /// Named here because `countries/in` denominates in it
    /// ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)); the constant is a
    /// convenience, and any three upper-case letters still parse.
    pub const INR: Self = Self(*b"INR");

    /// Parses a three-letter code.
    ///
    /// # Errors
    ///
    /// [`MoneyError::CurrencyCode`] if the input is not exactly three uppercase
    /// ASCII letters. Lowercase is rejected rather than normalised, so that one
    /// currency has exactly one representation on the wire.
    pub fn parse(text: &str) -> Result<Self, MoneyError> {
        let bytes = text.as_bytes();
        let [a, b, c] = *<&[u8; 3]>::try_from(bytes).map_err(|_| MoneyError::CurrencyCode)?;
        if [a, b, c].iter().any(|byte| !byte.is_ascii_uppercase()) {
            return Err(MoneyError::CurrencyCode);
        }
        Ok(Self([a, b, c]))
    }

    /// The code as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Every construction path checks for uppercase ASCII, so this is valid
        // UTF-8 by construction.
        core::str::from_utf8(&self.0).unwrap_or("???")
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Serialize for CurrencyCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CurrencyCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <&str>::deserialize(deserializer)?;
        Self::parse(text).map_err(serde::de::Error::custom)
    }
}

/// How to resolve a fraction of a minor unit.
///
/// The two half-modes differ only on an exact tie. `HalfUp` is what most tax
/// authorities specify; `HalfEven` is banker's rounding, which does not
/// systematically inflate a long series of ties.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rounding {
    /// Toward negative infinity.
    Floor,
    /// Toward positive infinity.
    Ceil,
    /// Toward zero — magnitude never increases.
    TowardZero,
    /// Ties away from zero; otherwise to the nearest.
    HalfUp,
    /// Ties to the even quotient; otherwise to the nearest.
    HalfEven,
}

/// An exact rational rate: a tax rate, a service charge, a percentage discount.
///
/// A rate is never a float. 10% is `Ratio::percent(10)`, which is 10/100 exactly,
/// and 8.25% is `Ratio::basis_points(825)`.
///
/// On the wire it is two integers, `{"numerator": 10, "denominator": 100}`. It has to
/// cross a boundary because a line snapshot captures **the tax rate in force at the
/// moment the line was added** (`docs/pos-spec.md` §14.2) — sending a decimal string
/// or a float instead would reintroduce exactly the imprecision the integer discipline
/// exists to prevent. `NonZeroI64` means a zero denominator is unrepresentable, so a
/// malformed rate is rejected at the edge rather than dividing by zero later.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Ratio {
    numerator: i64,
    denominator: NonZeroI64,
}

impl Ratio {
    /// Builds a rate from a numerator and a non-zero denominator.
    #[must_use]
    pub const fn new(numerator: i64, denominator: NonZeroI64) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// A whole-number percentage: `percent(10)` is exactly 10/100.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] never occurs here; the signature is fallible only
    /// for symmetry with [`Ratio::basis_points`].
    pub fn percent(value: i64) -> Result<Self, MoneyError> {
        NonZeroI64::new(100)
            .map(|denominator| Self::new(value, denominator))
            .ok_or(MoneyError::ZeroDenominator)
    }

    /// Hundredths of a percent: `basis_points(825)` is 8.25%.
    ///
    /// # Errors
    ///
    /// [`MoneyError::ZeroDenominator`] cannot occur in practice; see
    /// [`Ratio::percent`].
    pub fn basis_points(value: i64) -> Result<Self, MoneyError> {
        NonZeroI64::new(10_000)
            .map(|denominator| Self::new(value, denominator))
            .ok_or(MoneyError::ZeroDenominator)
    }

    /// The numerator.
    #[must_use]
    pub const fn numerator(self) -> i64 {
        self.numerator
    }

    /// The denominator, which is never zero.
    #[must_use]
    pub const fn denominator(self) -> NonZeroI64 {
        self.denominator
    }
}

/// An amount of money: an integer count of the currency's minor unit.
///
/// The wire form is `{"currency_code": "VND", "amount_minor": 150000}`, and the
/// database form is `char(3)` plus `bigint`. Both halves travel together always —
/// an amount without its currency is not money, it is a number.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Money {
    /// ISO 4217 code.
    pub currency_code: CurrencyCode,
    /// Whole minor units: đồng for VND, yen for JPY, cents for USD.
    pub amount_minor: i64,
}

impl Money {
    /// An amount in the given currency.
    #[must_use]
    pub const fn new(currency_code: CurrencyCode, amount_minor: i64) -> Self {
        Self {
            currency_code,
            amount_minor,
        }
    }

    /// Zero in the given currency.
    #[must_use]
    pub const fn zero(currency_code: CurrencyCode) -> Self {
        Self::new(currency_code, 0)
    }

    /// Whether the amount is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.amount_minor == 0
    }

    /// Whether the amount is below zero.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.amount_minor < 0
    }

    /// Adds two amounts.
    ///
    /// # Errors
    ///
    /// [`MoneyError::CurrencyMismatch`] if the currencies differ — a bill is
    /// single-currency by design (`docs/pos-spec.md` §19 excludes multi-currency
    /// bills), so a mismatch is a bug rather than a conversion request.
    /// [`MoneyError::Overflow`] if the sum leaves `i64`.
    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        self.amount_minor
            .checked_add(other.amount_minor)
            .map(|amount| Self::new(self.currency_code, amount))
            .ok_or(MoneyError::Overflow)
    }

    /// Subtracts one amount from another.
    ///
    /// # Errors
    ///
    /// As [`Money::checked_add`].
    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.require_same_currency(other)?;
        self.amount_minor
            .checked_sub(other.amount_minor)
            .map(|amount| Self::new(self.currency_code, amount))
            .ok_or(MoneyError::Overflow)
    }

    /// Negates the amount.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] for `i64::MIN`.
    pub fn checked_neg(self) -> Result<Self, MoneyError> {
        self.amount_minor
            .checked_neg()
            .map(|amount| Self::new(self.currency_code, amount))
            .ok_or(MoneyError::Overflow)
    }

    /// Multiplies by a whole number of units — a line's quantity, for instance.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the product leaves `i64`.
    pub fn checked_mul(self, factor: i64) -> Result<Self, MoneyError> {
        self.amount_minor
            .checked_mul(factor)
            .map(|amount| Self::new(self.currency_code, amount))
            .ok_or(MoneyError::Overflow)
    }

    /// Applies a rate: tax, service charge, or a percentage discount.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the result leaves `i64`.
    pub fn mul_ratio(self, rate: Ratio, mode: Rounding) -> Result<Self, MoneyError> {
        let numerator = i128::from(self.amount_minor)
            .checked_mul(i128::from(rate.numerator()))
            .ok_or(MoneyError::Overflow)?;
        let amount = div_round(numerator, i128::from(rate.denominator().get()), mode)?;
        Ok(Self::new(self.currency_code, amount))
    }

    /// Extracts tax that is **already included** in this amount: `self × r ÷ (1+r)`.
    ///
    /// This is not the same calculation as [`Money::mul_ratio`], and confusing the
    /// two is a classic way to misstate VAT. Vietnam displays tax-inclusive prices,
    /// so a 10% rate on a 110,000 đồng shelf price yields 10,000 đồng of tax here,
    /// where `mul_ratio` would yield 11,000.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the result leaves `i64`, or
    /// [`MoneyError::ZeroDenominator`] if the rate is exactly −100%.
    pub fn tax_included(self, rate: Ratio, mode: Rounding) -> Result<Self, MoneyError> {
        let rate_numerator = i128::from(rate.numerator());
        let rate_denominator = i128::from(rate.denominator().get());
        let numerator = i128::from(self.amount_minor)
            .checked_mul(rate_numerator)
            .ok_or(MoneyError::Overflow)?;
        let denominator = rate_denominator
            .checked_add(rate_numerator)
            .ok_or(MoneyError::Overflow)?;
        let amount = div_round(numerator, denominator, mode)?;
        Ok(Self::new(self.currency_code, amount))
    }

    /// Rounds to a cash increment — to the nearest 500 đồng, say, where no smaller
    /// coin circulates.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the result leaves `i64`.
    pub fn round_to_increment(
        self,
        increment: NonZeroI64,
        mode: Rounding,
    ) -> Result<Self, MoneyError> {
        let step = i128::from(increment.get());
        let steps = div_round(i128::from(self.amount_minor), step, mode)?;
        let amount = i128::from(steps)
            .checked_mul(step)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(MoneyError::Overflow)?;
        Ok(Self::new(self.currency_code, amount))
    }

    /// Splits into `parts` amounts that sum **exactly** to this one.
    ///
    /// The remainder lands on the last part, so `100,000` split three ways is
    /// `33,333 + 33,333 + 33,334`.
    ///
    /// # Errors
    ///
    /// [`MoneyError::EmptyAllocation`] if `parts` is zero, or
    /// [`MoneyError::Overflow`].
    pub fn split_into(self, parts: usize) -> Result<Vec<Self>, MoneyError> {
        let weights = vec![1_i64; parts];
        self.allocate(&weights)
    }

    /// Apportions this amount across `weights`, summing **exactly** to the original.
    ///
    /// Used to spread a bill-level discount over lines in proportion to each line's
    /// pre-discount total. Every share is floored and the whole residual is given to
    /// the last weight, which is what makes the sum exact by construction rather
    /// than by luck.
    ///
    /// Note the consequence of the rule `docs/pos-spec.md` §14.3 states: because the
    /// residual goes entirely to the last part, that part can exceed the others by
    /// as much as `weights.len() - 1` minor units. Largest-remainder apportionment
    /// would bound the difference at one, but it is not what the specification
    /// mandates.
    ///
    /// # Errors
    ///
    /// [`MoneyError::EmptyAllocation`] if `weights` is empty,
    /// [`MoneyError::NonPositiveWeight`] if any weight is negative or they sum to
    /// zero, or [`MoneyError::Overflow`].
    pub fn allocate(self, weights: &[i64]) -> Result<Vec<Self>, MoneyError> {
        let (Some(_), Some(last_index)) = (weights.first(), weights.len().checked_sub(1)) else {
            return Err(MoneyError::EmptyAllocation);
        };
        if weights.iter().any(|weight| *weight < 0) {
            return Err(MoneyError::NonPositiveWeight);
        }
        let total_weight: i128 = weights.iter().map(|weight| i128::from(*weight)).sum();
        if total_weight <= 0 {
            return Err(MoneyError::NonPositiveWeight);
        }

        let mut parts = Vec::with_capacity(weights.len());
        let mut assigned: i64 = 0;
        for weight in weights.iter().take(last_index) {
            let numerator = i128::from(self.amount_minor)
                .checked_mul(i128::from(*weight))
                .ok_or(MoneyError::Overflow)?;
            let share = div_round(numerator, total_weight, Rounding::Floor)?;
            assigned = assigned.checked_add(share).ok_or(MoneyError::Overflow)?;
            parts.push(Self::new(self.currency_code, share));
        }
        // The residual, not another rounded share. This is the line that makes the
        // sum exact.
        let remainder = self
            .amount_minor
            .checked_sub(assigned)
            .ok_or(MoneyError::Overflow)?;
        parts.push(Self::new(self.currency_code, remainder));
        Ok(parts)
    }

    fn require_same_currency(self, other: Self) -> Result<(), MoneyError> {
        if self.currency_code == other.currency_code {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch {
                left: self.currency_code,
                right: other.currency_code,
            })
        }
    }
}

impl fmt::Debug for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount_minor, self.currency_code)
    }
}

/// Rounds `numerator / denominator` to an integer under `mode`.
///
/// Every monetary calculation in the framework passes through here. It works on the
/// magnitude and applies the sign afterwards, rather than relying on Rust's `/` and
/// `%`, which truncate toward zero — that truncation makes `HalfUp` behave
/// differently on a refund than on a sale, which is precisely the kind of asymmetry
/// nobody notices until a credit note fails to reconcile.
///
/// # Errors
///
/// [`MoneyError::ZeroDenominator`] if `denominator` is zero, or
/// [`MoneyError::Overflow`] if the result leaves `i64`.
pub fn div_round(numerator: i128, denominator: i128, mode: Rounding) -> Result<i64, MoneyError> {
    if denominator == 0 {
        return Err(MoneyError::ZeroDenominator);
    }
    // Normalise so the denominator is positive; the sign rides on the numerator.
    let (numerator, denominator) = if denominator < 0 {
        (
            numerator.checked_neg().ok_or(MoneyError::Overflow)?,
            denominator.checked_neg().ok_or(MoneyError::Overflow)?,
        )
    } else {
        (numerator, denominator)
    };

    let negative = numerator < 0;
    let magnitude = numerator.checked_abs().ok_or(MoneyError::Overflow)?;
    let quotient = magnitude / denominator;
    let remainder = magnitude % denominator;
    let doubled = remainder.checked_mul(2).ok_or(MoneyError::Overflow)?;

    let increase_magnitude = match mode {
        Rounding::Floor => negative && remainder != 0,
        Rounding::Ceil => !negative && remainder != 0,
        Rounding::TowardZero => false,
        Rounding::HalfUp => doubled >= denominator,
        Rounding::HalfEven => {
            doubled > denominator || (doubled == denominator && quotient % 2 != 0)
        }
    };

    let magnitude = if increase_magnitude {
        quotient.checked_add(1).ok_or(MoneyError::Overflow)?
    } else {
        quotient
    };
    let signed = if negative {
        magnitude.checked_neg().ok_or(MoneyError::Overflow)?
    } else {
        magnitude
    };
    i64::try_from(signed).map_err(|_| MoneyError::Overflow)
}

/// Why a monetary calculation could not be completed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    /// Two amounts in different currencies were combined.
    #[error("cannot combine {left} and {right}: a bill is single-currency")]
    CurrencyMismatch {
        /// Currency of the left operand.
        left: CurrencyCode,
        /// Currency of the right operand.
        right: CurrencyCode,
    },
    /// The result does not fit in `i64`.
    #[error("monetary result does not fit in 64 bits")]
    Overflow,
    /// A division by zero was requested.
    #[error("denominator is zero")]
    ZeroDenominator,
    /// An allocation over no weights at all.
    #[error("cannot allocate across an empty set of weights")]
    EmptyAllocation,
    /// A negative weight, or weights summing to zero.
    #[error("allocation weights must be non-negative and sum to more than zero")]
    NonPositiveWeight,
    /// A currency code that is not three uppercase ASCII letters.
    #[error("currency code must be three uppercase ASCII letters")]
    CurrencyCode,
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroI64;

    use proptest::prelude::*;

    use super::{CurrencyCode, Money, MoneyError, Ratio, Rounding, div_round};

    const VND: CurrencyCode = CurrencyCode::VND;

    fn vnd(amount: i64) -> Money {
        Money::new(VND, amount)
    }

    fn increment(value: i64) -> NonZeroI64 {
        NonZeroI64::new(value).unwrap_or(NonZeroI64::MIN)
    }

    // -- div_round, mode by mode -------------------------------------------

    #[test]
    fn floor_goes_toward_negative_infinity() {
        assert_eq!(div_round(7, 2, Rounding::Floor), Ok(3));
        assert_eq!(div_round(-7, 2, Rounding::Floor), Ok(-4));
    }

    #[test]
    fn ceil_goes_toward_positive_infinity() {
        assert_eq!(div_round(7, 2, Rounding::Ceil), Ok(4));
        assert_eq!(div_round(-7, 2, Rounding::Ceil), Ok(-3));
    }

    #[test]
    fn toward_zero_never_grows_the_magnitude() {
        assert_eq!(div_round(7, 2, Rounding::TowardZero), Ok(3));
        assert_eq!(div_round(-7, 2, Rounding::TowardZero), Ok(-3));
    }

    #[test]
    fn half_up_breaks_ties_away_from_zero_symmetrically() {
        // The symmetry is the point: a refund must round the same way as the sale
        // it reverses, which truncating division would get wrong.
        assert_eq!(div_round(5, 2, Rounding::HalfUp), Ok(3));
        assert_eq!(div_round(-5, 2, Rounding::HalfUp), Ok(-3));
        assert_eq!(div_round(5, 4, Rounding::HalfUp), Ok(1));
        assert_eq!(div_round(-5, 4, Rounding::HalfUp), Ok(-1));
    }

    #[test]
    fn half_even_breaks_ties_toward_the_even_quotient() {
        assert_eq!(div_round(5, 2, Rounding::HalfEven), Ok(2));
        assert_eq!(div_round(7, 2, Rounding::HalfEven), Ok(4));
        assert_eq!(div_round(-5, 2, Rounding::HalfEven), Ok(-2));
        assert_eq!(div_round(-7, 2, Rounding::HalfEven), Ok(-4));
    }

    #[test]
    fn a_zero_denominator_is_an_error_not_a_panic() {
        assert_eq!(
            div_round(1, 0, Rounding::HalfUp),
            Err(MoneyError::ZeroDenominator)
        );
    }

    #[test]
    fn a_negative_denominator_is_normalised() {
        assert_eq!(
            div_round(7, -2, Rounding::Floor),
            div_round(-7, 2, Rounding::Floor)
        );
    }

    // -- the split-rounding law, pos-spec.md §14.3 -------------------------

    #[test]
    fn the_documented_three_way_split() {
        let parts = vnd(100_000).split_into(3).expect("splits");
        assert_eq!(
            parts,
            vec![vnd(33_333), vnd(33_333), vnd(33_334)],
            "the remainder must land on the last part"
        );
    }

    #[test]
    fn proportional_allocation_puts_the_residual_last() {
        let parts = vnd(100).allocate(&[1, 1, 1]).expect("allocates");
        assert_eq!(parts, vec![vnd(33), vnd(33), vnd(34)]);
    }

    #[test]
    fn concentrates_the_whole_remainder_on_the_last_part() {
        // `pos-spec.md` §14.3 mandates remainder-to-last, and that concentrates up
        // to `parts - 1` minor units on the final part rather than spreading them
        // one each. Splitting 100 seven ways gives six parts of 14 and one of 16.
        //
        // The alternative — largest-remainder, which hands one extra minor unit to
        // each of the first `r` parts — would bound the spread at one, but it
        // contradicts the documented rule. We conform to the specification and pin
        // the consequence here so it is a known trade-off rather than a surprise.
        // For VND the difference is a few đồng and immaterial; it is visible only
        // for currencies with a minor unit that circulates.
        let parts = vnd(100).split_into(7).expect("splits");
        assert_eq!(parts.iter().map(|p| p.amount_minor).sum::<i64>(), 100);
        assert_eq!(parts.last().copied(), Some(vnd(16)));
        assert!(parts.iter().take(6).all(|part| *part == vnd(14)));
    }

    #[test]
    fn allocation_respects_the_weights() {
        let parts = vnd(1_000).allocate(&[1, 4]).expect("allocates");
        assert_eq!(parts, vec![vnd(200), vnd(800)]);
    }

    #[test]
    fn a_zero_weight_still_gets_a_part() {
        // A comped line has zero weight but must still appear on the split, or the
        // part count stops matching the line count.
        let parts = vnd(100).allocate(&[0, 1]).expect("allocates");
        assert_eq!(parts, vec![vnd(0), vnd(100)]);
    }

    #[test]
    fn allocation_rejects_degenerate_weights() {
        assert_eq!(vnd(100).allocate(&[]), Err(MoneyError::EmptyAllocation));
        assert_eq!(
            vnd(100).allocate(&[0, 0]),
            Err(MoneyError::NonPositiveWeight)
        );
        assert_eq!(
            vnd(100).allocate(&[-1, 2]),
            Err(MoneyError::NonPositiveWeight)
        );
    }

    // -- tax ---------------------------------------------------------------

    #[test]
    fn inclusive_and_exclusive_tax_are_different_calculations() {
        let rate = Ratio::percent(10).expect("rate");
        let shelf_price = vnd(110_000);
        assert_eq!(
            shelf_price
                .tax_included(rate, Rounding::HalfUp)
                .expect("inclusive"),
            vnd(10_000),
            "tax already inside a 110,000 shelf price at 10% is 10,000"
        );
        assert_eq!(
            shelf_price
                .mul_ratio(rate, Rounding::HalfUp)
                .expect("exclusive"),
            vnd(11_000),
            "tax added on top of 110,000 at 10% is 11,000"
        );
    }

    #[test]
    fn basis_points_express_fractional_rates_exactly() {
        let rate = Ratio::basis_points(825).expect("rate");
        assert_eq!(
            vnd(1_000_000)
                .mul_ratio(rate, Rounding::HalfUp)
                .expect("tax"),
            vnd(82_500)
        );
    }

    // -- cash rounding -----------------------------------------------------

    #[test]
    fn rounds_to_the_smallest_circulating_note() {
        let total = vnd(12_300);
        assert_eq!(
            total
                .round_to_increment(increment(500), Rounding::HalfUp)
                .expect("rounds"),
            vnd(12_500)
        );
        assert_eq!(
            total
                .round_to_increment(increment(500), Rounding::Floor)
                .expect("rounds"),
            vnd(12_000)
        );
    }

    // -- currency ----------------------------------------------------------

    #[test]
    fn mixing_currencies_is_an_error() {
        let error = vnd(100)
            .checked_add(Money::new(CurrencyCode::JPY, 100))
            .expect_err("must reject");
        assert!(matches!(error, MoneyError::CurrencyMismatch { .. }));
    }

    #[test]
    fn currency_codes_must_be_three_uppercase_letters() {
        assert_eq!(CurrencyCode::parse("VND").expect("valid"), VND);
        for bad in ["vnd", "VN", "VNDD", "V1D", ""] {
            assert_eq!(
                CurrencyCode::parse(bad),
                Err(MoneyError::CurrencyCode),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn money_serialises_to_the_documented_wire_shape() {
        let json = serde_json::to_string(&vnd(150_000)).expect("serialise");
        assert_eq!(json, r#"{"currency_code":"VND","amount_minor":150000}"#);
        assert_eq!(
            serde_json::from_str::<Money>(&json).expect("deserialise"),
            vnd(150_000)
        );
    }

    #[test]
    fn a_fractional_amount_is_rejected_rather_than_coerced() {
        // Delivery marketplaces send prices as JSON floats. A float in a money
        // position is a bug worth failing on, not rounding away silently.
        let error =
            serde_json::from_str::<Money>(r#"{"currency_code":"VND","amount_minor":150000.5}"#);
        assert!(error.is_err(), "150000.5 must not deserialise into i64");
    }

    // -- properties --------------------------------------------------------

    proptest! {
        /// The third data-correctness law: the parts of a split sum EXACTLY to the
        /// original. `pos-spec.md` §14.3 asks CI to assert this.
        #[test]
        fn allocation_always_sums_to_the_original(
            amount in -1_000_000_000_000_i64..1_000_000_000_000,
            weights in prop::collection::vec(0_i64..1_000_000, 1..24),
        ) {
            prop_assume!(weights.iter().sum::<i64>() > 0);
            let parts = vnd(amount).allocate(&weights).expect("allocates");
            prop_assert_eq!(parts.len(), weights.len());
            let total: i64 = parts.iter().map(|part| part.amount_minor).sum();
            prop_assert_eq!(total, amount);
        }

        #[test]
        fn an_even_split_always_sums_to_the_original(
            amount in -1_000_000_000_000_i64..1_000_000_000_000,
            parts in 1_usize..64,
        ) {
            let split = vnd(amount).split_into(parts).expect("splits");
            prop_assert_eq!(split.len(), parts);
            let total: i64 = split.iter().map(|part| part.amount_minor).sum();
            prop_assert_eq!(total, amount);
        }

        /// Pins the spread that remainder-to-last actually produces, so the
        /// trade-off is visible rather than assumed. See
        /// `concentrates_the_whole_remainder_on_the_last_part` for why it is not
        /// bounded at one.
        #[test]
        fn the_spread_of_an_even_split_is_the_remainder(
            amount in 0_i64..1_000_000_000,
            parts in 1_usize..64,
        ) {
            let split = vnd(amount).split_into(parts).expect("splits");
            let smallest = split.iter().map(|p| p.amount_minor).min().unwrap_or(0);
            let largest = split.iter().map(|p| p.amount_minor).max().unwrap_or(0);
            let divisor = i64::try_from(parts).unwrap_or(1);
            prop_assert_eq!(largest - smallest, amount % divisor);
        }

        #[test]
        fn dividing_by_one_is_the_identity(
            value in -1_000_000_000_000_i128..1_000_000_000_000,
        ) {
            for mode in [
                Rounding::Floor, Rounding::Ceil, Rounding::TowardZero,
                Rounding::HalfUp, Rounding::HalfEven,
            ] {
                prop_assert_eq!(div_round(value, 1, mode), Ok(i64::try_from(value).unwrap_or(0)));
            }
        }

        /// Floor is the lower bound and Ceil the upper, with every nearest-mode
        /// between them. A mode that escaped this ordering would be misnamed.
        #[test]
        fn the_modes_are_ordered(
            numerator in -1_000_000_000_i128..1_000_000_000,
            denominator in 1_i128..1_000_000,
        ) {
            let floor = div_round(numerator, denominator, Rounding::Floor).expect("floor");
            let ceil = div_round(numerator, denominator, Rounding::Ceil).expect("ceil");
            prop_assert!(floor <= ceil);
            prop_assert!(ceil - floor <= 1);
            for mode in [Rounding::TowardZero, Rounding::HalfUp, Rounding::HalfEven] {
                let value = div_round(numerator, denominator, mode).expect("mode");
                prop_assert!(value >= floor && value <= ceil, "{:?} left the bounds", mode);
            }
        }

        /// Rounding is sign-symmetric for every mode that should be, which is what
        /// makes a refund reverse a sale exactly.
        #[test]
        fn half_modes_are_sign_symmetric(
            numerator in -1_000_000_000_i128..1_000_000_000,
            denominator in 1_i128..1_000_000,
        ) {
            for mode in [Rounding::TowardZero, Rounding::HalfUp, Rounding::HalfEven] {
                let positive = div_round(numerator, denominator, mode).expect("value");
                let negated = div_round(-numerator, denominator, mode).expect("negated");
                prop_assert_eq!(positive, -negated, "{:?} is not sign-symmetric", mode);
            }
        }
    }
}
