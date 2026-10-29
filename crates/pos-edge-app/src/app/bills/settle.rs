// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Settling a bill: the payments that cover it, its receipt number, and the drawer its cash goes
//! into, committed in one transaction.

use pos_core::billing;
use pos_core::billing::{BillTotals, Payment};
use pos_core::decision::{
    Actor, BillCommand, DecisionCtx, Effect, TableCommand, decide_bill, decide_table,
};
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_ports::TxContext;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{BillingBillSettled, BillingPaymentCaptured};
use pos_proto::ids::{BillId, DeviceId, EmployeeId, PaymentId, ShiftId, SubjectId, TableId};
use pos_proto::money::Money;
use pos_proto::{BillState, Open, PaymentMethod, PaymentOutcome, TableState};

use crate::app::records::tax_records;
use crate::app::{
    AppError, Approval, BillView, BuyerDetails, Edge, OrderSnapshot, TillScope, fee_records,
};
use crate::fanout::ServerMessage;

impl<S: EventStore> Edge<S> {
    /// Settles a bill at the store's one drawer: [`Self::settle_bill_at`] from a device that is no
    /// till, which is what every device is where the store keeps one drawer.
    ///
    /// # Errors
    ///
    /// As [`Self::settle_bill_at`].
    pub async fn settle_bill(
        &self,
        actor: Actor,
        bill_id: BillId,
        payments: Vec<Payment>,
        buyer: Option<&BuyerDetails>,
    ) -> Result<BillView, AppError>
    where
        S: SubjectStore,
    {
        self.settle_bill_at(actor, TillScope::default(), bill_id, payments, buyer, None)
            .await
    }

    /// Settles a bill (`billing.bill.settled`) and cycles its table to needs-cleaning.
    ///
    /// Assembles what is owed from the order's captured line totals ([`billing::assemble`], §14.2),
    /// proves the payments sum **exactly** to it ([`billing::settle`] via [`decide_bill`], ADR-0028),
    /// then allocates the gapless per-store receipt number for this bill (ADR-0025) before appending
    /// the event that carries it — so a crash after allocating reuses the number rather than skipping
    /// one. The receipt is **not** a legal invoice number.
    ///
    /// The [`Effect::PrintReceipt`] the domain returns is surfaced on [`BillView::print_receipt`] for
    /// the caller to run after commit; the edge holds no printer, so a rolled-back settle prints
    /// nothing.
    ///
    /// # The buyer, when the sale is a B2B one
    ///
    /// `buyer` is the corporate customer a tax invoice is issued to
    /// ([ADR-0107](../../../../../docs/adr/0107-the-buyer-is-a-subject.md)) and is `None` on every
    /// ordinary retail sale. When it is present a [`SubjectId`] is minted, the buyer's details are
    /// filed in the store's subject store **in this settle's own transaction**, and the settled
    /// event carries the identifier alone — never the name or the registration number, which
    /// `pos_proto::pii` would refuse and `docs/pos-spec.md` §15 explains at length.
    ///
    /// One transaction is what makes both halves impossible to get wrong: a settle that committed
    /// without its buyer would print a compliant invoice with no record of who it was for, and a
    /// buyer that committed without its settle would hold a person's tax code for a sale that never
    /// happened.
    ///
    /// # The drawer the cash goes into
    ///
    /// Where the store keeps one drawer, every till's cash goes into it. Where it keeps one per till,
    /// the cash goes into the drawer of the till `scope` says the device is, and the payments are
    /// stamped with that drawer's shift, which is how a replay puts them back
    /// ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 5 and 6). A
    /// device that is no till takes any other tender, and no cash. Cash into a drawer assigned to
    /// somebody else takes the override, held directly or approved for this settle (decision 9);
    /// any other tender is the sale's, not the drawer's, and takes none.
    ///
    /// # Errors
    ///
    /// [`AppError::NotATill`] for cash at a device that is no till where the store keeps a drawer per
    /// till; [`AppError::OpenShiftRequired`] at a store that refuses selling without a shift, for
    /// cash while the drawer it goes into has none open and for anything else while no drawer has;
    /// [`AppError::DrawerAssignedToAnother`] and the approval refusals of [`Self::open_shift_at`]
    /// for cash into somebody else's drawer;
    /// [`AppError::UnknownBill`] for a bill the edge does not know; [`AppError::Domain`] if the
    /// bill is not open, the table is not awaiting payment, the payments do not sum to the total, or
    /// a line's tax class has no configured rate; or [`AppError`] if the store cannot be written.
    pub async fn settle_bill_at(
        &self,
        actor: Actor,
        scope: TillScope,
        bill_id: BillId,
        payments: Vec<Payment>,
        buyer: Option<&BuyerDetails>,
        approval: Option<&Approval>,
    ) -> Result<BillView, AppError>
    where
        S: SubjectStore,
    {
        let mut ctx = self.decision_ctx(actor)?;
        // Before the bill is read: a store that refuses selling without a shift takes no money
        // outside one, so every payment's cash is in a shift's expected drawer (ADR-0160).
        let (drawer, shift, overrider) =
            self.settle_drawer(&mut ctx, scope, &payments, approval)?;
        let proved = self.prove_settle(&ctx, bill_id, &payments)?;

        // Allocate the gapless receipt number for this bill, then append the events that record it.
        let receipt_number = self
            .receipts
            .allocate_receipt(self.identity.store_id, bill_id)
            .await?;

        let reduction_total = proved
            .totals
            .discount_total
            .checked_add(proved.totals.comp_total)
            .map_err(DomainError::from)?;

        // One `billing.payment.captured` per tender, then `billing.bill.settled`, all in one
        // transaction so a crash never leaves a receipt without its payments (or the reverse).
        let (mut envelopes, mut messages) =
            self.capture_payments(&ctx, bill_id, &payments, shift)?;
        let settled = self.settled_event(
            bill_id,
            receipt_number,
            &proved.totals,
            reduction_total,
            buyer,
        );
        self.prepare_settled(
            &ctx,
            &settled,
            shift,
            overrider,
            &mut envelopes,
            &mut messages,
        )?;
        let buyer = settled.buyer_subject_id.zip(buyer);
        self.commit_settle(&ctx, &envelopes, &messages, buyer)
            .await?;

        let table_state =
            self.fold_settled(&settled, shift, &ctx, proved.cash_taken, proved.table_id)?;
        // The last part of a split settles the order off the boards, and its notes go with it.
        self.forget_notes_off_screen();
        Ok(Self::settled_view(&proved, &settled, table_state, drawer))
    }

    /// Everything a settle proves before it allocates a receipt number, so a refused settle
    /// consumes none: the bill is known, its table may cycle, and the payments sum exactly to the
    /// total assembled from the order's captured line totals.
    fn prove_settle(
        &self,
        ctx: &DecisionCtx,
        bill_id: BillId,
        payments: &[Payment],
    ) -> Result<ProvedSettle, AppError> {
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;

        // Assemble the amount owed from the order's captured line totals, grouped per tax class, at
        // the rate for the channel *this order* came in on (B1.2).
        let session = self.session();
        let order = self.bill_snapshot(&bill, &session)?;
        let totals = billing::assemble(&Self::bill_input(
            &session,
            &order,
            &bill.fee_rules,
            &bill.waived_fee_ids,
            bill.reductions(session.currency),
        ))?;

        if let Some(table_id) = bill.table_id {
            self.prove_table_settles(table_id, ctx)?;
        }
        let cash_taken = self.cash_tendered(payments)?;

        // Prove the settlement invariant against the assembled total, and that the bill can settle.
        // Done before allocating a number, so a refused settle consumes none. The payments are cloned
        // because each is recorded again below as a captured-payment event.
        let bill_decision = decide_bill(
            bill.state,
            BillCommand::Settle {
                total_due: totals.total_due,
                payments: payments.to_vec(),
            },
            ctx,
        )?;
        Ok(ProvedSettle {
            table_id: bill.table_id,
            next_state: bill_decision.next_state,
            // The domain asks for a receipt on every settle; the store may print none (ADR-0160).
            print_receipt: bill_decision.effects.contains(&Effect::PrintReceipt)
                && session.printing.receipt_printed_on_settle(),
            order,
            totals,
            cash_taken,
        })
    }

    /// The settled event, from the same totals the settle proved. A B2B sale's buyer gets an id
    /// minted here, and the details go to the subject store when the settle commits; the event
    /// gets the identifier and nothing else (ADR-0107).
    fn settled_event(
        &self,
        bill_id: BillId,
        receipt_number: u64,
        totals: &BillTotals,
        reduction_total: Money,
        buyer: Option<&BuyerDetails>,
    ) -> BillingBillSettled {
        let buyer_subject_id = buyer.map(|_ignored| SubjectId::new(self.next_ulid()));
        BillingBillSettled {
            bill_id,
            receipt_number,
            subtotal: totals.subtotal,
            reduction_total,
            service_charge: totals.service_charge,
            tax_total: totals.tax_total,
            rounding_adjustment: totals.rounding_adjustment,
            total_due: totals.total_due,
            buyer_subject_id,
            // Each fee charged and the tax per class (ADR-0159 decision 4), from the same totals
            // as every figure above, so the lines add up to `service_charge` and `tax_total`.
            fee_lines: fee_records(totals),
            tax_lines: tax_records(totals),
        }
    }

    /// What the till is answered once the settle has committed and folded.
    fn settled_view(
        proved: &ProvedSettle,
        settled: &BillingBillSettled,
        table_state: Option<TableState>,
        drawer: Option<DeviceId>,
    ) -> BillView {
        BillView {
            bill_id: settled.bill_id,
            state: proved.next_state,
            receipt_number: Some(settled.receipt_number),
            total_due: Some(proved.totals.total_due),
            totals: Some(proved.totals.clone()),
            table_state,
            print_receipt: proved.print_receipt,
            open_drawer: proved.cash_taken.amount_minor > 0,
            drawer_till: drawer,
            lines: Self::receipt_lines(&proved.order.lines),
        }
    }

    /// Proves the table may cycle `AwaitingPayment -> NeedsCleaning`.
    ///
    /// A counter order has no table, so there is no floor move to prove and no tables capability to
    /// require (ADR-0093) — settling is ordinary cashier work either way. The answer is discarded:
    /// whether the table actually moves depends on whether this is the order's last open bill,
    /// which only the projection can say once the settle has committed.
    fn prove_table_settles(&self, table_id: TableId, ctx: &DecisionCtx) -> Result<(), AppError> {
        let _legal = decide_table(self.table_state(table_id), TableCommand::Settle, ctx)?;
        Ok(())
    }

    /// The cash tenders (not card, not tips, not rounding): what lands in the drawer for the open
    /// shift's blind-close roll-up. Summed before the payments move into the command.
    fn cash_tendered(&self, payments: &[Payment]) -> Result<Money, AppError> {
        let mut cash_taken = Money::zero(self.session().currency);
        for payment in payments {
            if payment.method == PaymentMethod::Cash {
                cash_taken = cash_taken
                    .checked_add(payment.applied_to_bill)
                    .map_err(DomainError::from)?;
            }
        }
        Ok(cash_taken)
    }

    /// Prepares the settled event, then the override somebody else's drawer took when it did, onto
    /// the payments already prepared for the settle's transaction.
    fn prepare_settled(
        &self,
        ctx: &DecisionCtx,
        settled: &BillingBillSettled,
        shift: Option<ShiftId>,
        overrider: Option<EmployeeId>,
        envelopes: &mut Vec<EventEnvelope<RawPayload>>,
        messages: &mut Vec<ServerMessage>,
    ) -> Result<(), AppError> {
        let (settled_envelope, settled_message) = self.prepare_on(ctx, settled, shift)?;
        envelopes.push(settled_envelope);
        messages.push(settled_message);
        if let Some(approver) = overrider {
            let overridden =
                Self::override_record(Permission::OverrideDrawerAssignment, approver, None);
            let (envelope, message) = self.prepare_on(ctx, &overridden, shift)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        Ok(())
    }

    /// Commits the settle's events AND the buyer's record in ONE transaction (ADR-0107), which is
    /// why a settle drives the transaction itself instead of calling `append_and_publish`, then
    /// publishes the events once they are committed.
    async fn commit_settle(
        &self,
        ctx: &DecisionCtx,
        envelopes: &[EventEnvelope<RawPayload>],
        messages: &[ServerMessage],
        buyer: Option<(SubjectId, &BuyerDetails)>,
    ) -> Result<(), AppError>
    where
        S: SubjectStore,
    {
        let mut tx = self.store.begin().await?;
        self.store.append(&mut tx, envelopes).await?;
        if let Some((subject_id, buyer)) = buyer {
            // `ctx.now` rather than a second clock read: the retention period counts from the moment
            // the sale is stamped with, so the record and the event it belongs to age together.
            let record = SubjectRecord::new(ctx.now, buyer.fields());
            self.store
                .record(&mut tx, self.identity.store_id, subject_id, &record)
                .await?;
        }
        tx.commit().await?;
        for message in messages {
            self.fanout.publish(message);
        }
        Ok(())
    }

    /// Folds the committed settle into the projection — what the envelope carries, so a box that
    /// replays the log keeps the same record — and collects its cash into the drawer; answers the
    /// table's state after it.
    fn fold_settled(
        &self,
        settled: &BillingBillSettled,
        shift: Option<ShiftId>,
        ctx: &DecisionCtx,
        cash_taken: Money,
        table_id: Option<TableId>,
    ) -> Result<Option<TableState>, AppError> {
        let mut projection = self.lock_projection();
        projection.fold_settle(settled, shift, ctx.business_date, ctx.now)?;
        projection.collect_cash(shift, cash_taken)?;
        Ok(table_id.map(|table_id| projection.table_state(table_id)))
    }

    /// One `billing.payment.captured` per tender, prepared for the settle's transaction.
    ///
    /// The captured payments are what let the shift cash roll-up and the tip-out be rebuilt from the
    /// log; each records its own change and its own tip, held apart from the sale. Returned as
    /// envelopes and messages so the caller appends the settled event to the same pair and commits
    /// them together. Each is stamped with `shift`, the shift of the drawer the payment was taken
    /// into, which names that drawer (ADR-0167 decision 5).
    fn capture_payments(
        &self,
        ctx: &DecisionCtx,
        bill_id: BillId,
        payments: &[Payment],
        shift: Option<ShiftId>,
    ) -> Result<(Vec<EventEnvelope<RawPayload>>, Vec<ServerMessage>), AppError> {
        let zero = Money::zero(self.session().currency);
        let mut envelopes = Vec::with_capacity(payments.len() + 1);
        let mut messages = Vec::with_capacity(payments.len() + 1);
        for payment in payments {
            // `Payment::change` subtracts the tip; computing it here as `tendered − applied` was the
            // arithmetic half of B1.3 and over-reported the change by exactly the tip. The clamp
            // stays: a tender that does not cover its own share plus its tip owes no change, and
            // `settle` has already refused the settlement if that is true in total.
            let change = payment.change().map_err(AppError::Domain)?;
            let captured = BillingPaymentCaptured {
                bill_id,
                payment_id: PaymentId::new(self.next_ulid()),
                method: Open::from_known(payment.method),
                outcome: Open::from_known(PaymentOutcome::Captured),
                tendered: payment.tendered,
                applied_to_bill: payment.applied_to_bill,
                change_given: if change.is_negative() { zero } else { change },
                tip_amount: payment.tip,
            };
            let (envelope, message) = self.prepare_on(ctx, &captured, shift)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        Ok((envelopes, messages))
    }
}

/// What [`Edge::prove_settle`] proved, carried to the settle's record and its answer.
struct ProvedSettle {
    /// The bill's table, or `None` for a counter order.
    table_id: Option<TableId>,
    /// The bill's state once settled.
    next_state: BillState,
    /// Whether the domain asked for a receipt and the store prints one on settle.
    print_receipt: bool,
    /// The order the bill was assembled from, whose sold lines the receipt lists.
    order: OrderSnapshot,
    /// What was owed.
    totals: BillTotals,
    /// The cash tenders, which go into the drawer.
    cash_taken: Money,
}
