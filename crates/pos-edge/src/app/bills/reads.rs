// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a bill and an order owe: the totals, the bill check, the pre-bills, and the order behind a
//! bill.

use pos_core::billing;
use pos_core::billing::BillTotals;
use pos_core::error::DomainError;
use pos_ports::event_store::EventStore;
use pos_proto::BillState;
use pos_proto::ids::{BillId, OrderId, TableId};
use pos_proto::money::{CurrencyCode, Money};

use crate::app::{AppError, BillCheckView, Edge, PreBill};

impl<S: EventStore> Edge<S> {
    /// The running check on a table: what the guest owes **right now**, assembled from the order's
    /// captured line totals against the store's own tax table (roadmap-v3 slice E5).
    ///
    /// A pure read — no command, no event, no state change — so a screen may call it as freely as it
    /// redraws. It runs the *same* [`billing::assemble`] the settle path runs, from the same
    /// projection and the same [`EdgeSession`], which is the point: the figure the operator reads to
    /// the guest and the figure the bill settles against are one calculation, not two that agree
    /// until a store publishes a second tax class. A table with no open order owes nothing.
    ///
    /// A table whose bill has been split owes what its **open parts** owe together
    /// ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md)). It used to answer with the
    /// newest part alone, so a table split two ways quoted half of what it owed until the other half
    /// was paid.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if a line's tax class has no configured rate — a configuration error the
    /// caller surfaces rather than papers over with zero tax.
    pub fn check_totals(&self, table_id: TableId) -> Result<BillTotals, AppError> {
        // Bound to a local first: a `match` on the call would hold the projection guard across the
        // arm, and the arm takes it again — which is a deadlock, not a borrow error.
        let open_order = self.lock_projection().order_for_table(table_id);
        let Some(order_id) = open_order else {
            // A free table owes nothing, which is a real answer for a screen to show.
            return Ok(Self::nothing_owed(self.session().currency));
        };
        // Through the bill where there is one, because that is where a reduction is recorded. The
        // order's own worth is what it sold; the bill's is what the guest owes, and once anything
        // has come off they are different numbers. Quoting the first would have the operator read
        // out a figure the settle then disagrees with.
        //
        // Bound to a local for the reason the comment above gives, and which I proved by writing it
        // the other way first: `match self.lock_projection()...` holds the guard across the arm,
        // and both arms take it again. The test suite hung rather than failed.
        let open_bills = self.lock_projection().open_bills_for_order(order_id);
        if !open_bills.is_empty() {
            return self.totals_of_bills(&open_bills);
        }
        let settled_bill = self.lock_projection().bill_for_order(order_id);
        match settled_bill {
            Some(bill_id) => self.bill_totals(bill_id),
            None => self.order_totals(order_id),
        }
    }

    /// What several bills owe together — the open parts of a split table — each assembled on its
    /// own and then added up.
    ///
    /// Added rather than assembled again over all their lines at once, because each part computes
    /// its own tax and its own rounding (ADR-0128 decision 4): what the table will be asked for is
    /// the sum of what each guest is asked for. The fee lines and the tax lines are listed part by
    /// part rather than merged per fee or per class, for the same reason — a merged line would be
    /// a figure no bill computed.
    fn totals_of_bills(&self, bill_ids: &[BillId]) -> Result<BillTotals, AppError> {
        let mut sum = Self::nothing_owed(self.session().currency);
        for bill_id in bill_ids {
            let part = self.bill_totals(*bill_id)?;
            sum.subtotal = sum
                .subtotal
                .checked_add(part.subtotal)
                .map_err(DomainError::from)?;
            sum.discount_total = sum
                .discount_total
                .checked_add(part.discount_total)
                .map_err(DomainError::from)?;
            sum.comp_total = sum
                .comp_total
                .checked_add(part.comp_total)
                .map_err(DomainError::from)?;
            sum.service_charge = sum
                .service_charge
                .checked_add(part.service_charge)
                .map_err(DomainError::from)?;
            sum.tax_total = sum
                .tax_total
                .checked_add(part.tax_total)
                .map_err(DomainError::from)?;
            sum.rounding_adjustment = sum
                .rounding_adjustment
                .checked_add(part.rounding_adjustment)
                .map_err(DomainError::from)?;
            sum.total_due = sum
                .total_due
                .checked_add(part.total_due)
                .map_err(DomainError::from)?;
            sum.fee_lines.extend(part.fee_lines);
            sum.tax_lines.extend(part.tax_lines);
        }
        Ok(sum)
    }

    /// One bill read back: its state, the lines it covers and what they come to
    /// ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md)).
    ///
    /// The read a split needs. Once a table's bill is split, the table-keyed check answers for the
    /// table as a whole, and each guest at the till is asking about **their** part — so the till
    /// reads the part it is about to settle, by its id, from the same arithmetic the settle runs.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] if no such bill; [`AppError::Domain`] if a line's tax class has no
    /// configured rate.
    pub fn bill_check(&self, bill_id: BillId) -> Result<BillCheckView, AppError> {
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        let order_line_ids = self.covered_lines(&bill);
        let totals = self.bill_totals(bill_id)?;
        Ok(BillCheckView {
            state: bill.state,
            order_line_ids,
            totals,
        })
    }

    /// What a pre-bill for this bill prints: the lines it covers and what they come to
    /// (roadmap-v3 B2.1).
    ///
    /// Only an open bill has one. A settled bill's paper is its receipt; a voided, split or merged
    /// one owes nothing of its own any more.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] if no such bill; [`AppError::NothingToPrint`] if it is not open;
    /// [`AppError::Domain`] if a line's tax class has no configured rate.
    pub fn pre_bill_for_bill(&self, bill_id: BillId) -> Result<PreBill, AppError> {
        let session = self.session();
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        if bill.state != BillState::Open {
            return Err(AppError::NothingToPrint);
        }
        let order = self.bill_snapshot(&bill, &session)?;
        let totals = self.bill_totals(bill_id)?;
        Ok(PreBill {
            lines: Self::receipt_lines(&order.lines),
            totals,
        })
    }

    /// What a pre-bill for this order prints — one per open bill once it has bills, which is what a
    /// split table asks for: each guest takes the part they are paying
    /// ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md)).
    ///
    /// Before any bill is opened it is the order itself, priced the way [`Self::order_totals`]
    /// prices it. Empty when the order has nothing on it or its bill is already settled.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if a line's tax class has no configured rate.
    pub fn pre_bills_for_order(&self, order_id: OrderId) -> Result<Vec<PreBill>, AppError> {
        let open_bills = self.lock_projection().open_bills_for_order(order_id);
        if !open_bills.is_empty() {
            return open_bills
                .iter()
                .map(|bill_id| self.pre_bill_for_bill(*bill_id))
                .collect();
        }
        let settled = self.lock_projection().bill_for_order(order_id);
        if settled.is_some() {
            return Ok(Vec::new());
        }
        let session = self.session();
        let order = self.order_snapshot(order_id, &session)?;
        if order.class_bases.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![PreBill {
            lines: Self::receipt_lines(&order.lines),
            totals: self.order_totals(order_id)?,
        }])
    }

    /// The order a bill belongs to, for the same reason.
    #[must_use]
    pub fn order_for_bill(&self, bill_id: BillId) -> Option<OrderId> {
        self.lock_projection()
            .bill(bill_id)
            .map(|bill| bill.order_id)
    }

    /// What a **bill** owes: its order's lines, less what has been taken off it.
    ///
    /// The settle path's arithmetic, available as a read, so the till and the receipt and the
    /// settlement are one calculation rather than three that agree until a discount is applied.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] if no such bill; [`AppError::Domain`] if a line's tax class has no
    /// configured rate.
    pub fn bill_totals(&self, bill_id: BillId) -> Result<BillTotals, AppError> {
        let session = self.session();
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        let order = self.bill_snapshot(&bill, &session)?;
        if order.class_bases.is_empty() {
            return Ok(Self::nothing_owed(session.currency));
        }
        Ok(billing::assemble(&Self::bill_input(
            &session,
            &order,
            &bill.fee_rules,
            &bill.waived_fee_ids,
            bill.reductions(session.currency),
        ))?)
    }

    /// What an **order** owes right now — the primitive [`Self::check_totals`] delegates to.
    ///
    /// Keyed on the order because that is what a bill bills
    /// ([ADR-0093](../../../../../docs/adr/0093-bill-keyed-on-order.md)); a counter order has no table to
    /// ask about, so the table-keyed read cannot answer for it. Same `billing::assemble`, same
    /// projection, same session as the settle path.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if a line's tax class has no configured rate — a configuration error the
    /// caller surfaces rather than papers over with zero tax.
    pub fn order_totals(&self, order_id: OrderId) -> Result<BillTotals, AppError> {
        let session = self.session();
        let order = self.order_snapshot(order_id, &session)?;
        if order.class_bases.is_empty() {
            // Nothing ordered — a seated table before its first line, or one whose every line was
            // voided. `assemble` refuses an empty bill (there is nothing to allocate across), and
            // rightly so; but "owes nothing" is a real answer for a screen to show, not an error.
            return Ok(Self::nothing_owed(session.currency));
        }
        // An order's own total carries no reduction: a discount is applied to a **bill**, and an
        // order that has not had one opened has nothing to reduce. `check_totals` re-reads through
        // the bill when there is one, which is where the figures differ and where the guest's
        // question is actually being answered.
        let nothing_off = (Money::zero(session.currency), Money::zero(session.currency));
        // No bill has frozen any rule yet, so the order is priced with the rules a bill opening on
        // its channel now would freeze (ADR-0159 decision 3): the figure the bill will open at.
        let in_force = session.fees.in_force(order.sales_channel);
        Ok(billing::assemble(&Self::bill_input(
            &session,
            &order,
            &in_force,
            // No bill, so nothing waived.
            &[],
            nothing_off,
        ))?)
    }

    /// A zeroed set of totals in `currency`, for a table with nothing on it.
    fn nothing_owed(currency: CurrencyCode) -> BillTotals {
        let zero = Money::zero(currency);
        BillTotals {
            subtotal: zero,
            discount_total: zero,
            comp_total: zero,
            service_charge: zero,
            fee_lines: Vec::new(),
            tax_lines: Vec::new(),
            tax_total: zero,
            rounding_adjustment: zero,
            total_due: zero,
        }
    }
}
