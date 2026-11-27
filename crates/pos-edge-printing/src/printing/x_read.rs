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
    ];
    for (label, amount) in tenders {
        if !amount.is_zero() {
            blocks.push(amount_line(label, amount, money));
        }
    }
    // Each delivery app's in-app payments by its name, then every other tender (ADR-0259).
    let mut other = figures.tenders.other;
    for app in &figures.tenders.in_app {
        other = other.checked_sub(app.amount).unwrap_or(other);
        if !app.amount.is_zero() {
            let name = app.name.as_ref().map_or_else(
                || app.vendor_id.to_string(),
                |name| name.as_str().to_owned(),
            );
            let label = format!("{} {name}", labels.tender_paid_via);
            blocks.push(amount_line(&label, app.amount, money));
        }
    }
    let moved = [
        (labels.tender_other, other),
        (labels.discount, figures.discounts),
        (labels.voided_after_payment, figures.voided_cash),
        (labels.paid_in, figures.paid_in),
        (labels.paid_out, figures.paid_out),
        (labels.dropped_to_safe, figures.dropped),
    ];
    for (label, amount) in moved {
        if !amount.is_zero() {
            blocks.push(amount_line(label, amount, money));
        }
    }
    if let Some(expected) = figures.expected_cash {
        blocks.push(amount_line(labels.expected_in_drawer, expected, money));
    }
    channel_blocks(blocks, money, labels, figures);
}

/// What the paper calls a token: the store's name in the paper's language, else in English, else
/// the paper's own word for a built-in (ADR-0260 decision 6).
fn token_name(
    labels: &PaperLabels,
    token: &str,
    names: &std::collections::BTreeMap<String, pos_proto::text::DisplayName>,
) -> String {
    names
        .get(labels.language)
        .or_else(|| names.get("en"))
        .map(|name| name.as_str().to_owned())
        .or_else(|| {
            labels
                .sale_tokens
                .iter()
                .find(|(known, _)| *known == token)
                .map(|(_, label)| (*label).to_owned())
        })
        .unwrap_or_else(|| {
            labels
                .sale_tokens
                .last()
                .map_or_else(|| token.to_owned(), |(_, other)| (*other).to_owned())
        })
}

/// An X's settled bills by channel and order type, "GrabFood - Giao hàng (3)", then summed by
/// order type, as the till's X screen lists them (ADR-0259 decision 4, ADR-0260 decision 6).
fn channel_blocks(
    blocks: &mut Vec<PrintBlock>,
    money: &MoneyStyle,
    labels: &PaperLabels,
    figures: &XFigures,
) {
    if figures.channels.is_empty() {
        return;
    }
    blocks.push(centred(labels.x_by_channel, true));
    let mut by_type: Vec<(String, u32, pos_proto::money::Money)> = Vec::new();
    for line in &figures.channels {
        let source = match (&line.name, line.channel.vendor_id) {
            (Some(app), Some(_)) => app.as_str().to_owned(),
            (None, Some(app)) => app.to_string(),
            (_, None) => token_name(labels, line.channel.source.as_str(), &line.source_names),
        };
        let order_type = token_name(
            labels,
            line.channel.order_type.as_str(),
            &line.order_type_names,
        );
        // A hyphen, not a middle dot: a printer that cannot rasterise prints ASCII only.
        let label = format!("{source} - {order_type} ({})", line.bills);
        blocks.push(amount_line(&label, line.sales, money));
        match by_type.iter_mut().find(|(kept, _, _)| *kept == order_type) {
            Some(row) => {
                row.1 = row.1.saturating_add(line.bills);
                row.2 = row.2.checked_add(line.sales).unwrap_or(row.2);
            }
            None => by_type.push((order_type, line.bills, line.sales)),
        }
    }
    blocks.push(centred(labels.x_by_order_type, true));
    for (order_type, bills, sales) in by_type {
        blocks.push(amount_line(
            &format!("{order_type} ({bills})"),
            sales,
            money,
        ));
    }
}
