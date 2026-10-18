// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a settle leaves behind: the settled bills and today's takings, a receipt's copy, the buyer
//! it prints again, and the retention sweep that scrubs a buyer's record.

use pos_core::billing;
use pos_core::business_date::derive_business_date;
use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::events::BillingReceiptReprinted;
use pos_proto::ids::{BillId, SubjectId};
use pos_proto::time::Timestamp;
use pos_proto::{BillState, ClockSource};

use crate::app::{
    AppError, BuyerDetails, Edge, MAX_SETTLED_LISTED, MILLISECONDS_PER_DAY, ReceiptCopy,
    SettledBillView, Takings,
};

impl<S: EventStore> Edge<S> {
    /// The bills this store settled in the current business day, newest first, at most
    /// [`MAX_SETTLED_LISTED`] of them: what the till lists to reprint a receipt from
    /// ([ADR-0164](../../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
    ///
    /// A bill voided after it settled has no receipt to copy, so it is not listed.
    ///
    /// # Errors
    ///
    /// [`AppError::Clock`] if the business date cannot be derived.
    pub fn settled_bills(&self) -> Result<Vec<SettledBillView>, AppError> {
        let session = self.session();
        let today = derive_business_date(self.clock.now(), &session.timezone, session.cutoff)
            .map_err(|_ignored| AppError::Clock)?;
        let projection = self.lock_projection();
        Ok(projection
            .settled_on(today, MAX_SETTLED_LISTED)
            .into_iter()
            .filter_map(|(bill_id, receipt)| {
                let bill = projection.bill(bill_id)?;
                (bill.state == BillState::Settled).then_some(SettledBillView {
                    bill_id,
                    order_id: bill.order_id,
                    table_id: bill.table_id,
                    receipt_number: receipt.receipt_number,
                    total_due: receipt.total_due,
                    settle_time: receipt.settle_time,
                    copies: receipt.copies,
                })
            })
            .collect())
    }

    /// What this store has taken in the current business day ([`Takings`]): every bill settled
    /// today, each at what its guests paid, and how many. A bill the store no longer holds as
    /// settled is not in it, as it is not in [`Self::settled_bills`].
    ///
    /// Who may read it is the route's to decide, from the person's own role (ADR-0160 decision 2).
    ///
    /// # Errors
    ///
    /// [`AppError::Clock`] if the business date cannot be derived, or [`AppError::Domain`] if the
    /// bills cannot be added up.
    pub fn takings_today(&self) -> Result<Takings, AppError> {
        let session = self.session();
        let today = derive_business_date(self.clock.now(), &session.timezone, session.cutoff)
            .map_err(|_ignored| AppError::Clock)?;
        let (takings_amount, bill_count) =
            self.lock_projection().takings_on(today, session.currency)?;
        Ok(Takings {
            business_date: today,
            takings_amount,
            bill_count,
        })
    }

    /// Counts a copy of a settled bill's receipt (`billing.receipt.reprinted`) and hands back what
    /// to print, marked as a copy under the receipt's own number
    /// ([ADR-0164](../../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
    ///
    /// Under `billing.receipt.reprint`, which asks no PIN: every copy is counted, and flagging the
    /// outliers is the dashboard's job. The copy is counted when it is asked for, not when paper
    /// comes out, and its print job is keyed on the copy's own event, so a retried job prints once.
    /// The printing itself is the caller's, after the commit, as the settle's is.
    ///
    /// The figures are the settle's own (`SettledReceipt::totals`): the lines are the ones the
    /// bill covered, and the totals are recomputed only to recover what the settle did not record,
    /// which the copy prints when it still adds up to what was recorded.
    ///
    /// Two tills pressing at the same moment can both print copy *n*. Each is still its own event,
    /// so the count the dashboard reads stays right.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] for a bill the edge does not know; [`AppError::NotSettled`] for one
    /// that has not settled, or was voided since; [`AppError::ReceiptFromAnotherDay`] for one
    /// settled on an earlier business day; [`AppError::Domain`] without the permission; or
    /// [`AppError`] if the store cannot be written.
    pub async fn reprint_receipt(
        &self,
        actor: Actor,
        bill_id: BillId,
    ) -> Result<ReceiptCopy, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::ReprintReceipt)?;
        let (bill, receipt) = {
            let projection = self.lock_projection();
            let bill = projection.bill(bill_id).ok_or(AppError::UnknownBill)?;
            let receipt = projection.settled.get(&bill_id).cloned();
            (bill, receipt)
        };
        let Some(receipt) = receipt.filter(|_| bill.state == BillState::Settled) else {
            return Err(AppError::NotSettled);
        };
        if receipt.business_date != ctx.business_date {
            return Err(AppError::ReceiptFromAnotherDay);
        }

        let session = self.session();
        let order = self.bill_snapshot(&bill, &session)?;
        // Recomputed only for what the settle did not record: how its reductions divide, and the
        // per-rate tax lines, or their named parts, of a settle that recorded none. A rate table
        // that no longer prices one of these classes leaves the copy with the recorded figures.
        let recomputed = billing::assemble(&Self::bill_input(
            &session,
            &order,
            &bill.fee_rules,
            &bill.waived_fee_ids,
            bill.reductions(session.currency),
        ))
        .ok();
        let totals = receipt.totals(recomputed.as_ref());

        let copy_number = receipt.copies.saturating_add(1);
        let reprinted = BillingReceiptReprinted {
            bill_id,
            receipt_number: receipt.receipt_number,
            copy_number,
        };
        let (envelope, message) = self.prepare(&ctx, &reprinted)?;
        let job_id = envelope.event_id;
        self.append_and_publish(vec![envelope], vec![message])
            .await?;
        self.lock_projection().record_copy(bill_id, copy_number);
        Ok(ReceiptCopy {
            bill_id,
            receipt_number: receipt.receipt_number,
            copy_number,
            reprinted_time: ctx.now,
            lines: Self::receipt_lines(&order.lines),
            totals,
            buyer_subject_id: receipt.buyer_subject_id,
            job_id,
        })
    }

    /// The buyer a copy prints again, read back from the subject store (ADR-0107), or `None` when
    /// there was none, or its record has since been scrubbed by the retention sweep.
    ///
    /// # Errors
    ///
    /// [`AppError::Port`] if the subject store cannot be read.
    pub async fn buyer_of(&self, subject_id: SubjectId) -> Result<Option<BuyerDetails>, AppError>
    where
        S: SubjectStore,
    {
        let record = self
            .store
            .fetch(self.identity.store_id, subject_id)
            .await
            .map_err(AppError::Port)?;
        Ok(record
            .filter(|record| !record.is_masked())
            .and_then(|record| BuyerDetails::from_record(&record)))
    }

    /// Scrubs every personal record past the store's retention period, and says how many went
    /// ([ADR-0107](../../../../../docs/adr/0107-the-buyer-is-a-subject.md),
    /// [ADR-0035](../../../../../docs/adr/0035-retention-and-pii-masking.md)).
    ///
    /// The cutoff is `now − retention_days`, from the store's own published locale. One-way and
    /// idempotent: a record the sweep has already scrubbed is never touched again, so this is safe
    /// on a timer, safe after a restart, and safe after a clock step backwards.
    ///
    /// The **store** runs this, not the cloud, because the store is the only party holding these
    /// records and because ADR-0001's store sells with no internet — a retention obligation that
    /// only discharges while the link is up is not a retention obligation.
    ///
    /// # Errors
    ///
    /// [`AppError::Port`] if the store cannot be written.
    pub async fn sweep_expired_subjects(&self) -> Result<u64, AppError>
    where
        S: SubjectStore,
    {
        let now = self.clock.now();
        let days = i64::from(self.session().retention_days);
        // Saturating rather than wrapping: a cutoff that underflowed would land in the far future
        // and scrub every record the store holds, which is the one failure mode here that cannot be
        // undone.
        let cutoff_ms = now
            .as_milliseconds_since_epoch()
            .saturating_sub(days.saturating_mul(MILLISECONDS_PER_DAY));
        let Ok(cutoff) = Timestamp::from_milliseconds_since_epoch(cutoff_ms) else {
            // Before the epoch: the store's retention period is longer than the box has existed, so
            // nothing is due. Not an error, and not a reason to sweep with a cutoff of "now".
            return Ok(0);
        };
        self.store
            .mask_before(self.identity.store_id, cutoff, now)
            .await
            .map_err(AppError::Port)
    }
}

#[cfg(test)]
mod tests;
