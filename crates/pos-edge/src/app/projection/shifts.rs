// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection records of shifts: opening, counting and closing one, the drawer model the
//! open shifts run, the day-end warning, and the cash moved in and out.

use pos_core::error::DomainError;
use pos_proto::ShiftState;
use pos_proto::ids::{DeviceId, ShiftId};
use pos_proto::money::Money;
use pos_proto::shift::DrawerModel;
use pos_proto::time::BusinessDate;

use crate::app::{CashMovement, Projection, ShiftRecord};

impl Projection {
    pub(in crate::app) fn open_shift_record(&mut self, shift_id: ShiftId, record: ShiftRecord) {
        self.open_shifts.insert(record.till, shift_id);
        self.shifts.insert(shift_id, record);
    }

    /// Records that `shift_id`'s drawer was found open past its day's end on `today`, answering
    /// whether that is the first time today: one warning a drawer a day.
    pub(in crate::app) fn warn_day_ended(
        &mut self,
        shift_id: ShiftId,
        today: BusinessDate,
    ) -> bool {
        match self.shifts.get_mut(&shift_id) {
            Some(record) if record.day_end_warned != Some(today) => {
                record.day_end_warned = Some(today);
                true
            }
            _ => false,
        }
    }

    /// The shift `till`'s drawer has trading or counted, `None` for the store's drawer.
    pub(in crate::app) fn open_shift_of(&self, till: Option<DeviceId>) -> Option<ShiftId> {
        self.open_shifts.get(&till).copied()
    }

    /// The model the open shifts were opened under, or `None` when no drawer is open: a till's
    /// drawer is only ever opened under a drawer per till, and the store's under one drawer. Were
    /// both ever open, a till's decides, because that model lists every open drawer.
    pub(in crate::app) fn open_model(&self) -> Option<DrawerModel> {
        if self.open_shifts.keys().any(Option::is_some) {
            Some(DrawerModel::PerTerminal)
        } else if self.open_shifts.is_empty() {
            None
        } else {
            Some(DrawerModel::PerStore)
        }
    }

    pub(in crate::app) fn shift(&self, shift_id: ShiftId) -> Option<ShiftRecord> {
        self.shifts.get(&shift_id).cloned()
    }

    pub(in crate::app) fn record_shift_count(
        &mut self,
        shift_id: ShiftId,
        counted: Money,
        state: ShiftState,
    ) {
        if let Some(record) = self.shifts.get_mut(&shift_id) {
            record.counted = Some(counted);
            record.state = state;
        }
    }

    pub(in crate::app) fn close_shift_record(&mut self, shift_id: ShiftId) {
        let Some(record) = self.shifts.get_mut(&shift_id) else {
            return;
        };
        record.state = ShiftState::Closed;
        let till = record.till;
        if self.open_shift_of(till) == Some(shift_id) {
            self.open_shifts.remove(&till);
        }
    }

    /// Adds a paid in or a paid out to `shift_id`'s record (ADR-0165 decision 2).
    pub(in crate::app) fn record_movement(
        &mut self,
        shift_id: ShiftId,
        movement: CashMovement,
        amount: Money,
    ) -> Result<(), DomainError> {
        if let Some(record) = self.shifts.get_mut(&shift_id) {
            match movement {
                CashMovement::PaidIn => record.paid_in = record.paid_in.checked_add(amount)?,
                CashMovement::PaidOut => record.paid_out = record.paid_out.checked_add(amount)?,
            }
        }
        Ok(())
    }
}
