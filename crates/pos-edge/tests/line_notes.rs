// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A guest note reaches the kitchen and nothing else
//! ([ADR-0157](../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
//!
//! The note is where "peanut allergy" gets typed, so each case here pins one half of the same
//! promise. The kitchen can read it: the live orders carry it, and so does the frame that announces
//! the line. The store keeps it nowhere: the log records only that a note existed, a restart forgets
//! the text and says so, and a line leaving the boards takes its note with it.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_edge::line_notes::NoteText;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, OrderLineId, ReasonCodeId, StoreId, TableId,
};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{CurrencyCode, Open, PaymentMethod, SalesChannel};

const ALLERGY: &str = "Dị ứng đậu phộng — no peanuts";

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(800))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn keyed_wrong() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_001))
}

fn a_line() -> LineDraft {
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
    }
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

fn allergy() -> Option<NoteText> {
    NoteText::parse(Some(ALLERGY)).expect("a valid note")
}

fn session() -> EdgeSession {
    let mut session = EdgeSession::bootstrap();
    session.reason_codes = PublishedReasonCodes::from_parts(vec![PublishedReasonCode::new(
        keyed_wrong(),
        ReasonCode::new("KEYED_WRONG"),
        DisplayName::new("Keyed in error"),
        vec![ReasonAction::VoidLine],
    )]);
    session
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

/// The note on the one live line with this id, and whether the log says one was written.
fn live_note(edge: &Edge<FakeStore>, line: OrderLineId) -> (bool, Option<String>) {
    edge.live_orders()
        .into_iter()
        .flat_map(|order| order.lines)
        .find(|live| live.order_line_id == line)
        .map(|live| {
            (
                live.note_present,
                live.note.map(|note| note.as_str().to_owned()),
            )
        })
        .expect("the line is live")
}

/// Every event on the store's log, as the text the outbox would carry to the cloud.
async fn logged(store: &FakeStore) -> String {
    let query = EventQuery::first(store_id(), NonZeroU32::new(256).expect("non-zero"));
    store
        .read(&query)
        .await
        .expect("read the log")
        .iter()
        .map(|envelope| envelope.data.as_json().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_note_reaches_the_kitchen_and_never_the_log() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let mut feed = edge.fanout().subscribe();
        edge.seat_table(server(), table(), None)
            .await
            .expect("seats");

        let line = edge
            .add_noted_line(server(), table(), a_line(), allergy())
            .await
            .expect("adds a line with a note");

        // The kitchen reads it where it reads the line.
        assert_eq!(
            live_note(&edge, line.order_line_id),
            (true, Some(ALLERGY.to_owned()))
        );
        assert_eq!(
            edge.line_note(line.order_line_id)
                .map(|note| note.as_str().to_owned()),
            Some(ALLERGY.to_owned()),
            "the ticket printer asks for it by line"
        );

        // The frame that told every device about the line carried the note beside the event.
        let mut announced = None;
        while let Ok(frame) = feed.try_recv() {
            let frame: serde_json::Value = serde_json::from_str(&frame).expect("json");
            if frame.pointer("/event_type") == Some(&serde_json::json!("sales.order_line.added")) {
                announced = Some(frame);
            }
        }
        let announced = announced.expect("the line was fanned out");
        assert_eq!(
            announced.pointer("/payload/note"),
            Some(&serde_json::json!(ALLERGY))
        );
        assert_eq!(
            announced.pointer("/payload/note_present"),
            Some(&serde_json::json!(true))
        );

        // And the log, which is immutable and travels to the cloud, holds only the flag.
        let log = logged(&store).await;
        assert!(log.contains("\"note_present\":true"), "{log}");
        assert!(
            !log.contains("peanut"),
            "the note's text reached the log: {log}"
        );
        assert!(
            !log.contains("\"note\""),
            "the note's key reached the log: {log}"
        );
    });
}

#[test]
fn a_restart_forgets_the_text_and_says_a_note_was_lost() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let line = {
            let edge = edge_over(store.clone());
            edge.seat_table(server(), table(), None)
                .await
                .expect("seats");
            edge.add_noted_line(server(), table(), a_line(), allergy())
                .await
                .expect("adds")
                .order_line_id
        };

        let edge = edge_over(store);
        edge.rebuild().await.expect("rebuilds from the log");
        assert_eq!(
            live_note(&edge, line),
            (true, None),
            "the flag survives the restart and the text does not, so a screen can say so"
        );
    });
}

#[test]
fn a_voided_line_takes_its_note_with_it() {
    pos_fakes::executor::run_ready(async {
        let edge = edge_over(FakeStore::default());
        edge.seat_table(server(), table(), None)
            .await
            .expect("seats");
        let noted = edge
            .add_noted_line(server(), table(), a_line(), allergy())
            .await
            .expect("adds");
        assert_eq!(edge.held_note_count(), 1);

        edge.void_line(server(), noted.order_line_id, keyed_wrong(), None)
            .await
            .expect("voids a line the kitchen never saw");

        assert_eq!(edge.held_note_count(), 0);
        assert_eq!(edge.line_note(noted.order_line_id), None);
    });
}

#[test]
fn a_settled_order_takes_its_notes_off_the_boards() {
    pos_fakes::executor::run_ready(async {
        let edge = edge_over(FakeStore::default());
        edge.seat_table(server(), table(), None)
            .await
            .expect("seats");
        edge.add_noted_line(server(), table(), a_line(), allergy())
            .await
            .expect("adds");
        let bill = edge.open_bill(server(), table()).await.expect("opens");
        assert_eq!(
            edge.held_note_count(),
            1,
            "a bill being open is not the order leaving the boards"
        );

        edge.settle_bill(
            server(),
            bill.bill_id,
            vec![Payment {
                method: PaymentMethod::Cash,
                tendered: vnd(165_000),
                applied_to_bill: vnd(165_000),
                tip: vnd(0),
            }],
            None,
        )
        .await
        .expect("settles");

        assert_eq!(edge.held_note_count(), 0);
    });
}

#[test]
fn an_inbound_orders_note_is_kept_for_the_kitchen() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let note = NoteText::lenient("no peanuts\nallergy");
        edge.open_inbound_order(
            server().device_id,
            Open::from_known(SalesChannel::Takeaway),
            None,
            &[(a_priced_line(), note)],
            None,
        )
        .await
        .expect("a relayed order opens");

        let line = edge
            .live_orders()
            .into_iter()
            .flat_map(|order| order.lines)
            .next()
            .expect("its line is live");
        assert!(line.note_present);
        assert_eq!(
            line.note.map(|note| note.as_str().to_owned()),
            Some("no peanuts allergy".to_owned()),
            "the delivery's note used to be dropped here; it is made printable and kept"
        );
        assert!(!logged(&store).await.contains("peanuts"));
    });
}

#[test]
fn a_line_without_a_note_holds_nothing() {
    pos_fakes::executor::run_ready(async {
        let edge = edge_over(FakeStore::default());
        edge.seat_table(server(), table(), None)
            .await
            .expect("seats");
        let line = edge
            .add_line(server(), table(), a_line())
            .await
            .expect("adds");
        assert_eq!(live_note(&edge, line.order_line_id), (false, None));
        assert_eq!(edge.held_note_count(), 0);
    });
}
