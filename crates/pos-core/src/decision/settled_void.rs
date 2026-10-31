// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voiding a bill that has already been paid, whole
//! ([ADR-0184](../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)).
//!
//! The whole bill, never part of it: there is no amount to decide, so the decision is only whether
//! it may happen. Four things say no, in this order: the bill machine (only a `SETTLED` bill), the
//! store's window (decision 7), an issued legal invoice (decision 8), and the permission with its
//! PIN (decision 4). The window and the invoice come first because no approval changes them, so a
//! manager is not asked to approve an act that would be refused anyway.
//!
//! Everything arrives as a value. The caller reads the bill's business date and whether the shift
//! that took its money is still open, the store's published window, and — from the country module,
//! because the core names no country's invoice — whether an invoice was issued. The core decides.

use pos_proto::BillState;
use pos_proto::billing::SettledVoidWindow;
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
/// [`DomainError::EInvoiceIssued`] where a legal invoice was issued for the bill;
/// [`DomainError::PermissionDenied`] naming `billing.bill.void_settled` if it is not granted or its
/// PIN was not verified.
pub(super) fn decide_settled_void(
    current: BillState,
    void: SettledVoid,
    ctx: &DecisionCtx,
) -> Result<BillDecision, DomainError> {
    let next_state = Bill::step(current, BillTrigger::VoidSettled)?;
    if !within_settled_void_window(
        void.window,
        void.bill_business_date,
        ctx.business_date,
        void.taking_shift_open,
    ) {
        return Err(DomainError::SettledVoidWindowClosed {
            window: void.window,
        });
    }
    if void.e_invoice_issued {
        return Err(DomainError::EInvoiceIssued);
    }
    require_with_pin(ctx, Permission::VoidSettledBill, void.pin_verified)?;
    Ok(BillDecision {
        next_state,
        settlement: None,
        effects: vec![Effect::PrintVoidBillSlip],
    })
}
