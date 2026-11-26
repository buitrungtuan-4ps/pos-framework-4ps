// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The decision tests for voiding a paid bill, whole (ADR-0184): the machine, the store's window,
//! an issued invoice, and the permission with its PIN.

use super::*;
use crate::decision::settled_void::{decide_correction_request, invoiced_needs_request};
use crate::decision::{BillDecision, InvoicedVoid, SettledVoid, within_settled_void_window};
use crate::permission::{Role, default_grants};
use pos_proto::billing::SettledVoidWindow;
use pos_proto::einvoice::InvoicedCorrection;

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
        invoiced: InvoicedVoid::default(),
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

/// An invoiced bill of 165,000 whose invoice is 10 minutes old, under `route`.
fn invoiced_under(route: InvoicedCorrection, limit: i64, window: i64) -> SettledVoid {
    SettledVoid {
        e_invoice_issued: true,
        invoiced: InvoicedVoid {
            route,
            bill_total_minor: 165_000,
            manager_limit_minor: limit,
            invoice_age_minutes: 10,
            cancel_window_minutes: window,
            approved_by_hq: false,
        },
        ..today_s_void()
    }
}

#[test]
fn an_invoiced_bill_is_voided_at_the_till_only_where_the_stores_route_allows() {
    let ctx = ctx_granting(&[Permission::VoidSettledBill]);
    for (void, allowed, why) in [
        (
            invoiced_under(InvoicedCorrection::HqOnly, 1_000_000, 60),
            false,
            "HQ only, the default",
        ),
        (
            invoiced_under(InvoicedCorrection::Unspecified, 1_000_000, 60),
            false,
            "unspecified is HQ only",
        ),
        (
            invoiced_under(InvoicedCorrection::Request, 1_000_000, 60),
            false,
            "a request is not a void",
        ),
        (
            invoiced_under(InvoicedCorrection::ManagerUpToLimit, 165_000, 0),
            true,
            "at the limit",
        ),
        (
            invoiced_under(InvoicedCorrection::ManagerUpToLimit, 164_999, 0),
            false,
            "above the limit",
        ),
        (
            invoiced_under(InvoicedCorrection::ManagerUpToLimit, 0, 0),
            false,
            "a limit of 0 allows none",
        ),
        (
            invoiced_under(InvoicedCorrection::CancelWindow, 0, 11),
            true,
            "inside the window",
        ),
        (
            invoiced_under(InvoicedCorrection::CancelWindow, 0, 10),
            false,
            "the window has closed",
        ),
        (
            invoiced_under(InvoicedCorrection::CancelWindow, 0, 0),
            false,
            "a window of 0 allows none",
        ),
    ] {
        let decided = void_settled(BillState::Settled, void, &ctx);
        if allowed {
            assert!(decided.is_ok(), "{why}");
        } else {
            assert_eq!(decided.err(), Some(DomainError::EInvoiceIssued), "{why}");
        }
    }
}

#[test]
fn a_route_that_allows_the_till_still_needs_the_permission_and_its_pin() {
    let nobody = ctx_granting(&[]);
    let within = SettledVoid {
        pin_verified: false,
        ..invoiced_under(InvoicedCorrection::ManagerUpToLimit, 200_000, 0)
    };
    assert!(matches!(
        void_settled(BillState::Settled, within, &nobody),
        Err(DomainError::PermissionDenied { .. })
    ));
}

/// [`invoiced_under`], with HQ's approval as the store's node publishes it (ADR-0250).
fn approved_under(route: InvoicedCorrection, limit: i64, window: i64) -> SettledVoid {
    let mut void = invoiced_under(route, limit, window);
    void.invoiced.approved_by_hq = true;
    void
}

#[test]
fn hqs_approval_lets_the_till_void_under_every_route_that_asks_hq() {
    let manager = ctx_granting(&[Permission::VoidSettledBill]);
    for route in [
        InvoicedCorrection::Request,
        InvoicedCorrection::ManagerUpToLimit,
        InvoicedCorrection::CancelWindow,
    ] {
        assert!(
            void_settled(
                BillState::Settled,
                approved_under(route, 100_000, 5),
                &manager
            )
            .is_ok(),
            "{route:?}"
        );
        assert_eq!(
            void_settled(
                BillState::Settled,
                invoiced_under(route, 100_000, 5),
                &manager
            )
            .err(),
            Some(DomainError::EInvoiceIssued),
            "{route:?} without an approval"
        );
    }
    assert_eq!(
        void_settled(
            BillState::Settled,
            approved_under(InvoicedCorrection::HqOnly, 0, 0),
            &manager
        )
        .err(),
        Some(DomainError::EInvoiceIssued),
        "under HQ_ONLY, HQ corrects in the console"
    );
}

#[test]
fn an_approval_arriving_after_the_window_still_lets_the_till_void() {
    let manager = ctx_granting(&[Permission::VoidSettledBill]);
    let late = SettledVoid {
        bill_business_date: yesterday(),
        ..approved_under(InvoicedCorrection::Request, 0, 0)
    };
    assert!(void_settled(BillState::Settled, late, &manager).is_ok());
    let unapproved_late = SettledVoid {
        bill_business_date: yesterday(),
        ..invoiced_under(InvoicedCorrection::ManagerUpToLimit, 200_000, 0)
    };
    assert!(matches!(
        void_settled(BillState::Settled, unapproved_late, &manager),
        Err(DomainError::SettledVoidWindowClosed { .. })
    ));
}

#[test]
fn a_request_is_taken_only_where_the_route_asks_hq() {
    let manager = ctx_granting(&[Permission::VoidSettledBill]);
    let ask = |void: SettledVoid| {
        decide_correction_request(BillState::Settled, Some(void.invoiced), true, &manager)
    };
    assert_eq!(
        ask(invoiced_under(InvoicedCorrection::Request, 0, 0)),
        Ok(())
    );
    assert_eq!(
        ask(invoiced_under(
            InvoicedCorrection::ManagerUpToLimit,
            100_000,
            0
        )),
        Ok(()),
        "above the manager's limit"
    );
    assert_eq!(
        ask(invoiced_under(InvoicedCorrection::CancelWindow, 0, 30)),
        Err(DomainError::CorrectionNotNeeded),
        "inside the window the till voids it"
    );
    assert_eq!(
        ask(approved_under(InvoicedCorrection::Request, 0, 0)),
        Err(DomainError::CorrectionNotNeeded),
        "already approved"
    );
    assert_eq!(
        ask(invoiced_under(InvoicedCorrection::HqOnly, 0, 0)),
        Err(DomainError::EInvoiceIssued)
    );
    assert_eq!(
        decide_correction_request(BillState::Settled, None, true, &manager),
        Err(DomainError::CorrectionNotNeeded),
        "no invoice was issued"
    );
    assert_eq!(
        decide_correction_request(
            BillState::Settled,
            Some(invoiced_under(InvoicedCorrection::Request, 0, 0).invoiced),
            false,
            &manager
        ),
        Err(denied()),
        "a manager approves it with a PIN"
    );
    let window = |minutes| invoiced_under(InvoicedCorrection::CancelWindow, 0, minutes).invoiced;
    assert!(!invoiced_needs_request(window(30)), "inside the window");
    assert!(invoiced_needs_request(window(5)), "after the window");
}
