// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The country conformance packs: golden bills that a country module's own values must produce.
//!
//! A port suite asks whether an adapter keeps a port's promises. This asks something narrower,
//! about one country module: given its rate table, its tax posture and its cash rounding, does a
//! bill come out the way the case says it should? It runs each case through
//! [`pos_core::billing::assemble`], the same arithmetic every till and the cloud use.
//!
//! Each country keeps its own table in `countries/<cc>/tests/conformance.rs`. Each case names the
//! module value it relies on, and `countries/<cc>/CONFORMANCE.md` maps the legal requirements a
//! human must confirm to the cases that exercise them.
//!
//! # What a case may use
//!
//! Only what the module already publishes ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)):
//! rates, components, whether prices include tax, and the cash increment, all read from
//! [`LocalePack`]. A case supplies the line prices, which are illustrative inputs, and the totals
//! it expects, which are worked out by hand in the case's comment. Tax rounds half up, the default
//! for `locale.tax_rounding`.

use pos_core::DomainError;
use pos_core::billing::{BillInput, BillTotals, ClassBase, assemble};
use pos_proto::locale::LocalePack;
use pos_proto::money::{Money, Rounding};
use pos_proto::{SalesChannel, TaxClassId};

/// One sold line: its tax class and what it comes to, in minor units.
#[derive(Debug, Clone, Copy)]
pub struct Line {
    /// The line's tax class.
    pub tax_class_id: TaxClassId,
    /// The line's net, as quoted on the menu, in minor units.
    pub net_minor: i64,
}

/// One named part of an expected tax line: name, rate in basis points, tax in minor units.
pub type Component = (&'static str, u32, i64);

/// A tax line the bill must print.
#[derive(Debug, Clone)]
pub struct TaxLine {
    /// The class the line is for.
    pub tax_class_id: TaxClassId,
    /// The base printed beside the tax, in minor units.
    pub taxable_base_minor: i64,
    /// The rate, in basis points.
    pub rate_basis_points: u32,
    /// The tax, in minor units.
    pub tax_minor: i64,
    /// The breakdown, in order. Empty where the rate prints as one number.
    pub components: Vec<Component>,
}

/// What a case expects.
#[derive(Debug, Clone)]
pub enum Expected {
    /// The bill assembles, with these figures.
    Totals {
        /// One per class, in the order the classes first appear in the lines.
        tax_lines: Vec<TaxLine>,
        /// `BillTotals::subtotal`: net of tax under an inclusive posture.
        subtotal_minor: i64,
        /// `BillTotals::tax_total`.
        tax_total_minor: i64,
        /// `BillTotals::rounding_adjustment`.
        rounding_minor: i64,
        /// `BillTotals::total_due`: what the guest pays.
        total_due_minor: i64,
    },
    /// The bill is refused because a class has no rate on the channel.
    RefusedNoRate,
}

/// [`Expected::Totals`], in the order a receipt reads: tax lines, subtotal, tax, rounding, total.
#[must_use]
pub fn totals(
    tax_lines: Vec<TaxLine>,
    subtotal_minor: i64,
    tax_total_minor: i64,
    rounding_minor: i64,
    total_due_minor: i64,
) -> Expected {
    Expected::Totals {
        tax_lines,
        subtotal_minor,
        tax_total_minor,
        rounding_minor,
        total_due_minor,
    }
}

/// One golden bill.
#[derive(Debug, Clone)]
pub struct Case {
    /// The case's number, which `CONFORMANCE.md` cites.
    pub number: u32,
    /// What the case shows, in one line.
    pub title: &'static str,
    /// The module values the case relies on, by name.
    pub cites: &'static [&'static str],
    /// The channel the order came in on.
    pub channel: SalesChannel,
    /// The sold lines.
    pub lines: Vec<Line>,
    /// What the bill must come to.
    pub expected: Expected,
}

/// Runs every case against `pack`, and returns one message per case that did not hold.
///
/// An empty list means the pack conforms. A duplicated case number is reported too, because
/// `CONFORMANCE.md` cites cases by number.
#[must_use]
pub fn failures(pack: &LocalePack, cases: &[Case]) -> Vec<String> {
    let mut found = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        if cases
            .iter()
            .skip(index.saturating_add(1))
            .any(|other| other.number == case.number)
        {
            found.push(format!("case {} is numbered twice", case.number));
        }
        if let Err(message) = check(pack, case) {
            found.push(format!("case {} ({}): {message}", case.number, case.title));
        }
    }
    found
}

/// Runs one case.
fn check(pack: &LocalePack, case: &Case) -> Result<(), String> {
    let currency = pack.currency_code;
    let bases = class_bases(pack, &case.lines)?;
    let input = BillInput {
        currency_code: currency,
        class_bases: &bases,
        bill_discount: Money::zero(currency),
        comps: Money::zero(currency),
        service_charge: Money::zero(currency),
        service_charge_taxable: false,
        service_charge_tax_class: None,
        rates: &pack.tax_rate_table,
        sales_channel: case.channel,
        cash_rounding_increment: pack.cash_rounding_increment,
        tax_rounding: Rounding::HalfUp,
        prices_include_tax: pack.prices_include_tax,
        fee_rules: &[],
        waived_fee_ids: &[],
        lines: &[],
    };
    match (&case.expected, assemble(&input)) {
        (Expected::RefusedNoRate, Err(DomainError::TaxRateNotConfigured { .. })) => Ok(()),
        (Expected::RefusedNoRate, other) => Err(format!(
            "expected a refusal for a class with no rate, got {other:?}"
        )),
        (expected @ Expected::Totals { .. }, Ok(totals)) => compare(expected, &totals),
        (Expected::Totals { .. }, Err(error)) => Err(format!("expected totals, got {error:?}")),
    }
}

/// Sums the lines into one base per class, in the order the classes first appear.
fn class_bases(pack: &LocalePack, lines: &[Line]) -> Result<Vec<ClassBase>, String> {
    let mut bases: Vec<ClassBase> = Vec::new();
    for line in lines {
        let amount = Money::new(pack.currency_code, line.net_minor);
        match bases
            .iter_mut()
            .find(|base| base.tax_class_id == line.tax_class_id)
        {
            Some(base) => {
                base.amount = base
                    .amount
                    .checked_add(amount)
                    .map_err(|error| format!("{error:?}"))?;
            }
            None => bases.push(ClassBase {
                tax_class_id: line.tax_class_id,
                amount,
            }),
        }
    }
    Ok(bases)
}

/// Compares the assembled totals with what the case expects, figure by figure.
fn compare(expected: &Expected, totals: &BillTotals) -> Result<(), String> {
    let Expected::Totals {
        tax_lines,
        subtotal_minor,
        tax_total_minor,
        rounding_minor,
        total_due_minor,
    } = expected
    else {
        return Err("a refusal case reached the totals comparison".to_owned());
    };
    let figures = [
        ("subtotal", *subtotal_minor, totals.subtotal.amount_minor),
        ("tax_total", *tax_total_minor, totals.tax_total.amount_minor),
        (
            "rounding_adjustment",
            *rounding_minor,
            totals.rounding_adjustment.amount_minor,
        ),
        ("total_due", *total_due_minor, totals.total_due.amount_minor),
    ];
    for (name, want, got) in figures {
        if want != got {
            return Err(format!("{name}: expected {want}, got {got}"));
        }
    }
    if tax_lines.len() != totals.tax_lines.len() {
        return Err(format!(
            "expected {} tax lines, got {}",
            tax_lines.len(),
            totals.tax_lines.len()
        ));
    }
    for (want, got) in tax_lines.iter().zip(&totals.tax_lines) {
        let got_components: Vec<(&str, u32, i64)> = got
            .components
            .iter()
            .map(|part| {
                (
                    part.name.as_str(),
                    part.rate_basis_points,
                    part.tax.amount_minor,
                )
            })
            .collect();
        let same = want.tax_class_id == got.tax_class_id
            && want.taxable_base_minor == got.taxable_base.amount_minor
            && want.rate_basis_points == got.rate_basis_points
            && want.tax_minor == got.tax.amount_minor
            && want.components == got_components;
        if !same {
            return Err(format!("tax line: expected {want:?}, got {got:?}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Case, Expected, Line, TaxLine, failures, totals};
    use pos_proto::locale::{CountryCode, LocalePack, NumberFormat, TaxRate, TaxRateTable};
    use pos_proto::money::CurrencyCode;
    use pos_proto::text::TranslationKey;
    use pos_proto::{SalesChannel, TaxClassId, Ulid};

    fn class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(1))
    }

    /// A 10 % exclusive pack with no cash rounding: illustrative values, not a country's.
    fn pack() -> LocalePack {
        LocalePack {
            country_code: CountryCode::ZZ,
            currency_code: CurrencyCode::VND,
            currency_exponent: 0,
            tax_rate_table: TaxRateTable::new().with(
                class(),
                SalesChannel::DineIn,
                TaxRate::from_percent(10),
            ),
            number_format: NumberFormat::default(),
            default_language: TranslationKey::new("en"),
            default_retention_days: 365,
            prices_include_tax: false,
            cash_rounding_increment: None,
            cash_denominations: Vec::new(),
        }
    }

    fn case(
        number: u32,
        channel: SalesChannel,
        tax_minor: i64,
        components: Vec<super::Component>,
    ) -> Case {
        Case {
            number,
            title: "a test case",
            cites: &[],
            channel,
            lines: vec![Line {
                tax_class_id: class(),
                net_minor: 1_000,
            }],
            expected: totals(
                vec![TaxLine {
                    tax_class_id: class(),
                    taxable_base_minor: 1_000,
                    rate_basis_points: 1_000,
                    tax_minor,
                    components,
                }],
                1_000,
                tax_minor,
                0,
                1_000 + tax_minor,
            ),
        }
    }

    #[test]
    fn a_right_case_passes() {
        assert_eq!(
            failures(&pack(), &[case(1, SalesChannel::DineIn, 100, Vec::new())]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_wrong_figure_is_reported_with_its_case_number() {
        let found = failures(&pack(), &[case(7, SalesChannel::DineIn, 99, Vec::new())]);
        assert_eq!(found.len(), 1);
        assert!(
            found.iter().all(|message| message.starts_with("case 7 ")),
            "{found:?}"
        );
    }

    #[test]
    fn a_missing_breakdown_is_reported() {
        let found = failures(
            &pack(),
            &[case(
                1,
                SalesChannel::DineIn,
                100,
                vec![("CGST", 500, 50), ("SGST", 500, 50)],
            )],
        );
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_refusal_is_expected_only_where_the_rate_is_missing() {
        let mut refused = case(1, SalesChannel::Takeaway, 100, Vec::new());
        refused.expected = Expected::RefusedNoRate;
        assert!(failures(&pack(), &[refused]).is_empty());

        let mut wrongly_refused = case(2, SalesChannel::DineIn, 100, Vec::new());
        wrongly_refused.expected = Expected::RefusedNoRate;
        assert_eq!(failures(&pack(), &[wrongly_refused]).len(), 1);

        assert_eq!(
            failures(&pack(), &[case(3, SalesChannel::Takeaway, 100, Vec::new())]).len(),
            1,
            "totals expected where the rate is missing"
        );
    }

    #[test]
    fn a_case_number_used_twice_is_reported() {
        let cases = [
            case(4, SalesChannel::DineIn, 100, Vec::new()),
            case(4, SalesChannel::DineIn, 100, Vec::new()),
        ];
        assert_eq!(failures(&pack(), &cases), ["case 4 is numbered twice"]);
    }
}
