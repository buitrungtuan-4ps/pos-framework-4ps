// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A card taken through the card terminal
//! ([ADR-0230](../../../../../docs/adr/0230-a-card-is-taken-through-the-terminal-port-and-an-unknown-result-parks-the-bill.md)).
//!
//! The edge mints the payment's id, asks the terminal, and records what the terminal concluded. A
//! capture is a payment as any other; a decline records nothing; an outcome the terminal could not
//! give **parks** the bill, which then takes no payment, settle or void until a person looks the
//! attempt up, confirms it from the terminal's slip, or cancels it on the terminal. Nothing here
//! ever guesses: a timeout is never a decline. No card data is held, only the terminal's reference.

use std::sync::Arc;

use pos_core::StateMachine;
use pos_core::billing::Payment;
use pos_core::decision::{Actor, DecisionCtx};
use pos_core::error::DomainError;
use pos_core::machines::{Bill, BillTrigger};
use pos_core::permission::Permission;
use pos_ports::dynamic::DynPaymentTerminal;
use pos_ports::error::PortError;
use pos_ports::event_store::EventStore;
use pos_ports::payment::{PaymentAttempt, PaymentReference, PaymentRequest};
use pos_ports::subject_store::SubjectStore;
use pos_proto::error::ErrorStatus;
use pos_proto::events::BillingPaymentCaptured;
use pos_proto::ids::{BillId, PaymentId};
use pos_proto::money::Money;
use pos_proto::text::TerminalReference;
use pos_proto::{Open, PaymentMethod, PaymentOutcome};

use super::PaymentTaken;
use crate::app::{AppError, Edge, Projection, TillScope};

/// The terminal an edge takes cards through, behind a trait object.
#[derive(Clone)]
pub(in crate::app) struct CardTerminal(pub(in crate::app) Arc<dyn DynPaymentTerminal>);

impl core::fmt::Debug for CardTerminal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CardTerminal")
    }
}

/// Why a card through the terminal was refused (ADR-0230), each with the reason the till reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CardRefusal {
    /// The edge has no card terminal: a card is recorded by hand (decision 1).
    #[error("this store takes no card through a terminal")]
    TerminalNone,
    /// The card terminal was not reached, and nothing was recorded (decision 2).
    #[error("the card terminal could not be reached")]
    TerminalUnavailable,
    /// The bill is parked on a card whose outcome is not known, and takes nothing else until it is
    /// resolved (decision 2).
    #[error("a card's result on this bill is not known yet")]
    ResultUnknown,
    /// The bill is not parked on the card named (decision 3).
    #[error("this bill is not waiting on that card")]
    NotParked,
    /// A cancel of a card the terminal had already settled: confirm it instead (decision 3).
    #[error("the card terminal has already taken that card")]
    AlreadyCaptured,
}

impl CardRefusal {
    /// Every refusal, for a check that the edge answers each.
    pub const ALL: [Self; 5] = [
        Self::TerminalNone,
        Self::TerminalUnavailable,
        Self::ResultUnknown,
        Self::NotParked,
        Self::AlreadyCaptured,
    ];

    /// The token the till translates (`pos-error-reason`).
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::TerminalNone => "CARD_TERMINAL_NONE",
            Self::TerminalUnavailable => "CARD_TERMINAL_UNAVAILABLE",
            Self::ResultUnknown => "CARD_RESULT_UNKNOWN",
            Self::NotParked => "CARD_NOT_PARKED",
            Self::AlreadyCaptured => "CARD_ALREADY_CAPTURED",
        }
    }
}

/// A card a till asks the terminal to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardAsk {
    /// What it applies to the bill.
    pub amount: Money,
    /// The tip on it, taken on the terminal with the amount and held apart from the sale.
    pub tip: Money,
    /// How many equal shares an even split asked for, when it is one of them (ADR-0220).
    pub split_ways: Option<u8>,
}

/// The attempt a bill is parked on: its outcome is not known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedCard {
    /// The attempt.
    pub payment_id: PaymentId,
    /// The terminal's reference for it, which a look-up and a cancel use.
    pub reference: String,
    /// What it would apply to the bill.
    pub amount: Money,
    /// The tip on it.
    pub tip: Money,
}

/// What came of a card.
#[derive(Debug, Clone)]
pub enum CardOutcome {
    /// The terminal took it, and it is recorded as a payment.
    Captured {
        /// The attempt.
        payment_id: PaymentId,
        /// What the payment came to on the bill, boxed: it holds the settled bill.
        taken: Box<PaymentTaken>,
    },
    /// The terminal declined it, and nothing was recorded (or, for a parked attempt, its decline).
    Declined {
        /// The attempt.
        payment_id: PaymentId,
    },
    /// The outcome is not known: the bill is parked on this attempt.
    Unknown {
        /// The attempt.
        payment_id: PaymentId,
    },
}

/// How a person resolves a parked card (ADR-0230 decision 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardResolution {
    /// Ask the terminal what became of it.
    LookUp,
    /// The terminal's own slip says it was approved.
    Confirm,
    /// Void it on the terminal.
    Cancel,
}

/// A payment the terminal took: the id minted before it was asked and its reference, carried to
/// the `billing.payment.captured` that records it.
#[derive(Debug, Clone)]
pub(in crate::app) struct CardMark {
    pub(in crate::app) payment_id: PaymentId,
    pub(in crate::app) reference: String,
    /// Whether a kiosk's own record takes it, which may take this one payment and no other
    /// (ADR-0230 decision 7).
    pub(in crate::app) kiosk: bool,
}

impl CardMark {
    /// Lets a kiosk's own record take the card the terminal took for it, and nothing else: the
    /// fixed kiosk set does not hold `billing.payment.take`, and this is the one payment it makes.
    pub(in crate::app) fn admit(card: Option<&Self>, ctx: &mut DecisionCtx) {
        if card.is_some_and(|card| card.kiosk) {
            ctx.granted.insert(Permission::TakePayment);
            ctx.granted_directly.insert(Permission::TakePayment);
        }
    }
}

impl Projection {
    /// Parks `bill_id` on `card`.
    pub(in crate::app) fn park_card(&mut self, bill_id: BillId, card: ParkedCard) {
        self.parked_cards.insert(bill_id, card);
    }

    /// Ends `payment_id`'s park on `bill_id`, if the bill is parked on it.
    pub(in crate::app) fn unpark_card(&mut self, bill_id: BillId, payment_id: PaymentId) {
        if self
            .parked_cards
            .get(&bill_id)
            .is_some_and(|card| card.payment_id == payment_id)
        {
            self.parked_cards.remove(&bill_id);
        }
    }

    /// Folds what the terminal concluded of a payment (ADR-0230 decision 5): a capture ends its
    /// park and moves money, an attempt with no outcome parks its bill, and a decline ends the park.
    /// Whether the payment moves money.
    pub(in crate::app) fn fold_card_outcome(&mut self, event: &BillingPaymentCaptured) -> bool {
        match event.outcome.known() {
            PaymentOutcome::Captured => {
                self.unpark_card(event.bill_id, event.payment_id);
                true
            }
            PaymentOutcome::Declined => {
                self.unpark_card(event.bill_id, event.payment_id);
                false
            }
            _ => {
                let reference = event.terminal_reference.as_ref();
                self.park_card(
                    event.bill_id,
                    ParkedCard {
                        payment_id: event.payment_id,
                        reference: reference.map(|r| r.as_str().to_owned()).unwrap_or_default(),
                        amount: event.applied_to_bill,
                        tip: event.tip_amount,
                    },
                );
                false
            }
        }
    }

    /// The attempt `bill_id` is parked on.
    pub(in crate::app) fn parked_card(&self, bill_id: BillId) -> Option<&ParkedCard> {
        self.parked_cards.get(&bill_id)
    }
}

/// Whether `attempt` is a capture of exactly `total`: anything else that is not a decline is not
/// known, a partial authorisation included (ADR-0230 decision 2).
fn captured_exactly(attempt: &PaymentAttempt, total: Money) -> bool {
    attempt.is_captured() && attempt.amount == total
}

/// Whether the terminal concluded a decline.
fn declined(attempt: &PaymentAttempt) -> bool {
    attempt.outcome.known() == PaymentOutcome::Declined
}

/// The refusal a terminal's error is: the port promises an error only where the terminal was never
/// reached, so nothing happened.
fn terminal_error(error: &PortError) -> AppError {
    tracing::warn!(status = ?error.status(), "the card terminal was not reached");
    AppError::Card(CardRefusal::TerminalUnavailable)
}

impl<S: EventStore> Edge<S> {
    /// Refuses an act on `bill_id` while it is parked on a card (ADR-0230 decision 2).
    pub(in crate::app) fn refuse_parked(&self, bill_id: BillId) -> Result<(), AppError> {
        if self.lock_projection().parked_card(bill_id).is_some() {
            return Err(AppError::Card(CardRefusal::ResultUnknown));
        }
        Ok(())
    }
}

impl<S: EventStore + SubjectStore> Edge<S> {
    /// The attempt `bill_id` is parked on, for every till's read.
    #[must_use]
    pub fn parked_card(&self, bill_id: BillId) -> Option<ParkedCard> {
        self.lock_projection().parked_card(bill_id).cloned()
    }

    /// Whether this edge takes cards through a terminal.
    #[must_use]
    pub fn takes_card_by_terminal(&self) -> bool {
        self.card_terminal.is_some()
    }

    fn terminal(&self) -> Result<CardTerminal, AppError> {
        self.card_terminal
            .clone()
            .ok_or(AppError::Card(CardRefusal::TerminalNone))
    }

    /// Takes a card towards `bill_id` through the terminal (ADR-0230 decision 2).
    ///
    /// A kiosk's own record may take one only for a bill on an order it opened, for the whole of
    /// what remains, with no tip (decision 7).
    ///
    /// # Errors
    ///
    /// [`CardRefusal::TerminalNone`] where the edge has no terminal;
    /// [`CardRefusal::ResultUnknown`] for a parked bill; [`AppError::UnknownBill`];
    /// [`AppError::PaymentExceedsRemaining`]; [`AppError::Domain`] for a bill that is not open or a
    /// permission not held; [`CardRefusal::TerminalUnavailable`] where the terminal was not
    /// reached, when nothing was recorded; or the refusals of [`Self::take_payment_at`] for a
    /// capture, which then parks the bill instead.
    pub async fn take_card_at(
        &self,
        actor: Actor,
        scope: TillScope,
        bill_id: BillId,
        ask: CardAsk,
    ) -> Result<CardOutcome, AppError> {
        let terminal = self.terminal()?;
        self.refuse_parked(bill_id)?;
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        Bill::step(bill.state, BillTrigger::Settle).map_err(DomainError::from)?;
        let paid = self.paid_total(bill_id, self.session().currency)?;
        let remaining = self
            .bill_totals(bill_id)?
            .total_due
            .checked_sub(paid)
            .map_err(DomainError::from)?;
        if ask.amount.amount_minor <= 0
            || remaining
                .checked_sub(ask.amount)
                .map_err(DomainError::from)?
                .is_negative()
        {
            return Err(AppError::PaymentExceedsRemaining);
        }
        let kiosk = self.is_kiosk(actor.employee_id)
            && self.opened_by(bill.order_id, actor.employee_id)
            && ask.amount == remaining
            && ask.tip.is_zero();
        if !kiosk {
            // A kiosk's record holds no `billing.payment.take`, so a kiosk outside its one
            // payment is refused here as any person without it is.
            self.decision_ctx(actor)?.require(Permission::TakePayment)?;
        }

        // The id first: a key minted after the answer could not deduplicate an answer that never
        // came (the port's contract, rule 2).
        let payment_id = PaymentId::new(self.next_ulid());
        let total = ask.amount.checked_add(ask.tip).map_err(DomainError::from)?;
        let request = PaymentRequest {
            payment_id,
            store_id: self.identity.store_id,
            bill_id,
            amount: total,
            method: PaymentMethod::Card,
        };
        let attempt = terminal
            .0
            .authorize(&request)
            .await
            .map_err(|error| terminal_error(&error))?;
        let parked = ParkedCard {
            payment_id,
            reference: attempt.reference.as_str().to_owned(),
            amount: ask.amount,
            tip: ask.tip,
        };
        if declined(&attempt) {
            return Ok(CardOutcome::Declined { payment_id });
        }
        if captured_exactly(&attempt, total) {
            let mark = CardMark {
                payment_id,
                reference: parked.reference.clone(),
                kiosk,
            };
            return self
                .record_card_capture(actor, scope, bill_id, (&parked, &mark), ask.split_ways)
                .await;
        }
        self.record_card_attempt(actor, bill_id, &parked, PaymentOutcome::Unknown)
            .await?;
        Ok(CardOutcome::Unknown { payment_id })
    }

    /// Resolves the card `bill_id` is parked on (ADR-0230 decision 3).
    ///
    /// # Errors
    ///
    /// [`CardRefusal::NotParked`] where the bill is not parked on `payment_id`;
    /// [`CardRefusal::TerminalNone`] or [`CardRefusal::TerminalUnavailable`] for a look-up or a
    /// cancel the terminal cannot answer; [`CardRefusal::AlreadyCaptured`] for a cancel of an
    /// attempt the terminal settled; [`AppError::Domain`] for a permission not held; or the
    /// refusals of [`Self::take_payment_at`] for a capture.
    pub async fn resolve_card_at(
        &self,
        actor: Actor,
        scope: TillScope,
        (bill_id, payment_id): (BillId, PaymentId),
        resolution: CardResolution,
    ) -> Result<CardOutcome, AppError> {
        self.resolve_card_with(actor, scope, (bill_id, payment_id), resolution, None)
            .await
    }

    /// [`Self::resolve_card_at`], with the manager's `approval` a `CONFIRM` needs where the store's
    /// `billing.card_confirm_approval` is `MANAGER`, the default
    /// ([ADR-0243](../../../../../docs/adr/0243-a-hand-confirmed-card-needs-a-manager.md)). The
    /// approval is checked as a void's is, against `billing.bill.void`, and recorded beside the
    /// capture with the manager's id. A look-up and a cancel need none.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve_card_at`], and [`AppError::ApprovalRequired`] or
    /// [`AppError::ApprovalRefused`] for a `CONFIRM` without a manager who may approve it.
    pub async fn resolve_card_with(
        &self,
        actor: Actor,
        scope: TillScope,
        (bill_id, payment_id): (BillId, PaymentId),
        resolution: CardResolution,
        approval: Option<&crate::app::Approval>,
    ) -> Result<CardOutcome, AppError> {
        let parked = self
            .parked_card(bill_id)
            .filter(|card| card.payment_id == payment_id)
            .ok_or(AppError::Card(CardRefusal::NotParked))?;
        self.decision_ctx(actor)?.require(Permission::TakePayment)?;
        let approver = self.confirm_approver(actor, resolution, approval)?;
        let reference = PaymentReference::new(parked.reference.as_str());
        let total = parked
            .amount
            .checked_add(parked.tip)
            .map_err(DomainError::from)?;
        let captured = match resolution {
            CardResolution::Confirm => true,
            CardResolution::LookUp => match self.terminal()?.0.look_up(&reference).await {
                Ok(attempt) if captured_exactly(&attempt, total) => true,
                Ok(attempt) if declined(&attempt) => false,
                Ok(_) => return Ok(CardOutcome::Unknown { payment_id }),
                // A reference the acquirer never saw: the money did not move (the port, `look_up`).
                Err(error) if error.status() == ErrorStatus::NotFound => false,
                Err(error) => return Err(terminal_error(&error)),
            },
            CardResolution::Cancel => match self.terminal()?.0.void(&reference).await {
                Ok(attempt) if attempt.is_captured() => {
                    return Err(AppError::Card(CardRefusal::AlreadyCaptured));
                }
                Ok(_) => false,
                Err(error) if error.status() == ErrorStatus::FailedPrecondition => {
                    return Err(AppError::Card(CardRefusal::AlreadyCaptured));
                }
                Err(error) if error.status() == ErrorStatus::NotFound => false,
                Err(error) => return Err(terminal_error(&error)),
            },
        };
        if captured {
            let mark = CardMark {
                payment_id,
                reference: parked.reference.clone(),
                kiosk: false,
            };
            let outcome = self
                .record_card_capture(actor, scope, bill_id, (&parked, &mark), None)
                .await?;
            if let Some(approver) = approver {
                // The manager who agreed, beside the capture, as a void records its approver.
                let ctx = self.decision_ctx(actor)?;
                let record = Self::override_record(Permission::VoidBill, approver, None);
                let (envelope, message) = self.prepare_on(&ctx, &record, None)?;
                self.append_and_publish(vec![envelope], vec![message])
                    .await?;
            }
            return Ok(outcome);
        }
        self.record_card_attempt(actor, bill_id, &parked, PaymentOutcome::Declined)
            .await?;
        Ok(CardOutcome::Declined { payment_id })
    }

    /// Records a capture as a payment. Where the bill refuses it now (it changed while the terminal
    /// was asked), the money has still moved, so the bill parks on the attempt for a person.
    /// The manager who approves a `CONFIRM` where the store asks for one (ADR-0243), `None` where
    /// it does not or the act is not a `CONFIRM`.
    fn confirm_approver(
        &self,
        actor: Actor,
        resolution: CardResolution,
        approval: Option<&crate::app::Approval>,
    ) -> Result<Option<pos_proto::ids::EmployeeId>, AppError> {
        let manager = self.session().billing.card_confirm_approval()
            == pos_proto::billing::CardConfirmApproval::Manager;
        if resolution != CardResolution::Confirm || !manager {
            return Ok(None);
        }
        let mut ctx = self.decision_ctx(actor)?;
        let approver = self.resolve_step_up(&mut ctx, Permission::VoidBill, approval)?;
        ctx.require(Permission::VoidBill)?;
        Ok(approver)
    }

    async fn record_card_capture(
        &self,
        actor: Actor,
        scope: TillScope,
        bill_id: BillId,
        (card, mark): (&ParkedCard, &CardMark),
        split_ways: Option<u8>,
    ) -> Result<CardOutcome, AppError> {
        let payment = Payment {
            method: PaymentMethod::Card,
            tendered: card
                .amount
                .checked_add(card.tip)
                .map_err(DomainError::from)?,
            applied_to_bill: card.amount,
            tip: card.tip,
        };
        let parked_before = self.lock_projection().parked_card(bill_id).is_some();
        match self
            .take_tender(
                actor,
                scope,
                bill_id,
                (payment, Some(mark)),
                split_ways,
                None,
            )
            .await
        {
            Ok(taken) => {
                self.lock_projection().unpark_card(bill_id, card.payment_id);
                Ok(CardOutcome::Captured {
                    payment_id: card.payment_id,
                    taken: Box::new(taken),
                })
            }
            Err(error) if parked_before => Err(error),
            Err(error) => {
                tracing::warn!(%error, "a card the terminal took could not be recorded; the bill parks");
                self.record_card_attempt(actor, bill_id, card, PaymentOutcome::Unknown)
                    .await?;
                Ok(CardOutcome::Unknown {
                    payment_id: card.payment_id,
                })
            }
        }
    }

    /// Records an attempt that moved no money on the bill: `Unknown` parks it, `Declined` ends
    /// its park.
    async fn record_card_attempt(
        &self,
        actor: Actor,
        bill_id: BillId,
        card: &ParkedCard,
        outcome: PaymentOutcome,
    ) -> Result<(), AppError> {
        let ctx = self.decision_ctx(actor)?;
        let event = BillingPaymentCaptured {
            bill_id,
            payment_id: card.payment_id,
            method: Open::from_known(PaymentMethod::Card),
            outcome: Open::from_known(outcome),
            tendered: card
                .amount
                .checked_add(card.tip)
                .map_err(DomainError::from)?,
            applied_to_bill: card.amount,
            change_given: Money::zero(card.amount.currency_code),
            tip_amount: card.tip,
            split_ways: None,
            terminal_reference: Some(TerminalReference::new(card.reference.as_str())),
        };
        let (envelope, message) = self.prepare_on(&ctx, &event, None)?;
        self.append_and_publish(vec![envelope], vec![message])
            .await?;
        let mut projection = self.lock_projection();
        if outcome == PaymentOutcome::Unknown {
            projection.park_card(bill_id, card.clone());
        } else {
            projection.unpark_card(bill_id, card.payment_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
