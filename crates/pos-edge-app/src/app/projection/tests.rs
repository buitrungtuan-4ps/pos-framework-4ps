// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The projection's unit tests: the order index agrees with a scan through a whole service, a
//! settled history does not grow the live set, a projection rebuilt twice sells each line once, and
//! a rebuild keeps a seated table occupied and a line's changed quantity.

use std::sync::Arc;

use pos_fakes::FakeStore;
use pos_proto::ids::{OrderId, TableId};
use pos_proto::quantity::Quantity;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, TableState};

use crate::app::tests::{a_line, actor, cash, edge, identity};
use crate::app::{Edge, EdgeSession};
use crate::receipt::InMemoryReceipts;

// ---- the order index (F2) ---------------------------------------------------------------

/// What the index replaced: every line the store holds, filtered to one order, in id order.
fn lines_by_scan(projection: &super::Projection, order_id: OrderId) -> Vec<super::OrderLineId> {
    let mut ids: Vec<super::OrderLineId> = projection
        .lines
        .iter()
        .filter(|(_, record)| record.order_id == order_id)
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    ids
}

/// What the live set replaced: every order with a line and no settled bill, by a full scan.
fn open_orders_by_scan(projection: &super::Projection) -> Vec<OrderId> {
    let mut orders: Vec<OrderId> = projection
        .lines
        .values()
        .map(|record| record.order_id)
        .filter(|order_id| {
            projection
                .bill_for_order(*order_id)
                .and_then(|bill_id| projection.bill(bill_id))
                .is_none_or(|bill| bill.state != BillState::Settled)
        })
        .collect();
    orders.sort_unstable();
    orders.dedup();
    orders
}

/// The index and the live set agree with the scans they replaced.
fn assert_index_agrees(edge: &Edge<FakeStore>, step: &str) {
    let projection = edge.lock_projection();
    assert_eq!(
        projection.open_orders(),
        open_orders_by_scan(&projection),
        "live orders after {step}"
    );
    let mut every_order: Vec<OrderId> = projection
        .lines
        .values()
        .map(|record| record.order_id)
        .collect();
    every_order.sort_unstable();
    every_order.dedup();
    for order_id in every_order {
        let indexed: Vec<super::OrderLineId> = projection
            .identified_lines_for_order(order_id)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            indexed,
            lines_by_scan(&projection, order_id),
            "lines after {step}"
        );
    }
}

#[test]
fn the_order_index_agrees_with_a_scan_through_a_whole_service() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let (a, b) = (
            TableId::new(Ulid::from_u128(401)),
            TableId::new(Ulid::from_u128(402)),
        );

        edge.seat_table(actor(), a, None).await.expect("seats a");
        let mut on_a = Vec::new();
        for _ in 0..4 {
            on_a.push(
                edge.add_line(actor(), a, a_line())
                    .await
                    .expect("adds")
                    .order_line_id,
            );
        }
        assert_index_agrees(&edge, "four lines on a");
        let order_a = edge
            .lock_projection()
            .order_for_table(a)
            .expect("a has an order");

        edge.seat_table(actor(), b, None).await.expect("seats b");
        edge.add_line(actor(), b, a_line())
            .await
            .expect("adds to b");
        assert_index_agrees(&edge, "a second table");

        let bill = edge.open_bill(actor(), a).await.expect("opens a's bill");
        assert_index_agrees(&edge, "a bill opened");

        let parts = edge
            .split_bill(
                actor(),
                bill.bill_id,
                vec![vec![on_a[0], on_a[1]], vec![on_a[2], on_a[3]]],
            )
            .await
            .expect("splits");
        assert_index_agrees(&edge, "a split");

        edge.merge_bills(actor(), parts[0], vec![parts[1]])
            .await
            .expect("merges back");
        assert_index_agrees(&edge, "a merge");

        edge.settle_bill(actor(), parts[0], vec![cash(660_000)], None)
            .await
            .expect("settles a");
        assert_index_agrees(&edge, "a settle");
        // And a count: a split merged back and settled is finished. The single bill pointer
        // this replaced named the absorbed part, so the paid order stayed live.
        assert!(
            !edge.lock_projection().open_orders().contains(&order_a),
            "a's order is paid"
        );

        edge.clean_table(actor(), a).await.expect("cleans a");
        edge.seat_table(actor(), a, None).await.expect("re-seats a");
        edge.add_line(actor(), a, a_line())
            .await
            .expect("a new order on a");
        assert_index_agrees(&edge, "a re-seat");
    });
}

#[test]
fn a_settled_history_does_not_grow_the_live_set() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(410));
        for _ in 0..50 {
            edge.seat_table(actor(), table, None).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");
            let bill = edge.open_bill(actor(), table).await.expect("bills");
            edge.settle_bill(actor(), bill.bill_id, vec![cash(165_000)], None)
                .await
                .expect("settles");
            edge.clean_table(actor(), table).await.expect("cleans");
        }
        edge.seat_table(actor(), table, None)
            .await
            .expect("seats the live one");
        edge.add_line(actor(), table, a_line()).await.expect("adds");

        let projection = edge.lock_projection();
        assert_eq!(
            projection.live.len(),
            1,
            "fifty settled orders and one live one"
        );
        assert_eq!(
            projection.order_lines.len(),
            51,
            "every order is still indexed"
        );
    });
}

#[test]
fn a_projection_rebuilt_twice_sells_each_line_once() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let live = Edge::new(
            store.clone(),
            identity(),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(420));
        live.seat_table(actor(), table, None).await.expect("seats");
        for _ in 0..3 {
            live.add_line(actor(), table, a_line()).await.expect("adds");
        }

        let restarted = Edge::new(
            store,
            identity(),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        restarted.rebuild().await.expect("rebuilds");
        restarted.rebuild().await.expect("rebuilds again");

        assert_eq!(restarted.live_orders(), live.live_orders());
        assert_eq!(
            restarted
                .live_orders()
                .first()
                .map(|order| order.lines.len()),
            Some(3),
            "no line is indexed twice"
        );
    });
}

/// An edge over `store` that has replayed its log, as a box does when it boots.
async fn restarted_over(store: FakeStore) -> Edge<FakeStore> {
    let restarted = Edge::new(
        store,
        identity(),
        EdgeSession::bootstrap(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds");
    restarted.rebuild().await.expect("rebuilds");
    restarted
}

#[test]
fn a_restart_keeps_a_seated_table_occupied_and_a_lines_changed_quantity() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let live = Edge::new(
            store.clone(),
            identity(),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(430));
        live.seat_table(actor(), table, None).await.expect("seats");
        let added = live.add_line(actor(), table, a_line()).await.expect("adds");
        let three = Quantity::from_whole(3).expect("three units");
        live.set_line_quantity(actor(), added.order_line_id, three)
            .await
            .expect("changes the quantity");

        let restarted = restarted_over(store).await;
        assert_eq!(restarted.table_state(table), TableState::Occupied);
        assert_eq!(
            restarted.check_totals(table).expect("the check"),
            live.check_totals(table).expect("the check"),
            "the check is priced at three, not at the one the line was added with"
        );
    });
}
