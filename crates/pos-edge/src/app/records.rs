// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection remembers about orders, bills and shifts, and what a settle records.

mod bills;
mod orders;
mod shifts;

pub(in crate::app) use bills::{BillRecord, SettledReceipt, by_name_and_rate, tax_records};
pub(crate) use bills::{fee_records, fee_records_in};
pub(in crate::app) use orders::{
    LineRecord, LiveSnapshot, OrderOrigin, OrderSnapshot, StaffDecision, StaffStanding, attach_note,
};
pub(in crate::app) use shifts::{SettleDrawer, ShiftRecord, another_till, closed_view, is_till};
