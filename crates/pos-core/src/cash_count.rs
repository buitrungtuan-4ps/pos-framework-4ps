// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A drawer counted note by note
//! ([ADR-0186](../../../docs/adr/0186-a-drawer-is-counted-by-denomination.md)).
//!
//! A float or a close may be entered as one amount, or as a quantity of each note and coin plus an
//! amount the list does not name. The breakdown is kept beside the amount on the event, and the
//! amount keeps its meaning, so the two must agree.

use pos_proto::events::DenominationCount;
use pos_proto::money::Money;

/// How a drawer was counted, note by note: empty when it was entered as one amount.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountBreakdown {
    /// One row per note or coin counted.
    pub denomination_counts: Vec<DenominationCount>,
    /// Anything the rows do not name.
    pub uncounted_amount: Option<Money>,
}

impl CountBreakdown {
    /// Whether the count was entered as one amount, with no breakdown at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.denomination_counts.is_empty() && self.uncounted_amount.is_none()
    }

    /// Whether this breakdown adds up to `amount`: every row a positive note in `amount`'s currency,
    /// the uncounted part not negative and in that currency too, and the sum exactly `amount`. An
    /// empty breakdown states nothing, so it agrees with any amount.
    #[must_use]
    pub fn adds_up_to(&self, amount: Money) -> bool {
        if self.is_empty() {
            return true;
        }
        let currency = amount.currency_code;
        let uncounted = self.uncounted_amount.unwrap_or(Money::zero(currency));
        if uncounted.currency_code != currency || uncounted.amount_minor < 0 {
            return false;
        }
        let total = self
            .denomination_counts
            .iter()
            .try_fold(uncounted.amount_minor, |sum, row| {
                if row.denomination.currency_code != currency || row.denomination.amount_minor <= 0
                {
                    return None;
                }
                row.denomination
                    .amount_minor
                    .checked_mul(i64::from(row.quantity))
                    .and_then(|value| sum.checked_add(value))
            });
        total == Some(amount.amount_minor)
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::events::DenominationCount;
    use pos_proto::money::{CurrencyCode, Money};

    use super::CountBreakdown;

    fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    fn rows(rows: &[(i64, u32)]) -> Vec<DenominationCount> {
        rows.iter()
            .map(|&(denomination, quantity)| DenominationCount {
                denomination: vnd(denomination),
                quantity,
            })
            .collect()
    }

    #[test]
    fn a_count_typed_as_one_amount_states_nothing_to_disagree_with() {
        assert!(CountBreakdown::default().adds_up_to(vnd(1_234_000)));
    }

    #[test]
    fn rows_and_the_uncounted_part_add_up_to_the_amount() {
        let breakdown = CountBreakdown {
            denomination_counts: rows(&[(500_000, 2), (100_000, 2), (1_000, 0)]),
            uncounted_amount: Some(vnd(3_000)),
        };
        assert!(breakdown.adds_up_to(vnd(1_203_000)));
        assert!(
            !breakdown.adds_up_to(vnd(1_200_000)),
            "the uncounted 3,000 is missing"
        );
    }

    #[test]
    fn a_note_worth_nothing_a_negative_rest_or_another_currency_does_not_add_up() {
        let nothing = CountBreakdown {
            denomination_counts: rows(&[(0, 5), (10_000, 1)]),
            uncounted_amount: None,
        };
        assert!(!nothing.adds_up_to(vnd(10_000)));
        let negative = CountBreakdown {
            denomination_counts: rows(&[(10_000, 2)]),
            uncounted_amount: Some(vnd(-5_000)),
        };
        assert!(!negative.adds_up_to(vnd(15_000)));
        let yen = CountBreakdown {
            denomination_counts: vec![DenominationCount {
                denomination: Money::new(CurrencyCode::JPY, 1_000),
                quantity: 1,
            }],
            uncounted_amount: None,
        };
        assert!(!yen.adds_up_to(vnd(1_000)));
    }

    #[test]
    fn a_count_too_large_to_add_does_not_add_up() {
        let huge = CountBreakdown {
            denomination_counts: rows(&[(i64::MAX, 2)]),
            uncounted_amount: None,
        };
        assert!(!huge.adds_up_to(vnd(1)));
    }
}
