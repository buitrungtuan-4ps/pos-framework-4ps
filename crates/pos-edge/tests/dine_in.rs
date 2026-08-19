// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The dine-in acceptance flow (P5 exit criterion, `docs/roadmap.md`).
//!
//! One table, two devices, the cable unplugged: seat → both devices order → fire by course → add
//! more → open the bill → settle it split across cash and card → a gapless receipt → the table
//! cycles to clean. Every committed change reaches **both** devices over the fan-out, and the whole
//! flow runs on the in-memory fakes with no network — which *is* the offline demonstration, since
//! the fakes have no cable to unplug and the bootstrap session trades `Offline`.
//!
//! This is the archive's first end-to-end acceptance flow, as an automated test rather than a
//! checklist. The crash-safety half ("kill mid-sale, lose only the uncommitted transaction") is
//! `tests/recovery.rs`; the split payment here is the "pay cash + card" beat.

use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, CurrencyCode, PaymentMethod, TableState};

fn device_a() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(1)),
        device_id: DeviceId::new(Ulid::from_u128(1)),
    }
}

fn device_b() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(2)),
        device_id: DeviceId::new(Ulid::from_u128(2)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn a_pizza(item: u128) -> LineDraft {
    LineDraft {
        menu_item_id: MenuItemId::new(Ulid::from_u128(item)),
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

#[test]
fn a_dine_in_service_runs_end_to_end_offline_across_two_devices() {
    pos_fakes::executor::run_ready(async {
        let edge = Arc::new(
            Edge::new(
                FakeStore::default(),
                StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
                EdgeSession::bootstrap(),
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("seed"),
        );
        // Two devices on the floor, each watching the same table over the fan-out — a POS terminal
        // and the kitchen display.
        let mut terminal_feed = edge.fanout().subscribe();
        let mut kitchen_feed = edge.fanout().subscribe();

        let table = TableId::new(Ulid::from_u128(900));
        let station = StationId::new(Ulid::from_u128(9));

        // The server seats the table.
        let seated = edge.seat_table(device_a(), table).await.expect("seats");
        assert_eq!(seated.state, TableState::Occupied);

        // Two devices order onto the one table concurrently: device A a pizza, device B another.
        let line_a = edge
            .add_line(device_a(), table, a_pizza(500))
            .await
            .expect("device A adds a line");
        let line_b = edge
            .add_line(device_b(), table, a_pizza(501))
            .await
            .expect("device B adds a line");

        // Fire the first course to the kitchen.
        edge.fire_line(device_a(), line_a.order_line_id, station)
            .await
            .expect("fires device A's line");
        edge.fire_line(device_b(), line_b.order_line_id, station)
            .await
            .expect("fires device B's line");

        // More items land after the first course fired, and fire too.
        let dessert = edge
            .add_line(device_a(), table, a_pizza(502))
            .await
            .expect("adds a later course");
        edge.fire_line(device_a(), dessert.order_line_id, station)
            .await
            .expect("fires the later course");

        // Ask for the bill: the table moves to awaiting payment.
        let bill = edge
            .open_bill(device_a(), table)
            .await
            .expect("opens the bill");
        assert_eq!(bill.table_state, TableState::AwaitingPayment);

        // Three 150k lines at the 10% standard rate: 450k + 45k tax = 495k. Pay it split cash + card.
        let settled = edge
            .settle_bill(
                device_a(),
                bill.bill_id,
                vec![
                    Payment {
                        method: PaymentMethod::Cash,
                        tendered: vnd(200_000),
                        applied_to_bill: vnd(200_000),
                    },
                    Payment {
                        method: PaymentMethod::Card,
                        tendered: vnd(295_000),
                        applied_to_bill: vnd(295_000),
                    },
                ],
                vec![],
            )
            .await
            .expect("settles split across cash and card");
        assert_eq!(settled.state, BillState::Settled);
        assert_eq!(settled.total_due, Some(vnd(495_000)));
        assert_eq!(
            settled.receipt_number,
            Some(1),
            "the first receipt is number one"
        );
        assert!(settled.print_receipt, "a receipt prints after settlement");
        assert_eq!(settled.table_state, TableState::NeedsCleaning);

        // Clean down: the table cycles back to free, ready for the next guests.
        let cleaned = edge.clean_table(device_a(), table).await.expect("cleans");
        assert_eq!(cleaned.state, TableState::Free);

        // Both devices saw the whole service over the fan-out — the same committed truth on each.
        for feed in [&mut terminal_feed, &mut kitchen_feed] {
            let mut seen = Vec::new();
            while let Ok(frame) = feed.try_recv() {
                seen.push(frame);
            }
            let all = seen.join("\n");
            for event_type in [
                "sales.table.opened",
                "sales.order_line.added",
                "sales.order_line.fired",
                "billing.bill.opened",
                "billing.payment.captured",
                "billing.bill.settled",
                "sales.table.closed",
            ] {
                assert!(
                    all.contains(event_type),
                    "a device should have seen {event_type} over the fan-out"
                );
            }
        }
    });
}
