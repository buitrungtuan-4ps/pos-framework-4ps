// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Crash recovery: a restart rebuilds the projection from the durable log (P5).
//!
//! The P5 promise is "kill the process mid-sale and lose only the uncommitted transaction". Here a
//! first edge sells and settles against a store, then a **second** edge over the *same* store — a
//! stand-in for a restart — rebuilds and finds every committed fact: the settled bill, the cleaned-
//! down table, the fired line, and the shift's cash roll-up. Nothing was carried over in memory; it
//! all came back from the log.

use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{CurrencyCode, OrderLineState, PaymentMethod, ShiftState, TableState};

fn actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn a_line() -> pos_edge::LineDraft {
    pos_edge::LineDraft {
        menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        note_present: false,
    }
}

fn edge_over(store: FakeStore) -> Edge<FakeStore> {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1)));
    Edge::new(
        store,
        identity,
        EdgeSession::bootstrap(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed")
}

#[test]
fn a_restart_rebuilds_the_projection_from_the_log() {
    pos_fakes::executor::run_ready(async {
        // The durable log, shared across the "crash": both edges talk to the same store.
        let store = FakeStore::default();
        let table = TableId::new(Ulid::from_u128(800));
        let station = StationId::new(Ulid::from_u128(9));

        // Session one: open a shift, sell a table, fire the line, settle it in cash.
        let (shift_id, line_id) = {
            let edge = edge_over(store.clone());
            let shift = edge
                .open_shift(actor(), vnd(500_000))
                .await
                .expect("opens a shift");
            edge.seat_table(actor(), table).await.expect("seats");
            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
            edge.fire_line(actor(), line.order_line_id, station)
                .await
                .expect("fires");
            let bill = edge.open_bill(actor(), table).await.expect("opens a bill");
            let settled = edge
                .settle_bill(
                    actor(),
                    bill.bill_id,
                    vec![Payment {
                        method: PaymentMethod::Cash,
                        tendered: vnd(165_000),
                        applied_to_bill: vnd(165_000),
                    }],
                    vec![],
                )
                .await
                .expect("settles");
            assert_eq!(settled.receipt_number, Some(1));
            (shift.shift_id, line.order_line_id)
        };

        // "Restart": a fresh edge over the same store starts with an empty projection.
        let edge = edge_over(store.clone());
        assert_eq!(
            edge.table_state(table),
            TableState::Free,
            "a fresh projection knows nothing until it replays the log"
        );
        assert_eq!(edge.line_state(line_id), None);

        // Replay the log: every committed fact comes back.
        edge.rebuild().await.expect("rebuilds from the log");
        assert_eq!(
            edge.table_state(table),
            TableState::NeedsCleaning,
            "the settled bill cycled the table, and that survived the restart"
        );
        assert_eq!(
            edge.line_state(line_id),
            Some(OrderLineState::Fired),
            "the fired line survived the restart"
        );

        // The shift's cash roll-up survived too: close reveals float + the cash sale.
        edge.count_shift(actor(), shift_id, 665_000)
            .await
            .expect("counts");
        let closed = edge.close_shift(actor(), shift_id).await.expect("closes");
        assert_eq!(closed.state, ShiftState::Closed);
        assert_eq!(
            closed.expected_amount,
            Some(vnd(665_000)),
            "the rebuilt roll-up is the 500k float plus the 165k cash sale"
        );
        assert_eq!(closed.variance, Some(vnd(0)));

        // And the recovered table finishes its cycle: a clean releases it.
        let cleaned = edge.clean_table(actor(), table).await.expect("cleans");
        assert_eq!(cleaned.state, TableState::Free);
    });
}

#[test]
fn rebuild_is_idempotent() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let table = TableId::new(Ulid::from_u128(801));
        {
            let edge = edge_over(store.clone());
            edge.seat_table(actor(), table).await.expect("seats");
        }
        let edge = edge_over(store.clone());
        edge.rebuild().await.expect("first rebuild");
        edge.rebuild().await.expect("second rebuild");
        // Replaying committed facts twice lands on the same state.
        assert_eq!(edge.table_state(table), TableState::Occupied);
    });
}
