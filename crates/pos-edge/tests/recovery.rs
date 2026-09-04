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
use pos_proto::{
    CurrencyCode, Open, OrderLineState, PaymentMethod, SalesChannel, ShiftState, TableState,
};

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
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// One already-priced line, in the shape the intake path hands to `open_inbound_order`.
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
        repriced: false,
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
            edge.fire_line(actor(), line.order_line_id, Some(station))
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

#[test]
fn a_bump_survives_a_restart() {
    // A durable KDS bump (#44) is projection state like any other, so it must come back on rebuild —
    // otherwise a restart would re-show a ticket the kitchen already made.
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let table = TableId::new(Ulid::from_u128(802));
        let station = StationId::new(Ulid::from_u128(9));
        let line_id = {
            let edge = edge_over(store.clone());
            edge.seat_table(actor(), table).await.expect("seats");
            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
            edge.fire_line(actor(), line.order_line_id, Some(station))
                .await
                .expect("fires");
            edge.bump_ticket(actor(), line.order_id, station, vec![line.order_line_id])
                .await
                .expect("bumps");
            line.order_line_id
        };

        // A fresh edge over the same store knows nothing until it replays the log.
        let edge = edge_over(store.clone());
        assert!(edge.bumped_line_ids().is_empty());
        edge.rebuild().await.expect("rebuilds from the log");
        assert_eq!(
            edge.bumped_line_ids(),
            vec![line_id],
            "the bump was folded back from the log, so the ticket stays made across a restart"
        );
    });
}

#[test]
fn a_counter_bill_survives_the_restart_that_used_to_drop_it() {
    // The fold arm that records a bill used to sit inside `if let Some(table_id) =
    // table_for_order(..)`, because a bill was assumed to have a table. A counter order has none
    // ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)), so on every rebuild the bill was
    // silently dropped: a box that restarted between opening a takeaway bill and settling it came
    // back not knowing the bill existed, and the guest could no longer be charged. Keyed on the
    // order, it comes back.
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();

        // Session one: a relayed counter order, and a bill opened on it. No table anywhere.
        let bill_id = {
            let edge = edge_over(store.clone());
            let (order_id, _business_date) = edge
                .open_inbound_order(
                    actor().device_id,
                    Open::from_known(SalesChannel::Takeaway),
                    None,
                    &[(a_priced_line(), false)],
                    None,
                )
                .await
                .expect("a counter order opens");
            edge.open_bill_for_order(actor(), order_id)
                .await
                .expect("and takes a bill")
                .bill_id
        };

        // Session two, over the same log: the restart. Settling proves the bill came back *and*
        // that its order's lines came back with it — the total is assembled from them, so a bill
        // recovered without its order would fail here rather than pass quietly.
        let edge = edge_over(store.clone());
        edge.rebuild().await.expect("rebuilds from the log");
        let settled = edge
            .settle_bill(
                actor(),
                bill_id,
                vec![Payment {
                    method: PaymentMethod::Cash,
                    tendered: vnd(165_000),
                    applied_to_bill: vnd(165_000),
                }],
                vec![],
            )
            .await
            .expect("the counter bill is still there after the restart, and settles");
        assert_eq!(settled.total_due, Some(vnd(165_000)));
        assert_eq!(settled.receipt_number, Some(1));
        assert_eq!(settled.table_state, None, "it never had a table to cycle");
    });
}
