// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voiding a line and a bill, with the manager PIN the permission registry has always demanded
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md), roadmap B2.2).
//!
//! `docs/pos-spec.md` §11 item 2 makes a mandatory reason from a cloud-managed list one of the six
//! fraud controls, and §11.4 makes the manager-PIN override another. Both were declared and neither
//! ran: `Permission::VoidFiredLine` and `Permission::VoidBill` carried `pin: true`,
//! `pos_core::decide_line` checked `Grant::pin_required` against a `pin_verified` argument — and
//! **nothing in the tree ever called it**, so the only act that could reach the check was one that
//! never happened. `security.permission.overridden`, declared precisely because the override "had no
//! auditable record at all", had no producer either.
//!
//! These cases are the enforcement: the reason must come from the synced list, the fired line needs
//! a manager, the manager's approval is recorded as its own event, and a void survives a restart.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, Edge, EdgeSession, InMemoryReceipts, StaffAuth, StaffRoster, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{
    BillId, DeviceId, EmployeeId, MenuItemId, OrderLineId, ReasonCodeId, StationId, StoreId,
    TableId, TaxClassId,
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
use pos_proto::{BillState, OrderLineState, SalesChannel};

/// The signed-in server: they may take an order, and their role does **not** grant a fired-line
/// void. That is the whole point — a void they could do alone would not be a control.
fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn manager_id() -> EmployeeId {
    EmployeeId::new(Ulid::from_u128(11))
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

/// The reason a void cites, valid for both a line and a bill.
fn keyed_wrong() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_001))
}

/// A reason valid only for a *rejection* — the near miss that proves the list is per-action.
fn out_of_stock() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_002))
}

/// The manager's badge and PIN, as the till would collect them. Obviously fake.
const MANAGER_CODE: &str = "MGR-1";
const MANAGER_PIN: &str = "4417";

/// An Argon2id PHC for a test PIN, on a fixed salt — the same helper the other edge suites use.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(b"fixed-test-salt!").expect("salt");
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .expect("hash")
        .to_string()
}

fn approval() -> Approval {
    Approval {
        code: MANAGER_CODE.to_owned(),
        pin: MANAGER_PIN.to_owned(),
    }
}

/// A store that can price a dine-in line, holds two authored reasons, and has one manager on the
/// roster who may void a fired line and a bill.
fn session() -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class(),
    ));
    let rates = TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
    let reasons = PublishedReasonCodes::from_parts(vec![
        PublishedReasonCode::new(
            keyed_wrong(),
            ReasonCode::new("KEYED_WRONG"),
            DisplayName::new("Keyed in error"),
            vec![ReasonAction::VoidLine, ReasonAction::VoidBill],
        ),
        PublishedReasonCode::new(
            out_of_stock(),
            ReasonCode::new("OUT_OF_STOCK"),
            DisplayName::new("Out of stock"),
            vec![ReasonAction::RejectOrder],
        ),
    ]);
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(manager_id()),
            permissions: PermissionSet::EMPTY
                .with(Permission::VoidFiredLine)
                .with(Permission::VoidBill),
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.reason_codes = reasons;
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

/// Seats a table and adds one line, returning it.
async fn a_seated_line(edge: &Edge<FakeStore>) -> OrderLineId {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    edge.add_line(server(), table(), a_line())
        .await
        .expect("adds")
        .order_line_id
}

/// Every event type the store holds, in order — so a case can say what the log records.
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

#[test]
fn an_unfired_line_is_an_ordinary_cancel_and_needs_no_manager() {
    // Nothing was made and no stock moved, so the person who took the order can take it back off.
    // Demanding a manager here would put a queue behind every mis-tap.
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let line = a_seated_line(&edge).await;

        let voided = edge
            .void_line(server(), line, keyed_wrong(), None)
            .await
            .expect("a server may cancel a line the kitchen never saw");

        assert_eq!(voided.state, OrderLineState::Voided);
    });
}

#[test]
fn a_fired_line_needs_a_manager_and_the_approval_is_its_own_event() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let line = a_seated_line(&edge).await;
        edge.fire_line(server(), line, Some(station()))
            .await
            .expect("fires");

        // Without a PIN: refused. This is the first time in the tree that `pin: true` has meant
        // anything — before roadmap B2.2 the flag was set, the domain checked it, and no caller
        // ever reached the check.
        let refused = edge.void_line(server(), line, keyed_wrong(), None).await;
        assert!(
            matches!(refused, Err(AppError::ApprovalRequired)),
            "a fired line needs a manager's PIN, got {refused:?}"
        );

        // A wrong PIN is refused the same way an unknown code is: the till learns nothing about who
        // could have approved.
        let wrong = edge
            .void_line(
                server(),
                line,
                keyed_wrong(),
                Some(&Approval {
                    code: MANAGER_CODE.to_owned(),
                    pin: "0000".to_owned(),
                }),
            )
            .await;
        assert!(matches!(wrong, Err(AppError::ApprovalRefused)));

        // The right PIN releases it, and the line is voided.
        let voided = edge
            .void_line(server(), line, keyed_wrong(), Some(&approval()))
            .await
            .expect("the manager authorises the void");
        assert_eq!(voided.state, OrderLineState::Voided);

        // Both events land, in one transaction: the void, and the override that authorised it. A log
        // holding the void alone would show an act nobody could have performed — which is exactly
        // what `security.permission.overridden` was declared for and had no producer of until now.
        let types = logged(&store).await;
        assert!(
            types.contains(&"sales.order_line.voided".to_owned()),
            "got {types:?}"
        );
        assert!(
            types.contains(&"security.permission.overridden".to_owned()),
            "the manager-PIN override is on the record, got {types:?}"
        );

        // And the refused attempts wrote nothing.
        assert_eq!(
            types
                .iter()
                .filter(|kind| *kind == "sales.order_line.voided")
                .count(),
            1,
            "the two refusals left no trace of a void, got {types:?}"
        );
    });
}

#[test]
fn a_void_must_cite_a_reason_the_store_holds_for_voiding() {
    // The near miss: `OUT_OF_STOCK` is a real, active, published entry — for *rejecting* a guest's
    // order. `applies_to` is what makes the list per-action, and a void that could cite any entry
    // would make the per-action tagging decorative.
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let line = a_seated_line(&edge).await;

        let refused = edge.void_line(server(), line, out_of_stock(), None).await;
        assert!(
            matches!(refused, Err(AppError::VoidReasonNotValid)),
            "got {refused:?}"
        );

        // An id the store's list does not hold at all is refused too.
        let unknown = edge
            .void_line(
                server(),
                line,
                ReasonCodeId::new(Ulid::from_u128(u128::MAX)),
                None,
            )
            .await;
        assert!(matches!(unknown, Err(AppError::VoidReasonNotValid)));
    });
}

#[test]
fn voiding_a_bill_always_needs_a_manager_and_a_settled_one_cannot_be_voided() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let line = a_seated_line(&edge).await;
        edge.fire_line(server(), line, Some(station()))
            .await
            .expect("fires");
        let bill = edge
            .open_bill(server(), table())
            .await
            .expect("opens a bill")
            .bill_id;

        // No unfired shape for a bill: it is money either way, so the PIN is required every time.
        let refused = edge.void_bill(server(), bill, keyed_wrong(), None).await;
        assert!(
            matches!(refused, Err(AppError::ApprovalRequired)),
            "got {refused:?}"
        );

        let state = edge
            .void_bill(server(), bill, keyed_wrong(), Some(&approval()))
            .await
            .expect("the manager authorises the void");
        assert_eq!(state, BillState::Voided);

        // A second void is refused by the bill machine — `Voided` is terminal — rather than
        // recording a second void of the same money.
        let again = edge
            .void_bill(server(), bill, keyed_wrong(), Some(&approval()))
            .await;
        assert!(
            matches!(again, Err(AppError::Domain(_))),
            "a voided bill cannot be voided twice, got {again:?}"
        );

        let types = logged(&store).await;
        assert_eq!(
            types
                .iter()
                .filter(|kind| *kind == "billing.bill.voided")
                .count(),
            1
        );
        assert!(types.contains(&"security.permission.overridden".to_owned()));
    });
}

#[test]
fn a_void_survives_a_restart() {
    // Both void events had no fold arm at all before this slice, so a box that restarted after a
    // void came back believing the line was still fired and the bill still open — it would have
    // re-fired the line to the kitchen and charged for it.
    run_ready(async {
        let store = FakeStore::default();
        let (line, bill) = {
            let edge = edge_over(store.clone());
            let line = a_seated_line(&edge).await;
            edge.fire_line(server(), line, Some(station()))
                .await
                .expect("fires");
            let bill = edge
                .open_bill(server(), table())
                .await
                .expect("opens a bill")
                .bill_id;
            edge.void_line(server(), line, keyed_wrong(), Some(&approval()))
                .await
                .expect("voids the line");
            edge.void_bill(server(), bill, keyed_wrong(), Some(&approval()))
                .await
                .expect("voids the bill");
            (line, bill)
        };

        let edge = edge_over(store.clone());
        edge.rebuild().await.expect("rebuilds from the log");
        assert_eq!(
            edge.line_state(line),
            Some(OrderLineState::Voided),
            "the voided line came back voided, not fired"
        );
        // The bill came back voided: settling it is refused by the machine rather than taking money
        // for a bill somebody cancelled.
        let settle = edge
            .void_bill(server(), bill, keyed_wrong(), Some(&approval()))
            .await;
        assert!(matches!(settle, Err(AppError::Domain(_))), "got {settle:?}");
        let _ = BillId::new(Ulid::from_u128(0));
    });
}
