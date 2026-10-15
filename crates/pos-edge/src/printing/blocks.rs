// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The blocks paper is made of: money in the store's style, a row with its amount, the totals, the
//! fees and the tax lines, and digits grouped the store's way.

use std::borrow::Cow;

use pos_core::billing::BillTotals;
use pos_ports::printer::{PrintBlock, TextStyle};
use pos_proto::locale::NumberFormat;
use pos_proto::money::Money;
use pos_proto::text::DisplayName;

use crate::app::{EdgeSession, ReceiptLine};
use crate::printing::devices::TillPrinting;
use crate::printing::language::{SecondLanguage, Words, receipt_language};
use crate::printing::tickets::format_quantity;

/// A `label            amount` line, which is how every figure on a receipt is read.
/// How this store writes money on paper: how many decimals its currency has
/// ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)) and what marks
/// its country groups and points a figure with
/// ([ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)).
///
/// One value rather than two arguments, because the exponent alone was already threaded through four
/// functions here and a second parallel parameter beside it at every call would be the third
/// occurrence AGENTS.md §2 says to extract on. It also stops a caller pairing a store's exponent with
/// another store's marks, which two positional arguments of different types invite.
#[derive(Debug, Clone)]
pub struct MoneyStyle {
    /// How many decimal places the currency has.
    pub exponent: u8,
    /// The marks the store's country writes numbers with.
    pub format: NumberFormat,
}

impl MoneyStyle {
    /// The style a store's live session is publishing.
    #[must_use]
    pub fn of(session: &EdgeSession) -> Self {
        Self {
            exponent: session.currency_exponent,
            format: session.number_format.clone(),
        }
    }
}

pub(super) fn amount_line(label: &str, amount: Money, money: &MoneyStyle) -> PrintBlock {
    PrintBlock::Text {
        line: format!("{label}  {}", format_money(amount, money)),
        style: TextStyle::default(),
    }
}

/// A tax rate in basis points as a percentage, without floating point — `250` is `2.50%`.
pub(super) fn percent(basis_points: u32) -> String {
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

/// A left-aligned line in the printer's ordinary style.
pub(super) fn plain(line: String) -> PrintBlock {
    PrintBlock::Text {
        line,
        style: TextStyle::default(),
    }
}

/// A ticket reference short enough to read across a kitchen.
///
/// The tail of a ULID, which is its random half — six characters distinguish every order a station
/// has open at once, and the whole 26 would be read out wrong.
#[must_use]
pub fn short_reference(id: &str) -> String {
    let tail: String = id.chars().rev().take(6).collect();
    tail.chars().rev().collect()
}

/// Renders money for a receipt — `VND 99,000`, or `INR 261.45` where the currency has decimals.
///
/// This printed minor units with no decimal at all, and said so on purpose: *"the decimal place is a
/// locale question and a receipt that invented one would be wrong in half the countries this
/// framework targets."* That was right while nothing in the tree held the answer. Since
/// [ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md) the store
/// publishes it, so nothing is invented — and what was being printed was not a safe abstention but a
/// wrong number on a tax document: a guest in India who paid ₹261.45 was handed a receipt reading
/// `INR 26145`.
///
/// # Why the ISO code and not the symbol
///
/// The till draws `99,000₫`, and matching it exactly would be better for a guest holding the paper
/// beside the screen. It is not worth what it costs. A thermal printer's own character set does not
/// reach `₫` ([ADR-0102](../../../docs/adr/0102-printing-any-script.md)), so the total line would
/// have to be rasterised — and a store with no font package installed prints what the printer's
/// firmware covers and nothing else. That store prints its total today. It would stop.
///
/// So the line stays ASCII: the figure is corrected, and the one thing a receipt must always carry
/// keeps working on a box nobody has finished installing.
///
/// The marks are the store's own, published on its `locale` node
/// ([ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)). This used to
/// group with `,` and point with `.` for every store on earth, and said so: *"making a country's
/// typography authoritative is a separate decision from getting its arithmetic right."* It was, it
/// has since been made, and a Vietnamese guest's receipt now reads `VND 97.900` rather than a
/// spelling nobody in the country writes.
pub(super) fn format_money(amount: Money, money: &MoneyStyle) -> String {
    let negative = amount.amount_minor < 0;
    let absolute = amount.amount_minor.unsigned_abs();
    // Integer the whole way, like every other figure in this crate: `10_u128.pow` rather than a
    // float, and a remainder rather than a fractional part.
    let scale = 10_u128.pow(u32::from(money.exponent));
    let major = u128::from(absolute) / scale;
    let minor = u128::from(absolute) % scale;
    let sign = if negative { "-" } else { "" };
    let grouped = group_digits(major, &money.format);
    if money.exponent == 0 {
        return format!("{} {sign}{grouped}", amount.currency_code);
    }
    let digits = usize::from(money.exponent);
    let point = money.format.decimal_separator;
    format!(
        "{} {sign}{grouped}{point}{minor:0digits$}",
        amount.currency_code
    )
}

/// What was sold, one row per line: its name, then the quantity at the unit price with the extended
/// amount beside it.
///
/// What the line was made with comes between, as the kitchen ticket prints it (ADR-0144): after the
/// name and before the amount, because the unit price already includes every modifier's. On a
/// bilingual receipt each name the second language has prints under the first, indented.
pub(super) fn row_blocks(
    lines: &[ReceiptLine],
    money: &MoneyStyle,
    second: Option<&SecondLanguage>,
) -> Vec<PrintBlock> {
    let mut blocks = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let names = second.and_then(|second| second.rows.get(index));
        blocks.push(plain(line.display_name.as_str().to_owned()));
        if let Some(name) = names.and_then(|names| names.item.as_ref()) {
            blocks.push(plain(format!("  {name}")));
        }
        for (position, modifier) in line.modifier_display_names.iter().enumerate() {
            blocks.push(plain(format!("  + {}", modifier.as_str())));
            if let Some(name) = names
                .and_then(|names| names.modifiers.get(position))
                .and_then(Option::as_ref)
            {
                blocks.push(plain(format!("    {name}")));
            }
        }
        blocks.push(amount_line(
            &format!(
                "  {} x {}",
                format_quantity(line.quantity),
                format_money(line.unit_price, money)
            ),
            line.line_total,
            money,
        ));
    }
    blocks
}

/// What a bill was charged, above its total: the subtotal, whatever came off or was added, each tax
/// rate, and the rounding.
///
/// `subtotal` reads net of tax under the inclusive posture (ADR-0104), so the column adds up on paper
/// in both postures — which is what makes the document check out when somebody totals it by hand.
/// Its own function so the document around it stays readable; the rows it prints are the same.
pub(super) fn totals_blocks(
    totals: &BillTotals,
    money: &MoneyStyle,
    words: Words<'_>,
) -> Vec<PrintBlock> {
    let mut blocks = words.amount(|labels| labels.subtotal, totals.subtotal, money);
    if !totals.discount_total.is_zero() {
        blocks.extend(words.amount(|labels| labels.discount, totals.discount_total, money));
    }
    if !totals.comp_total.is_zero() {
        blocks.extend(words.amount(|labels| labels.comps, totals.comp_total, money));
    }
    blocks.extend(fee_blocks(totals, money, words));
    for line in &totals.tax_lines {
        blocks.extend(tax_block(line, money, words));
    }
    // A copy whose per-rate lines could not be recovered prints the tax the settle recorded as one
    // figure (ADR-0164 decision 5). `assemble` always fills the lines, so no other document gets here.
    if totals.tax_lines.is_empty() && !totals.tax_total.is_zero() {
        blocks.extend(words.amount(|labels| labels.tax, totals.tax_total, money));
    }
    if !totals.rounding_adjustment.is_zero() {
        blocks.extend(words.amount(|labels| labels.rounding, totals.rounding_adjustment, money));
    }
    blocks
}

/// The fees a bill was charged, each on its own line under its own name
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4), in the order of its
/// rules. A bill with no fee line prints its service charge on one line, as every receipt did
/// before fees were itemised; a bill with fees prints a service charge of its own only for what no
/// fee accounts for, which is nothing on a bill the edge assembled. On a bilingual receipt a fee
/// whose rule names it in the second language prints that name as a label's second language prints.
fn fee_blocks(totals: &BillTotals, money: &MoneyStyle, words: Words<'_>) -> Vec<PrintBlock> {
    let mut blocks: Vec<PrintBlock> = totals
        .fee_lines
        .iter()
        .enumerate()
        .filter(|(_, fee)| !fee.amount.is_zero())
        .flat_map(|(index, fee)| {
            let second = words
                .second
                .and_then(|second| second.fees.get(index))
                .and_then(Option::as_ref);
            Words::lines(words.pair(
                fee.display_name.as_str(),
                second.map(DisplayName::as_str),
                &format!("  {}", format_money(fee.amount, money)),
            ))
        })
        .collect();
    let itemised = totals.fee_lines.iter().try_fold(
        Money::zero(totals.service_charge.currency_code),
        |sum, fee| sum.checked_add(fee.amount),
    );
    let rest = itemised.and_then(|fees| totals.service_charge.checked_sub(fees));
    match rest {
        Ok(rest) if rest.is_zero() => {}
        Ok(rest) => blocks.extend(words.amount(|labels| labels.service_charge, rest, money)),
        // Fees that do not add up in the bill's currency cannot be split from the service charge,
        // so the paper prints the service charge whole, as it did before fees were itemised.
        Err(_) => {
            blocks = words.amount(|labels| labels.service_charge, totals.service_charge, money);
        }
    }
    blocks
}

/// A bill's totals as the store's paper prints them.
///
/// Each fee is named in the receipt's language
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4): the store's current
/// rule's translation where it has one, and the name the bill froze where it does not. Unlike an
/// item's, a fee's frozen name is the rule's own rather than one already resolved to the display
/// language, so it is looked up in whatever language the receipt prints in.
///
/// Each tax line keeps its named components only where the store prints them, its
/// `printing.receipt_tax_components`
/// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 4).
/// Off, the paper prints the line alone; a settle records the components either way.
pub(super) fn printed_totals<'a>(
    session: &EdgeSession,
    till: &TillPrinting,
    totals: &'a BillTotals,
) -> Cow<'a, BillTotals> {
    let mut printed = Cow::Borrowed(totals);
    if let Some(language) = receipt_language(session, till)
        && totals
            .fee_lines
            .iter()
            .any(|fee| session.fee_translation(fee.fee_id, &language).is_some())
    {
        for fee in &mut printed.to_mut().fee_lines {
            if let Some(name) = session.fee_translation(fee.fee_id, &language) {
                fee.display_name = name.clone();
            }
        }
    }
    if !session.printing.receipt_tax_components()
        && totals
            .tax_lines
            .iter()
            .any(|line| !line.components.is_empty())
    {
        for line in &mut printed.to_mut().tax_lines {
            line.components.clear();
        }
    }
    printed
}

/// One tax rate's line, and the components it is made of beneath it.
///
/// Its own function because `receipt_document` is at the line ceiling and this is the part of it
/// with a shape of its own: a rate, then the parts of that rate, indented under the line they
/// explain. They are allocated out of the *rounded* tax, so they sum to it exactly
/// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)) — which is the property
/// that makes the block printable at all, rather than three figures that nearly add up.
fn tax_block(
    line: &pos_core::billing::TaxLine,
    money: &MoneyStyle,
    words: Words<'_>,
) -> Vec<PrintBlock> {
    let rest = format!(
        " {}  {}",
        percent(line.rate_basis_points),
        format_money(line.tax, money)
    );
    let mut blocks = Words::lines(words.label(|labels| labels.tax, &rest));
    blocks.extend(line.components.iter().map(|component| {
        amount_line(
            &format!(
                "  {} {}",
                component.name,
                percent(component.rate_basis_points)
            ),
            component.tax,
            money,
        )
    }));
    blocks
}

/// `1234567` as `1,234,567`, or as `1.234.567` where that is how the country writes it.
///
/// Written out rather than taken from a formatting crate, because this is a handful of lines and the
/// alternative is a dependency on a receipt path — and because `pos-edge` has no allocator pressure
/// here: a receipt has a few dozen figures on it.
///
/// `digits_per_group` is a single number, so this groups uniformly. India writes `12,34,567` —
/// three digits then pairs — which no single number can express;
/// [ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md) records that gap and
/// [ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md) deliberately leaves
/// it open, so an Indian receipt reads `1,234,567` after this change exactly as it did before.
///
/// A group size of zero would loop forever here; it cannot arrive, because the edge refuses to apply
/// a published format carrying one, but the guard is written rather than assumed.
fn group_digits(value: u128, format: &NumberFormat) -> String {
    let digits = value.to_string();
    let size = usize::from(format.digits_per_group.max(1));
    let mut grouped = String::with_capacity(digits.len() + digits.len() / size);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(size) {
            grouped.push(format.group_separator);
        }
        grouped.push(digit);
    }
    grouped
}
