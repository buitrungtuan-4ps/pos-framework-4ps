// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A shift's X on paper, or the store's across its drawers: headed as closing nothing
//! ([ADR-0187](../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
//! decisions 4 and 5).

use pos_core::business_date::{StoreTimeZone, local_time};
use pos_ports::printer::{PrintBlock, PrintDocument, TextStyle};
use pos_proto::store_profile::StoreProfile;

use crate::app::{ShiftX, StoreX, XFigures};
use crate::paper_labels::PaperLabels;
use crate::printing::blocks::{MoneyStyle, amount_line, short_reference};
use crate::printing::documents::centred;

/// What an X prints: one shift's, or the store's.
#[derive(Debug, Clone, Copy)]
pub enum XPaper<'a> {
    /// One shift's X.
    Shift(&'a ShiftX),
    /// The store's, every drawer's shift with the totals over them.
    Store(&'a StoreX),
}

/// An X on paper: "X - not a close" in the paper's language, the store's totals over several
/// shifts, and each shift under its till's name with when it opened and its figures. A store whose
/// drawers have all closed for the day says so under the heading. Names no person.
#[must_use]
pub fn x_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    zone: &StoreTimeZone,
    paper: XPaper<'_>,
) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.push(centred(labels.x_not_a_close, true));
    let shifts: Vec<&ShiftX> = match paper {
        XPaper::Shift(shift) => vec![shift],
        XPaper::Store(store) => {
            if store.all_closed {
                blocks.push(centred(labels.all_tills_closed, true));
            }
            if let (Some(totals), true) = (&store.totals, store.shifts.len() > 1) {
                blocks.push(centred(labels.store_totals, true));
                figure_blocks(&mut blocks, money, labels, totals);
            }
            store.shifts.iter().collect()
        }
    };
    for shift in shifts {
        if let Some(till) = &shift.till_name {
            blocks.push(centred(till.as_str(), true));
        }
        blocks.push(centred(short_reference(&shift.shift_id.to_string()), false));
        if let Ok(opened) = local_time(shift.opened_time, zone) {
            blocks.push(text(&format!("{}: {opened}", labels.opened_at)));
        }
        figure_blocks(&mut blocks, money, labels, &shift.figures);
    }
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// One plain line.
fn text(line: &str) -> PrintBlock {
    PrintBlock::Text {
        line: line.to_owned(),
        style: TextStyle::default(),
    }
}

/// An X's figures: sales and bills, each tender that took something, then what reduced or moved
/// the drawer where there was any, then the expected cash where the reader may see it.
fn figure_blocks(
    blocks: &mut Vec<PrintBlock>,
    money: &MoneyStyle,
    labels: &PaperLabels,
    figures: &XFigures,
) {
    blocks.push(amount_line(labels.sales, figures.sales, money));
    blocks.push(text(&format!("{}: {}", labels.bills, figures.bills)));
    let tenders = [
        (labels.tender_cash, figures.tenders.cash),
        (labels.tender_card, figures.tenders.card),
        (labels.tender_qr, figures.tenders.qr),
        (labels.tender_other, figures.tenders.other),
    ];
    let moved = [
        (labels.discount, figures.discounts),
        (labels.voided_after_payment, figures.voided_cash),
        (labels.paid_in, figures.paid_in),
        (labels.paid_out, figures.paid_out),
        (labels.dropped_to_safe, figures.dropped),
    ];
    for (label, amount) in tenders.into_iter().chain(moved) {
        if !amount.is_zero() {
            blocks.push(amount_line(label, amount, money));
        }
    }
    if let Some(expected) = figures.expected_cash {
        blocks.push(amount_line(labels.expected_in_drawer, expected, money));
    }
}
