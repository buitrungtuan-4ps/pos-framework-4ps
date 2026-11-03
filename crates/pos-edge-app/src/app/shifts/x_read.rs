// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A shift's X, and the store's across its drawers: a read that resets nothing
//! ([ADR-0187](../../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
//! decisions 4 and 5).
//!
//! The Z stays the close and its printed report. An X reads the live record the close will read,
//! and writes no event, so reading it twice reads the same, and the close after it finds every
//! figure where it was.

use pos_core::business_date::derive_business_date;
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ids::ShiftId;
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::{ClockSource, PaymentMethod};

use crate::app::{AppError, Edge, ShiftRecord, ShiftX, StoreX, TenderTotals, XFigures};

impl<S: EventStore> Edge<S> {
    /// Whether `actor` may read an X at all: whoever sees the day's takings, or holds
    /// `reports.shift.read_x`, by their own role, as the takings read asks.
    #[must_use]
    pub fn reads_x(&self, actor: Actor) -> bool {
        let staff = &self.session().staff;
        staff.grants(actor.employee_id, Permission::ViewTakings)
            || staff.grants(actor.employee_id, Permission::ReadShiftX)
    }

    /// Whether `actor`'s X shows the cash a drawer should hold: always where the store does not
    /// count blind, and in a blind-close store only to a holder of `reports.shift.read_x`.
    fn x_shows_expected(&self, actor: Actor) -> bool {
        let session = self.session();
        !session.shift.blind_close()
            || session
                .staff
                .grants(actor.employee_id, Permission::ReadShiftX)
    }

    /// `shift_id`'s X, as `actor` may read it.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownShift`] for a shift the edge does not know, and [`AppError`] where a
    /// figure cannot be computed.
    pub fn shift_x(&self, actor: Actor, shift_id: ShiftId) -> Result<ShiftX, AppError> {
        let record = self
            .lock_projection()
            .shift(shift_id)
            .ok_or(AppError::UnknownShift)?;
        self.x_of(shift_id, &record, self.x_shows_expected(actor))
    }

    /// The store's X, as `actor` may read it: every drawer's shift that is open, trading or
    /// counted; or, where none is, every shift of today's business date, all closed, which is the
    /// store's end of day. Oldest first, with their totals.
    ///
    /// # Errors
    ///
    /// [`AppError::Clock`] where today's business date cannot be read, and [`AppError`] where a
    /// figure cannot be computed.
    pub fn store_x(&self, actor: Actor) -> Result<StoreX, AppError> {
        let session = self.session();
        let today = derive_business_date(self.clock.now(), &session.timezone, session.cutoff)
            .map_err(|_ignored| AppError::Clock)?;
        let shows = self.x_shows_expected(actor);
        let mut records: Vec<(ShiftId, ShiftRecord)> = {
            let projection = self.lock_projection();
            let open: Vec<ShiftId> = projection.open_shifts.values().copied().collect();
            if open.is_empty() {
                projection
                    .shifts
                    .iter()
                    .filter(|(_, record)| record.business_date == today)
                    .map(|(shift_id, record)| (*shift_id, record.clone()))
                    .collect()
            } else {
                open.into_iter()
                    .filter_map(|shift_id| Some((shift_id, projection.shift(shift_id)?)))
                    .collect()
            }
        };
        records.sort_by_key(|(shift_id, record)| (record.opened_time, *shift_id));
        let all_closed = !records.is_empty()
            && records
                .iter()
                .all(|(_, record)| record.state == pos_proto::ShiftState::Closed);
        let mut shifts = Vec::with_capacity(records.len());
        let mut totals: Option<XFigures> = None;
        for (shift_id, record) in &records {
            let x = self.x_of(*shift_id, record, shows)?;
            totals = Some(match totals {
                Some(sum) => sum.plus(&x.figures)?,
                None => x.figures,
            });
            shifts.push(x);
        }
        Ok(StoreX {
            business_date: today,
            all_closed,
            shifts,
            totals,
        })
    }

    /// `record`'s X, with its expectation where `shows` says.
    fn x_of(
        &self,
        shift_id: ShiftId,
        record: &ShiftRecord,
        shows: bool,
    ) -> Result<ShiftX, AppError> {
        Ok(ShiftX {
            shift_id,
            till: record.till,
            till_name: self.till_name(record.till),
            state: record.state,
            business_date: record.business_date,
            opened_time: record.opened_time,
            figures: XFigures {
                sales: record.sales,
                bills: record.bills,
                tenders: tenders_of(record, record.opening_float.currency_code)?,
                discounts: record.discounts,
                voided_bills: record.voided_bills,
                voided_cash: record.voided_cash,
                paid_in: record.paid_in,
                paid_out: record.paid_out,
                dropped: record.dropped,
                expected_cash: if shows {
                    Some(record.expected()?)
                } else {
                    None
                },
            },
        })
    }
}

/// `record`'s tender mix in four kinds, every tender that is not cash, card or a wallet as other.
fn tenders_of(record: &ShiftRecord, currency: CurrencyCode) -> Result<TenderTotals, AppError> {
    let zero = Money::zero(currency);
    let mut totals = TenderTotals {
        cash: zero,
        card: zero,
        qr: zero,
        other: zero,
    };
    for (method, applied) in &record.tenders {
        let slot = match method {
            PaymentMethod::Cash => &mut totals.cash,
            PaymentMethod::Card => &mut totals.card,
            PaymentMethod::Qr => &mut totals.qr,
            _ => &mut totals.other,
        };
        *slot = slot.checked_add(*applied).map_err(DomainError::from)?;
    }
    Ok(totals)
}

#[cfg(test)]
mod tests;
