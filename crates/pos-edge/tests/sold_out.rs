// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Staff marking an item sold out at the till (86), and bringing it back.
//!
//! `inventory.item.sold_out` and `inventory.item.restored` have been in the schema since it was
//! written, with `sales.item.mark_unavailable` in the permission catalogue beside them, and nothing
//! emitted either: an item that ran out mid-service stayed on every till until somebody changed the
//! menu in the console. These cases hold what the till now relies on — the edge refuses a line for a
//! sold-out item whatever a device still showing it sends, and whichever way the order arrives; a
//! sold-out modifier refuses the line it is on; a second tap writes nothing; the right permission is
//! required; and a store that restarts still knows what ran out.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_edge::{
    AppError, Edge, EdgeOrderIn, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, LineDraft,
    StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::order_in::{ExternalReference, InboundOrder, InboundOrderLine, OrderIn};
use pos_proto::SalesChannel;
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

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

fn cheese() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(510))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

/// A pizza, and the extra cheese that goes on one. The pizza attaches no modifier group, so the edge
/// takes the cheese as a modifier without one being published (`check_modifiers`).
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new()
        .with(MenuEntry::new(
            pizza(),
            DisplayName::new("Margherita"),
            vnd(150_000),
            class,
        ))
        .with(MenuEntry::new(
            cheese(),
            DisplayName::new("Extra cheese"),
            vnd(25_000),
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

/// **A sold-out item cannot be ordered**, and once restored it can.
///
/// The till greys the item out on every device, but a device that missed the event, or a caller
/// that is not a till at all, still sends the line — so the refusal is the edge's.
#[test]
fn a_sold_out_item_is_refused_until_it_is_restored() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        edge.seat_table(server(), table(), None)
            .await
            .expect("seats");

        edge.mark_item_sold_out(server(), pizza())
            .await
            .expect("staff mark it sold out");
        assert!(edge.item_sold_out(pizza()));
        let refused = edge.add_line(server(), table(), a_pizza()).await;
        assert!(
            matches!(refused, Err(AppError::ItemNotSellable)),
            "a sold-out item is not sellable, got {refused:?}"
        );

        edge.restore_item(server(), pizza())
            .await
            .expect("and bring it back");
        assert!(!edge.item_sold_out(pizza()));
        edge.add_line(server(), table(), a_pizza())
            .await
            .expect("a restored item sells again");
    });
}

/// A second tap writes nothing: a device marking an item another has already marked records no
/// second decision, and restoring an item nobody marked records none.
#[test]
fn marking_twice_records_one_decision() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), session());
        edge.restore_item(server(), pizza())
            .await
            .expect("restoring an item nobody marked is not an error");
        edge.mark_item_sold_out(server(), pizza())
            .await
            .expect("marks");
        edge.mark_item_sold_out(server(), pizza())
            .await
            .expect("and again");
        edge.restore_item(server(), pizza())
            .await
            .expect("restores");

        let events = logged(&store).await;
        assert_eq!(
            events,
            vec!["inventory.item.sold_out", "inventory.item.restored"],
            "one of each, not four"
        );
    });
}

/// A store that restarts still knows what ran out, and what came back.
///
/// Every write comes from the one edge, and the restarted one only reads. Two edges built in the
/// same millisecond mint ids in no particular order, so a log written by both can replay a restore
/// ahead of the mark it undid; a real restart takes longer than that.
#[test]
fn a_sold_out_item_stays_sold_out_across_a_restart() {
    run_ready(async {
        let store = FakeStore::default();
        {
            let edge = edge_over(store.clone(), session());
            edge.mark_item_sold_out(server(), pizza())
                .await
                .expect("the pizza runs out");
            edge.mark_item_sold_out(server(), cheese())
                .await
                .expect("so does the cheese");
            edge.restore_item(server(), cheese())
                .await
                .expect("and the cheese comes back");
        }
        let rebuilt = edge_over(store, session());
        rebuilt.rebuild().await.expect("replays the log");
        assert!(rebuilt.item_sold_out(pizza()), "the 86 survives");
        assert!(!rebuilt.item_sold_out(cheese()), "and so does the restore");
    });
}

/// Without `sales.item.mark_unavailable` the act is refused, and an item the store does not sell
/// cannot be marked at all.
#[test]
fn marking_needs_the_permission_and_a_real_item() {
    run_ready(async {
        let mut without = session();
        without.granted = Permission::ALL
            .iter()
            .copied()
            .filter(|permission| *permission != Permission::MarkItemUnavailable)
            .collect();
        let edge = edge_over(FakeStore::default(), without);
        let refused = edge.mark_item_sold_out(server(), pizza()).await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied { .. }))
            ),
            "got {refused:?}"
        );
        assert!(!edge.item_sold_out(pizza()));

        let edge = edge_over(FakeStore::default(), session());
        let unknown = edge
            .mark_item_sold_out(server(), MenuItemId::new(Ulid::from_u128(999)))
            .await;
        assert!(
            matches!(unknown, Err(AppError::ItemNotSellable)),
            "got {unknown:?}"
        );
    });
}

/// **A sold-out modifier refuses the line it is on.** A modifier is an item in the price book, so
/// staff can mark the extra cheese sold out, and a pizza that asks for it is then a promise the
/// kitchen cannot keep. The same pizza without it still sells.
#[test]
fn a_sold_out_modifier_refuses_the_line_it_is_on() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session());
        edge.seat_table(server(), table(), None)
            .await
            .expect("seats");
        edge.mark_item_sold_out(server(), cheese())
            .await
            .expect("the cheese runs out");

        let with_cheese = LineDraft {
            modifier_menu_item_ids: vec![cheese()],
            unit_price: vnd(175_000),
            line_total: vnd(175_000),
            ..a_pizza()
        };
        let refused = edge.add_line(server(), table(), with_cheese).await;
        assert!(
            matches!(refused, Err(AppError::ItemNotSellable)),
            "a line asking for a sold-out modifier is not sellable, got {refused:?}"
        );
        edge.add_line(server(), table(), a_pizza())
            .await
            .expect("the pizza itself still sells");
    });
}

/// **An order from outside the store is refused too** — a guest's QR order or a marketplace's. The
/// menu it was chosen from was published before the kitchen ran out, so the edge refuses it the way
/// it refuses an item the console withdrew (`failed_precondition`), and opens nothing. Once the item
/// is back the same order goes through: the refusal left nothing behind to trip over.
#[test]
fn an_order_from_outside_the_store_naming_a_sold_out_item_is_refused() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = Arc::new(edge_over(store.clone(), session()));
        let intake = EdgeOrderIn::new(
            Arc::clone(&edge),
            InMemoryQueueNumbers::new(),
            DeviceId::new(Ulid::from_u128(20)),
        );
        let order = InboundOrder {
            external_reference: ExternalReference::parse("QR-86").expect("a valid reference"),
            sales_channel: Open::from_known(SalesChannel::Qr),
            store_id: StoreId::new(Ulid::from_u128(1)),
            table_id: None,
            subject_id: None,
            lines: vec![InboundOrderLine {
                menu_item_id: pizza(),
                quantity: Quantity::ONE,
                modifier_menu_item_ids: Vec::new(),
                quoted_unit_price: None,
                note: None,
            }],
            placed_at: pos_contract_tests::fixtures::instant(),
        };
        edge.mark_item_sold_out(server(), pizza())
            .await
            .expect("marks");

        let refused = intake
            .submit(&order)
            .await
            .expect_err("a sold-out item is refused");
        assert_eq!(refused.status(), ErrorStatus::FailedPrecondition);
        assert_eq!(
            logged(&store).await,
            vec!["inventory.item.sold_out"],
            "no order was opened"
        );

        edge.restore_item(server(), pizza())
            .await
            .expect("restores");
        let accepted = intake
            .submit(&order)
            .await
            .expect("the same order goes through once the item is back");
        assert!(accepted.created, "and it is a new order, not a replay");
    });
}
