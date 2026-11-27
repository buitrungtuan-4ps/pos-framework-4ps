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
use pos_proto::events::BillingBillSettled;
use pos_proto::ids::{BillId, OrderId, ShiftId};
use pos_proto::money::Money;
use pos_proto::{BillState, PaymentMethod, ShiftState};

use super::channels::take_channel_out;
use crate::app::{PaidBill, Projection, TakenPayment};

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
        // One more bill on the shift's report.
        if let Some(record) = shift.and_then(|shift| self.shifts.get_mut(&shift)) {
            record.bills = record.bills.saturating_add(1);
        }
    }

    /// Adds a settled bill's total and its discounts and comps to `shift`'s, for its X
    /// ([ADR-0187](../../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
    /// decision 4), and the bill to its channel's line (ADR-0259).
    pub(in crate::app) fn record_shift_sale(
        &mut self,
        shift: Option<ShiftId>,
        settled: &BillingBillSettled,
    ) -> Result<(), DomainError> {
        if let Some(record) = shift.and_then(|shift| self.shifts.get_mut(&shift)) {
            record.sales = record.sales.checked_add(settled.total_due)?;
            record.discounts = record.discounts.checked_add(settled.reduction_total)?;
        }
        self.record_shift_channel(shift, settled.bill_id, settled.total_due)
    }

    /// Adds one tender's `applied` towards `bill_id` to `shift`'s tender mix, for its report, and
    /// remembers it against the bill so a void can take it back out (ADR-0258).
    pub(in crate::app) fn collect_tender(
        &mut self,
        (shift, bill_id): (Option<ShiftId>, BillId),
        method: PaymentMethod,
        applied: Money,
    ) -> Result<(), DomainError> {
        self.record_shift_in_app((shift, bill_id), method, applied)?;
        let Some(record) = shift.and_then(|shift| self.shifts.get_mut(&shift)) else {
            return Ok(());
        };
        match record
            .tenders
            .iter_mut()
            .find(|(known, _)| *known == method)
        {
            Some((_, total)) => *total = total.checked_add(applied)?,
            None => record.tenders.push((method, applied)),
        }
        record
            .bill_tenders
            .entry(bill_id)
            .or_default()
            .push((method, applied));
        Ok(())
    }

    /// Folds one captured tender: onto its bill ([`Self::record_payment`]) and into its shift's
    /// tender mix ([`Self::collect_tender`]).
    pub(in crate::app) fn record_tender(
        &mut self,
        shift: Option<ShiftId>,
        bill_id: BillId,
        method: PaymentMethod,
        applied: Money,
    ) -> Result<(), DomainError> {
        self.record_payment(bill_id, method, applied)?;
        self.collect_tender((shift, bill_id), method, applied)
    }

    /// Takes a bill voided after payment out of the X of each shift it reached while that shift is
    /// open
    /// ([ADR-0258](../../../../../docs/adr/0258-the-tills-x-leaves-out-a-bill-voided-after-payment.md)):
    /// its count, sales, discounts and tax components out of the shift that settled it, and each
    /// payment out of the tenders of the shift that took it. A closed shift's close stands.
    fn take_out_of_open_shifts(&mut self, bill_id: BillId) -> Result<(), DomainError> {
        let settled_on = self.paid(bill_id).and_then(|paid| paid.shift_id);
        let receipt = self.settled.get(&bill_id).cloned();
        for (shift_id, record) in &mut self.shifts {
            if record.state != ShiftState::Open {
                continue;
            }
            for (method, applied) in record.bill_tenders.remove(&bill_id).unwrap_or_default() {
                if let Some((_, total)) = record.tenders.iter_mut().find(|(m, _)| *m == method) {
                    *total = total.checked_sub(applied)?;
                }
            }
            let here = receipt.as_ref().filter(|_| settled_on == Some(*shift_id));
            take_channel_out(record, bill_id, here.map(|receipt| receipt.total_due))?;
            let Some(receipt) = here else {
                continue;
            };
            record.bills = record.bills.saturating_sub(1);
            record.sales = record.sales.checked_sub(receipt.total_due)?;
            record.discounts = record.discounts.checked_sub(receipt.reduction_total)?;
            for component in receipt.tax_lines.iter().flat_map(|line| &line.components) {
                if let Some(sum) = record.tax_components.iter_mut().find(|sum| {
                    sum.name == component.name
                        && sum.rate_basis_points == component.rate_basis_points
                }) {
                    sum.tax = sum.tax.checked_sub(component.tax)?;
                }
            }
        }
        Ok(())
    }

    /// Remembers that a payment towards `bill_id` went into the drawer whose shift is `shift`, while
    /// the bill is still open (ADR-0220 decision 5). A payment with no shift touched no drawer.
    pub(in crate::app) fn record_part_payment(&mut self, bill_id: BillId, shift: Option<ShiftId>) {
        let Some(shift) = shift else { return };
        let shifts = self.part_paid.entry(bill_id).or_default();
        if !shifts.contains(&shift) {
            shifts.push(shift);
        }
    }

    /// Adds `payment` to what `bill_id`, still open, has taken (ADR-0220 decision 3).
    pub(in crate::app) fn record_taken(&mut self, bill_id: BillId, payment: TakenPayment) {
        self.taken.entry(bill_id).or_default().push(payment);
    }

    /// The payments `bill_id` has taken while open, in the order taken: none once it closed.
    pub(in crate::app) fn taken(&self, bill_id: BillId) -> &[TakenPayment] {
        self.taken.get(&bill_id).map_or(&[], Vec::as_slice)
    }

    /// Whether a bill that is still open holds a payment taken into the drawer whose shift is
    /// `shift`, which keeps that shift from closing (ADR-0220 decision 5).
    pub(in crate::app) fn part_paid_in(&self, shift: ShiftId) -> bool {
        self.part_paid.iter().any(|(bill_id, shifts)| {
            shifts.contains(&shift)
                && self
                    .bills
                    .get(bill_id)
                    .is_some_and(|bill| bill.state == BillState::Open)
        })
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
        let cash = self.paid(bill_id).map(|paid| paid.cash);
        if let Some(record) = shift.and_then(|shift_id| self.shifts.get_mut(&shift_id)) {
            if let Some(cash) = cash {
                record.voided_cash = record.voided_cash.checked_add(cash)?;
            }
            record.voided_bills = record.voided_bills.saturating_add(1);
        }
        self.take_out_of_open_shifts(bill_id)?;
        self.void_bill_record(bill_id);
        // An invoice still waiting is never issued, and so takes no number (ADR-0229 decision 3).
        self.invoices.drop_pending(bill_id);
        Ok(())
    }

    /// Whether `order_id`'s sale ended with a bill voided after payment: no bill of it is open, and
    /// one that settled was voided since. A settled bill keeps its receipt when it is voided, which
    /// is how such a bill is told from one voided before anybody paid it.
    ///
    /// Such an order has ended as a paid one has (ADR-0184 decision 2: the correction is a new
    /// bill on a new sale), so it is not owing again. Before this it was, and the live read and the
    /// kitchen board brought its fired lines back as tickets nobody would bump.
    pub(in crate::app) fn voided_after_payment(&self, order_id: OrderId) -> bool {
        self.latest_bill_in(order_id, BillState::Open).is_none()
            && self.order_bill_ids.get(&order_id).is_some_and(|ids| {
                ids.iter().any(|bill_id| {
                    self.settled.contains_key(bill_id)
                        && self
                            .bills
                            .get(bill_id)
                            .is_some_and(|bill| bill.state == BillState::Voided)
                })
            })
    }

    /// Whether `bill_id` is settled, so a `billing.bill.voided` is a void after payment.
    pub(in crate::app) fn is_settled(&self, bill_id: BillId) -> bool {
        self.bills
            .get(&bill_id)
            .is_some_and(|bill| bill.state == BillState::Settled)
    }
}
