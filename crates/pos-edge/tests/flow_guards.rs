// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge re-checks a package's flow guards before a command
//! ([ADR-0192](../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md) decision
//! 4): a store whose flow asks the guest count before the first line refuses a line on an order
//! seated without one, with `FLOW_PREREQUISITE_UNMET` naming the step, and takes it once the count
//! is there. A store with no package, or a takeaway the guard does not apply to, is untouched.
//!
//! A flow's guards apply only to the orders its scope takes in
//! ([ADR-0244](../../../docs/adr/0244-a-flows-guards-apply-to-the-orders-it-governs.md)): by
//! default those its start act opened, so a dine-in flow seated at a table never refuses a counter
//! order; under `ALL_ORDERS` or `CHANNELS` it does, as the flow says.
//!
//! An order keeps the rules of the version it opened under (decision 5): a publish while it is open
//! does not change them, unless the edge has had to let that version go, when the order is checked
//! under the current one.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_edge::{
    AppError, Edge, EdgeSession, InMemoryReceipts, LineDraft, OrderLineChoice, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::events::SalesOrderOpened;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, PackageVersionId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::package::PackageContent;
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{CurrencyCode, SalesChannel};

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

fn version_id(n: u128) -> PackageVersionId {
    PackageVersionId::new(Ulid::from_u128(n))
}

/// Version `n` of the package: [`guest_count_first`]'s flow when `guarded`, no flow otherwise.
fn version(n: u128, guarded: bool) -> PackageContent {
    let mut content = guest_count_first();
    content.package_version_id = version_id(n);
    if !guarded {
        content.flows.clear();
    }
    content
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

fn edge(package: Option<PackageContent>) -> Arc<Edge<FakeStore>> {
    edge_over(FakeStore::default(), package)
}

/// Publishes `package` to the running edge, as the config-pull loop would.
fn publish(edge: &Edge<FakeStore>, package: Option<PackageContent>) {
    let mut session = (*edge.session()).clone();
    session.package = package.map(Arc::new);
    edge.apply_session(session);
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

#[test]
fn a_line_waits_for_the_guest_count_the_flow_asks_first() {
    pos_fakes::executor::run_ready(async {
        let edge = edge(Some(guest_count_first()));
        let no_count = table(0);
        edge.seat_table(server(), no_count, None)
            .await
            .expect("seats");
        let refused = edge.add_line(server(), no_count, a_pizza()).await;
        assert!(refused_for_guests(&refused), "{refused:?}");

        let counted = table(1);
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
        edge.seat_table(server(), table(0), None)
            .await
            .expect("seats");
        edge.add_line(server(), table(0), a_pizza())
            .await
            .expect("no package, no guard");
    });
}

/// A publish does not change an open order's rules: the order seated under the guarded version
/// still waits for its guest count after a version without the flow arrives, and an order seated
/// after it does not. Its `sales.order.opened` names the version.
#[test]
fn an_open_order_keeps_the_rules_it_opened_under() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), Some(version(1, true)));
        edge.seat_table(server(), table(0), None)
            .await
            .expect("seats");
        publish(&edge, Some(version(2, false)));

        let refused = edge.add_line(server(), table(0), a_pizza()).await;
        assert!(refused_for_guests(&refused), "{refused:?}");
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        edge.add_line(server(), table(1), a_pizza())
            .await
            .expect("opened under a version with no guard");

        let query = EventQuery::first(store_id(), NonZeroU32::new(200).expect("a positive limit"));
        let opened: Vec<Option<PackageVersionId>> = store
            .read(&query)
            .await
            .expect("read the log")
            .into_iter()
            .filter(|envelope| envelope.event_type.as_str() == "sales.order.opened")
            .map(|envelope| {
                let event: SalesOrderOpened = envelope.data.decode().expect("decodes");
                event.package_version_id
            })
            .collect();
        assert_eq!(opened, [Some(version_id(1)), Some(version_id(2))]);
    });
}

/// An order opened under the default package has no guards, whatever is published after.
#[test]
fn an_order_opened_under_the_default_package_has_no_guards() {
    pos_fakes::executor::run_ready(async {
        let edge = edge(None);
        edge.seat_table(server(), table(0), None)
            .await
            .expect("seats");
        publish(&edge, Some(version(1, true)));
        edge.add_line(server(), table(0), a_pizza())
            .await
            .expect("the order runs under the default package");
    });
}

/// The edge holds four versions. Seating under a fifth lets the oldest go, and the order that
/// opened under it is then checked under the version the store runs now; an order whose version is
/// still held keeps it.
#[test]
fn an_order_whose_version_was_let_go_is_checked_under_the_current_one() {
    pos_fakes::executor::run_ready(async {
        let edge = edge(Some(version(1, false)));
        for n in 1..=5 {
            publish(&edge, Some(version(n, false)));
            edge.seat_table(server(), table(n), None)
                .await
                .expect("seats");
        }
        publish(&edge, Some(version(6, true)));

        let refused = edge.add_line(server(), table(1), a_pizza()).await;
        assert!(refused_for_guests(&refused), "{refused:?}");
        edge.add_line(server(), table(5), a_pizza())
            .await
            .expect("version 5 is still held, and it has no guard");
    });
}

/// A pizza as a counter order's line names it: the store's menu prices it after the guard.
fn a_choice() -> OrderLineChoice {
    OrderLineChoice {
        menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
        quantity: Quantity::ONE,
        modifier_menu_item_ids: Vec::new(),
        seat: None,
        course_id: None,
        note_present: false,
        typed_unit_price: None,
        weight_source: None,
    }
}

/// Whether a counter line got past the flow's guard: this store's empty menu then refuses the
/// item, which it checks only after the guard.
fn passed_the_guard(result: &Result<impl std::fmt::Debug, AppError>) -> bool {
    matches!(result, Err(AppError::ItemNotSellable))
}

/// [`guest_count_first`] with its guard scope set to `scope`, and `channels` where it names some.
fn scoped(scope: &str, channels: &[&str]) -> PackageContent {
    let mut content = guest_count_first();
    let flow = content
        .flows
        .first_mut()
        .map(|variant| &mut variant.body)
        .expect("a flow");
    flow.guard_scope = serde_json::from_value(serde_json::json!(scope)).expect("a scope");
    flow.guard_channels = channels
        .iter()
        .map(|token| serde_json::from_value(serde_json::json!(token)).expect("a channel"))
        .collect();
    content
}

/// The phase 10 defect: a dine-in flow that asks the guest count first refused every counter order,
/// which no seat opened. By default it governs only the orders a seat opened (ADR-0244), and each
/// `sales.order.opened` names the act that opened it.
#[test]
fn a_flow_governs_only_the_orders_its_start_act_opened_by_default() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), Some(guest_count_first()));
        let counter = edge
            .open_counter_order(server(), SalesChannel::DineIn)
            .await
            .expect("opens");
        let passed = edge
            .add_line_to_order(server(), counter.order_id, a_choice())
            .await;
        assert!(passed_the_guard(&passed), "{passed:?}");
        edge.seat_table(server(), table(0), None)
            .await
            .expect("seats");
        let refused = edge.add_line(server(), table(0), a_pizza()).await;
        assert!(refused_for_guests(&refused), "{refused:?}");

        let query = EventQuery::first(store_id(), NonZeroU32::new(200).expect("a positive limit"));
        let opened_by: Vec<Option<String>> = store
            .read(&query)
            .await
            .expect("read the log")
            .into_iter()
            .filter(|envelope| envelope.event_type.as_str() == "sales.order.opened")
            .map(|envelope| {
                let event: SalesOrderOpened = envelope.data.decode().expect("decodes");
                event.opened_by_action_id.map(|act| act.as_str().to_owned())
            })
            .collect();
        assert_eq!(
            opened_by,
            [
                Some("sales.order.open".to_owned()),
                Some("sales.table.seat".to_owned())
            ]
        );
    });
}

/// Under `ALL_ORDERS` the flow's guard refuses a counter order too, and under `CHANNELS` it
/// refuses an order on a channel it names and lets another through.
#[test]
fn a_flow_scoped_wider_guards_the_orders_it_names() {
    pos_fakes::executor::run_ready(async {
        let all = edge(Some(scoped("FLOW_GUARD_SCOPE_ALL_ORDERS", &[])));
        let counter = all
            .open_counter_order(server(), SalesChannel::DineIn)
            .await
            .expect("opens");
        let refused = all
            .add_line_to_order(server(), counter.order_id, a_choice())
            .await;
        assert!(refused_for_guests(&refused), "{refused:?}");

        let edge = edge(Some(scoped(
            "FLOW_GUARD_SCOPE_CHANNELS",
            &["SALES_CHANNEL_DINE_IN"],
        )));
        let dine_in = edge
            .open_counter_order(server(), SalesChannel::DineIn)
            .await
            .expect("opens");
        let refused = edge
            .add_line_to_order(server(), dine_in.order_id, a_choice())
            .await;
        assert!(refused_for_guests(&refused), "{refused:?}");
        let takeaway = edge
            .open_counter_order(server(), SalesChannel::Takeaway)
            .await
            .expect("opens");
        let passed = edge
            .add_line_to_order(server(), takeaway.order_id, a_choice())
            .await;
        assert!(passed_the_guard(&passed), "{passed:?}");
    });
}
