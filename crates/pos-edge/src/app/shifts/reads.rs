// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The shift a device reads: its view, whether its business day has ended, what the drawer should
//! hold where the count is not blind, and the till's name.

use pos_core::business_date::derive_business_date;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{DeviceId, ShiftId};
use pos_proto::money::Money;
use pos_proto::shift::DrawerDayEnd;
use pos_proto::text::DisplayName;
use pos_proto::{ClockSource, ShiftState};

use crate::app::records::is_till;
use crate::app::{AppError, Edge, ShiftRecord, ShiftView, TillScope};

impl<S: EventStore> Edge<S> {
    /// The shift trading now, as a device may see it: its id, its state and — once counted — the
    /// count the cashier entered. `None` when no shift is open.
    ///
    /// Blind (§11.1): neither the expected amount nor the variance is in the view, whatever the
    /// shift's state, because a count entered after reading the expectation is not a blind count.
    /// The expected amount is there only where the store has turned the blind close off
    /// ([`Self::expectation_shown`]), and the variance never is.
    #[must_use]
    pub fn current_shift(&self) -> Option<ShiftView> {
        self.current_shift_at(TillScope::default())
    }

    /// [`Self::current_shift`] for the drawer of the till `scope` says the device is: the store's
    /// where it keeps one drawer, and the device's own till's where it keeps one per till, which a
    /// device that is no till does not have (ADR-0167 decision 2).
    #[must_use]
    pub fn current_shift_at(&self, scope: TillScope) -> Option<ShiftView> {
        let shift_id = self.drawer_shift(scope)?;
        let record = self.lock_projection().shift(shift_id)?;
        Some(self.shift_view(shift_id, &record))
    }

    /// A shift as any read before its close may show it: blind, with the expectation only where the
    /// store has turned the blind close off, and never the variance.
    pub(in crate::app) fn shift_view(&self, shift_id: ShiftId, record: &ShiftRecord) -> ShiftView {
        ShiftView {
            shift_id,
            till: record.till,
            owner: record.owner,
            state: record.state,
            expected_amount: self.expectation_shown(record),
            counted_amount: record.counted,
            variance: None,
            paid_in: record.paid_in,
            paid_out: record.paid_out,
            print_shift_report: false,
            report: None,
            day_ended: self.day_ended(shift_id, record),
        }
    }

    /// Whether `shift_id`'s drawer is still open past its day's end: it opened on an earlier
    /// business date than today's, at the store's cutoff
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 11). The
    /// first time a day finds a drawer so, the edge warns, once, naming the shift and its till and
    /// no person. A clock it cannot read finds no drawer so.
    fn day_ended(&self, shift_id: ShiftId, record: &ShiftRecord) -> bool {
        let session = self.session();
        let Ok(today) = derive_business_date(self.clock.now(), &session.timezone, session.cutoff)
        else {
            return false;
        };
        if record.state == ShiftState::Closed || record.business_date >= today {
            return false;
        }
        if self.lock_projection().warn_day_ended(shift_id, today) {
            tracing::warn!(
                %shift_id,
                till = ?record.till,
                opened_on = %record.business_date,
                %today,
                "a drawer's shift is still open past its business day; count and close it"
            );
        }
        true
    }

    /// Refuses cash into `shift_id`'s drawer once its day has ended, where the store's
    /// `shift.drawer_day_end` requires such a drawer counted and closed first (ADR-0167 decision
    /// 11). Only cash: a card, the count, the close and a no-sale opening go on.
    pub(in crate::app) fn refuse_after_day_end(
        &self,
        shift_id: ShiftId,
        record: &ShiftRecord,
    ) -> Result<(), AppError> {
        if self.session().shift.drawer_day_end() == DrawerDayEnd::RequireClose
            && self.day_ended(shift_id, record)
        {
            return Err(AppError::DrawerDayEnded);
        }
        Ok(())
    }

    /// What the drawer should hold, for a shift before it closes, where the store shows it: `None`
    /// while `shift.blind_close` is on, which is the default and what every count was before the
    /// setting (ADR-0160 decision 2), and the shift's expectation where the store has turned it off.
    ///
    /// An expectation that cannot be computed is withheld rather than guessed at: the close computes
    /// it again and refuses there, where the cashier can act on the refusal.
    fn expectation_shown(&self, record: &ShiftRecord) -> Option<Money> {
        if self.session().shift.blind_close() {
            return None;
        }
        record.expected().ok()
    }

    /// The name of `till`'s `TERMINAL` entry, which the store's one drawer, `None`, has none of.
    pub(in crate::app) fn till_name(&self, till: Option<DeviceId>) -> Option<DisplayName> {
        let till = till?;
        self.session()
            .devices
            .devices()
            .iter()
            .find(|device| device.device_id == till && is_till(device))
            .map(|device| device.name.clone())
    }
}
