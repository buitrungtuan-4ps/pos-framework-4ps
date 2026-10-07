// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cash paid into and out of the drawer outside a sale, and the drawer opened without one
//! ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)).
//!
//! `docs/pos-spec.md` §6 asked for paid-in and paid-out entries with reasons, and for a drawer
//! opened outside a sale to need a permission and be logged. The events, the permissions and the
//! reasons all existed and nothing produced them, so cash taken out for a supplier showed up as a
//! variance the cashier did not cause. These cases hold what the till now relies on: a movement
//! moves what the close expects, it needs a reason listed for its own act and a shift still open,
//! a restart keeps it, and a no-sale opening needs a manager's PIN and is recorded as standalone.
//!
//! A close over or short by more than the store's `shift.variance_reason_minor` gives a reason its
//! list holds for that, and `cash.shift.closed` records it
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 12). Within the
//! limit, with none set, or with no such reason listed, a close is what it always was.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, CashMovement, Edge, EdgeSession, InMemoryReceipts, LineDraft, StaffAuth,
    StaffRoster, StoreIdentity, TillScope,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{CashDrawerOpened, CashDrawerPaidOut, CashShiftClosed};
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, ReasonCodeId, ShiftId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{PaymentMethod, SalesChannel, ShiftState};

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

/// The manager's badge and PIN, as the till would collect them. Obviously fake.
const MANAGER_CODE: &str = "MGR-1";
const MANAGER_PIN: &str = "4417";
/// A supervisor on the roster whose role does not hold the no-sale opening.
const SUPERVISOR_CODE: &str = "SUP-1";
const SUPERVISOR_PIN: &str = "2280";

fn cashier() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn pizza() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

/// Valid for all three of the drawer's acts, as the framework's own "Making change" is.
fn making_change() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_101))
}

/// Valid for a paid out only: a supplier is paid, never paid in.
fn supplier() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_102))
}

/// Valid for a drawer's over or short alone, as the framework's own "Counting difference" is.
fn miscounted() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_103))
}

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

fn manager() -> Approval {
    Approval {
        code: MANAGER_CODE.to_owned(),
        pin: MANAGER_PIN.to_owned(),
    }
}

/// A store that sells a 150k pizza at 10 %, holds the two reasons above, and has a manager who may
/// open the drawer without a sale and a supervisor who may not.
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class,
    ));
    let rates = TaxRateTable::new().with(class, SalesChannel::DineIn, TaxRate::from_percent(10));
    let reasons = PublishedReasonCodes::from_parts(vec![
        PublishedReasonCode::new(
            making_change(),
            ReasonCode::new("MAKING_CHANGE"),
            DisplayName::new("Making change"),
            vec![
                ReasonAction::DrawerOpen,
                ReasonAction::CashPaidIn,
                ReasonAction::CashPaidOut,
            ],
        ),
        PublishedReasonCode::new(
            supplier(),
            ReasonCode::new("SUPPLIER"),
            DisplayName::new("Supplier paid"),
            vec![ReasonAction::CashPaidOut],
        ),
    ]);
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::EMPTY.with(Permission::OpenDrawerNoSale),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    staff.insert(
        SUPERVISOR_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(12))),
            permissions: PermissionSet::EMPTY.with(Permission::VoidBill),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(SUPERVISOR_PIN)),
        },
    );
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.reason_codes = reasons;
    session.staff = staff;
    session
}

fn edge_over(store: FakeStore, session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn a_pizza() -> LineDraft {
    LineDraft {
        menu_item_id: pizza(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// Sells one pizza on a table and takes the 165k in cash.
async fn a_cash_sale(edge: &Edge<FakeStore>) {
    let table = TableId::new(Ulid::from_u128(41));
    edge.seat_table(cashier(), table, None)
        .await
        .expect("seats");
    edge.add_line(cashier(), table, a_pizza())
        .await
        .expect("rings a pizza");
    let bill = edge
        .open_bill(cashier(), table)
        .await
        .expect("opens the bill")
        .bill_id;
    let due = edge.check_totals(table).expect("the check reads").total_due;
    edge.settle_bill(
        cashier(),
        bill,
        vec![Payment {
            method: PaymentMethod::Cash,
            tendered: due,
            applied_to_bill: due,
            tip: vnd(0),
        }],
        None,
    )
    .await
    .expect("settles in cash");
}

async fn open_with_float(edge: &Edge<FakeStore>, float: i64) -> ShiftId {
    edge.open_shift(cashier(), vnd(float))
        .await
        .expect("opens a shift")
        .shift_id
}

async fn logged(store: &FakeStore) -> Vec<EventEnvelope<RawPayload>> {
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(200).expect("a positive limit"),
    );
    store.read(&query).await.expect("read the log")
}

async fn types(store: &FakeStore) -> Vec<String> {
    logged(store)
        .await
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

#[test]
fn paid_in_and_paid_out_move_what_the_close_expects() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session());
        let shift = open_with_float(&edge, 500_000).await;
        a_cash_sale(&edge).await;

        let paid_in = edge
            .record_cash_movement(
                cashier(),
                shift,
                CashMovement::PaidIn,
                vnd(200_000),
                making_change(),
            )
            .await
            .expect("pays change in");
        assert_eq!(paid_in.paid_in, vnd(200_000));
        assert_eq!(
            paid_in.expected_amount, None,
            "a movement reveals nothing the close keeps blind"
        );
        let paid_out = edge
            .record_cash_movement(
                cashier(),
                shift,
                CashMovement::PaidOut,
                vnd(150_000),
                supplier(),
            )
            .await
            .expect("pays a supplier");
        assert_eq!(
            (paid_out.paid_in, paid_out.paid_out),
            (vnd(200_000), vnd(150_000))
        );

        // 500k float + 165k taken + 200k paid in - 150k paid out.
        edge.count_shift(cashier(), shift, 715_000)
            .await
            .expect("counts");
        let closed = edge.close_shift(cashier(), shift).await.expect("closes");
        assert_eq!(closed.expected_amount, Some(vnd(715_000)));
        assert_eq!(closed.variance, Some(vnd(0)), "the drawer balances");
        let report = closed.report.expect("the close carries its report");
        assert_eq!(
            (report.cash_collected, report.paid_in, report.paid_out),
            (vnd(165_000), vnd(200_000), vnd(150_000)),
            "the report prints every term of the sum"
        );

        let log = logged(&store).await;
        let out = log
            .iter()
            .find(|envelope| envelope.event_type.as_str() == "cash.drawer.paid_out")
            .expect("the paid out is in the log");
        assert_eq!(
            out.shift_id,
            Some(shift),
            "a movement is on its shift's envelope"
        );
        let event: CashDrawerPaidOut = out.data.decode().expect("decodes");
        assert_eq!(
            (event.amount, event.reason_code_id),
            (vnd(150_000), supplier())
        );
        assert!(
            log.iter()
                .any(|envelope| envelope.event_type.as_str() == "cash.drawer.paid_in")
        );
    });
}

#[test]
fn a_movement_needs_a_reason_listed_for_its_own_act() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session());
        let shift = open_with_float(&edge, 500_000).await;
        let before = types(&store).await.len();

        // A supplier is paid out, never paid in, and a reason the store never authored is none.
        for (movement, reason) in [
            (CashMovement::PaidIn, supplier()),
            (
                CashMovement::PaidOut,
                ReasonCodeId::new(Ulid::from_u128(9_999)),
            ),
        ] {
            let refused = edge
                .record_cash_movement(cashier(), shift, movement, vnd(10_000), reason)
                .await;
            assert!(
                matches!(refused, Err(AppError::CashReasonNotValid)),
                "{movement:?}: {refused:?}"
            );
        }
        assert_eq!(types(&store).await.len(), before, "nothing is written");
    });
}

#[test]
fn a_movement_needs_the_shift_that_is_trading_and_not_yet_counted() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session());
        let unknown = ShiftId::new(Ulid::from_u128(77));
        let refused = edge
            .record_cash_movement(
                cashier(),
                unknown,
                CashMovement::PaidIn,
                vnd(10_000),
                making_change(),
            )
            .await;
        assert!(
            matches!(refused, Err(AppError::UnknownShift)),
            "{refused:?}"
        );

        let shift = open_with_float(&edge, 500_000).await;
        edge.count_shift(cashier(), shift, 500_000)
            .await
            .expect("counts");
        let after_count = edge
            .record_cash_movement(
                cashier(),
                shift,
                CashMovement::PaidOut,
                vnd(10_000),
                making_change(),
            )
            .await;
        assert!(
            matches!(after_count, Err(AppError::ShiftNotOpen)),
            "the count was made against the drawer as it stood: {after_count:?}"
        );

        edge.close_shift(cashier(), shift).await.expect("closes");
        let after_close = edge
            .record_cash_movement(
                cashier(),
                shift,
                CashMovement::PaidIn,
                vnd(10_000),
                making_change(),
            )
            .await;
        assert!(
            matches!(after_close, Err(AppError::ShiftNotOpen)),
            "{after_close:?}"
        );
        assert!(
            !types(&store)
                .await
                .iter()
                .any(|kind| kind.starts_with("cash.drawer.")),
            "nothing was paid in or out"
        );
    });
}

#[test]
fn a_movement_needs_the_permission() {
    run_ready(async {
        let mut without = session();
        without.granted = Permission::ALL
            .iter()
            .copied()
            .filter(|permission| *permission != Permission::RecordCashMovement)
            .collect();
        let edge = edge_over(FakeStore::new(), without);
        let shift = open_with_float(&edge, 500_000).await;
        let refused = edge
            .record_cash_movement(
                cashier(),
                shift,
                CashMovement::PaidOut,
                vnd(10_000),
                making_change(),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied { .. }))
            ),
            "{refused:?}"
        );
    });
}

#[test]
fn a_movement_survives_a_restart() {
    run_ready(async {
        let store = FakeStore::new();
        let shift = {
            let edge = edge_over(store.clone(), session());
            let shift = open_with_float(&edge, 500_000).await;
            edge.record_cash_movement(
                cashier(),
                shift,
                CashMovement::PaidOut,
                vnd(120_000),
                supplier(),
            )
            .await
            .expect("pays a supplier");
            shift
        };

        // A box that restarts mid-shift folds the log back, and expects the drawer a running one
        // does: the movement is on its shift, not lost to the restart.
        let restarted = edge_over(store.clone(), session());
        restarted.rebuild().await.expect("replays the log");
        let current = restarted.current_shift().expect("the shift is still open");
        assert_eq!(current.shift_id, shift);
        assert_eq!(current.state, ShiftState::Open);
        assert_eq!(current.paid_out, vnd(120_000));
        restarted
            .count_shift(cashier(), shift, 380_000)
            .await
            .expect("counts");
        let closed = restarted
            .close_shift(cashier(), shift)
            .await
            .expect("closes");
        assert_eq!(closed.expected_amount, Some(vnd(380_000)));
        assert_eq!(closed.variance, Some(vnd(0)));
    });
}

#[test]
fn a_paid_out_larger_than_the_drawer_is_recorded_and_the_variance_shows_it() {
    run_ready(async {
        // Refusing it would tell the cashier roughly what the drawer holds, and the close is blind.
        let edge = edge_over(FakeStore::new(), session());
        let shift = open_with_float(&edge, 100_000).await;
        edge.record_cash_movement(
            cashier(),
            shift,
            CashMovement::PaidOut,
            vnd(300_000),
            supplier(),
        )
        .await
        .expect("a paid out is not checked against the drawer");
        edge.count_shift(cashier(), shift, 0).await.expect("counts");
        let closed = edge.close_shift(cashier(), shift).await.expect("closes");
        assert_eq!(closed.expected_amount, Some(vnd(-200_000)));
        assert_eq!(
            closed.variance,
            Some(vnd(200_000)),
            "the arithmetic holds, and the report shows it"
        );
    });
}

#[test]
fn a_drawer_opened_without_a_sale_needs_a_manager_s_pin_and_a_reason_and_is_logged() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session());

        let no_pin = edge
            .open_drawer_no_sale(cashier(), making_change(), None)
            .await;
        assert!(
            matches!(no_pin, Err(AppError::ApprovalRequired)),
            "{no_pin:?}"
        );
        let wrong_act = edge
            .open_drawer_no_sale(cashier(), supplier(), Some(&manager()))
            .await;
        assert!(
            matches!(wrong_act, Err(AppError::CashReasonNotValid)),
            "a supplier is a paid out, not a reason to open the drawer: {wrong_act:?}"
        );
        let not_theirs = edge
            .open_drawer_no_sale(
                cashier(),
                making_change(),
                Some(&Approval {
                    code: SUPERVISOR_CODE.to_owned(),
                    pin: SUPERVISOR_PIN.to_owned(),
                }),
            )
            .await;
        assert!(
            matches!(not_theirs, Err(AppError::ApprovalRefused)),
            "a PIN whose role does not hold the act does not authorise it: {not_theirs:?}"
        );
        assert!(
            types(&store).await.is_empty(),
            "a refused opening writes nothing"
        );

        // No shift is open: an opening moves no cash, so it does not need one.
        edge.open_drawer_no_sale(cashier(), making_change(), Some(&manager()))
            .await
            .expect("a manager opens the drawer");
        let log = logged(&store).await;
        let opened = log
            .iter()
            .find(|envelope| envelope.event_type.as_str() == "cash.drawer.opened")
            .expect("the opening is in the log");
        let event: CashDrawerOpened = opened.data.decode().expect("decodes");
        assert!(event.standalone, "outside a sale");
        assert_eq!(event.reason_code_id, Some(making_change()));
        assert!(
            log.iter()
                .any(|envelope| envelope.event_type.as_str() == "security.permission.overridden"),
            "the manager's approval is its own record"
        );
    });
}

/// The store of [`session`], asking a reason of a close over or short by more than `limit`, and
/// listing [`miscounted`] for it where `listed`.
fn varying(limit: i64, listed: bool) -> EdgeSession {
    let mut session = session();
    session.shift.variance_reason_minor = Some(limit);
    if listed {
        let mut codes = session.reason_codes.codes().to_vec();
        codes.push(PublishedReasonCode::new(
            miscounted(),
            ReasonCode::new("COUNT_DIFFERENCE"),
            DisplayName::new("Counting difference"),
            vec![ReasonAction::CashVariance],
        ));
        session.reason_codes = PublishedReasonCodes::from_parts(codes);
    }
    session
}

/// A shift opened on 500k that took the 165k sale in cash and was counted at `counted`, so it is
/// `counted - 665_000` over.
async fn counted_at(edge: &Edge<FakeStore>, counted: i64) -> ShiftId {
    let shift = open_with_float(edge, 500_000).await;
    a_cash_sale(edge).await;
    edge.count_shift(cashier(), shift, counted)
        .await
        .expect("counts");
    shift
}

/// Each `cash.shift.closed` in `store`'s log, as written.
async fn closings(store: &FakeStore) -> Vec<String> {
    logged(store)
        .await
        .iter()
        .filter(|envelope| envelope.event_type.as_str() == "cash.shift.closed")
        .map(|envelope| envelope.data.as_json().to_owned())
        .collect()
}

#[test]
fn a_close_beyond_the_limit_asks_its_reason_and_records_the_one_it_is_given() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), varying(50_000, true));
        let shift = counted_at(&edge, 600_000).await;
        let close =
            |reason| edge.close_shift_at(cashier(), TillScope::default(), shift, None, reason);

        let asked = close(None).await;
        let Err(AppError::VarianceReasonRequired(owing)) = asked else {
            panic!("65k short is beyond 50k: {asked:?}");
        };
        let figures: Vec<_> = owing
            .iter()
            .map(|view| {
                (
                    view.shift_id,
                    view.state,
                    view.expected_amount,
                    view.counted_amount,
                    view.variance,
                )
            })
            .collect();
        assert_eq!(
            figures,
            [(
                shift,
                ShiftState::Counted,
                Some(vnd(665_000)),
                Some(vnd(600_000)),
                Some(vnd(-65_000))
            )],
            "the figures its count has fixed, still counted"
        );
        for wrong in [making_change(), ReasonCodeId::new(Ulid::from_u128(9_999))] {
            let refused = close(Some(wrong)).await;
            assert!(
                matches!(refused, Err(AppError::CashReasonNotValid)),
                "{refused:?}"
            );
        }
        assert!(closings(&store).await.is_empty(), "nothing closed");
        assert_eq!(
            edge.current_shift().map(|view| view.state),
            Some(ShiftState::Counted)
        );

        let closed = close(Some(miscounted()))
            .await
            .expect("closes with a reason");
        assert_eq!(closed.variance, Some(vnd(-65_000)));
        let event: CashShiftClosed = logged(&store)
            .await
            .iter()
            .find(|envelope| envelope.event_type.as_str() == "cash.shift.closed")
            .expect("the close is in the log")
            .data
            .decode()
            .expect("decodes");
        assert_eq!(event.reason_code_id, Some(miscounted()));
    });
}

#[test]
fn a_close_within_the_limit_or_with_none_set_writes_what_it_always_did() {
    run_ready(async {
        // 65k short against no limit, and exactly 50k short against a limit of 50k: neither is
        // beyond, so neither asks, and a reason sent anyway is not recorded.
        for (limit, counted) in [(0, 600_000), (50_000, 615_000)] {
            let store = FakeStore::new();
            let edge = edge_over(store.clone(), varying(limit, true));
            let shift = counted_at(&edge, counted).await;
            let closed = edge
                .close_shift_at(
                    cashier(),
                    TillScope::default(),
                    shift,
                    None,
                    Some(miscounted()),
                )
                .await
                .expect("closes without being asked");
            assert_eq!(closed.variance, Some(vnd(counted - 665_000)));
            let written = closings(&store).await;
            assert_eq!(written.len(), 1);
            assert!(
                written.iter().all(|json| !json.contains("reason_code_id")),
                "{limit}: {written:?}"
            );
        }
    });
}

#[test]
fn a_store_whose_list_offers_no_reason_for_a_variance_still_closes() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), varying(50_000, false));
        let shift = counted_at(&edge, 600_000).await;
        let closed = edge
            .close_shift(cashier(), shift)
            .await
            .expect("no reason to give, so none is asked");
        assert_eq!(closed.variance, Some(vnd(-65_000)));
        assert!(
            closings(&store)
                .await
                .iter()
                .all(|json| !json.contains("reason_code_id"))
        );
    });
}

#[test]
fn a_restart_reads_a_closes_reason_back_and_the_drawer_stays_closed() {
    run_ready(async {
        let store = FakeStore::new();
        let shift = {
            let edge = edge_over(store.clone(), varying(50_000, true));
            let shift = counted_at(&edge, 600_000).await;
            edge.close_shift_at(
                cashier(),
                TillScope::default(),
                shift,
                None,
                Some(miscounted()),
            )
            .await
            .expect("closes with a reason");
            shift
        };
        let restarted = edge_over(store.clone(), varying(50_000, true));
        restarted.rebuild().await.expect("replays the log");
        assert!(restarted.current_shift().is_none(), "the drawer is closed");
        let event: CashShiftClosed = logged(&store)
            .await
            .iter()
            .find(|envelope| envelope.event_type.as_str() == "cash.shift.closed")
            .expect("the close is in the log")
            .data
            .decode()
            .expect("decodes");
        assert_eq!(
            (event.closed_shift_id, event.reason_code_id),
            (shift, Some(miscounted()))
        );
        let next = open_with_float(&restarted, 500_000).await;
        assert_ne!(next, shift, "the next shift opens");
    });
}
