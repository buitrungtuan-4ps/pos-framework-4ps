// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection records of bills: opening, settling and voiding one, the receipts and
//! takings a settle leaves, its reductions and fees, and the tax and cash a settle folds.

use pos_core::billing;
use pos_core::billing::{MergedFees, MergingBill, TaxComponentLine};
use pos_core::error::DomainError;
use pos_proto::events::BillingBillSettled;
use pos_proto::ids::{BillId, FeeId, OrderId, OrderLineId, ShiftId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::{BillState, OrderLineState, ReductionKind, TableState};

use crate::app::{BillRecord, Projection, SettledReceipt};

impl Projection {
    /// Records a bill and indexes it by the order it bills.
    pub(in crate::app) fn open_bill_record(&mut self, bill_id: BillId, record: BillRecord) {
        let order_id = record.order_id;
        // Indexed only when new, as a line is: a rebuild can fold a bill the projection holds.
        let ids = self.order_bill_ids.entry(order_id).or_default();
        if !ids.contains(&bill_id) {
            ids.push(bill_id);
        }
        self.bills.insert(bill_id, record);
        self.refresh_live(order_id);
    }

    /// The newest of an order's bills that is in `state`.
    pub(in crate::app) fn latest_bill_in(
        &self,
        order_id: OrderId,
        state: BillState,
    ) -> Option<BillId> {
        self.order_bill_ids
            .get(&order_id)?
            .iter()
            .rev()
            .copied()
            .find(|bill_id| {
                self.bills
                    .get(bill_id)
                    .is_some_and(|bill| bill.state == state)
            })
    }

    /// The bill an order is being charged on: its newest open bill, or if none is open its newest
    /// settled one. `None` when it never had a bill, or every bill it had was voided or folded into
    /// another, which leaves it owing and free to bill again.
    ///
    /// This is what keeps one order to one bill now that a bill is keyed on an order rather than a
    /// table ([ADR-0093](../../../../../docs/adr/0093-bill-keyed-on-order.md)). It used to hold by
    /// accident: a table can only be `Occupied` once, so `decide_table(RequestBill)` refused the
    /// second request on its way through, and nothing had to say so. An order key has no state
    /// machine behind it, and dropping the rule silently would be expensive — two open bills on one
    /// order means two receipt numbers and a guest charged twice for one meal.
    ///
    /// Open before settled, so an order with one split part paid and another still owing is
    /// still owing, whichever part was opened last.
    pub(in crate::app) fn bill_for_order(&self, order_id: OrderId) -> Option<BillId> {
        self.latest_bill_in(order_id, BillState::Open)
            .or_else(|| self.latest_bill_in(order_id, BillState::Settled))
    }

    /// Every bill on an order that is still open, oldest first: one for an ordinary table, one per
    /// unpaid part once its bill has been split (ADR-0128).
    pub(in crate::app) fn open_bills_for_order(&self, order_id: OrderId) -> Vec<BillId> {
        self.order_bill_ids
            .get(&order_id)
            .map_or_else(Vec::new, |ids| {
                ids.iter()
                    .copied()
                    .filter(|bill_id| {
                        self.bills
                            .get(bill_id)
                            .is_some_and(|bill| bill.state == BillState::Open)
                    })
                    .collect()
            })
    }

    /// Keeps what a settle recorded, live and replayed alike, so the receipt can be copied.
    pub(in crate::app) fn record_settled(&mut self, bill_id: BillId, receipt: SettledReceipt) {
        self.settled.insert(bill_id, receipt);
    }

    /// Counts a copy (`billing.receipt.reprinted`). The highest number seen wins, so a replay that
    /// meets the copies in any order ends where the live box did.
    pub(in crate::app) fn record_copy(&mut self, bill_id: BillId, copy_number: u32) {
        if let Some(receipt) = self.settled.get_mut(&bill_id) {
            receipt.copies = receipt.copies.max(copy_number);
        }
    }

    /// What the bills settled on `business_date` came to and how many there were, in `currency`.
    /// A bill the projection no longer holds as settled took nothing, so it is not counted.
    ///
    /// Every bill the day settled, and not the most recent [`MAX_SETTLED_LISTED`]: this fold keeps
    /// each settle the log holds, and a busy day settles more bills than the till lists.
    pub(in crate::app) fn takings_on(
        &self,
        business_date: BusinessDate,
        currency: CurrencyCode,
    ) -> Result<(Money, u32), DomainError> {
        let mut taken = Money::zero(currency);
        let mut count: u32 = 0;
        for (bill_id, receipt) in &self.settled {
            let still_settled = self
                .bills
                .get(bill_id)
                .is_some_and(|bill| bill.state == BillState::Settled);
            if receipt.business_date == business_date && still_settled {
                taken = taken.checked_add(receipt.total_due)?;
                count = count.saturating_add(1);
            }
        }
        Ok((taken, count))
    }

    /// The receipts settled on `business_date`, newest first, at most `limit` of them.
    pub(in crate::app) fn settled_on(
        &self,
        business_date: BusinessDate,
        limit: usize,
    ) -> Vec<(BillId, SettledReceipt)> {
        let mut receipts: Vec<(BillId, SettledReceipt)> = self
            .settled
            .iter()
            .filter(|(_, receipt)| receipt.business_date == business_date)
            .map(|(bill_id, receipt)| (*bill_id, receipt.clone()))
            .collect();
        receipts.sort_unstable_by(|(_, a), (_, b)| {
            b.settle_time
                .cmp(&a.settle_time)
                .then(b.receipt_number.cmp(&a.receipt_number))
        });
        receipts.truncate(limit);
        receipts
    }

    /// Settles a bill, and moves its table on when nothing else on the order still owes.
    ///
    /// Only then: the parts of a split share one table, and moving it to `NeedsCleaning` on the
    /// first part's settle left every other part refused by the table machine, with no way to take
    /// the rest of the money. Decided here, under the projection's lock after the commit, so two
    /// tills settling the last two parts at once cannot each see the other still open.
    fn settle_bill_record(&mut self, bill_id: BillId) {
        self.set_bill_state(bill_id, BillState::Settled);
        let Some((order_id, Some(table_id))) = self
            .bills
            .get(&bill_id)
            .map(|bill| (bill.order_id, bill.table_id))
        else {
            return;
        };
        if self.latest_bill_in(order_id, BillState::Open).is_none() {
            self.set_table(table_id, TableState::NeedsCleaning);
        }
    }

    /// Voids a bill, and gives its table back when nothing else on the order still owes.
    ///
    /// A void cancels the bill, not the meal: the lines are still there, and a cashier who voided
    /// the wrong bill opens another on the same table. So the table leaves `AwaitingPayment`: to
    /// `Occupied` when nothing was paid, or to `NeedsCleaning` when a sibling part was, because
    /// that meal is finished and the voided part's lines were forgiven with it. Before this, the
    /// table stayed `AwaitingPayment` for good, a state only a settle leaves, and a voided bill
    /// cannot settle.
    pub(in crate::app) fn void_bill_record(&mut self, bill_id: BillId) {
        self.set_bill_state(bill_id, BillState::Voided);
        let Some((order_id, Some(table_id))) = self
            .bills
            .get(&bill_id)
            .map(|bill| (bill.order_id, bill.table_id))
        else {
            return;
        };
        if self.table_state(table_id) != TableState::AwaitingPayment
            || self.latest_bill_in(order_id, BillState::Open).is_some()
        {
            return;
        }
        let next = if self.latest_bill_in(order_id, BillState::Settled).is_some() {
            TableState::NeedsCleaning
        } else {
            TableState::Occupied
        };
        self.set_table(table_id, next);
    }

    pub(in crate::app) fn bill(&self, bill_id: BillId) -> Option<BillRecord> {
        self.bills.get(&bill_id).cloned()
    }

    /// The unvoided lines of `order_id`, in id order — what opening a bill covers.
    ///
    /// Unvoided, because a voided line is owed nothing and taxed nothing; in id order, because that
    /// is the order the merge semantics fix (ADR-0029) and a partition has to be comparable against
    /// something stable.
    pub(in crate::app) fn sold_line_ids(&self, order_id: OrderId) -> Vec<OrderLineId> {
        self.line_ids_for_order(order_id)
            .iter()
            .copied()
            .filter(|id| {
                self.lines
                    .get(id)
                    .is_some_and(|record| record.state != OrderLineState::Voided)
            })
            .collect()
    }

    /// The lines a bill covers: the ones it names, or — for a bill that names none, opened before
    /// ADR-0128 recorded them — every unvoided line on its order, which is what such a bill meant.
    pub(in crate::app) fn covered_lines(&self, bill: &BillRecord) -> Vec<OrderLineId> {
        if bill.order_line_ids.is_empty() {
            self.sold_line_ids(bill.order_id)
        } else {
            bill.order_line_ids.clone()
        }
    }

    /// Records a reduction against a bill. Additive, because two discounts on one bill are two
    /// facts rather than a correction of the first.
    ///
    /// A kind this build does not know — `VOID`, or a token from a newer sender — moves nothing.
    /// The alternative is to guess which column it belongs in, and guessing wrong understates what
    /// a guest owes.
    pub(in crate::app) fn add_reduction(
        &mut self,
        bill_id: BillId,
        kind: ReductionKind,
        amount: Money,
    ) -> Result<(), DomainError> {
        if let Some(record) = self.bills.get_mut(&bill_id) {
            let slot = match kind {
                ReductionKind::Discount => &mut record.discount,
                ReductionKind::Comp => &mut record.comp,
                ReductionKind::Void | ReductionKind::Unspecified => return Ok(()),
            };
            *slot = Some(match *slot {
                Some(running) => running.checked_add(amount)?,
                None => amount,
            });
        }
        Ok(())
    }

    /// Gives each part of a split its source's wholes and waived fees (ADR-0159 decisions 5 and
    /// 6), after each part's own `billing.bill.opened`: merging the parts back puts their shares
    /// together again, and a fee waived on the bill stays waived on every part.
    pub(in crate::app) fn inherit_fees(&mut self, source: BillId, parts: &[BillId]) {
        let Some((wholes, waived)) = self
            .bills
            .get(&source)
            .map(|bill| (bill.fee_wholes.clone(), bill.waived_fee_ids.clone()))
        else {
            return;
        };
        for part in parts {
            if let Some(record) = self.bills.get_mut(part) {
                record.fee_wholes.clone_from(&wholes);
                record.waived_fee_ids.clone_from(&waived);
            }
        }
    }

    /// Records a fee waived on a bill (ADR-0159 decision 5), once: a second waive of the same fee
    /// is refused before it is written, and a replay that meets one anyway changes nothing.
    pub(in crate::app) fn waive_fee_record(&mut self, bill_id: BillId, fee_id: FeeId) {
        if let Some(record) = self.bills.get_mut(&bill_id)
            && !record.waived_fee_ids.contains(&fee_id)
        {
            record.waived_fee_ids.push(fee_id);
        }
    }

    /// The fee rules and wholes `target` holds once `absorbed` are merged into it, in that order
    /// (ADR-0159 decision 6), from what each holds now; `None` when the target is unknown.
    pub(in crate::app) fn merged_fees(
        &self,
        target: BillId,
        absorbed: &[BillId],
    ) -> Result<Option<MergedFees>, DomainError> {
        let Some(holder) = self.bills.get(&target) else {
            return Ok(None);
        };
        let absorbed: Vec<MergingBill<'_>> = absorbed
            .iter()
            .filter_map(|bill_id| self.bills.get(bill_id))
            .map(BillRecord::merging)
            .collect();
        Ok(Some(billing::merge_fee_rules(holder.merging(), &absorbed)?))
    }

    /// Records what a merged bill now holds.
    pub(in crate::app) fn set_bill_fees(&mut self, bill_id: BillId, fees: MergedFees) {
        if let Some(record) = self.bills.get_mut(&bill_id) {
            record.fee_rules = fees.fee_rules;
            record.fee_wholes = fees.fee_wholes;
            record.waived_fee_ids = fees.waived_fee_ids;
        }
    }

    pub(in crate::app) fn set_bill_state(&mut self, bill_id: BillId, state: BillState) {
        let Some(record) = self.bills.get_mut(&bill_id) else {
            return;
        };
        record.state = state;
        let order_id = record.order_id;
        self.refresh_live(order_id);
    }

    /// Replaces what a bill covers — the target of a merge, taking the absorbed bills' lines.
    pub(in crate::app) fn set_bill_lines(
        &mut self,
        bill_id: BillId,
        order_line_ids: Vec<OrderLineId>,
    ) {
        if let Some(record) = self.bills.get_mut(&bill_id) {
            record.order_line_ids = order_line_ids;
        }
    }

    /// Folds a settle into the projection, live and replayed alike: the bill settles, its receipt is
    /// kept to be copied (ADR-0164), and its tax by component joins `shift`, the shift its envelope
    /// is stamped with ([`Self::collect_tax_components`]).
    pub(in crate::app) fn fold_settle(
        &mut self,
        settled: &BillingBillSettled,
        shift: Option<ShiftId>,
        business_date: BusinessDate,
        settle_time: Timestamp,
    ) -> Result<(), DomainError> {
        self.settle_bill_record(settled.bill_id);
        self.record_settled(
            settled.bill_id,
            SettledReceipt::of(settled, business_date, settle_time),
        );
        self.collect_tax_components(shift, settled)
    }

    /// Adds a settled bill's tax by component to `shift_id`'s record, if the settle was stamped with
    /// a shift: each component its tax lines recorded, to the entry of its name and rate (ADR-0168
    /// decision 5). Fee tax is inside the tax lines, so the fee lines' own components are not
    /// added.
    fn collect_tax_components(
        &mut self,
        shift_id: Option<ShiftId>,
        settled: &BillingBillSettled,
    ) -> Result<(), DomainError> {
        let Some(record) = shift_id.and_then(|shift_id| self.shifts.get_mut(&shift_id)) else {
            return Ok(());
        };
        for component in settled.tax_lines.iter().flat_map(|line| &line.components) {
            billing::add_component(
                &mut record.tax_components,
                TaxComponentLine {
                    name: component.name.clone(),
                    rate_basis_points: component.rate_basis_points,
                    tax: component.tax,
                },
            )?;
        }
        Ok(())
    }

    /// Adds cash taken on a bill to `shift_id`'s rollup, if a shift took it.
    pub(in crate::app) fn collect_cash(
        &mut self,
        shift_id: Option<ShiftId>,
        amount: Money,
    ) -> Result<(), DomainError> {
        if let Some(shift_id) = shift_id
            && let Some(record) = self.shifts.get_mut(&shift_id)
        {
            record.cash_collected = record.cash_collected.checked_add(amount)?;
        }
        Ok(())
    }
}
