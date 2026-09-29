// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Guests moving to another table, and taking their order with them.
//!
//! `sales.table.transferred` has been in the schema since it was written, with
//! `sales.order.transfer` in the permission catalogue beside it, and nothing emitted either: guests
//! who asked to sit by the window had to be seated again at the new table, as a new order, while
//! the old table kept a check nobody was sitting at. These cases hold what the till now relies on:
//! the order moves whole (lines, total, the time they sat down), the table left waits to be
//! cleared, the move is refused where it would lose track of something, and a restart keeps it.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_edge::{AppError, Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, OrderId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{PublishedReasonCodes, ReasonAction};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{Open, SalesChannel, TableState};

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

/// A table by number, so a case reads "table 1 moves to table 2".
fn table(number: u128) -> TableId {
    TableId::new(Ulid::from_u128(40 + number))
}

/// A pizza, taxed on the dine-in and QR channels, with the QR hold on the default (on).
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class,
    ));
    let rates = TaxRateTable::new()
        .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class, SalesChannel::Qr, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
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

/// Seats a table and rings a pizza on it, returning the order the table holds.
async fn a_seated_table(edge: &Edge<FakeStore>, number: u128) -> OrderId {
    edge.seat_table(server(), table(number), None)
        .await
        .expect("seats");
    edge.add_line(server(), table(number), a_pizza())
        .await
        .expect("rings a pizza")
        .order_id
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

/// **The order moves whole.** The new table holds the same order, owes the same amount, and says
/// the guests sat down when they first did; the table they left owes nothing and waits to be
/// cleared. Nothing is rung again: one event, and no second order.
#[test]
fn guests_move_to_a_free_table_and_take_their_order_with_them() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), session());
        let order_id = a_seated_table(&edge, 1).await;
        let owed = edge.check_totals(table(1)).expect("the check reads");
        let sat_down = edge.seated_since(table(1));
        assert!(sat_down.is_some());

        let moved = edge
            .transfer_table(server(), table(1), table(2))
            .await
            .expect("the guests move");
        assert_eq!(moved.order_id, order_id);
        assert_eq!(moved.from.state, TableState::NeedsCleaning);
        assert_eq!(moved.to.state, TableState::Occupied);

        assert_eq!(edge.table_state(table(1)), TableState::NeedsCleaning);
        assert_eq!(edge.table_state(table(2)), TableState::Occupied);
        assert_eq!(edge.check_totals(table(2)).expect("reads"), owed);
        assert_eq!(
            edge.check_totals(table(1)).expect("reads").total_due,
            vnd(0),
            "the table they left owes nothing"
        );
        assert_eq!(
            edge.seated_since(table(2)),
            sat_down,
            "they sat down when they did"
        );
        assert_eq!(
            edge.seated_since(table(1)),
            None,
            "nobody is sitting there now"
        );
        let live = edge.live_orders();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].order_id, order_id);
        assert_eq!(live[0].table_id, Some(table(2)));

        let second = edge
            .add_line(server(), table(2), a_pizza())
            .await
            .expect("a second round at the new table");
        assert_eq!(second.order_id, order_id, "joins the order they brought");
        edge.clean_table(server(), table(1))
            .await
            .expect("the table they left is cleared");
        assert_eq!(edge.table_state(table(1)), TableState::Free);
        edge.open_bill(server(), table(2))
            .await
            .expect("and they ask for the bill where they sit");
        assert_eq!(edge.table_state(table(2)), TableState::AwaitingPayment);

        let events = logged(&store).await;
        let count = |kind: &str| events.iter().filter(|event| *event == kind).count();
        assert_eq!(count("sales.table.transferred"), 1);
        assert_eq!(count("sales.order.opened"), 1, "no second order");
        assert_eq!(count("sales.table.opened"), 1, "and no second seating");
    });
}

/// **Only to a free table.** Not to one with people at it, not to one waiting to be cleared, and
/// not to the table they are at. A refusal writes nothing and moves nobody.
#[test]
fn guests_move_only_to_a_free_table() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), session());
        a_seated_table(&edge, 1).await;
        a_seated_table(&edge, 2).await;
        a_seated_table(&edge, 3).await;
        edge.transfer_table(server(), table(3), table(4))
            .await
            .expect("table 3 moves, and leaves itself to be cleared");
        let before = logged(&store).await.len();

        for (to, why) in [
            (table(2), "a table with people at it"),
            (table(3), "a table waiting to be cleared"),
            (table(1), "the table they are at"),
        ] {
            let refused = edge.transfer_table(server(), table(1), to).await;
            assert!(
                matches!(refused, Err(AppError::Domain(DomainError::Transition(_)))),
                "{why}: got {refused:?}"
            );
        }
        assert_eq!(logged(&store).await.len(), before, "nothing was written");
        assert_eq!(edge.table_state(table(1)), TableState::Occupied);
        assert_eq!(edge.live_orders().len(), 3);
    });
}

/// **Only before the bill.** A table whose guests have asked for it pays where it sits, and a table
/// nobody is sitting at has nobody to move.
#[test]
fn guests_who_have_asked_for_the_bill_pay_where_they_sit() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        a_seated_table(&edge, 1).await;
        edge.open_bill(server(), table(1))
            .await
            .expect("they ask for the bill");

        let refused = edge.transfer_table(server(), table(1), table(2)).await;
        assert!(
            matches!(refused, Err(AppError::Domain(DomainError::Transition(_)))),
            "got {refused:?}"
        );
        let nobody = edge.transfer_table(server(), table(5), table(6)).await;
        assert!(
            matches!(nobody, Err(AppError::Domain(DomainError::Transition(_)))),
            "got {nobody:?}"
        );
        assert_eq!(edge.table_state(table(1)), TableState::AwaitingPayment);
        assert_eq!(edge.table_state(table(2)), TableState::Free);
    });
}

/// Without `sales.order.transfer` the move is refused, and nothing is written.
#[test]
fn moving_guests_needs_the_transfer_permission() {
    run_ready(async {
        let mut without = session();
        without.granted = Permission::ALL
            .iter()
            .copied()
            .filter(|permission| *permission != Permission::TransferOrder)
            .collect();
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), without);
        a_seated_table(&edge, 1).await;
        let before = logged(&store).await.len();

        let refused = edge.transfer_table(server(), table(1), table(2)).await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied { .. }))
            ),
            "got {refused:?}"
        );
        assert_eq!(logged(&store).await.len(), before);
        assert_eq!(edge.table_state(table(1)), TableState::Occupied);
        assert_eq!(edge.table_state(table(2)), TableState::Free);
    });
}

/// A store that restarts still has the guests at the table they moved to, with their order, and
/// the table they left still waiting to be cleared.
#[test]
fn the_move_survives_a_restart() {
    run_ready(async {
        let store = FakeStore::default();
        let order_id = {
            let edge = edge_over(store.clone(), session());
            let order_id = a_seated_table(&edge, 1).await;
            edge.transfer_table(server(), table(1), table(2))
                .await
                .expect("the guests move");
            order_id
        };
        let rebuilt = edge_over(store, session());
        rebuilt.rebuild().await.expect("replays the log");

        assert_eq!(rebuilt.table_state(table(1)), TableState::NeedsCleaning);
        assert_eq!(rebuilt.table_state(table(2)), TableState::Occupied);
        assert!(rebuilt.seated_since(table(2)).is_some());
        let live = rebuilt.live_orders();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].order_id, order_id);
        assert_eq!(live[0].table_id, Some(table(2)));
    });
}

/// Opens a guest's QR order at a table, as the intake path does.
async fn a_guest_order(edge: &Edge<FakeStore>, at: TableId) -> OrderId {
    let class = EdgeSession::standard_tax_class();
    let line = pos_core::menu::PricedLine {
        menu_item_id: pizza(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: class,
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        modifier_menu_item_ids: Vec::new(),
        modifier_display_names: Vec::new(),
        repriced: false,
    };
    edge.open_inbound_order(
        server().device_id,
        Open::from_known(SalesChannel::Qr),
        Some(at),
        &[(line, false)],
        None,
    )
    .await
    .expect("a guest order opens")
    .order_id
}

/// **A guest's order still waiting for staff is decided first** (ADR-0116). It took the table over
/// from any order a server had started there, and refusing it hands the table back to that order,
/// which only works while both are where they opened. Once staff confirm it, it moves like any other.
#[test]
fn a_guest_order_waiting_for_staff_is_decided_before_it_moves() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), session());
        let guests = a_guest_order(&edge, table(1)).await;
        let before = logged(&store).await.len();

        let refused = edge.transfer_table(server(), table(1), table(2)).await;
        assert!(
            matches!(refused, Err(AppError::AwaitingStaffConfirmation)),
            "got {refused:?}"
        );
        assert_eq!(logged(&store).await.len(), before, "nothing was written");

        edge.confirm_inbound_order(server(), guests)
            .await
            .expect("staff confirm it");
        let moved = edge
            .transfer_table(server(), table(1), table(2))
            .await
            .expect("and then it moves");
        assert_eq!(moved.order_id, guests);
    });
}

/// **A refused guest order hands the table back to the order that moved there.** Guests move from
/// table 1 to table 2 with their order; somebody at table 2 then sends a QR order, which takes the
/// table over, and staff refuse it. Table 2 goes back to the guests' order, as it would had they sat
/// there from the start, and not free with people eating at it.
#[test]
fn a_refused_guest_order_hands_the_new_table_back_to_the_order_that_moved_there() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), session());
        let waiters = a_seated_table(&edge, 1).await;
        edge.transfer_table(server(), table(1), table(2))
            .await
            .expect("the guests move");
        let guests = a_guest_order(&edge, table(2)).await;
        let reason = PublishedReasonCodes::framework_default()
            .for_action(ReasonAction::RejectOrder)
            .next()
            .expect("the framework set covers rejecting an order")
            .id;
        edge.reject_inbound_order(server(), guests, reason)
            .await
            .expect("staff refuse the guest's order");

        for (when, edge) in [
            ("live", edge),
            ("after a restart", edge_over(store, session())),
        ] {
            edge.rebuild().await.expect("rebuilds from the log");
            assert_eq!(edge.table_state(table(2)), TableState::Occupied, "{when}");
            let live = edge.live_orders();
            assert_eq!(live.len(), 1, "{when}");
            assert_eq!(live[0].order_id, waiters, "{when}");
            assert_eq!(live[0].table_id, Some(table(2)), "{when}");
        }
    });
}
