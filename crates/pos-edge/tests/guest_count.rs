// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A seated table's guest count, recorded in place
//! ([ADR-0254](../../../docs/adr/0254-a-seated-tables-guest-count-is-recorded-in-place.md)): a
//! table seated without its count, at a store whose flow asks for one before the first line, is
//! given it with `sales.order.guests_counted` rather than released and seated again. The line the
//! guard refused then goes, the latest count wins, a replay keeps it, and a free table or a count
//! of zero is refused.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_edge::{AppError, Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::events::SalesOrderGuestsCounted;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::package::PackageContent;
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{CurrencyCode, TableState};

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(1)),
        device_id: DeviceId::new(Ulid::from_u128(1)),
    }
}

fn a_pizza() -> LineDraft {
    let price = Money::new(CurrencyCode::VND, 150_000);
    LineDraft {
        menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: price,
        line_total: price,
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// A package whose dine-in flow asks the guest count before the first line, which the edge sees
/// from the order's own count.
fn guest_count_first() -> PackageContent {
    serde_json::from_value(serde_json::json!({
        "package_id": "01J00000000000000000000P01",
        "package_version_id": "01J00000000000000000000V01",
        "version_label": "Guests first",
        "min_edge_version": "0.0.0",
        "flows": [{ "body": {
            "flow_id": "dine_in",
            "start_action_id": "sales.table.seat",
            "steps": [
                { "step_id": "guests", "action_id": "sales.table.seat", "kind": "STEP_KIND_REQUIRED",
                  "applies_when": "channel == 'SALES_CHANNEL_DINE_IN'",
                  "done_when": "has(order.guest_count) && order.guest_count > 0" },
                { "step_id": "first_line", "action_id": "sales.line.add", "kind": "STEP_KIND_REQUIRED",
                  "prerequisites": ["guests"] },
            ],
        }}],
    }))
    .expect("the package parses")
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

fn edge_over(store: FakeStore, package: Option<PackageContent>) -> Arc<Edge<FakeStore>> {
    let mut session = EdgeSession::bootstrap();
    session.package = package.map(Arc::new);
    Arc::new(
        Edge::new(
            store,
            StoreIdentity::for_store(store_id()),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    )
}

fn table(n: u128) -> TableId {
    TableId::new(Ulid::from_u128(900 + n))
}

fn refused_for_guests(result: &Result<impl std::fmt::Debug, AppError>) -> bool {
    matches!(
        result,
        Err(AppError::FlowPrerequisiteUnmet { flow_id, step_id })
            if flow_id == "dine_in" && step_id == "guests"
    )
}

/// The counts the log holds, in the order they were recorded.
async fn counts_logged(store: &FakeStore) -> Vec<u16> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(200).expect("a positive limit"));
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "sales.order.guests_counted")
        .map(|envelope| {
            envelope
                .data
                .decode::<SalesOrderGuestsCounted>()
                .expect("decodes")
                .guest_count
        })
        .collect()
}

#[test]
fn a_count_given_in_place_lets_the_waiting_line_go() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), Some(guest_count_first()));
        edge.seat_table(server(), table(0), None)
            .await
            .expect("seats");
        let refused = edge.add_line(server(), table(0), a_pizza()).await;
        assert!(refused_for_guests(&refused), "{refused:?}");

        let view = edge
            .count_guests(server(), table(0), 3)
            .await
            .expect("records the count");
        assert_eq!(view.state, TableState::Occupied, "the table does not move");
        edge.add_line(server(), table(0), a_pizza())
            .await
            .expect("the count is there now, so the line goes");
        assert_eq!(counts_logged(&store).await, vec![3]);
    });
}

#[test]
fn the_latest_count_wins_and_a_replay_keeps_it() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), Some(guest_count_first()));
        edge.seat_table(server(), table(0), Some(2))
            .await
            .expect("seats");
        edge.count_guests(server(), table(0), 5)
            .await
            .expect("corrects the count");
        assert_eq!(counts_logged(&store).await, vec![5]);

        // A table seated without its count, given one, then the box restarts before the line.
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        edge.count_guests(server(), table(1), 4)
            .await
            .expect("records the count");
        let restarted = edge_over(store.clone(), Some(guest_count_first()));
        restarted.rebuild().await.expect("replays the log");
        restarted
            .add_line(server(), table(1), a_pizza())
            .await
            .expect("the replayed count lets the line go");
    });
}

#[test]
fn a_free_table_or_a_count_of_zero_is_refused() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), Some(guest_count_first()));
        let free = edge.count_guests(server(), table(0), 2).await;
        assert!(
            matches!(free, Err(AppError::Domain(DomainError::Transition(_)))),
            "{free:?}"
        );
        edge.seat_table(server(), table(0), None)
            .await
            .expect("seats");
        let zero = edge.count_guests(server(), table(0), 0).await;
        assert!(
            matches!(zero, Err(AppError::Domain(DomainError::Empty { .. }))),
            "{zero:?}"
        );
        assert!(counts_logged(&store).await.is_empty(), "nothing is written");
    });
}
