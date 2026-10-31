// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The decision tests for voiding a paid bill, whole (ADR-0184): the machine, the store's window,
//! an issued invoice, and the permission with its PIN.

use super::*;
use crate::decision::{BillDecision, SettledVoid, within_settled_void_window};
use crate::permission::{Role, default_grants};
use pos_proto::billing::SettledVoidWindow;

fn today() -> BusinessDate {
    BusinessDate::from_ymd(2026, 8, 19).expect("valid date")
}

fn yesterday() -> BusinessDate {
    BusinessDate::from_ymd(2026, 8, 18).expect("valid date")
}

/// A void of a bill settled today, by the shift still open, with no invoice, its PIN verified.
fn today_s_void() -> SettledVoid {
    SettledVoid {
        pin_verified: true,
        window: SettledVoidWindow::BusinessDate,
        bill_business_date: today(),
        taking_shift_open: true,
        e_invoice_issued: false,
    }
}

fn void_settled(
    state: BillState,
    void: SettledVoid,
    ctx: &DecisionCtx,
) -> Result<BillDecision, DomainError> {
    decide_bill(state, BillCommand::VoidSettled(void), ctx)
}

fn denied() -> DomainError {
    DomainError::PermissionDenied {
        permission: "billing.bill.void_settled",
    }
}

#[test]
fn a_paid_bill_is_voided_whole_and_prints_a_void_slip() {
    let ctx = ctx_granting(&[Permission::VoidSettledBill]);
    let decided = void_settled(BillState::Settled, today_s_void(), &ctx).expect("voided");
    assert_eq!(decided.next_state, BillState::Voided);
    assert_eq!(decided.settlement, None, "nothing is settled again");
    assert_eq!(decided.effects, vec![Effect::PrintVoidBillSlip]);
}

#[test]
fn only_a_paid_bill_is_voided_after_payment_and_a_plain_void_never_voids_one() {
    let ctx = ctx_granting(&[Permission::VoidSettledBill, Permission::VoidBill]);
    for state in [
        BillState::Open,
        BillState::Voided,
        BillState::Split,
        BillState::Merged,
    ] {
        assert!(
            matches!(
                void_settled(state, today_s_void(), &ctx),
                Err(DomainError::Transition(_))
            ),
            "{state:?}"
        );
    }
    assert!(matches!(
        decide_bill(
            BillState::Settled,
            BillCommand::Void { pin_verified: true },
            &ctx
        ),
        Err(DomainError::Transition(_))
    ));
}

#[test]
fn it_needs_its_own_permission_and_a_pin() {
    // Voiding an unpaid bill is another act, and does not let anyone void a paid one.
    let unpaid_only = ctx_granting(&[Permission::VoidBill]);
    assert_eq!(
        void_settled(BillState::Settled, today_s_void(), &unpaid_only),
        Err(denied())
    );

    let granted = ctx_granting(&[Permission::VoidSettledBill]);
    let no_pin = SettledVoid {
        pin_verified: false,
        ..today_s_void()
    };
    assert_eq!(
        void_settled(BillState::Settled, no_pin, &granted),
        Err(denied()),
        "PIN-flagged, so an unverified PIN is refused"
    );

    // Held directly (ADR-0158 decision 4), nobody's PIN is asked for on the spot.
    let mut direct = granted;
    direct.granted_directly = PermissionSet::EMPTY.with(Permission::VoidSettledBill);
    assert!(void_settled(BillState::Settled, no_pin, &direct).is_ok());

    let meta = Permission::VoidSettledBill.meta();
    assert_eq!(meta.id, "billing.bill.void_settled");
    assert!(meta.pin_required);
    assert_eq!(
        meta.default_roles,
        Permission::VoidBill.meta().default_roles
    );
    assert!(default_grants(Role::Manager).contains(Permission::VoidSettledBill));
    assert!(default_grants(Role::Owner).contains(Permission::VoidSettledBill));
    assert!(!default_grants(Role::Supervisor).contains(Permission::VoidSettledBill));
    assert!(!default_grants(Role::Cashier).contains(Permission::VoidSettledBill));
}

#[test]
fn by_business_date_a_bill_from_an_earlier_day_is_refused_whatever_the_shift() {
    let ctx = ctx_granting(&[Permission::VoidSettledBill]);
    let earlier = SettledVoid {
        bill_business_date: yesterday(),
        ..today_s_void()
    };
    assert_eq!(
        void_settled(BillState::Settled, earlier, &ctx),
        Err(DomainError::SettledVoidWindowClosed {
            window: SettledVoidWindow::BusinessDate
        })
    );
    let shift_closed = SettledVoid {
        taking_shift_open: false,
        ..today_s_void()
    };
    assert!(
        void_settled(BillState::Settled, shift_closed, &ctx).is_ok(),
        "today's bill is voidable after its shift closed"
    );
    // Unset reads as the default, the business date.
    assert!(!within_settled_void_window(
        SettledVoidWindow::Unspecified,
        yesterday(),
        today(),
        true
    ));
    assert!(within_settled_void_window(
        SettledVoidWindow::Unspecified,
        today(),
        today(),
        false
    ));
}

#[test]
fn by_shift_a_bill_is_refused_once_the_shift_that_took_its_money_has_closed() {
    let ctx = ctx_granting(&[Permission::VoidSettledBill]);
    let closed = SettledVoid {
        window: SettledVoidWindow::Shift,
        taking_shift_open: false,
        ..today_s_void()
    };
    assert_eq!(
        void_settled(BillState::Settled, closed, &ctx),
        Err(DomainError::SettledVoidWindowClosed {
            window: SettledVoidWindow::Shift
        }),
        "the same business date is not enough"
    );
    let open = SettledVoid {
        window: SettledVoidWindow::Shift,
        ..today_s_void()
    };
    assert!(void_settled(BillState::Settled, open, &ctx).is_ok());
}

#[test]
fn a_bill_with_an_issued_invoice_is_refused() {
    let ctx = ctx_granting(&[Permission::VoidSettledBill]);
    let invoiced = SettledVoid {
        e_invoice_issued: true,
        ..today_s_void()
    };
    assert_eq!(
        void_settled(BillState::Settled, invoiced, &ctx),
        Err(DomainError::EInvoiceIssued)
    );
}

#[test]
fn what_no_approval_changes_is_refused_before_the_permission_is_asked() {
    // Nobody is granted anything, so a refusal other than the permission's was decided first.
    let nobody = ctx_granting(&[]);
    let earlier_and_invoiced = SettledVoid {
        bill_business_date: yesterday(),
        e_invoice_issued: true,
        pin_verified: false,
        ..today_s_void()
    };
    assert!(matches!(
        void_settled(BillState::Settled, earlier_and_invoiced, &nobody),
        Err(DomainError::SettledVoidWindowClosed { .. })
    ));
    let invoiced = SettledVoid {
        e_invoice_issued: true,
        pin_verified: false,
        ..today_s_void()
    };
    assert_eq!(
        void_settled(BillState::Settled, invoiced, &nobody),
        Err(DomainError::EInvoiceIssued)
    );
}
