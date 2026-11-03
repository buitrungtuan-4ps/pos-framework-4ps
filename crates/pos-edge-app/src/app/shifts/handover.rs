// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a handover slip says: the cash one shift counted out and the next counted in, on one drawer
//! ([ADR-0187](../../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
//! decision 3).

use pos_core::business_date::local_time;
use pos_core::cash_count::CountBreakdown;
use pos_ports::event_store::EventStore;
use pos_proto::ids::ShiftId;

use crate::app::{AppError, Edge, HandoverSlip};

impl<S: EventStore> Edge<S> {
    /// The slip for the handover from `outgoing` to `incoming`, the shift just opened with
    /// `incoming_breakdown` as its float note by note: the till, when it was handed over in the
    /// store's time, each count with its notes, and the incoming float less the outgoing count.
    /// Names no person, as the void slip names none (ADR-0184, amendment).
    ///
    /// `None` where either shift is unknown, or the outgoing one was never counted, which a close
    /// does not allow.
    #[must_use]
    pub fn handover_slip(
        &self,
        outgoing: ShiftId,
        incoming: ShiftId,
        incoming_breakdown: CountBreakdown,
    ) -> Option<HandoverSlip> {
        let (out, into) = {
            let projection = self.lock_projection();
            (projection.shift(outgoing)?, projection.shift(incoming)?)
        };
        let outgoing_count = out.counted?;
        let zone = self.session().timezone.clone();
        Some(HandoverSlip {
            till_name: self.till_name(into.till),
            handed_over_at: local_time(into.opened_time, &zone)
                .ok()
                .map(|time| time.to_string()),
            outgoing_shift_id: outgoing,
            outgoing_count,
            outgoing_breakdown: out.count_breakdown,
            incoming_shift_id: incoming,
            incoming_count: into.opening_float,
            incoming_breakdown,
            difference: into.opening_float.checked_sub(outgoing_count).ok()?,
        })
    }

    /// The slip of the handover `incoming` opened with, to print again from the drawer list
    /// (ADR-0187 decision 3), its float's notes as it recorded them.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownShift`] for a shift the edge does not know, and
    /// [`AppError::NothingToPrint`] for one that took no drawer over.
    pub fn handover_slip_of(&self, incoming: ShiftId) -> Result<HandoverSlip, AppError> {
        let record = self
            .lock_projection()
            .shift(incoming)
            .ok_or(AppError::UnknownShift)?;
        let outgoing = record.handed_over_from.ok_or(AppError::NothingToPrint)?;
        self.handover_slip(outgoing, incoming, record.float_breakdown)
            .ok_or(AppError::NothingToPrint)
    }
}
