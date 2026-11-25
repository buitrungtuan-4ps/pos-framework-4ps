// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bill taking its payments one at a time
//! ([ADR-0220](../../../../../docs/adr/0220-a-bill-takes-its-payments-one-at-a-time.md)): each is
//! recorded as it is taken, every till sees what is paid, and the last one settles.

use pos_core::StateMachine;
use pos_core::billing;
use pos_core::billing::{BillTotals, Payment};
use pos_core::capability::Capability;
use pos_core::decision::{Actor, DecisionCtx};
use pos_core::error::DomainError;
use pos_core::machines::{Bill, BillTrigger};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::events::BillingPaymentCaptured;
use pos_proto::ids::{BillId, DeviceId, EmployeeId, FeeId, OrderId, PaymentId, ShiftId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::TerminalReference;
use pos_proto::time::Timestamp;
use pos_proto::{Open, PaymentMethod, PaymentOutcome};

use super::card::CardMark;
use crate::app::{AppError, Approval, BillRecord, BillView, Edge, TillScope};

/// What came of one payment towards a bill.
#[derive(Debug, Clone)]
pub struct PaymentTaken {
    /// What the bill's payments now come to.
    pub paid_total: Money,
    /// What the bill still owes: zero once this payment settled it.
    pub remaining: Money,
    /// Whether cash went into a drawer, which the till opens.
    pub open_drawer: bool,
    /// The till whose drawer took the cash, where the store keeps one per till.
    pub drawer_till: Option<DeviceId>,
    /// The settle, when this payment was the last.
    pub settled: Option<BillView>,
}

/// One payment taken towards a bill that is still open, as every till reads it (ADR-0220 decision
/// 3): how, how much, and when. No person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakenPayment {
    /// The tender.
    pub method: PaymentMethod,
    /// What it applied to the bill, change and tip apart.
    pub amount: Money,
    /// When it was taken.
    pub taken_time: Timestamp,
    /// How many equal shares an even split asked for, when it was one of them.
    pub split_ways: Option<u8>,
}

impl TakenPayment {
    /// `captured`, taken at `taken_time`.
    pub(in crate::app) fn of(captured: &BillingPaymentCaptured, taken_time: Timestamp) -> Self {
        Self {
            method: captured.method.known(),
            amount: captured.applied_to_bill,
            taken_time,
            split_ways: captured.split_ways,
        }
    }
}

impl<S: EventStore> Edge<S> {
    /// Takes one payment towards an open bill (ADR-0220 decisions 1 and 2).
    ///
    /// A payment that covers what remains settles the bill, as [`Self::settle_bill_at`] does, in
    /// the same transaction as its `billing.payment.captured`, taking the receipt number then: one
    /// bill, one receipt. Any other is recorded at once, in its own transaction, and the bill stays
    /// open; its cash goes into the drawer and its shift as a settle's does, and the drawer's shift
    /// does not close until the bill settles. A payment above what remains is refused, never
    /// turned into change on another tender. `split_ways` is the number of equal shares an even
    /// split asked for, recorded on the payment for every till to read.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`]; [`AppError::PaymentExceedsRemaining`];
    /// [`AppError::PartPaymentsOff`] for a payment that does not settle the bill at a store that takes
    /// a bill in one payment; [`AppError::CheckRequired`] as a settle; the drawer refusals of
    /// [`Self::settle_bill_at`]; [`AppError::Domain`] for a bill that is not open, a permission not
    /// held, tips at a store that takes none, or a tender that does not cover itself; or
    /// [`AppError::Port`] if the store cannot be written.
    pub async fn take_payment_at(
        &self,
        actor: Actor,
        scope: TillScope,
        bill_id: BillId,
        payment: Payment,
        split_ways: Option<u8>,
        approval: Option<&Approval>,
    ) -> Result<PaymentTaken, AppError>
    where
        S: SubjectStore,
    {
        self.refuse_parked(bill_id)?;
        self.take_tender(actor, scope, bill_id, (payment, None), split_ways, approval)
            .await
    }

    /// [`Self::take_payment_at`] for a bill already known not to be parked, recording the payment
    /// as `card`'s attempt where the card terminal took it (ADR-0230): its `payment_id`, its
    /// reference, and for a kiosk the one payment its record may take.
    pub(in crate::app) async fn take_tender(
        &self,
        actor: Actor,
        scope: TillScope,
        bill_id: BillId,
        (payment, card): (Payment, Option<&CardMark>),
        split_ways: Option<u8>,
        approval: Option<&Approval>,
    ) -> Result<PaymentTaken, AppError>
    where
        S: SubjectStore,
    {
        let session = self.session();
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        let totals = self.bill_totals(bill_id)?;
        let paid = self.paid_total(bill_id, session.currency)?;
        let remaining = owed(&totals, paid)?;
        let applied = payment.applied_to_bill;
        if remaining
            .checked_sub(applied)
            .map_err(DomainError::from)?
            .is_negative()
        {
            return Err(AppError::PaymentExceedsRemaining);
        }
        if applied == remaining {
            let settled = self
                .settle_shares(
                    actor,
                    scope,
                    bill_id,
                    (vec![payment], split_ways, card),
                    None,
                    approval,
                )
                .await?;
            return Ok(PaymentTaken {
                paid_total: totals.total_due,
                remaining: Money::zero(session.currency),
                open_drawer: settled.open_drawer,
                drawer_till: settled.drawer_till,
                settled: Some(settled),
            });
        }
        if !session.billing.part_payments() {
            return Err(AppError::PartPaymentsOff);
        }

        let mut ctx = self.decision_ctx(actor)?;
        CardMark::admit(card, &mut ctx);
        let (drawer, shift, overrider) =
            self.settle_drawer(&mut ctx, scope, std::slice::from_ref(&payment), approval)?;
        // The checks a settle makes, for a payment that does not settle: the bill is open, the
        // person takes payment, a tip needs tips on, and the tender covers itself.
        Bill::step(bill.state, BillTrigger::Settle).map_err(DomainError::from)?;
        ctx.require(Permission::TakePayment)?;
        if !payment.tip.is_zero() {
            ctx.require_capability(Capability::Tips)?;
        }
        if session.billing.check_before_payment() {
            self.require_fresh_check(bill.order_id, bill_id)?;
        }
        let change = payment.change().map_err(AppError::Domain)?;
        if change.is_negative() {
            return Err(AppError::Domain(DomainError::NegativeChange));
        }

        let captured = BillingPaymentCaptured {
            bill_id,
            payment_id: card
                .map_or_else(|| PaymentId::new(self.next_ulid()), |card| card.payment_id),
            method: Open::from_known(payment.method),
            outcome: Open::from_known(PaymentOutcome::Captured),
            tendered: payment.tendered,
            applied_to_bill: applied,
            change_given: change,
            tip_amount: payment.tip,
            split_ways,
            terminal_reference: card.map(|card| TerminalReference::new(card.reference.as_str())),
        };
        self.commit_part_payment(&ctx, &captured, shift, (overrider, card))
            .await?;
        let paid_total = paid.checked_add(applied).map_err(DomainError::from)?;
        Ok(PaymentTaken {
            paid_total,
            remaining: owed(&totals, paid_total)?,
            open_drawer: payment.method == PaymentMethod::Cash,
            drawer_till: drawer,
            settled: None,
        })
    }

    /// Commits a payment that does not settle its bill, with the override somebody else's drawer
    /// took when it did, in one transaction stamped with the drawer's `shift`, then folds it as the
    /// replay folds it: onto its bill, the shift's tenders and, for cash, the drawer.
    async fn commit_part_payment(
        &self,
        ctx: &DecisionCtx,
        captured: &BillingPaymentCaptured,
        shift: Option<ShiftId>,
        (overrider, card): (Option<EmployeeId>, Option<&CardMark>),
    ) -> Result<(), AppError> {
        let (envelope, message) = self.prepare_on(ctx, captured, shift)?;
        let mut envelopes = vec![envelope];
        let mut messages = vec![message];
        // A card confirmed by hand names its manager in the same transaction (ADR-0243).
        if let Some(record) = CardMark::approval_record(card) {
            let (envelope, message) = self.prepare_on(ctx, &record, shift)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        if let Some(approver) = overrider {
            let record =
                Self::override_record(Permission::OverrideDrawerAssignment, approver, None);
            let (envelope, message) = self.prepare_on(ctx, &record, shift)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;

        let method = captured.method.known();
        let applied = captured.applied_to_bill;
        let mut projection = self.lock_projection();
        projection.record_payment(captured.bill_id, method, applied)?;
        projection.collect_tender(shift, method, applied)?;
        if method == PaymentMethod::Cash {
            projection.collect_cash(shift, applied)?;
        }
        projection.record_part_payment(captured.bill_id, shift);
        projection.record_taken(captured.bill_id, TakenPayment::of(captured, ctx.now));
        Ok(())
    }

    /// What has been paid towards the bills of `order_id` that are still open, or towards `bill_id`
    /// alone where one is named (ADR-0220 decision 3): `None` where nobody has paid anything yet.
    #[must_use]
    pub fn paid_towards(&self, order_id: OrderId, bill_id: Option<BillId>) -> Option<Money> {
        let projection = self.lock_projection();
        let bills =
            bill_id.map_or_else(|| projection.open_bills_for_order(order_id), |id| vec![id]);
        let mut paid: Option<Money> = None;
        for paid_bill in bills.iter().filter_map(|id| projection.paid(*id)) {
            let sum = paid_bill.cash.checked_add(paid_bill.other).ok()?;
            paid = Some(match paid {
                Some(running) => running.checked_add(sum).ok()?,
                None => sum,
            });
        }
        paid
    }

    /// The payments taken towards the bills of `order_id` that are still open, or towards `bill_id`
    /// alone where one is named, in the order they were taken (ADR-0220 decision 3).
    #[must_use]
    pub fn payments_towards(
        &self,
        order_id: OrderId,
        bill_id: Option<BillId>,
    ) -> Vec<TakenPayment> {
        let projection = self.lock_projection();
        let bills =
            bill_id.map_or_else(|| projection.open_bills_for_order(order_id), |id| vec![id]);
        let mut taken: Vec<TakenPayment> = bills
            .iter()
            .flat_map(|id| projection.taken(*id).iter().copied())
            .collect();
        taken.sort_by_key(|payment| payment.taken_time);
        taken
    }

    /// What `bill_id`'s payments have come to so far, in `currency`: zero for a bill nobody paid.
    pub(in crate::app) fn paid_total(
        &self,
        bill_id: BillId,
        currency: CurrencyCode,
    ) -> Result<Money, AppError> {
        Ok(match self.lock_projection().paid(bill_id) {
            Some(paid) => paid
                .cash
                .checked_add(paid.other)
                .map_err(DomainError::from)?,
            None => Money::zero(currency),
        })
    }

    /// Refuses an act that would regroup `bill_ids` while any of them holds a payment
    /// (ADR-0220 decision 4): what was paid is the bill's, and a split or a merge would leave it on
    /// nobody's.
    pub(in crate::app) fn refuse_part_paid(&self, bill_ids: &[BillId]) -> Result<(), AppError> {
        let projection = self.lock_projection();
        if bill_ids
            .iter()
            .any(|bill_id| projection.paid(*bill_id).is_some())
        {
            return Err(AppError::BillPartPaid);
        }
        Ok(())
    }

    /// Refuses a reduction that would bring `bill_id`'s total below what has been paid towards it
    /// (ADR-0220 decision 4). `reductions` and `waived` are the bill's discount and comp and its
    /// waived fees as they would stand with the reduction on it; a bill nobody has paid towards is
    /// not computed again.
    pub(in crate::app) fn refuse_below_paid(
        &self,
        bill_id: BillId,
        bill: &BillRecord,
        reductions: (Money, Money),
        waived: &[FeeId],
    ) -> Result<(), AppError> {
        if self.lock_projection().paid(bill_id).is_none() {
            return Ok(());
        }
        let session = self.session();
        let order = self.bill_snapshot(bill, &session)?;
        let reduced = billing::assemble(&Self::bill_input(
            &session,
            &order,
            &bill.fee_rules,
            waived,
            reductions,
        ))?;
        let paid = self.paid_total(bill_id, session.currency)?;
        if owed(&reduced, paid)?.is_negative() {
            return Err(AppError::BillPartPaid);
        }
        Ok(())
    }
}

/// What a bill whose total is `totals` still owes once `paid` has been paid towards it.
fn owed(totals: &BillTotals, paid: Money) -> Result<Money, AppError> {
    Ok(totals
        .total_due
        .checked_sub(paid)
        .map_err(DomainError::from)?)
}
