// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store that enforces each person's own permissions decides with them
//! ([ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decisions 1,
//! 4 and 5).
//!
//! Until a store turns `permissions.enforced` on, every command is decided with a store-wide set
//! that grants every permission, and a PIN-flagged one asks for a holder's PIN. These cases hold
//! the two modes apart:
//!
//! * **enforced** — a person holds what their roles grant and nothing else; a permission held
//!   directly needs nobody's PIN; one held with approval needs somebody else who holds it directly;
//!   a person the roster does not know holds nothing;
//! * **not enforced** — exactly as before, which is what every existing store runs until an owner
//!   turns the switch on.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, Edge, EdgeSession, InMemoryReceipts, StaffAuth, StaffRoster, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, OrderLineId, ReasonCodeId, StationId, StoreId, TableId,
    TaxClassId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{OrderLineState, SalesChannel};

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

const SERVER_CODE: &str = "SRV-1";
const SERVER_PIN: &str = "2580";
const MANAGER_CODE: &str = "MGR-1";
const MANAGER_PIN: &str = "4417";
const HOST_CODE: &str = "HST-1";

fn person(n: u128) -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(n)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

/// Takes orders and settles them, and voids a fired line only with a manager's approval.
fn server() -> Actor {
    person(10)
}

/// Holds the fired-line void directly.
fn manager() -> Actor {
    person(11)
}

/// Holds seating a table only with approval: an act that asks for no approver.
fn host() -> Actor {
    person(12)
}

/// Signed in on a till, but on no roster this store has: holds nothing once the store enforces.
fn stranger() -> Actor {
    person(99)
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn item() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

fn station() -> StationId {
    StationId::new(Ulid::from_u128(900))
}

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

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

fn approval_by(code: &str, pin: &str) -> Approval {
    Approval {
        code: code.to_owned(),
        pin: pin.to_owned(),
    }
}

/// What both roles may do on the floor.
fn taking_orders() -> PermissionSet {
    PermissionSet::EMPTY
        .with(Permission::ManageTables)
        .with(Permission::AddLine)
        .with(Permission::FireLines)
}

/// A store with a server and a manager on its roster, deciding with each person's own set when
/// `enforced`.
fn session(enforced: bool) -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class(),
    ));
    let rates = TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
    let reasons = PublishedReasonCodes::from_parts(vec![PublishedReasonCode::new(
        keyed_wrong(),
        ReasonCode::new("KEYED_WRONG"),
        DisplayName::new("Keyed in error"),
        vec![ReasonAction::VoidLine],
    )]);
    let mut staff = StaffRoster::new();
    staff.insert(
        SERVER_CODE,
        StaffAuth {
            employee_id: Some(server().employee_id),
            permissions: taking_orders(),
            permissions_with_approval: PermissionSet::EMPTY.with(Permission::VoidFiredLine),
            discount_ceiling: None,
            pin_phc: Some(hash_of(SERVER_PIN)),
        },
    );
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(manager().employee_id),
            permissions: taking_orders().with(Permission::VoidFiredLine),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    staff.insert(
        HOST_CODE,
        StaffAuth {
            employee_id: Some(host().employee_id),
            permissions: PermissionSet::EMPTY,
            permissions_with_approval: PermissionSet::EMPTY.with(Permission::ManageTables),
            discount_ceiling: None,
            pin_phc: None,
        },
    );
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.reason_codes = reasons;
    session.staff = staff;
    session.permissions_enforced = enforced;
    session
}

fn edge_over(store: FakeStore, enforced: bool) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session(enforced),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn a_line() -> pos_edge::LineDraft {
    pos_edge::LineDraft {
        menu_item_id: item(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// Seats the table, adds a line and fires it, all as `who`.
async fn a_fired_line(edge: &Edge<FakeStore>, who: Actor) -> OrderLineId {
    edge.seat_table(who, table(), None).await.expect("seats");
    let line = edge
        .add_line(who, table(), a_line())
        .await
        .expect("adds")
        .order_line_id;
    edge.fire_line(who, line, Some(station()))
        .await
        .expect("fires");
    line
}

/// Every event type the store holds, in order.
async fn logged(store: &FakeStore) -> Vec<String> {
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(100).expect("a positive limit"),
    );
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

fn refused_for<T: core::fmt::Debug>(result: Result<T, AppError>, permission: Permission) {
    match result {
        Err(AppError::Domain(DomainError::PermissionDenied { permission: id })) => {
            assert_eq!(id, permission.meta().id);
        }
        other => panic!(
            "expected a refusal for {}, got {other:?}",
            permission.meta().id
        ),
    }
}

#[test]
fn an_enforcing_store_lets_each_person_do_what_their_roles_grant_and_no_more() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), true);

        // The server's roles grant the floor, so the floor works.
        a_fired_line(&edge, server()).await;

        // Their roles do not grant opening a shift, which the store-wide set always did.
        refused_for(
            edge.open_shift(server(), vnd(500_000)).await,
            Permission::OpenShift,
        );

        // Somebody the roster does not know holds nothing, not even seating a table.
        refused_for(
            edge.seat_table(stranger(), TableId::new(Ulid::from_u128(43)), None)
                .await,
            Permission::ManageTables,
        );
    });
}

#[test]
fn a_permission_held_directly_needs_nobodys_pin() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), true);
        let line = a_fired_line(&edge, manager()).await;

        let voided = edge
            .void_line(manager(), line, keyed_wrong(), None)
            .await
            .expect("a direct holder voids a fired line alone");
        assert_eq!(voided.state, OrderLineState::Voided);

        // Nobody overrode anything, so the log says nobody did.
        assert!(
            !logged(&store)
                .await
                .iter()
                .any(|kind| kind == "security.permission.overridden"),
            "acting on a permission you hold is not an override"
        );
    });
}

#[test]
fn a_permission_held_with_approval_needs_somebody_else_who_holds_it() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), true);
        let line = a_fired_line(&edge, server()).await;

        // No approver: the till is told to ask for one.
        let asked = edge.void_line(server(), line, keyed_wrong(), None).await;
        assert!(
            matches!(asked, Err(AppError::ApprovalRequired)),
            "got {asked:?}"
        );

        // Their own code and PIN do not approve their own act (decision 5).
        let own = edge
            .void_line(
                server(),
                line,
                keyed_wrong(),
                Some(&approval_by(SERVER_CODE, SERVER_PIN)),
            )
            .await;
        assert!(matches!(own, Err(AppError::ApprovalRefused)), "got {own:?}");

        // The manager, who holds it directly, does — and the log names them.
        let voided = edge
            .void_line(
                server(),
                line,
                keyed_wrong(),
                Some(&approval_by(MANAGER_CODE, MANAGER_PIN)),
            )
            .await
            .expect("a direct holder approves");
        assert_eq!(voided.state, OrderLineState::Voided);
        assert!(
            logged(&store)
                .await
                .iter()
                .any(|kind| kind == "security.permission.overridden"),
            "an approval is its own event"
        );
    });
}

#[test]
fn a_permission_held_neither_way_is_refused_without_asking_for_an_approver() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), true);
        let line = a_fired_line(&edge, manager()).await;

        // The stranger holds nothing, so there is nobody to ask: a manager's PIN does not make a
        // person who may not void at all into one who may.
        refused_for(
            edge.void_line(
                stranger(),
                line,
                keyed_wrong(),
                Some(&approval_by(MANAGER_CODE, MANAGER_PIN)),
            )
            .await,
            Permission::VoidFiredLine,
        );
    });
}

#[test]
fn a_permission_held_with_approval_is_refused_on_an_act_that_asks_for_no_approver() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), true);

        // Only the acts that ask for an approver today take one: voiding a fired line or a bill, a
        // discount above the ceiling, and opening the drawer without a sale. Seating a table is
        // not one of them, so holding it with approval cannot be used, and a role holds it directly.
        refused_for(
            edge.seat_table(host(), table(), None).await,
            Permission::ManageTables,
        );
    });
}

#[test]
fn a_store_that_does_not_enforce_decides_as_it_always_has() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), false);

        // The store-wide set: the stranger seats a table and fires a line.
        let line = a_fired_line(&edge, stranger()).await;

        // A PIN-flagged permission still asks for a holder's PIN, even of a manager...
        let asked = edge.void_line(manager(), line, keyed_wrong(), None).await;
        assert!(
            matches!(asked, Err(AppError::ApprovalRequired)),
            "got {asked:?}"
        );

        // ...and a manager alone on shift approves their own void, as before.
        edge.void_line(
            manager(),
            line,
            keyed_wrong(),
            Some(&approval_by(MANAGER_CODE, MANAGER_PIN)),
        )
        .await
        .expect("self-approval stands until the store enforces each person's own set");
    });
}
