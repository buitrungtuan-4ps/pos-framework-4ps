// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A paid order without a table stays on the kitchen board until the kitchen is done, and its note
//! stays with it
//! ([ADR-0161](../../../docs/adr/0161-a-paid-order-without-a-table-stays-on-the-kitchen-board-until-it-is-done.md)).
//!
//! A counter order is paid before it is cooked. The live read drops an order the moment it is
//! paid, so the kitchen read is the one that must keep it, with its note, until a station bumps
//! it. A table keeps today's rule: its ticket leaves when its bill settles.

use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_edge::line_notes::NoteText;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, OrderId, OrderLineId, StationId, StoreId, TableId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{CurrencyCode, Open, PaymentMethod, SalesChannel};

const ALLERGY: &str = "no peanuts";

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn station() -> StationId {
    StationId::new(Ulid::from_u128(900))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn allergy() -> Option<NoteText> {
    NoteText::parse(Some(ALLERGY)).expect("a valid note")
}

/// Dine-in and takeaway both taxed at 10%, so either kind of order can be billed.
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    EdgeSession::bootstrap().with_tax_rates(
        TaxRateTable::new()
            .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
            .with(class, SalesChannel::Takeaway, TaxRate::from_percent(10)),
    )
}

fn edge_over(store: FakeStore) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(store_id()),
        session(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn a_priced_line() -> pos_core::menu::PricedLine {
    pos_core::menu::PricedLine {
        menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        modifier_menu_item_ids: Vec::new(),
        modifier_display_names: Vec::new(),
        repriced: false,
    }
}

/// A takeaway order with no table and one noted line, as a counter or a delivery brings one.
async fn a_counter_order(edge: &Edge<FakeStore>) -> (OrderId, OrderLineId) {
    let opened = edge
        .open_inbound_order(
            server().device_id,
            Open::from_known(SalesChannel::Takeaway),
            None,
            &[(a_priced_line(), allergy())],
            None,
        )
        .await
        .expect("a counter order opens");
    let line = *edge
        .order_line_ids(opened.order_id)
        .first()
        .expect("its line");
    (opened.order_id, line)
}

/// Bills the whole order and pays it in cash, exactly.
async fn pay(edge: &Edge<FakeStore>, order_id: OrderId) {
    let bill = edge
        .open_bill_for_order(server(), order_id)
        .await
        .expect("bills");
    let due = edge.order_totals(order_id).expect("totals").total_due;
    edge.settle_bill(
        server(),
        bill.bill_id,
        vec![Payment {
            method: PaymentMethod::Cash,
            tendered: due,
            applied_to_bill: due,
            tip: vnd(0),
        }],
        None,
    )
    .await
    .expect("settles");
}

/// Whether the kitchen read shows the line with this id.
fn on_the_board(edge: &Edge<FakeStore>, line: OrderLineId) -> bool {
    edge.kitchen_orders()
        .into_iter()
        .flat_map(|order| order.lines)
        .any(|kitchen| kitchen.order_line_id == line)
}

/// The note the kitchen read shows under the line with this id, if it shows the line and a note.
fn note_on_the_board(edge: &Edge<FakeStore>, line: OrderLineId) -> Option<String> {
    edge.kitchen_orders()
        .into_iter()
        .flat_map(|order| order.lines)
        .find(|kitchen| kitchen.order_line_id == line)
        .and_then(|kitchen| kitchen.note.map(|note| note.as_str().to_owned()))
}

/// Paid before the kitchen made it: the order leaves the live read, stays on the kitchen board
/// with its note, and leaves both only when a station bumps it.
#[test]
fn a_paid_counter_order_waits_on_the_board_until_it_is_bumped() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (order_id, line) = a_counter_order(&edge).await;
        edge.fire_line(server(), line, Some(station()))
            .await
            .expect("fires");
        pay(&edge, order_id).await;

        assert!(
            edge.live_orders().is_empty(),
            "paid, so it owes nothing and leaves the live read"
        );
        assert!(
            on_the_board(&edge, line),
            "the kitchen still has it to make"
        );
        assert_eq!(
            note_on_the_board(&edge, line),
            Some(ALLERGY.to_owned()),
            "and the note with it"
        );

        edge.bump_ticket(server(), order_id, station(), vec![line])
            .await
            .expect("the kitchen is done");
        assert!(!on_the_board(&edge, line));
        assert_eq!(edge.held_note_count(), 0, "made, so the note is forgotten");
    });
}

/// A table keeps today's rule: its food reaches the guests before the bill does, so a paid table
/// leaves the board whether or not the kitchen bumped.
#[test]
fn a_paid_table_leaves_the_board_as_it_always_has() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let table = TableId::new(Ulid::from_u128(800));
        edge.seat_table(server(), table, None).await.expect("seats");
        let line = edge
            .add_noted_line(
                server(),
                table,
                LineDraft {
                    menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
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
                },
                allergy(),
            )
            .await
            .expect("adds");
        edge.fire_line(server(), line.order_line_id, Some(station()))
            .await
            .expect("fires");
        pay(&edge, line.order_id).await;

        assert!(!on_the_board(&edge, line.order_line_id));
        assert_eq!(edge.held_note_count(), 0);
    });
}

/// A table's order stays a table's order after its table is seated again. The table points at its
/// new guests' order then, and a cook bumping the old order's last ticket late must not bring the
/// old order back to the board as though it had come from the counter.
#[test]
fn a_table_s_order_stays_off_the_board_once_its_table_is_seated_again() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let table = TableId::new(Ulid::from_u128(800));
        let other_station = StationId::new(Ulid::from_u128(901));
        edge.seat_table(server(), table, None).await.expect("seats");
        let draft = || LineDraft {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
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
        };
        let pizza = edge.add_line(server(), table, draft()).await.expect("adds");
        let salad = edge.add_line(server(), table, draft()).await.expect("adds");
        edge.fire_line(server(), pizza.order_line_id, Some(station()))
            .await
            .expect("fires");
        edge.fire_line(server(), salad.order_line_id, Some(other_station))
            .await
            .expect("fires");
        pay(&edge, pizza.order_id).await;
        edge.clean_table(server(), table).await.expect("cleans");
        edge.seat_table(server(), table, None)
            .await
            .expect("the next guests sit down");

        edge.bump_ticket(
            server(),
            pizza.order_id,
            station(),
            vec![pizza.order_line_id],
        )
        .await
        .expect("a cook bumps the old ticket late");
        assert!(
            !on_the_board(&edge, salad.order_line_id),
            "the old table's other dish is not back on the board"
        );
    });
}

/// A dish paid for and never sent to the kitchen is not waiting there.
#[test]
fn an_unfired_line_on_a_paid_order_is_not_on_the_board() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (order_id, line) = a_counter_order(&edge).await;
        pay(&edge, order_id).await;

        assert!(!on_the_board(&edge, line));
        assert_eq!(edge.held_note_count(), 0);
    });
}

/// The waiting set is folded from the log like the rest of the projection, so a store that
/// restarts with food still to make puts the ticket back on the board. The note's text does not
/// survive a restart (ADR-0157), and the line still says one was written.
#[test]
fn a_restart_puts_a_paid_order_back_on_the_board() {
    run_ready(async {
        let store = FakeStore::default();
        let line = {
            let edge = edge_over(store.clone());
            let (order_id, line) = a_counter_order(&edge).await;
            edge.fire_line(server(), line, Some(station()))
                .await
                .expect("fires");
            pay(&edge, order_id).await;
            line
        };

        let edge = edge_over(store);
        edge.rebuild().await.expect("rebuilds from the log");
        let waiting = edge
            .kitchen_orders()
            .into_iter()
            .flat_map(|order| order.lines)
            .find(|kitchen| kitchen.order_line_id == line)
            .expect("the ticket is back");
        assert!(waiting.note_present);
        assert_eq!(waiting.note, None, "the text went with the restart");
    });
}
