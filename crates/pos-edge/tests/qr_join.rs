// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A guest's QR order that joins the table's order
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! item 2, the `qr.table_order` setting).
//!
//! With `TABLE_ORDER_JOIN` set, the guest's lines are the table's order's: one order, one bill, one
//! send to the kitchen. The guest's order is still opened in the log and then folded into the
//! table's (`sales.table.merged`), at once when nobody has to confirm it and when staff confirm it
//! otherwise, so each line keeps the order it was ordered on. Where the table's order cannot take
//! a line — its bill is open, split or paid — the guest's order is its own, as it always was, and
//! `TABLE_ORDER_SEPARATE`, the default, changes nothing.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_core::menu::PricedLine;
use pos_edge::line_notes::NoteText;
use pos_edge::{
    AppError, Edge, EdgeOrderIn, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts,
    InboundOrderOpened, LineDraft, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::order_in::{ExternalReference, InboundOrder, InboundOrderLine, OrderIn};
use pos_proto::events::SalesTableMerged;
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, OrderId, OrderLineId, ReasonCodeId, StationId, StoreId,
    TableId, TaxClassId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::qr::{PublishedQr, TableOrder};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{PublishedReasonCodes, ReasonAction};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, Open, OrderLineState, PaymentMethod, SalesChannel, TableState};

fn waiter() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn pizza() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

/// The fallback station: the bootstrap session publishes no station plan.
fn station() -> StationId {
    StationId::new(Ulid::from_u128(900))
}

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

/// What the guest page quotes and the intake reprices to: the `QR` book's price, at the `QR`
/// channel's rate. Both differ from the dining room's here, so a test can tell which one a bill
/// used.
const GUEST_PRICE: i64 = 160_000;
const WAITER_PRICE: i64 = 150_000;

/// A guest's line as the intake hands it to `open_inbound_order`, priced from the `QR` book.
fn a_guest_line() -> PricedLine {
    PricedLine {
        menu_item_id: pizza(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(GUEST_PRICE),
        line_total: vnd(GUEST_PRICE),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(800).expect("a valid rate"),
        modifier_menu_item_ids: Vec::new(),
        modifier_display_names: Vec::new(),
        repriced: false,
    }
}

/// A waiter's line, priced from the dining room's book.
fn a_waiters_line() -> LineDraft {
    LineDraft {
        menu_item_id: pizza(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(WAITER_PRICE),
        line_total: vnd(WAITER_PRICE),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// A store that sets `qr.table_order` to `table_order` and the staff hold to `hold`, with the
/// dining room taxed at 10% and the `QR` channel at 8%.
fn session(table_order: TableOrder, hold: bool) -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        vnd(WAITER_PRICE),
        class(),
    ));
    let rates = TaxRateTable::new()
        .with(class(), SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class(), SalesChannel::Qr, TaxRate::from_percent(8));
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.qr = PublishedQr {
        table_order: Open::from_known(table_order),
        ..PublishedQr::default()
    };
    session.qr_staff_confirmation_required = hold;
    session
}

/// Joins, and nobody has to confirm a guest's order.
fn joins_at_once() -> EdgeSession {
    session(TableOrder::Join, false)
}

/// Joins once a member of staff confirms the guest's order.
fn joins_on_confirmation() -> EdgeSession {
    session(TableOrder::Join, true)
}

fn edge_over(store: FakeStore, session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(store_id()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

/// A waiter seats the table and rings one line: the table's order, and that line.
async fn a_waiters_order(edge: &Edge<FakeStore>) -> (OrderId, OrderLineId) {
    edge.seat_table(waiter(), table(), None)
        .await
        .expect("a waiter seats the table");
    let line = edge
        .add_line(waiter(), table(), a_waiters_line())
        .await
        .expect("and rings a line");
    (line.order_id, line.order_line_id)
}

/// A guest's QR order for the table, with one line.
async fn a_guest_order(edge: &Edge<FakeStore>, note: Option<NoteText>) -> InboundOrderOpened {
    edge.open_inbound_order(
        waiter().device_id,
        Open::from_known(SalesChannel::Qr),
        Some(table()),
        &[(a_guest_line(), note)],
        None,
    )
    .await
    .expect("a guest's order is taken")
}

/// The one line on the guest's order, read before it joins anything.
fn the_only_line(edge: &Edge<FakeStore>, order_id: OrderId) -> OrderLineId {
    let lines = edge.order_line_ids(order_id);
    assert_eq!(lines.len(), 1, "the guest's order has its one line");
    lines[0]
}

async fn logged_types(store: &FakeStore) -> Vec<String> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(256).expect("a positive limit"));
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

async fn merges(store: &FakeStore) -> Vec<SalesTableMerged> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(256).expect("a positive limit"));
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "sales.table.merged")
        .map(|envelope| envelope.data.decode().expect("a merge decodes"))
        .collect()
}

/// The table has one live order, `order_id`, holding exactly `lines`.
fn one_order_holding(edge: &Edge<FakeStore>, order_id: OrderId, lines: &[OrderLineId], when: &str) {
    let live = edge.live_orders();
    assert_eq!(
        live.len(),
        1,
        "{when}: one order at the table, got {live:?}"
    );
    assert_eq!(live[0].order_id, order_id, "{when}: the table's own order");
    assert_eq!(live[0].table_id, Some(table()), "{when}");
    let mut on_it: Vec<OrderLineId> = live[0]
        .lines
        .iter()
        .map(|line| line.order_line_id)
        .collect();
    on_it.sort_unstable();
    let mut expected = lines.to_vec();
    expected.sort_unstable();
    assert_eq!(
        on_it, expected,
        "{when}: every line is on the table's order"
    );
    assert_eq!(edge.order_for_table(table()), Some(order_id), "{when}");
    assert_eq!(edge.table_state(table()), TableState::Occupied, "{when}");
}

#[test]
fn a_guest_order_that_waits_for_nobody_joins_the_table_s_order_as_it_arrives() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_at_once());
        let (tables, waiters_line) = a_waiters_order(&edge).await;

        let guests = a_guest_order(&edge, None).await;
        assert_ne!(
            guests.order_id, tables,
            "the guest's submission is an order of its own in the log, for its retries to find"
        );
        assert!(!guests.awaiting_staff_confirmation);
        let guests_line = edge
            .live_orders()
            .into_iter()
            .flat_map(|order| order.lines)
            .map(|line| line.order_line_id)
            .find(|line| *line != waiters_line)
            .expect("the guest's line is on the floor");

        one_order_holding(&edge, tables, &[waiters_line, guests_line], "live");
        assert!(edge.order_line_ids(guests.order_id).is_empty());

        // The log says what happened, in the order it happened: the guest's order, its line, and
        // the order the line moved to.
        let types = logged_types(&store).await;
        assert_eq!(
            &types[types.len() - 3..],
            [
                "sales.order.opened",
                "sales.order_line.added",
                "sales.table.merged"
            ],
        );
        let merged = merges(&store).await;
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].target_order_id, tables);
        assert_eq!(merged[0].merged_order_id, guests.order_id);

        // A rebuild over the same projection lands on the same table, with every line's record on
        // the order its index puts it on.
        edge.rebuild().await.expect("rebuilds over itself");
        one_order_holding(&edge, tables, &[waiters_line, guests_line], "rebuilt");

        // One send takes both lines to the kitchen, as for any line added to the order, and each
        // ticket names the table's order.
        let fired = edge
            .fire_order(waiter(), tables, Some(station()))
            .await
            .expect("the table's order is sent");
        assert_eq!(fired.len(), 2, "the waiter's line and the guest's");
        assert!(fired.iter().all(|line| line.order_id == tables));
        // A till still showing the guest's order finds nothing left on it to send.
        let stale = edge
            .fire_order(waiter(), guests.order_id, Some(station()))
            .await
            .expect("an order with nothing unsent answers with nothing");
        assert!(stale.is_empty());

        // And a restart from the log lands on the same table.
        let restarted = edge_over(store, joins_at_once());
        restarted.rebuild().await.expect("rebuilds from the log");
        one_order_holding(
            &restarted,
            tables,
            &[waiters_line, guests_line],
            "after a restart",
        );
        assert!(
            restarted.live_orders()[0]
                .lines
                .iter()
                .all(|line| line.state == OrderLineState::Fired)
        );
    });
}

#[test]
fn a_held_guest_order_joins_the_table_s_order_when_staff_confirm_it() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_on_confirmation());
        let (tables, waiters_line) = a_waiters_order(&edge).await;

        let guests = a_guest_order(&edge, None).await;
        assert!(guests.awaiting_staff_confirmation);
        let guests_line = the_only_line(&edge, guests.order_id);

        // While it waits it is its own order, which the kitchen cannot see, and the waiter's order
        // goes on being served.
        assert_eq!(edge.live_orders().len(), 2);
        assert!(edge.awaits_staff_confirmation(guests.order_id));
        let sent = edge
            .fire_order(waiter(), tables, Some(station()))
            .await
            .expect("the waiter's order is sent");
        assert_eq!(sent.len(), 1, "only the waiter's line");
        assert!(matches!(
            edge.fire_order(waiter(), guests.order_id, Some(station()))
                .await,
            Err(AppError::AwaitingStaffConfirmation)
        ));

        edge.confirm_inbound_order(waiter(), guests.order_id)
            .await
            .expect("staff confirm the guest's order");

        one_order_holding(&edge, tables, &[waiters_line, guests_line], "confirmed");
        assert!(edge.orders_awaiting_staff_confirmation().is_empty());
        let merged = merges(&store).await;
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].target_order_id, tables);
        assert_eq!(merged[0].merged_order_id, guests.order_id);

        // The guest's line goes with the table's next send, like a line a waiter added.
        let sent = edge
            .fire_order(waiter(), tables, Some(station()))
            .await
            .expect("the table's order is sent again");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].order_line_id, guests_line);

        let restarted = edge_over(store, joins_on_confirmation());
        restarted.rebuild().await.expect("rebuilds from the log");
        one_order_holding(
            &restarted,
            tables,
            &[waiters_line, guests_line],
            "after a restart",
        );
        assert!(!restarted.awaits_staff_confirmation(guests.order_id));
    });
}

#[test]
fn refusing_a_held_guest_order_leaves_the_table_s_order_as_it_was() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_on_confirmation());
        let (tables, waiters_line) = a_waiters_order(&edge).await;
        let guests = a_guest_order(&edge, None).await;
        let reason: ReasonCodeId = PublishedReasonCodes::framework_default()
            .for_action(ReasonAction::RejectOrder)
            .next()
            .expect("a reason for refusing an order")
            .id;

        edge.reject_inbound_order(waiter(), guests.order_id, reason)
            .await
            .expect("staff refuse the guest's order");

        one_order_holding(&edge, tables, &[waiters_line], "refused");
        assert!(merges(&store).await.is_empty(), "nothing joined");
    });
}

#[test]
fn a_guest_order_at_a_table_nobody_seated_opens_one_and_the_next_joins_it() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_at_once());

        let first = a_guest_order(&edge, None).await;
        let first_line = the_only_line(&edge, first.order_id);
        one_order_holding(&edge, first.order_id, &[first_line], "the first guest");
        assert!(merges(&store).await.is_empty(), "there was nothing to join");

        let second = a_guest_order(&edge, None).await;
        let lines = edge.order_line_ids(first.order_id);
        assert_eq!(
            lines.len(),
            2,
            "the second guest's line joined the first's order"
        );
        one_order_holding(&edge, first.order_id, &lines, "the second guest");
        assert_eq!(merges(&store).await[0].merged_order_id, second.order_id);
    });
}

#[test]
fn the_held_orders_of_one_table_end_up_as_one_order() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_on_confirmation());

        // Two guests at a table nobody seated, both waiting. Confirming the first leaves it the
        // table's order, since there is nothing to join while the second still waits; confirming
        // the second joins it to the first.
        let first = a_guest_order(&edge, None).await;
        let second = a_guest_order(&edge, None).await;
        let lines = [
            the_only_line(&edge, first.order_id),
            the_only_line(&edge, second.order_id),
        ];
        edge.confirm_inbound_order(waiter(), first.order_id)
            .await
            .expect("the first is confirmed");
        assert_eq!(edge.live_orders().len(), 2);
        edge.confirm_inbound_order(waiter(), second.order_id)
            .await
            .expect("the second is confirmed");
        one_order_holding(&edge, first.order_id, &lines, "both confirmed");
    });
}

#[test]
fn a_guest_order_is_its_own_where_the_table_s_bill_is_open() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_at_once());
        let (tables, _) = a_waiters_order(&edge).await;
        edge.open_bill(waiter(), table())
            .await
            .expect("the table asks for the bill");
        // GĐ0 0.1: the bill covers the lines it opened on, so the floor takes no new one.
        assert!(matches!(
            edge.add_line(waiter(), table(), a_waiters_line()).await,
            Err(AppError::BillAlreadyOpen)
        ));

        let guests = a_guest_order(&edge, None).await;

        assert_eq!(
            edge.order_line_ids(tables).len(),
            1,
            "the bill's order takes no line"
        );
        assert_eq!(edge.order_line_ids(guests.order_id).len(), 1);
        assert_eq!(
            edge.live_orders().len(),
            2,
            "two orders, as before the setting"
        );
        assert!(merges(&store).await.is_empty());
    });
}

#[test]
fn a_held_guest_order_stays_its_own_when_the_table_s_bill_opens_before_it_is_confirmed() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_on_confirmation());
        let (tables, _) = a_waiters_order(&edge).await;
        let guests = a_guest_order(&edge, None).await;
        // The waiter's order has the bill opened on it while the guest's still waits. The table's
        // bill names the order it opened on, and the guest's order holds the table, so this asks
        // for the waiter's bill by its order.
        edge.open_bill_for_order(waiter(), tables)
            .await
            .expect("the waiter's order asks for its bill");

        edge.confirm_inbound_order(waiter(), guests.order_id)
            .await
            .expect("staff confirm the guest's order");

        assert_eq!(edge.order_line_ids(tables).len(), 1);
        assert_eq!(edge.order_line_ids(guests.order_id).len(), 1);
        assert!(merges(&store).await.is_empty());
    });
}

#[test]
fn a_guest_order_is_its_own_at_a_table_whose_bill_was_split() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_at_once());
        let (tables, first) = a_waiters_order(&edge).await;
        let second = edge
            .add_line(waiter(), table(), a_waiters_line())
            .await
            .expect("a second line")
            .order_line_id;
        let bill = edge
            .open_bill(waiter(), table())
            .await
            .expect("the table asks for the bill");
        let parts = edge
            .split_bill(waiter(), bill.bill_id, vec![vec![first], vec![second]])
            .await
            .expect("the bill splits in two");
        assert_eq!(parts.len(), 2);

        let guests = a_guest_order(&edge, None).await;

        assert_eq!(edge.order_line_ids(tables).len(), 2);
        assert_eq!(edge.order_line_ids(guests.order_id).len(), 1);
        assert!(merges(&store).await.is_empty());
    });
}

#[test]
fn a_guest_order_after_the_table_has_paid_opens_an_order_of_its_own() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), joins_at_once());
        let (tables, _) = a_waiters_order(&edge).await;
        let bill = edge
            .open_bill(waiter(), table())
            .await
            .expect("the table asks for the bill");
        let due = edge.check_totals(table()).expect("the check").total_due;
        let settled = edge
            .settle_bill(
                waiter(),
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
            .expect("the table pays");
        assert_eq!(settled.state, BillState::Settled);

        let guests = a_guest_order(&edge, None).await;

        assert_eq!(
            edge.order_line_ids(tables).len(),
            1,
            "a paid order takes no line"
        );
        let line = the_only_line(&edge, guests.order_id);
        one_order_holding(&edge, guests.order_id, &[line], "after the table paid");
        assert!(merges(&store).await.is_empty());
    });
}

#[test]
fn separate_the_default_keeps_a_guest_order_its_own() {
    run_ready(async {
        let store = FakeStore::default();
        // A store that sets nothing: no `qr.table_order`, and the hold off so nothing waits.
        let mut session = joins_at_once();
        session.qr = PublishedQr::default();
        let edge = edge_over(store.clone(), session);
        let (tables, _) = a_waiters_order(&edge).await;

        let guests = a_guest_order(&edge, None).await;

        assert_eq!(edge.order_line_ids(tables).len(), 1);
        assert_eq!(edge.order_line_ids(guests.order_id).len(), 1);
        assert_eq!(edge.live_orders().len(), 2);
        assert_eq!(
            edge.order_for_table(table()),
            Some(guests.order_id),
            "the guest's order takes the table over, as it did before the setting"
        );
        assert!(merges(&store).await.is_empty());
    });
}

#[test]
fn a_joined_line_keeps_the_guest_s_price_and_is_taxed_as_the_table_s_order_is() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), joins_at_once());
        a_waiters_order(&edge).await;
        a_guest_order(&edge, None).await;

        let check = edge.check_totals(table()).expect("the table's check");
        assert_eq!(
            check.subtotal,
            vnd(WAITER_PRICE + GUEST_PRICE),
            "the guest pays the price the guest page quoted"
        );
        // The table's order is the dining room's, and a bill taxes its order's channel: 10% of
        // 310,000, where the guest's own order would have been taxed at the QR channel's 8%.
        assert_eq!(check.tax_total, vnd(31_000));
        assert_eq!(check.total_due, vnd(341_000));
    });
}

#[test]
fn a_joined_line_keeps_its_note() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), joins_at_once());
        let (tables, waiters_line) = a_waiters_order(&edge).await;
        let note = NoteText::lenient("no onions");
        a_guest_order(&edge, note.clone()).await;
        let guests_line = edge
            .order_line_ids(tables)
            .into_iter()
            .find(|line| *line != waiters_line)
            .expect("the guest's line joined");

        // Reading the kitchen's orders sweeps the notes of lines no screen shows any more. The
        // guest's line is on a screen, on the table's order, and not on the guest's, which ended.
        assert!(note.is_some());
        assert_eq!(edge.kitchen_orders().len(), 1);
        assert_eq!(edge.line_note(guests_line), note);
    });
}

#[test]
fn turning_the_hold_on_later_does_not_hold_an_order_that_has_joined() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), joins_at_once());
        let (tables, _) = a_waiters_order(&edge).await;
        let guests = a_guest_order(&edge, None).await;

        edge.apply_session(joins_on_confirmation());

        assert!(
            edge.orders_awaiting_staff_confirmation().is_empty(),
            "the joined order has nothing on it to confirm"
        );
        assert!(!edge.awaits_staff_confirmation(guests.order_id));
        assert_eq!(
            edge.fire_order(waiter(), tables, Some(station()))
                .await
                .expect("the table's order is sent")
                .len(),
            2
        );
    });
}

/// Retries of one guest submission, through the intake: the relay delivers at least once.
mod retries {
    use super::{
        Arc, Edge, EdgeOrderIn, ExternalReference, FakeStore, InMemoryQueueNumbers, InboundOrder,
        InboundOrderLine, Open, OrderIn, SalesChannel, a_waiters_order, edge_over, joins_at_once,
        joins_on_confirmation, merges, pizza, run_ready, store_id, table, waiter,
    };
    use pos_proto::ids::DeviceId;
    use pos_proto::quantity::Quantity;
    use pos_proto::ulid::Ulid;

    fn intake(edge: &Arc<Edge<FakeStore>>) -> EdgeOrderIn<FakeStore, InMemoryQueueNumbers> {
        EdgeOrderIn::new(
            Arc::clone(edge),
            InMemoryQueueNumbers::new(),
            DeviceId::new(Ulid::from_u128(20)),
        )
    }

    fn a_submission(reference: &str) -> InboundOrder {
        InboundOrder {
            external_reference: ExternalReference::parse(reference).expect("a valid reference"),
            sales_channel: Open::from_known(SalesChannel::Qr),
            store_id: store_id(),
            table_id: Some(table()),
            subject_id: None,
            lines: vec![InboundOrderLine {
                menu_item_id: pizza(),
                quantity: Quantity::ONE,
                modifier_menu_item_ids: Vec::new(),
                quoted_unit_price: None,
                note: None,
            }],
            placed_at: pos_contract_tests::fixtures::instant(),
        }
    }

    #[test]
    fn a_retried_submission_joins_once_and_answers_as_it_did() {
        run_ready(async {
            let store = FakeStore::default();
            let edge = Arc::new(edge_over(store.clone(), joins_at_once()));
            let (tables, _) = a_waiters_order(&edge).await;
            let intake = intake(&edge);
            let submission = a_submission("QR-JOIN-1");

            let first = intake.submit(&submission).await.expect("taken");
            let again = intake.submit(&submission).await.expect("a repeat");

            assert!(first.created && !again.created);
            assert_eq!(again.order_id, first.order_id, "the same order, its own");
            assert_ne!(first.order_id, tables);
            assert_eq!(again.total, first.total);
            assert_eq!(
                edge.order_line_ids(tables).len(),
                2,
                "one guest line, not two"
            );
            assert_eq!(merges(&store).await.len(), 1);

            // A second guest at the same table is a second submission and a second order, which
            // joins the same table's order.
            let other = intake
                .submit(&a_submission("QR-JOIN-2"))
                .await
                .expect("taken");
            assert!(other.created);
            assert_ne!(other.order_id, first.order_id);
            assert_eq!(edge.order_line_ids(tables).len(), 3);
        });
    }

    #[test]
    fn a_submission_retried_after_staff_confirmed_it_joins_nothing_again() {
        run_ready(async {
            let store = FakeStore::default();
            let edge = Arc::new(edge_over(store.clone(), joins_on_confirmation()));
            let (tables, _) = a_waiters_order(&edge).await;
            let intake = intake(&edge);
            let submission = a_submission("QR-JOIN-3");

            let first = intake.submit(&submission).await.expect("taken");
            assert!(first.awaiting_staff_confirmation);
            edge.confirm_inbound_order(waiter(), first.order_id)
                .await
                .expect("staff confirm it");
            let again = intake.submit(&submission).await.expect("a repeat");

            assert!(!again.created);
            assert_eq!(again.order_id, first.order_id);
            assert_eq!(edge.order_line_ids(tables).len(), 2);
            assert_eq!(merges(&store).await.len(), 1);
        });
    }
}
