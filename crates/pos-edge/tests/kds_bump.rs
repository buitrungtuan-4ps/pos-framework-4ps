// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The durable KDS bump (#44, P6 residual).
//!
//! A bump used to be UI-local: each kitchen screen kept its own "done" set, so a second screen never
//! agreed. This proves the fix — `Edge::bump_ticket` writes the durable `kitchen.ticket.bumped`
//! event, marks the projection, and fans it out — so a KDS coming online after the bump reads the
//! same prepared set (`bumped_line_ids`) rather than a private, divergent flag.

use std::sync::Arc;

use pos_core::decision::Actor;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{CurrencyCode, OrderLineState};

fn device() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(1)),
        device_id: DeviceId::new(Ulid::from_u128(1)),
    }
}

fn a_pizza(item: u128) -> LineDraft {
    LineDraft {
        menu_item_id: MenuItemId::new(Ulid::from_u128(item)),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: Money::new(CurrencyCode::VND, 150_000),
        line_total: Money::new(CurrencyCode::VND, 150_000),
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

#[test]
fn a_bump_is_durable_fanned_out_and_read_back_by_a_late_kds() {
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
        let mut kitchen_feed = edge.fanout().subscribe();

        let table = TableId::new(Ulid::from_u128(900));
        let station = StationId::new(Ulid::from_u128(9));

        // Seat, add two lines, fire them to the kitchen.
        edge.seat_table(device(), table).await.expect("seats");
        let line_a = edge
            .add_line(device(), table, a_pizza(500))
            .await
            .expect("adds a line");
        let line_b = edge
            .add_line(device(), table, a_pizza(501))
            .await
            .expect("adds a second line");
        edge.fire_line(device(), line_a.order_line_id, Some(station))
            .await
            .expect("fires line a");
        edge.fire_line(device(), line_b.order_line_id, Some(station))
            .await
            .expect("fires line b");

        // Nothing is bumped yet.
        assert!(
            edge.bumped_line_ids().is_empty(),
            "no line is prepared before a bump"
        );

        // The station bumps the first line — one line of the ticket is made, the other still cooking.
        let view = edge
            .bump_ticket(
                device(),
                line_a.order_id,
                station,
                vec![line_a.order_line_id],
            )
            .await
            .expect("bumps the ticket");
        assert_eq!(view.order_id, line_a.order_id);
        assert_eq!(view.station_id, station);
        assert_eq!(view.order_line_ids, vec![line_a.order_line_id]);

        // The projection now reflects the bump — the prepared set a late-joining KDS reads on connect.
        assert_eq!(
            edge.bumped_line_ids(),
            vec![line_a.order_line_id],
            "the bumped line is in the prepared set; the unbumped line is not"
        );

        // A bump is orthogonal to the line's order state: a made line is still `Fired`.
        assert_eq!(
            edge.line_state(line_a.order_line_id),
            Some(OrderLineState::Fired),
            "a bump does not change the line's order state"
        );

        // The bump reached the fan-out, so every connected KDS folds the same truth.
        let mut seen = Vec::new();
        while let Ok(frame) = kitchen_feed.try_recv() {
            seen.push(frame);
        }
        assert!(
            seen.join("\n").contains("kitchen.ticket.bumped"),
            "the bump was fanned out to the kitchen displays"
        );
    });
}
