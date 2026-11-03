// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The float of each shift that has closed, set beside the figures its close brought.
//!
//! `opening_float` adds a shift's float on the day it opens, but a drawer's expected amount, count
//! and over/short arrive only at its close. So a drawer whose second shift is still open shows two
//! floats beside one shift's figures. `closed_float` is the float of the shifts that closed, added
//! on the day and to the drawer of the close, so it stands beside the expected amount it went
//! into. The rollup keeps each open shift's float from `cash.shift.opened` until its
//! `cash.shift.closed`. The fold stays one event at a time
//! ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)), so the materialised rollup and
//! a fold of the whole log agree.
//!
//! Bounded by the store's open shifts: a shift's float is let go at its close.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{CashShiftClosed, CashShiftOpened};

use crate::types::{DailyCash, STORE_DRAWER};

/// Each open shift's opening float, minor units, keyed by shift id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptFloats {
    /// The float of each shift opened and not yet closed.
    #[serde(default)]
    pub shifts: BTreeMap<String, i64>,
}

/// Folds a shift's close into its day's and its drawer's `closed_float`, and keeps each open
/// shift's float so a close can. Run after [`fold_cash`](super::rollup::fold_cash), which makes
/// the close's day and drawer. Every other event is ignored, and a payload that fails to decode is
/// skipped, as the other folds skip one. A shift the rollup never saw open, one opened before the
/// rollup kept floats, adds nothing.
pub fn fold_closed_floats(
    kept: &mut KeptFloats,
    cash: &mut BTreeMap<String, DailyCash>,
    event: &EventEnvelope<RawPayload>,
) {
    match event.event_type.as_str() {
        "cash.shift.opened" => {
            if let Ok(opened) = event.data.decode::<CashShiftOpened>() {
                kept.shifts.insert(
                    opened.opened_shift_id.to_string(),
                    opened.opening_float.amount_minor,
                );
            }
        }
        "cash.shift.closed" => {
            if let Ok(closed) = event.data.decode::<CashShiftClosed>()
                && let Some(float) = kept.shifts.remove(&closed.closed_shift_id.to_string())
                && let Some(day) = cash.get_mut(&event.business_date.to_string())
            {
                day.closed_float = day.closed_float.saturating_add(float);
                let drawer = closed
                    .terminal_device_id
                    .map_or_else(|| STORE_DRAWER.to_owned(), |till| till.to_string());
                if let Some(figures) = day.by_drawer.get_mut(&drawer) {
                    figures.closed_float = figures.closed_float.saturating_add(float);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
