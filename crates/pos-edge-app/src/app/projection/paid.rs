// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What each bill was paid with, and what voiding a paid bill gives back
//! ([ADR-0184](../../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)
//! decision 6).
//!
//! Folded the same way live and on replay: a payment adds to its bill, a settle names the shift it
//! was stamped with, and a void of a settled bill takes the bill's cash out of the drawer whose shift
//! the void was stamped with — the till's where the void was done, wherever the cash was taken.
//! Every other tender is recorded as reversed and touches no drawer; tips are a ledger of their own
//! (ADR-0028) and are not touched.

use pos_core::error::DomainError;
use pos_proto::ids::{BillId, ShiftId};
use pos_proto::money::Money;
use pos_proto::{BillState, PaymentMethod};

use crate::app::{PaidBill, Projection};

impl Projection {
    /// Adds one tender's `applied` to its bill: to its cash where `method` is cash, and to what
    /// other tenders paid otherwise.
    pub(in crate::app) fn record_payment(
        &mut self,
        bill_id: BillId,
        method: PaymentMethod,
        applied: Money,
    ) -> Result<(), DomainError> {
        let paid = self
            .paid
            .entry(bill_id)
            .or_insert_with(|| PaidBill::nothing(applied.currency_code));
        if method == PaymentMethod::Cash {
            paid.cash = paid.cash.checked_add(applied)?;
        } else {
            paid.other = paid.other.checked_add(applied)?;
        }
        Ok(())
    }

    /// Names the shift a bill's settle was stamped with, which a `SETTLED_VOID_WINDOW_SHIFT` store
    /// asks is still open before the bill is voided.
    pub(in crate::app) fn record_settle_shift(&mut self, bill_id: BillId, shift: Option<ShiftId>) {
        if let Some(paid) = self.paid.get_mut(&bill_id) {
            paid.shift_id = shift;
        }
    }

    /// What `bill_id`'s tenders put on it, or `None` for a bill nobody paid.
    pub(in crate::app) fn paid(&self, bill_id: BillId) -> Option<PaidBill> {
        self.paid.get(&bill_id).copied()
    }

    /// Voids a bill that was paid: its cash leaves the drawer whose shift is `shift`, the one the
    /// void was stamped with, so that drawer's expected cash falls by exactly the cash applied, and
    /// the bill is voided as any bill is. A drawer with no open shift, as at a store selling
    /// without one, has no expectation to lower, as its settle had none to raise.
    pub(in crate::app) fn void_settled_record(
        &mut self,
        bill_id: BillId,
        shift: Option<ShiftId>,
    ) -> Result<(), DomainError> {
        if let Some(cash) = self.paid(bill_id).map(|paid| paid.cash)
            && let Some(record) = shift.and_then(|shift_id| self.shifts.get_mut(&shift_id))
        {
            record.voided_cash = record.voided_cash.checked_add(cash)?;
        }
        self.void_bill_record(bill_id);
        Ok(())
    }

    /// Whether `bill_id` is settled, so a `billing.bill.voided` is a void after payment.
    pub(in crate::app) fn is_settled(&self, bill_id: BillId) -> bool {
        self.bills
            .get(&bill_id)
            .is_some_and(|bill| bill.state == BillState::Settled)
    }
}
