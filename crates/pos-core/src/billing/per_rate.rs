// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Tax rounded once per rate rather than once per class
//! ([ADR-0226](../../../../docs/adr/0226-a-japanese-receipt-is-a-qualified-invoice.md) decision 1).
//!
//! Japan's qualified invoice rounds the consumption tax once for each rate on the invoice. The bill
//! keeps one line per class, as every report reads it, so the classes at one rate are summed, the
//! rate's tax is taken once on that sum, and the rounded tax is shared back across the classes in
//! proportion to what each contributed, with `Money::allocate`, so the lines still add up to it.

use pos_proto::money::Money;

use super::{BillInput, TaxLine, rate_on_channel, split_components, tax_for};
use crate::error::DomainError;

/// The class lines with each rate's tax rounded once, and their new total.
///
/// # Errors
///
/// [`DomainError::TaxRateNotConfigured`] if a line's class has no rate on this channel (which the
/// per-class pass already refused), and [`DomainError::Money`] on overflow.
pub(super) fn round_once_per_rate(
    input: &BillInput<'_>,
    mut lines: Vec<TaxLine>,
) -> Result<(Vec<TaxLine>, Money), DomainError> {
    let mut rates: Vec<u32> = lines.iter().map(|line| line.rate_basis_points).collect();
    rates.sort_unstable();
    rates.dedup();
    for rate_basis_points in rates {
        let at_rate: Vec<usize> = (0..lines.len())
            .filter(|index| {
                lines
                    .get(*index)
                    .is_some_and(|line| line.rate_basis_points == rate_basis_points)
            })
            .collect();
        if at_rate.len() < 2 {
            continue;
        }
        // What each class was charged on, as quoted: the base, plus its tax where prices include it.
        let mut quoted: Vec<Money> = Vec::with_capacity(at_rate.len());
        for index in &at_rate {
            let Some(line) = lines.get(*index) else {
                continue;
            };
            quoted.push(if input.prices_include_tax {
                line.taxable_base.checked_add(line.tax)?
            } else {
                line.taxable_base
            });
        }
        let Some(first) = at_rate.first().and_then(|index| lines.get(*index)) else {
            continue;
        };
        let rate = rate_on_channel(input, first.tax_class_id)?;
        let mut sum = Money::zero(input.currency_code);
        for amount in &quoted {
            sum = sum.checked_add(*amount)?;
        }
        let weights: Vec<i64> = quoted.iter().map(|amount| amount.amount_minor).collect();
        if sum.amount_minor <= 0 || weights.iter().any(|weight| *weight < 0) {
            continue;
        }
        let (rate_tax, _base, _parts) =
            tax_for(sum, rate, &[], input.prices_include_tax, input.tax_rounding)?;
        let shares = rate_tax.allocate(&weights)?;
        for ((index, share), amount) in at_rate.iter().zip(shares).zip(quoted) {
            let Some(line) = lines.get_mut(*index) else {
                continue;
            };
            let components = input
                .rates
                .components_for(line.tax_class_id, input.sales_channel);
            line.tax = share;
            line.taxable_base = if input.prices_include_tax {
                amount.checked_sub(share)?
            } else {
                amount
            };
            line.components = split_components(share, components)?;
        }
    }
    let mut total = Money::zero(input.currency_code);
    for line in &lines {
        total = total.checked_add(line.tax)?;
    }
    Ok((lines, total))
}
