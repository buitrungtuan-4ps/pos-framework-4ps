// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voiding a paid bill at the edge (ADR-0184): who may, when, what leaves the drawer, and that a
//! restart gives the same answer.

use std::num::NonZeroU32;
use std::sync::Arc;

use argon2::Argon2;
use argon2::password_hash::PasswordHasher as _;
use pos_core::billing::Payment;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::billing::{PublishedBilling, SettledVoidWindow};
use pos_proto::ids::{BillId, EmployeeId, ReasonCodeId, ShiftId, TableId};
use pos_proto::reason_codes::{PublishedReasonCodes, ReasonAction};
use pos_proto::time::BusinessDate;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use pos_proto::{BillState, PaymentMethod};

use crate::app::tests::{a_line, actor, cash, identity, vnd};
use crate::app::{AppError, Approval, Edge, EdgeSession, StaffAuth, StaffRoster, TillScope};
use crate::receipt::InMemoryReceipts;

/// A store whose manager, `MGR-1`, holds `billing.bill.void_settled` directly and whose
/// supervisor, `SUP-1`, holds only the void of an unpaid bill.
fn session() -> EdgeSession {
    let pin_phc = Argon2::default()
        .hash_password_with_salt(b"4417", b"a-fixed-test-slt")
        .expect("hash")
        .to_string();
    let mut staff = StaffRoster::new();
    for (code, id, permission) in [
        ("MGR-1", 11, Permission::VoidSettledBill),
        ("SUP-1", 12, Permission::VoidBill),
    ] {
        staff.insert(
            code,
            StaffAuth {
                employee_id: Some(EmployeeId::new(Ulid::from_u128(id))),
                permissions: PermissionSet::EMPTY.with(permission),
                permissions_with_approval: PermissionSet::EMPTY,
                discount_ceiling: None,
                pin_phc: Some(pin_phc.clone()),
            },
        );
    }
    let mut session = EdgeSession::bootstrap();
    session.staff = staff;
    session
}

fn over(store: FakeStore, session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        store,
        identity(),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
}

fn approval(code: &str) -> Approval {
    Approval {
        code: code.to_owned(),
        pin: "4417".to_owned(),
    }
}

fn reason() -> ReasonCodeId {
    PublishedReasonCodes::framework_default()
        .for_action(ReasonAction::VoidBill)
        .next()
        .expect("the framework set covers voiding a bill")
        .id
}

fn card(minor: i64) -> Payment {
    Payment {
        method: PaymentMethod::Card,
        ..cash(minor)
    }
}

/// Seats `table`, sells one 150k line (165k with tax) and opens its bill.
async fn an_open_bill(edge: &Edge<FakeStore>, table: u128) -> BillId {
    let table = TableId::new(Ulid::from_u128(table));
    edge.seat_table(actor(), table, None).await.expect("seats");
    edge.add_line(actor(), table, a_line()).await.expect("adds");
    edge.open_bill(actor(), table)
        .await
        .expect("opens a bill")
        .bill_id
}

/// [`an_open_bill`], settled with `payments`.
async fn a_paid_bill(edge: &Edge<FakeStore>, table: u128, payments: Vec<Payment>) -> BillId {
    let bill_id = an_open_bill(edge, table).await;
    edge.settle_bill(actor(), bill_id, payments, None)
        .await
        .expect("settles");
    bill_id
}

async fn void(
    edge: &Edge<FakeStore>,
    bill_id: BillId,
    approver: Option<&str>,
) -> Result<crate::app::SettledVoidView, AppError> {
    let approval = approver.map(approval);
    edge.void_settled_bill(
        actor(),
        TillScope::default(),
        bill_id,
        reason(),
        approval.as_ref(),
    )
    .await
}

fn expected(edge: &Edge<FakeStore>, shift_id: ShiftId) -> i64 {
    edge.lock_projection()
        .shift(shift_id)
        .expect("the shift")
        .expected()
        .expect("adds up")
        .amount_minor
}

/// The cash a paid bill took leaves the drawer the void is done at, by exactly what was applied
/// — not the tip, which is its own ledger — and the bill is voided.
#[test]
fn a_cash_void_lowers_the_drawer_by_exactly_the_cash_applied() {
    pos_fakes::executor::run_ready(async {
        let edge = over(FakeStore::default(), session());
        let shift = edge
            .open_shift(actor(), vnd(100_000))
            .await
            .expect("opens")
            .shift_id;
        let tipped = Payment {
            tendered: vnd(175_000),
            tip: vnd(10_000),
            ..cash(165_000)
        };
        let bill_id = a_paid_bill(&edge, 501, vec![tipped]).await;
        assert_eq!(expected(&edge, shift), 265_000);

        let voided = void(&edge, bill_id, Some("MGR-1")).await.expect("voids");
        assert_eq!(voided.state, BillState::Voided);
        assert_eq!(voided.cash_returned, vnd(165_000));
        assert_eq!(voided.other_reversed, vnd(0));
        assert_eq!(voided.total_due, vnd(165_000));
        assert!(voided.open_drawer, "the drawer opens to hand the cash back");
        assert!(voided.print_void_slip);
        assert_eq!(expected(&edge, shift), 100_000);
        assert_eq!(
            edge.lock_projection().bill(bill_id).map(|bill| bill.state),
            Some(BillState::Voided)
        );
    });
}

/// A card is recorded as reversed and leaves the drawer alone; a split bill gives back its cash
/// part only.
#[test]
fn a_card_void_leaves_the_drawer_alone_and_a_split_tender_gives_back_its_cash() {
    pos_fakes::executor::run_ready(async {
        let edge = over(FakeStore::default(), session());
        let shift = edge
            .open_shift(actor(), vnd(100_000))
            .await
            .expect("opens")
            .shift_id;
        let by_card = a_paid_bill(&edge, 502, vec![card(165_000)]).await;
        let voided = void(&edge, by_card, Some("MGR-1")).await.expect("voids");
        assert_eq!(voided.cash_returned, vnd(0));
        assert_eq!(voided.other_reversed, vnd(165_000));
        assert!(!voided.open_drawer);
        assert_eq!(expected(&edge, shift), 100_000);

        let split = a_paid_bill(&edge, 503, vec![cash(65_000), card(100_000)]).await;
        assert_eq!(expected(&edge, shift), 165_000);
        let voided = void(&edge, split, Some("MGR-1")).await.expect("voids");
        assert_eq!(voided.cash_returned, vnd(65_000));
        assert_eq!(voided.other_reversed, vnd(100_000));
        assert_eq!(expected(&edge, shift), 100_000);
    });
}

/// A restart replays the void onto the same drawer: the bill comes back voided and the drawer
/// expects what it expected before the restart.
#[test]
fn a_restart_rebuilds_the_void_and_the_drawer() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let (shift, bill_id) = {
            let edge = over(store.clone(), session());
            let shift = edge
                .open_shift(actor(), vnd(100_000))
                .await
                .expect("opens")
                .shift_id;
            let bill_id = a_paid_bill(&edge, 504, vec![cash(165_000)]).await;
            void(&edge, bill_id, Some("MGR-1")).await.expect("voids");
            (shift, bill_id)
        };
        let types: Vec<(String, Option<ShiftId>)> = store
            .read(&EventQuery::first(
                identity().store_id,
                NonZeroU32::new(100).expect("not zero"),
            ))
            .await
            .expect("reads the log")
            .iter()
            .map(|envelope| (envelope.event_type.as_str().to_owned(), envelope.shift_id))
            .collect();
        let tail: Vec<&(String, Option<ShiftId>)> = types.iter().rev().take(2).rev().collect();
        assert_eq!(
            tail,
            [
                &("billing.bill.voided".to_owned(), Some(shift)),
                &("security.permission.overridden".to_owned(), Some(shift)),
            ],
            "the void and its approval, stamped with the drawer's shift"
        );

        let edge = over(store, session());
        edge.rebuild().await.expect("rebuilds");
        assert_eq!(expected(&edge, shift), 100_000);
        assert_eq!(
            edge.lock_projection().bill(bill_id).map(|bill| bill.state),
            Some(BillState::Voided)
        );
        let again = void(&edge, bill_id, Some("MGR-1")).await;
        assert!(
            matches!(again, Err(AppError::Domain(DomainError::Transition(_)))),
            "a voided bill is not voided twice, got {again:?}"
        );
    });
}

/// It takes its own permission, approved by a holder, and a reason the store holds for voiding.
#[test]
fn it_needs_a_holder_of_its_own_permission_and_a_void_reason() {
    pos_fakes::executor::run_ready(async {
        let edge = over(FakeStore::default(), session());
        let bill_id = a_paid_bill(&edge, 505, vec![cash(165_000)]).await;

        let nobody = void(&edge, bill_id, None).await;
        assert!(
            matches!(nobody, Err(AppError::ApprovalRequired)),
            "got {nobody:?}"
        );
        let unpaid_only = void(&edge, bill_id, Some("SUP-1")).await;
        assert!(
            matches!(unpaid_only, Err(AppError::ApprovalRefused)),
            "voiding an unpaid bill does not approve voiding a paid one, got {unpaid_only:?}"
        );
        let wrong_reason = edge
            .void_settled_bill(
                actor(),
                TillScope::default(),
                bill_id,
                ReasonCodeId::new(Ulid::from_u128(1)),
                Some(&approval("MGR-1")),
            )
            .await;
        assert!(
            matches!(wrong_reason, Err(AppError::VoidReasonNotValid)),
            "got {wrong_reason:?}"
        );
        assert_eq!(
            edge.lock_projection().bill(bill_id).map(|bill| bill.state),
            Some(BillState::Settled)
        );

        let open = an_open_bill(&edge, 506).await;
        let unpaid = void(&edge, open, Some("MGR-1")).await;
        assert!(
            matches!(unpaid, Err(AppError::Domain(DomainError::Transition(_)))),
            "an unpaid bill is voided by the other route, got {unpaid:?}"
        );
    });
}

/// By business date (the default), a bill from an earlier day is refused, before anybody is asked
/// to approve it.
#[test]
fn a_bill_from_an_earlier_business_date_is_refused_before_any_approval() {
    pos_fakes::executor::run_ready(async {
        let edge = over(FakeStore::default(), session());
        let bill_id = a_paid_bill(&edge, 507, vec![cash(165_000)]).await;
        {
            let mut projection = edge.lock_projection();
            let receipt = projection.settled.get_mut(&bill_id).expect("recorded");
            receipt.business_date = BusinessDate::from_ymd(2020, 1, 1).expect("a real date");
        }
        let refused = void(&edge, bill_id, None).await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::SettledVoidWindowClosed {
                    window: SettledVoidWindow::BusinessDate
                }))
            ),
            "got {refused:?}"
        );
    });
}

/// By shift, a bill no open shift took is refused, and one the open shift took is voided.
#[test]
fn by_shift_only_a_bill_the_open_shift_took_is_voided() {
    pos_fakes::executor::run_ready(async {
        let mut by_shift = session();
        by_shift.billing = PublishedBilling {
            settled_void_window: Open::from_known(SettledVoidWindow::Shift),
        };
        let edge = over(FakeStore::default(), by_shift);
        let before_the_shift = a_paid_bill(&edge, 508, vec![card(165_000)]).await;
        edge.open_shift(actor(), vnd(0)).await.expect("opens");
        let refused = void(&edge, before_the_shift, Some("MGR-1")).await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::SettledVoidWindowClosed {
                    window: SettledVoidWindow::Shift
                }))
            ),
            "got {refused:?}"
        );
        let in_the_shift = a_paid_bill(&edge, 509, vec![card(165_000)]).await;
        void(&edge, in_the_shift, Some("MGR-1"))
            .await
            .expect("the shift that took it is open");
    });
}

/// The voided bill is still among today's, marked voided, and its copy is marked VOID under the
/// number it kept (ADR-0184 decision 5). The answer carries what the void slip prints.
#[test]
fn a_voided_bill_stays_listed_and_its_copy_is_marked_void() {
    pos_fakes::executor::run_ready(async {
        let edge = over(FakeStore::default(), session());
        let bill_id = a_paid_bill(&edge, 510, vec![cash(165_000)]).await;
        let voided = void(&edge, bill_id, Some("MGR-1")).await.expect("voids");
        assert!(voided.reason.is_some(), "the slip names the reason");
        assert_eq!(
            voided.till_name, None,
            "a device that is no till names none"
        );

        let listed = edge.settled_bills().expect("lists");
        let row = listed
            .iter()
            .find(|bill| bill.bill_id == bill_id)
            .expect("still listed");
        assert!(row.voided);
        assert_eq!(row.receipt_number, voided.receipt_number);

        let copy = edge
            .reprint_receipt(actor(), bill_id)
            .await
            .expect("a copy of a voided bill");
        assert!(copy.voided);
        assert_eq!(copy.receipt_number, voided.receipt_number);
    });
}
