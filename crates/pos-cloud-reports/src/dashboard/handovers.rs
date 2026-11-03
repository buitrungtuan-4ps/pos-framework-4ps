// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A drawer handed over from one shift to the next is counted once, and each shift reads on its own.
//!
//! A handover ([ADR-0187](../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md))
//! closes a shift and opens the next on the same cash. Summed shift by shift, that cash counts twice:
//! the outgoing close expects and counts it, and the incoming float and close carry it again. So a
//! drawer handed over from A to B read floats of A + B and an expected amount of A's plus B's,
//! more than was ever in it. Where the opening names the shift it took over
//! (`handed_over_from_shift_id`), its float is that cash carried on, not new cash: it adds no
//! float; its close's expected amount and closed float leave the float out, and its count leaves
//! out the outgoing count. The drawer then expects what the drawer holds, and its over or short
//! is both shifts' plus the handover difference, which the day and the drawer also carry apart.
//!
//! An opening that names none, which every one before ADR-0187 is, may have brought new cash or
//! carried the old, and nothing says which. So each shift's own figures are kept in `by_shift`, and
//! the console reads such a drawer one row per shift rather than a sum.
//!
//! One event at a time ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)), after
//! [`fold_cash`](super::rollup::fold_cash) and [`fold_closed_floats`](super::floats::fold_closed_floats),
//! whose figures it corrects. Bounded: one entry per drawer, and one per open handed-over shift.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{
    CashDrawerDropped, CashDrawerPaidIn, CashDrawerPaidOut, CashShiftClosed, CashShiftOpened,
};
use pos_proto::ids::DeviceId;

use super::rollup::{empty_cash, set_currency};
use crate::types::{DailyCash, STORE_DRAWER, ShiftCash};

/// What the fold remembers between events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptHandovers {
    /// Each drawer's last closed shift and what it counted, minor units.
    #[serde(default)]
    pub last_closed: BTreeMap<String, (String, i64)>,
    /// Each open shift that took a drawer over: its float and the outgoing count it carried on.
    #[serde(default)]
    pub carried: BTreeMap<String, (i64, i64)>,
}

fn drawer_of(till: Option<DeviceId>) -> String {
    till.map_or_else(|| STORE_DRAWER.to_owned(), |till| till.to_string())
}

/// Folds one event's handover and its shift's own figures into `cash`. Every other event is
/// ignored, and a payload that fails to decode is skipped, as the other folds skip one.
pub fn fold_handovers(
    kept: &mut KeptHandovers,
    cash: &mut BTreeMap<String, DailyCash>,
    event: &EventEnvelope<RawPayload>,
) {
    let date = event.business_date.to_string();
    match event.event_type.as_str() {
        "cash.shift.opened" => {
            if let Ok(opened) = event.data.decode::<CashShiftOpened>() {
                let day = cash
                    .entry(date.clone())
                    .or_insert_with(|| empty_cash(&date));
                fold_opened(kept, day, &opened);
            }
        }
        "cash.shift.closed" => {
            if let Ok(closed) = event.data.decode::<CashShiftClosed>() {
                let day = cash
                    .entry(date.clone())
                    .or_insert_with(|| empty_cash(&date));
                fold_closed(kept, day, &closed);
            }
        }
        kind => {
            let amount = match kind {
                "cash.drawer.paid_in" => event
                    .data
                    .decode::<CashDrawerPaidIn>()
                    .ok()
                    .map(|ev| ev.amount),
                "cash.drawer.paid_out" => event
                    .data
                    .decode::<CashDrawerPaidOut>()
                    .ok()
                    .map(|ev| ev.amount),
                "cash.drawer.dropped" => event
                    .data
                    .decode::<CashDrawerDropped>()
                    .ok()
                    .map(|ev| ev.amount),
                _ => None,
            };
            let row = event
                .shift_id
                .and_then(|shift| cash.get_mut(&date)?.by_shift.get_mut(&shift.to_string()));
            if let (Some(amount), Some(row)) = (amount, row) {
                let slot = match kind {
                    "cash.drawer.paid_in" => &mut row.paid_in,
                    "cash.drawer.paid_out" => &mut row.paid_out,
                    _ => &mut row.dropped,
                };
                *slot = slot.saturating_add(amount.amount_minor);
            }
        }
    }
}

/// An opening: its own row, and where it took over the shift just closed on its drawer, its float
/// out of the floats opened and its handover difference in.
fn fold_opened(kept: &mut KeptHandovers, day: &mut DailyCash, opened: &CashShiftOpened) {
    let drawer = drawer_of(opened.terminal_device_id);
    let float = opened.opening_float.amount_minor;
    let shift = opened.opened_shift_id.to_string();
    let from = opened
        .handed_over_from_shift_id
        .map(|from| from.to_string());
    let outgoing = kept
        .last_closed
        .get(&drawer)
        .filter(|(closed, _)| Some(closed) == from.as_ref())
        .map(|(_, counted)| *counted);
    if let Some(outgoing) = outgoing {
        let difference = float.saturating_sub(outgoing);
        day.opening_float = day.opening_float.saturating_sub(float);
        day.handover_difference = day.handover_difference.saturating_add(difference);
        let figures = day.by_drawer.entry(drawer.clone()).or_default();
        figures.opening_float = figures.opening_float.saturating_sub(float);
        figures.handover_difference = figures.handover_difference.saturating_add(difference);
        kept.carried.insert(shift.clone(), (float, outgoing));
    }
    day.by_shift.insert(
        shift,
        ShiftCash {
            drawer,
            handed_over: outgoing.is_some(),
            opening_float: float,
            ..ShiftCash::default()
        },
    );
}

/// A close: its row's figures, the drawer's last close, and where the shift took a drawer over, the
/// carried cash out of the expectation, the closed float and the count, once.
fn fold_closed(kept: &mut KeptHandovers, day: &mut DailyCash, closed: &CashShiftClosed) {
    let drawer = drawer_of(closed.terminal_device_id);
    let shift = closed.closed_shift_id.to_string();
    let counted = closed.counted_amount.amount_minor;
    set_currency(
        &mut day.currency_code,
        closed.counted_amount.currency_code.to_string(),
    );
    if let Some((float, outgoing)) = kept.carried.remove(&shift) {
        // The carried float went into this close's expectation and closed float, and the outgoing
        // count is in the drawer's count already: the drawer holds it once.
        let difference = float.saturating_sub(outgoing);
        day.expected = day.expected.saturating_sub(float);
        day.closed_float = day.closed_float.saturating_sub(float);
        day.counted = day.counted.saturating_sub(outgoing);
        day.variance = day.variance.saturating_add(difference);
        let figures = day.by_drawer.entry(drawer.clone()).or_default();
        figures.expected = figures.expected.saturating_sub(float);
        figures.closed_float = figures.closed_float.saturating_sub(float);
        figures.counted = figures.counted.saturating_sub(outgoing);
        figures.variance = figures.variance.saturating_add(difference);
    }
    let row = day
        .by_shift
        .entry(shift.clone())
        .or_insert_with(|| ShiftCash {
            drawer: drawer.clone(),
            ..ShiftCash::default()
        });
    row.closed = true;
    row.expected = closed.expected_amount.amount_minor;
    row.counted = counted;
    row.variance = closed.variance.amount_minor;
    kept.last_closed.insert(drawer, (shift, counted));
}

#[cfg(test)]
mod tests;
