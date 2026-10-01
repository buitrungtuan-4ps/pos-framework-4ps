// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A table seated by mistake goes back to the floor, and a bill is never opened on nothing
//! ([ADR-0163](../../../docs/adr/0163-a-table-seated-by-mistake-is-released.md)).
//!
//! Before this a seated table left `Occupied` only by a bill or a move. A bill on nothing could
//! never settle, and voiding it put the table back where it started, so a table seated by mistake,
//! or whose every line was voided, stayed taken until the store was re-provisioned. These cases
//! hold the way out: release, only while nothing is sold, written so a restart replays it.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, Edge, EdgeSession, InMemoryReceipts, LineDraft, StaffAuth, StaffRoster,
    StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, ReasonCodeId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{SalesChannel, TableState};

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

/// The manager's badge and PIN, as the till would collect them. Obviously fake.
const MANAGER_CODE: &str = "MGR-1";
const MANAGER_PIN: &str = "4417";

fn server() -> Actor {
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

/// A table by number, so a case reads "table 1".
fn table(number: u128) -> TableId {
    TableId::new(Ulid::from_u128(40 + number))
}

/// The reason a void cites, valid for a line and a bill.
fn keyed_wrong() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_001))
}

/// An Argon2id PHC for a test PIN, on a fixed salt — the same helper the other edge suites use.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

fn approval() -> Approval {
    Approval {
        code: MANAGER_CODE.to_owned(),
        pin: MANAGER_PIN.to_owned(),
    }
}

/// A pizza taxed for dine-in, one reason for voiding, and a manager who may void a bill.
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class,
    ));
    let rates = TaxRateTable::new().with(class, SalesChannel::DineIn, TaxRate::from_percent(10));
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::EMPTY.with(Permission::VoidBill),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.reason_codes = PublishedReasonCodes::from_parts(vec![PublishedReasonCode::new(
        keyed_wrong(),
        ReasonCode::new("KEYED_WRONG"),
        DisplayName::new("Keyed in error"),
        vec![ReasonAction::VoidLine, ReasonAction::VoidBill],
    )]);
    session.staff = staff;
    session
}

fn edge_over(store: FakeStore) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session(),
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

/// Every event type the store holds, in order.
async fn logged(store: &FakeStore) -> Vec<String> {
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(200).expect("a positive limit"),
    );
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

/// **Seated by mistake, released.** The table is free again with nobody sitting at it, the order
/// ends owing nothing in the same transaction, and the next guests get an order of their own.
#[test]
fn a_table_seated_by_mistake_goes_back_to_the_floor() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        let mistaken = edge.order_for_table(table(1)).expect("an order opened");

        let released = edge
            .release_table(server(), table(1))
            .await
            .expect("releases");
        assert_eq!(released.state, TableState::Free);
        assert_eq!(edge.table_state(table(1)), TableState::Free);
        assert_eq!(edge.seated_since(table(1)), None, "nobody is sitting there");
        assert!(edge.live_orders().is_empty());
        let events = logged(&store).await;
        assert_eq!(
            &events[events.len() - 2..],
            ["sales.order.closed", "sales.table.closed"],
            "the order ends and the table frees, together"
        );
        // A till still showing the table sends a dish to it: there is no order there to take it,
        // rather than the one that ended, where it would be on no screen and never charged.
        let stale = edge.add_line(server(), table(1), a_pizza()).await;
        assert!(matches!(stale, Err(AppError::NoOpenOrder)), "got {stale:?}");
        assert_eq!(
            logged(&store).await.len(),
            events.len(),
            "nothing was written"
        );

        edge.seat_table(server(), table(1), None)
            .await
            .expect("the next guests sit down");
        let ordered = edge
            .add_line(server(), table(1), a_pizza())
            .await
            .expect("and order");
        assert_ne!(ordered.order_id, mistaken, "on an order of their own");
        let live = edge.live_orders();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].order_id, ordered.order_id);
    });
}

/// **Only while nothing is sold.** A table with a line on it pays or voids first: releasing says
/// nobody ate. The refusal writes nothing and moves nobody.
#[test]
fn a_table_with_something_sold_on_it_is_not_released() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        edge.add_line(server(), table(1), a_pizza())
            .await
            .expect("rings a pizza");
        let before = logged(&store).await.len();

        let refused = edge.release_table(server(), table(1)).await;
        assert!(
            matches!(refused, Err(AppError::OrderNotEmpty)),
            "got {refused:?}"
        );
        assert_eq!(logged(&store).await.len(), before, "nothing was written");
        assert_eq!(edge.table_state(table(1)), TableState::Occupied);
        assert_eq!(edge.live_orders().len(), 1);
    });
}

/// **Every line voided is nothing sold.** The voids stay in the log for the report; the table goes
/// back to the floor. This is the second way a table used to be stuck.
#[test]
fn a_table_whose_every_line_was_voided_is_released() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        let line = edge
            .add_line(server(), table(1), a_pizza())
            .await
            .expect("rings a pizza");
        edge.void_line(server(), line.order_line_id, keyed_wrong(), None)
            .await
            .expect("voids it before it is sent");

        edge.release_table(server(), table(1))
            .await
            .expect("releases");
        assert_eq!(edge.table_state(table(1)), TableState::Free);
        assert!(edge.live_orders().is_empty());
        assert!(
            logged(&store)
                .await
                .contains(&"sales.order_line.voided".to_owned()),
            "the void is still on the record"
        );
    });
}

/// **A bill is never opened on nothing**: not on a table nobody ordered at, and not on one whose
/// every line was voided. Such a bill could never settle, and its table would wait for a payment
/// that cannot come.
#[test]
fn a_bill_is_never_opened_on_nothing() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        let before = logged(&store).await.len();

        let refused = edge.open_bill(server(), table(1)).await;
        assert!(
            matches!(refused, Err(AppError::NothingToBill)),
            "got {refused:?}"
        );
        assert_eq!(logged(&store).await.len(), before, "nothing was written");
        assert_eq!(edge.table_state(table(1)), TableState::Occupied);

        let line = edge
            .add_line(server(), table(1), a_pizza())
            .await
            .expect("rings a pizza");
        edge.void_line(server(), line.order_line_id, keyed_wrong(), None)
            .await
            .expect("and voids it");
        let refused = edge.open_bill(server(), table(1)).await;
        assert!(
            matches!(refused, Err(AppError::NothingToBill)),
            "got {refused:?}"
        );
        assert_eq!(edge.table_state(table(1)), TableState::Occupied);
    });
}

/// **Guests who asked for the bill and left.** The table waits for payment, so it cannot be
/// released; a manager voids the bill, the lines are voided, and then it goes back to the floor.
#[test]
fn guests_who_asked_for_the_bill_and_left_are_released_once_it_is_voided() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        let line = edge
            .add_line(server(), table(1), a_pizza())
            .await
            .expect("rings a pizza");
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("they ask for the bill")
            .bill_id;

        let refused = edge.release_table(server(), table(1)).await;
        assert!(
            matches!(refused, Err(AppError::Domain(DomainError::Transition(_)))),
            "a table waiting for payment pays or is voided first, got {refused:?}"
        );

        edge.void_bill(server(), bill, keyed_wrong(), Some(&approval()))
            .await
            .expect("the manager voids the bill");
        let refused = edge.release_table(server(), table(1)).await;
        assert!(
            matches!(refused, Err(AppError::OrderNotEmpty)),
            "the pizza is still on the order, got {refused:?}"
        );

        edge.void_line(server(), line.order_line_id, keyed_wrong(), None)
            .await
            .expect("the pizza is voided");
        edge.release_table(server(), table(1))
            .await
            .expect("and the table is released");
        assert_eq!(edge.table_state(table(1)), TableState::Free);
        assert!(edge.live_orders().is_empty());
    });
}

/// **Only a seated table.** A free table has nobody to send away, and one waiting to be cleared is
/// cleaned, not released.
#[test]
fn only_a_seated_table_is_released() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let free = edge.release_table(server(), table(1)).await;
        assert!(
            matches!(free, Err(AppError::Domain(DomainError::Transition(_)))),
            "got {free:?}"
        );

        edge.seat_table(server(), table(2), None)
            .await
            .expect("seats");
        edge.add_line(server(), table(2), a_pizza())
            .await
            .expect("rings a pizza");
        edge.transfer_table(server(), table(2), table(3))
            .await
            .expect("the guests move, leaving table 2 to be cleared");
        let left = edge.release_table(server(), table(2)).await;
        assert!(
            matches!(left, Err(AppError::Domain(DomainError::Transition(_)))),
            "got {left:?}"
        );
        assert_eq!(edge.table_state(table(2)), TableState::NeedsCleaning);
    });
}

/// **A release survives a restart.** The fold replays `sales.order.closed`, so a store that
/// restarts has the table free, no live order on it, and seats it again.
#[test]
fn a_release_survives_a_restart() {
    run_ready(async {
        let store = FakeStore::default();
        {
            let edge = edge_over(store.clone());
            edge.seat_table(server(), table(1), None)
                .await
                .expect("seats");
            let line = edge
                .add_line(server(), table(1), a_pizza())
                .await
                .expect("rings a pizza");
            edge.void_line(server(), line.order_line_id, keyed_wrong(), None)
                .await
                .expect("voids it");
            edge.release_table(server(), table(1))
                .await
                .expect("releases");
        }

        let edge = edge_over(store);
        edge.rebuild().await.expect("replays the log");
        assert_eq!(edge.table_state(table(1)), TableState::Free);
        assert_eq!(edge.seated_since(table(1)), None);
        assert!(
            edge.live_orders().is_empty(),
            "the voided line does not bring the order back"
        );
        let stale = edge.add_line(server(), table(1), a_pizza()).await;
        assert!(
            matches!(stale, Err(AppError::NoOpenOrder)),
            "the replayed table holds no order either, got {stale:?}"
        );
        edge.seat_table(server(), table(1), None)
            .await
            .expect("the table is seated again");
    });
}
