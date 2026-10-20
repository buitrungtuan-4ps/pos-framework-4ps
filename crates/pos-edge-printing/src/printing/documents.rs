// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The documents a till prints: the receipt and its copy, the pre-bill, the shift reports, and the
//! test page.

use pos_core::billing::{BillTotals, TaxComponentLine};
use pos_ports::printer::{PrintBlock, PrintDocument, TextStyle};
use pos_proto::devices::PublishedDevice;
use pos_proto::store_profile::StoreProfile;

use crate::app::{BuyerDetails, CombinedReport, DrawerFigures, ReceiptLine, ShiftReport};
use crate::paper_labels::PaperLabels;
use crate::printing::blocks::{
    MoneyStyle, amount_line, format_money, percent, row_blocks, short_reference, totals_blocks,
};
use crate::printing::language::{SecondLanguage, Words};

/// A centred line, emphasised or not — the header block's shape.
pub(super) fn centred(line: impl Into<String>, emphasised: bool) -> PrintBlock {
    PrintBlock::Text {
        line: line.into(),
        style: TextStyle {
            emphasised,
            double_size: false,
            centred: true,
        },
    }
}

/// The page a manager prints to check a printer is wired, from the Devices screen.
///
/// Says which printer it is, where the edge reached it and when, so the paper in a technician's hand
/// identifies itself — a shop with three printers learns which one is which by printing. The last
/// line answers the next question, whether the printer will print Vietnamese: a sample drawn by the
/// rasteriser when this box has fonts (ADR-0102), or an ASCII line saying it has none. Never a
/// Vietnamese line the box cannot draw — that refuses the whole page, and a wired printer would look
/// broken.
#[must_use]
pub fn test_page_document(
    profile: &StoreProfile,
    device: &PublishedDevice,
    printed_at: &str,
    can_rasterise: bool,
) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.push(centred("Printer test", true));
    for line in [
        format!("Printer: {}", device.name.as_str()),
        format!("Address: {}", device.address),
        format!("Printed: {printed_at}"),
        if can_rasterise {
            "Tiếng Việt: Phở bò · Bánh mì · Cà phê sữa đá".to_owned()
        } else {
            "Vietnamese text: no fonts on this store PC, so it will not print".to_owned()
        },
    ] {
        blocks.push(PrintBlock::Text {
            line,
            style: TextStyle::default(),
        });
    }
    PrintDocument { blocks }
}

/// The customer's receipt for a settled bill — and, where a country's law is satisfied by it, its
/// tax invoice ([ADR-0106](../../../../docs/adr/0106-the-store-is-a-legal-person.md)).
///
/// Composed from four things and nothing else: who the store is, **what was sold**, what the bill
/// came to, and the store's own gapless receipt number — which is explicitly **not** a legal invoice
/// number ([ADR-0025](../../../../docs/adr/0025-receipt-number-authority.md)); a country whose law wants
/// an allocated number gets it from `Fiscalization` and prints it beside this one.
///
/// # Every block is omitted when it has nothing to say
///
/// No registration number, no registration line. No service charge, no service-charge line. No
/// components, no indented parts. A store that has filled nothing in gets the receipt this framework
/// printed before ADR-0106 — a number and a total — rather than a page of blank labels, because an
/// empty label on a legal document reads as a value somebody forgot to type.
///
/// # The buyer block, when there is one
///
/// A Japanese qualified invoice issued to a company must carry that company's name and 登録番号, and
/// an Indian tax invoice under Rule 46 must carry the buyer's name, address and GSTIN — without them
/// the buyer cannot claim input tax, which is the reason a business asks for the document at all.
/// The block is printed only when a buyer was captured
/// ([ADR-0107](../../../../docs/adr/0107-the-buyer-is-a-subject.md)); an ordinary retail sale is the
/// receipt it always was.
///
/// # The lines, which every regime asks for
///
/// One row per line sold: the name, then the quantity at the unit price with the extended amount
/// beside it ([ADR-0129](../../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)). Vietnam's
/// Decree 123/2020 Art. 10 asks for exactly this set (minus a unit of measure the catalog has not
/// got), and a Japanese qualified invoice, an Indian tax invoice and an EU one all want the goods
/// named and counted. The rows sum to `subtotal` printed under them, because they are the same
/// records it was assembled from.
///
/// The name gets a line of its own. It is the one field with no bound on its length, and a receipt
/// whose amounts have been pushed off the right-hand edge of the paper is not a document anybody can
/// check.
///
/// # The tax section is per rate, not per line
///
/// Which is what a Japanese qualified invoice (8 % and 10 % separately) and an Indian tax invoice
/// (CGST and SGST separately) both ask for, what `BillTotals::tax_lines` already computes, and what
/// keeps a long bill's receipt short.
///
/// # A second language
///
/// With `second` (ADR-0160 decision 2), each label prints in both languages, as `Tạm tính /
/// Subtotal` where the two fit on one line and with the second under the first, indented, where
/// they do not; and each name the second language has prints under its row's, indented.
#[must_use]
pub fn receipt_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    receipt_number: u64,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    buyer: Option<&BuyerDetails>,
    second: Option<&SecondLanguage>,
) -> PrintDocument {
    bill_document(
        BillDocument::Receipt { receipt_number },
        profile,
        money,
        Words { labels, second },
        lines,
        totals,
        buyer,
    )
}

/// A copy of a settled bill's receipt
/// ([ADR-0164](../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)):
/// the receipt as it printed, under the same number, with **COPY** and which copy it is, and when
/// it was printed, under that number. `reprinted` is the store's local time, as the caller formats
/// it. In a second language as [`receipt_document`] is.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the receipt's own arguments plus the two a copy adds; a struct for them would be \
              built at one call site and taken apart at the next"
)]
pub fn receipt_copy_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    receipt_number: u64,
    copy_number: u32,
    reprinted: &str,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    buyer: Option<&BuyerDetails>,
    second: Option<&SecondLanguage>,
) -> PrintDocument {
    bill_document(
        BillDocument::ReceiptCopy {
            receipt_number,
            copy_number,
            reprinted,
        },
        profile,
        money,
        Words { labels, second },
        lines,
        totals,
        buyer,
    )
}

/// The check a table asks for before it pays: the rows the receipt will print and what they come to,
/// headed as a pre-bill and with **no receipt number** (roadmap-v3 B2.1).
///
/// No number because nothing has been settled. The gapless receipt sequence (ADR-0025) numbers
/// settled bills, and a pre-bill printed three times while a table argues about the wine is not three
/// sales; printing one under a number would leave a hole in the series the day nobody paid. The
/// `reference` is the order's short reference, the one its kitchen tickets carry, so a server holding
/// the paper and a cook holding a ticket are talking about the same table.
///
/// "Not a receipt" is printed under the total because a pre-bill left on the table looks like one to
/// a guest who is not looking closely, and the difference is whether anything was paid. In a second
/// language as [`receipt_document`] is.
#[must_use]
pub fn pre_bill_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    reference: &str,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    second: Option<&SecondLanguage>,
) -> PrintDocument {
    bill_document(
        BillDocument::PreBill { reference },
        profile,
        money,
        Words { labels, second },
        lines,
        totals,
        None,
    )
}

/// A closed shift's drawer on paper: the float, the cash taken, what was paid in and out outside a
/// sale, what that should come to, what was counted, and the difference.
///
/// The drawer's own arithmetic, in the order a supervisor checks it: the float, the cash taken and
/// the paid in, less the paid out, add up to the expectation, and the count less the expectation is
/// the variance. A shift that paid nothing in or out prints neither line. Printed because the domain asks for it on close
/// (`Effect::PrintShiftReport`), which it has done since the shift machine was written while nothing
/// turned the effect into paper — a cashier handing over a drawer had only a screen to show for it.
///
/// "Over" or "short" is written out beside the variance rather than left to its sign, because a
/// minus sign on thermal paper is a single dot-width wide and the word is the thing being checked.
#[must_use]
pub fn shift_report_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    report: &ShiftReport,
) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.push(centred(labels.shift_report, true));
    blocks.push(centred(
        short_reference(&report.shift_id.to_string()),
        false,
    ));
    figure_blocks(&mut blocks, money, labels, &report.figures());
    component_blocks(&mut blocks, money, labels, &report.tax_components);
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// Several drawers closed at once on one slip
/// ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 10): the store's
/// totals, then each drawer's report under its till's name, each set of figures printed as
/// [`shift_report_document`] prints one drawer's.
#[must_use]
pub fn combined_shift_report_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    combined: &CombinedReport,
) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.push(centred(labels.shift_report, true));
    blocks.push(centred(labels.store_totals, true));
    figure_blocks(&mut blocks, money, labels, &combined.totals);
    component_blocks(&mut blocks, money, labels, &combined.tax_components);
    for (till, report) in &combined.drawers {
        if let Some(till) = till {
            blocks.push(centred(till.as_str(), true));
        }
        blocks.push(centred(
            short_reference(&report.shift_id.to_string()),
            false,
        ));
        figure_blocks(&mut blocks, money, labels, &report.figures());
        component_blocks(&mut blocks, money, labels, &report.tax_components);
    }
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// A shift's tax by component and rate under its heading, each as its tax line prints it on a
/// receipt (ADR-0168 decision 5). Nothing at all where the shift's bills recorded none, so a store
/// whose `tax` node names no components prints the report it always did; and no total, since a bill
/// that recorded none is not in it.
fn component_blocks(
    blocks: &mut Vec<PrintBlock>,
    money: &MoneyStyle,
    labels: &PaperLabels,
    components: &[TaxComponentLine],
) {
    if components.is_empty() {
        return;
    }
    blocks.push(centred(labels.tax_by_component, true));
    blocks.extend(components.iter().map(|component| {
        amount_line(
            &format!(
                "{} {}",
                component.name,
                percent(component.rate_basis_points)
            ),
            component.tax,
            money,
        )
    }));
}

/// A drawer's figures, or several drawers' together, in the order a supervisor checks them: the
/// float, the cash taken and the paid in, less the paid out, add up to the expectation, and the
/// count less the expectation is the variance, with the verdict in words beneath it.
fn figure_blocks(
    blocks: &mut Vec<PrintBlock>,
    money: &MoneyStyle,
    labels: &PaperLabels,
    figures: &DrawerFigures,
) {
    blocks.push(amount_line(
        labels.opening_float,
        figures.opening_float,
        money,
    ));
    blocks.push(amount_line(
        labels.cash_taken,
        figures.cash_collected,
        money,
    ));
    // Only when there was one, so a shift that paid nothing in or out prints the report it always
    // did; when there was, the lines are what make the expectation add up on paper (ADR-0165).
    if figures.paid_in.amount_minor != 0 {
        blocks.push(amount_line(labels.paid_in, figures.paid_in, money));
    }
    if figures.paid_out.amount_minor != 0 {
        blocks.push(amount_line(labels.paid_out, figures.paid_out, money));
    }
    blocks.push(amount_line(
        labels.expected_in_drawer,
        figures.expected_amount,
        money,
    ));
    blocks.push(amount_line(labels.counted, figures.counted_amount, money));
    blocks.push(amount_line(labels.variance, figures.variance, money));
    let verdict = match figures.variance.amount_minor.cmp(&0) {
        core::cmp::Ordering::Less => labels.short,
        core::cmp::Ordering::Equal => labels.balanced,
        core::cmp::Ordering::Greater => labels.over,
    };
    blocks.push(centred(verdict, true));
}

/// Which document a bill's rows and totals are printed as.
///
/// One body with a kind rather than two functions, because the two documents are the same rows and
/// the same arithmetic and differ only in how they are headed and signed off. A second copy of the
/// body is how a receipt and a pre-bill come to disagree about the tax line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BillDocument<'a> {
    /// The guest's receipt for a settled bill, under its gapless number.
    Receipt { receipt_number: u64 },
    /// A copy of that receipt, under the same number and marked as a copy (ADR-0164).
    ReceiptCopy {
        receipt_number: u64,
        copy_number: u32,
        reprinted: &'a str,
    },
    /// The check before payment: headed as such, under the order's reference, and unnumbered.
    PreBill { reference: &'a str },
}

fn bill_document(
    kind: BillDocument<'_>,
    profile: &StoreProfile,
    money: &MoneyStyle,
    words: Words<'_>,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    buyer: Option<&BuyerDetails>,
) -> PrintDocument {
    let mut blocks = seller_blocks(profile);
    blocks.extend(heading_blocks(kind, words));
    if let Some(buyer) = buyer {
        blocks.extend(buyer_blocks(buyer, words));
    }

    // What was sold, before what it came to. A settle always has rows — an empty bill has nothing to
    // allocate across and `assemble` refuses it — so this is not a "some receipts are itemised"
    // branch; passing none is how the cases below hold the rest of the document still while they
    // check one block of it.
    blocks.extend(row_blocks(lines, money, words.second));

    blocks.extend(totals_blocks(totals, money, words));

    blocks.push(PrintBlock::Text {
        line: format_money(totals.total_due, money),
        style: TextStyle {
            emphasised: true,
            double_size: true,
            centred: true,
        },
    });
    if matches!(kind, BillDocument::PreBill { .. }) {
        blocks.extend(words.centred(|labels| labels.not_a_receipt, "", false));
    }

    blocks.extend(
        profile
            .contact_lines
            .iter()
            .chain(profile.footer_lines.iter())
            .filter(|line| !line.trim().is_empty())
            .map(|line| centred(line.clone(), false)),
    );
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// Who sold. The trading name leads and the legal name backs it up; the address and registration
/// follow, each only if it is there.
fn seller_blocks(profile: &StoreProfile) -> Vec<PrintBlock> {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.extend(
        profile
            .address_lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| centred(line.clone(), false)),
    );
    if let Some(registration) = profile.registration_line() {
        blocks.push(centred(registration, false));
    }
    blocks
}

/// What the paper is: a receipt under its number, a copy of one, or the check before payment.
fn heading_blocks(kind: BillDocument<'_>, words: Words<'_>) -> Vec<PrintBlock> {
    let mut blocks = Vec::new();
    match kind {
        BillDocument::Receipt { receipt_number } => {
            blocks.push(centred(format!("#{receipt_number}"), false));
        }
        // The number the original printed, then what this paper is: a copy, which copy, and when it
        // was printed. A copy the guest takes away must not read as a second sale.
        BillDocument::ReceiptCopy {
            receipt_number,
            copy_number,
            reprinted,
        } => {
            blocks.push(centred(format!("#{receipt_number}"), false));
            blocks.extend(words.centred(|labels| labels.copy, "", true));
            blocks.extend(words.centred(
                |labels| labels.reprint,
                &format!(" {copy_number} - {reprinted}"),
                false,
            ));
        }
        BillDocument::PreBill { reference } => {
            blocks.extend(words.centred(|labels| labels.pre_bill, "", true));
            blocks.push(centred(reference, false));
        }
    }
    blocks
}

/// Who bought, on a B2B invoice. Left-aligned rather than centred: this is a party to the
/// document, not a letterhead, and the two must not read as one block.
fn buyer_blocks(buyer: &BuyerDetails, words: Words<'_>) -> Vec<PrintBlock> {
    let mut blocks =
        Words::lines(words.label(|labels| labels.bill_to, &format!(": {}", buyer.name)));
    for line in [buyer.tax_code.as_ref(), buyer.address.as_ref()]
        .into_iter()
        .flatten()
        .filter(|line| !line.trim().is_empty())
    {
        blocks.push(PrintBlock::Text {
            line: line.clone(),
            style: TextStyle::default(),
        });
    }
    blocks
}
