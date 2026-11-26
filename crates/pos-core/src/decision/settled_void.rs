// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voiding a bill that has already been paid, whole
//! ([ADR-0184](../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)).
//!
//! The whole bill, never part of it: there is no amount to decide, so the decision is only whether
//! it may happen. Four things say no, in this order: the bill machine (only a `SETTLED` bill), the
//! store's window (decision 7), an issued legal invoice the store's route keeps from the till (decision
//! 8, ADR-0245), and the permission with its
//! PIN (decision 4). The window and the invoice come first because no approval changes them, so a
//! manager is not asked to approve an act that would be refused anyway.
//!
//! Everything arrives as a value. The caller reads the bill's business date and whether the shift
//! that took its money is still open, the store's published window, and — from the country module,
//! because the core names no country's invoice — whether an invoice was issued. The core decides.

use pos_proto::BillState;
use pos_proto::billing::SettledVoidWindow;
use pos_proto::einvoice::InvoicedCorrection;
use pos_proto::time::BusinessDate;

use super::{BillDecision, DecisionCtx, Effect, require_with_pin};
use crate::error::DomainError;
use crate::machines::{Bill, BillTrigger};
use crate::permission::Permission;
use crate::state_machine::StateMachine;

/// What the caller knows about a paid bill that its void is decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettledVoid {
    /// Whether the edge has collected and verified a PIN for this act.
    pub pin_verified: bool,
    /// The store's `billing.settled_void_window`.
    pub window: SettledVoidWindow,
    /// The business date the bill belongs to.
    pub bill_business_date: BusinessDate,
    /// Whether the shift that took the bill's money is still open; `false` where no shift took it.
    pub taking_shift_open: bool,
    /// Whether a legal invoice was issued for the bill, as the store's country module says.
    pub e_invoice_issued: bool,
    /// The store's route for a bill with an issued invoice, and what it is decided on.
    pub invoiced: InvoicedVoid,
}

/// What the till decides an invoiced bill's void on, from the store's `einvoice` node
/// ([ADR-0245](../../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InvoicedVoid {
    /// `einvoice.invoiced_correction`.
    pub route: InvoicedCorrection,
    /// The bill's total, in the minor unit.
    pub bill_total_minor: i64,
    /// `einvoice.manager_correction_limit`, in the minor unit.
    pub manager_limit_minor: i64,
    /// Whole minutes since the invoice was issued.
    pub invoice_age_minutes: i64,
    /// `einvoice.cancel_window_minutes`.
    pub cancel_window_minutes: i64,
    /// Whether HQ approved this bill's correction request, as the cloud published it on the store's
    /// `einvoice` node ([ADR-0250](../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)).
    pub approved_by_hq: bool,
}

/// Whether the till may void a bill whose invoice was issued (ADR-0245 decision 1): under
/// `MANAGER_UP_TO_LIMIT` when its total is within the limit, under `CANCEL_WINDOW` while the invoice
/// is younger than the window, under any route but `HQ_ONLY` once HQ approved its request
/// (ADR-0250 decision 4), and never otherwise. The cloud then corrects the invoice.
#[must_use]
pub(super) fn invoiced_void_at_till(invoiced: InvoicedVoid) -> bool {
    if invoiced.approved_by_hq && invoiced_needs_request(invoiced) {
        return true;
    }
    match invoiced.route {
        InvoicedCorrection::ManagerUpToLimit => {
            (0..=invoiced.manager_limit_minor).contains(&invoiced.bill_total_minor)
                && invoiced.manager_limit_minor > 0
        }
        InvoicedCorrection::CancelWindow => {
            (0..invoiced.cancel_window_minutes).contains(&invoiced.invoice_age_minutes)
        }
        InvoicedCorrection::Unspecified
        | InvoicedCorrection::HqOnly
        | InvoicedCorrection::Request => false,
    }
}

/// Whether the till must ask HQ before voiding a bill whose invoice was issued (ADR-0250 decision 5):
/// always under `REQUEST`, above the manager's limit under `MANAGER_UP_TO_LIMIT`, and after the
/// window under `CANCEL_WINDOW`. Never under `HQ_ONLY`, where HQ corrects in the console. Public so
/// the till can offer the request form instead of a refusal.
#[must_use]
pub fn invoiced_needs_request(invoiced: InvoicedVoid) -> bool {
    match invoiced.route {
        InvoicedCorrection::Request => true,
        InvoicedCorrection::ManagerUpToLimit | InvoicedCorrection::CancelWindow => {
            !invoiced_void_at_till(InvoicedVoid {
                approved_by_hq: false,
                ..invoiced
            })
        }
        InvoicedCorrection::Unspecified | InvoicedCorrection::HqOnly => false,
    }
}

/// Whether a paid bill is still inside the window in which the till may void it
/// (ADR-0184 decision 7).
///
/// [`SettledVoidWindow::BusinessDate`] — and `UNSPECIFIED`, which is the default — while `today` is
/// still the bill's own business date; [`SettledVoidWindow::Shift`] only while the shift that took
/// its money is open. Public so the till can be told before anyone is asked for a PIN.
#[must_use]
pub fn within_settled_void_window(
    window: SettledVoidWindow,
    bill_business_date: BusinessDate,
    today: BusinessDate,
    taking_shift_open: bool,
) -> bool {
    match window {
        SettledVoidWindow::Shift => taking_shift_open,
        SettledVoidWindow::Unspecified | SettledVoidWindow::BusinessDate => {
            bill_business_date == today
        }
    }
}

/// Decides the void of a paid bill: the machine, the window, the invoice, then the permission.
///
/// # Errors
///
/// [`DomainError::Transition`] unless the bill is `SETTLED`;
/// [`DomainError::SettledVoidWindowClosed`] outside the store's window;
/// [`DomainError::EInvoiceIssued`] where a legal invoice was issued for the bill and the store's route
/// does not let the till void it;
/// [`DomainError::PermissionDenied`] naming `billing.bill.void_settled` if it is not granted or its
/// PIN was not verified.
pub(super) fn decide_settled_void(
    current: BillState,
    void: SettledVoid,
    ctx: &DecisionCtx,
) -> Result<BillDecision, DomainError> {
    let next_state = Bill::step(current, BillTrigger::VoidSettled)?;
    // HQ approved this void (ADR-0250), and a correction past the window is HQ's to allow.
    let hq_approved = void.e_invoice_issued
        && void.invoiced.approved_by_hq
        && invoiced_needs_request(void.invoiced);
    if !hq_approved
        && !within_settled_void_window(
            void.window,
            void.bill_business_date,
            ctx.business_date,
            void.taking_shift_open,
        )
    {
        return Err(DomainError::SettledVoidWindowClosed {
            window: void.window,
        });
    }
    if void.e_invoice_issued && !invoiced_void_at_till(void.invoiced) {
        return Err(DomainError::EInvoiceIssued);
    }
    require_with_pin(ctx, Permission::VoidSettledBill, void.pin_verified)?;
    Ok(BillDecision {
        next_state,
        settlement: None,
        effects: vec![Effect::PrintVoidBillSlip],
    })
}

/// Decides a manager's request that HQ correct a paid bill's issued invoice
/// ([ADR-0250](../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)
/// decision 3). `invoiced` is `None` where no invoice was issued for the bill.
///
/// # Errors
///
/// [`DomainError::Transition`] unless the bill is `SETTLED`; [`DomainError::CorrectionNotNeeded`]
/// where no invoice was issued, HQ already approved, or the till may void the bill itself;
/// [`DomainError::EInvoiceIssued`] under `HQ_ONLY`, where HQ corrects in the console;
/// [`DomainError::PermissionDenied`] naming `billing.bill.void_settled` if it is not granted or its
/// PIN was not verified.
pub fn decide_correction_request(
    current: BillState,
    invoiced: Option<InvoicedVoid>,
    pin_verified: bool,
    ctx: &DecisionCtx,
) -> Result<(), DomainError> {
    Bill::step(current, BillTrigger::VoidSettled)?;
    let Some(invoiced) = invoiced else {
        return Err(DomainError::CorrectionNotNeeded);
    };
    if invoiced_void_at_till(invoiced) {
        return Err(DomainError::CorrectionNotNeeded);
    }
    if !invoiced_needs_request(invoiced) {
        return Err(DomainError::EInvoiceIssued);
    }
    require_with_pin(ctx, Permission::VoidSettledBill, pin_verified)
}
