// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge re-checks a package's flow guards before a command
//! ([ADR-0192](../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md) decision
//! 4): a store whose flow asks the guest count before the first line refuses a line on an order
//! seated without one, with `FLOW_PREREQUISITE_UNMET` naming the step, and takes it once the count
//! is there. A store with no package, or a takeaway the guard does not apply to, is untouched.

use std::sync::Arc;

use pos_core::decision::Actor;
use pos_edge::{AppError, Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_proto::CurrencyCode;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::package::PackageContent;
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

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

fn edge(package: Option<PackageContent>) -> Arc<Edge<FakeStore>> {
    let mut session = EdgeSession::bootstrap();
    session.package = package.map(Arc::new);
    Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    )
}

#[test]
fn a_line_waits_for_the_guest_count_the_flow_asks_first() {
    pos_fakes::executor::run_ready(async {
        let edge = edge(Some(guest_count_first()));
        let no_count = TableId::new(Ulid::from_u128(900));
        edge.seat_table(server(), no_count, None)
            .await
            .expect("seats");
        let refused = edge.add_line(server(), no_count, a_pizza()).await;
        assert!(
            matches!(
                &refused,
                Err(AppError::FlowPrerequisiteUnmet { flow_id, step_id })
                    if flow_id == "dine_in" && step_id == "guests"
            ),
            "{refused:?}"
        );

        let counted = TableId::new(Ulid::from_u128(901));
        edge.seat_table(server(), counted, Some(4))
            .await
            .expect("seats");
        edge.add_line(server(), counted, a_pizza())
            .await
            .expect("the guest count is there, so the line goes");
    });
}

#[test]
fn a_store_with_no_package_is_untouched() {
    pos_fakes::executor::run_ready(async {
        let edge = edge(None);
        let table = TableId::new(Ulid::from_u128(900));
        edge.seat_table(server(), table, None).await.expect("seats");
        edge.add_line(server(), table, a_pizza())
            .await
            .expect("no package, no guard");
    });
}
