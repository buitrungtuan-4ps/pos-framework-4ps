// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The shift's unit tests: opening, counting and closing, the chain head a close anchors, the
//! roll-up and variance, the store that refuses selling without a shift, and the day-end warning.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_fakes::FakeStore;
use pos_ports::EventStore;
use pos_ports::event_store::EventQuery;
use pos_proto::events::StoreChainAnchored;
use pos_proto::ids::{ShiftId, TableId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::{EventPayload, Open, PaymentMethod, SalesChannel, ShiftState, TableState};

use crate::app::tests::{a_line, actor, cash, edge, identity, vnd};
use crate::app::{Edge, EdgeSession};
use crate::receipt::InMemoryReceipts;

#[test]
fn a_shift_opens_counts_and_closes_blind() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let opened = edge
            .open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");
        assert_eq!(opened.state, ShiftState::Open);
        // Counting reveals nothing: no expected amount, no variance yet.
        let counted = edge
            .count_shift(actor(), opened.shift_id, 500_000)
            .await
            .expect("counts");
        assert_eq!(counted.state, ShiftState::Counted);
        assert_eq!(counted.counted_amount, Some(vnd(500_000)));
        assert_eq!(counted.expected_amount, None, "the count is blind");
        assert_eq!(counted.variance, None, "the count is blind");
        // Closing reveals the expected amount (just the float, no sales) and a zero variance.
        let closed = edge
            .close_shift(actor(), opened.shift_id)
            .await
            .expect("closes");
        assert_eq!(closed.state, ShiftState::Closed);
        assert_eq!(closed.expected_amount, Some(vnd(500_000)));
        assert_eq!(closed.variance, Some(vnd(0)));
        assert!(closed.print_shift_report, "closing prints the shift report");
    });
}

/// Closing a shift publishes the chain head
/// ([ADR-0131](../../../../../docs/adr/0131-a-chained-event-log.md) decision 4).
///
/// This is the half the chain cannot do alone. A hash chain verifies happily after its tail has
/// been cut off, and after an edit whose later links were re-derived — the algorithm is in the
/// source. What a store cannot do is rewrite what the cloud has already received, and this
/// event is that record leaving the building.
#[test]
fn closing_a_shift_publishes_the_chain_head() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let opened = edge
            .open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");
        edge.count_shift(actor(), opened.shift_id, 500_000)
            .await
            .expect("counts");
        edge.close_shift(actor(), opened.shift_id)
            .await
            .expect("closes");

        let anchor = anchor_payload(&edge).await.expect("a shift close anchors");
        let head = edge
            .store
            .chain_head(edge.identity.store_id)
            .await
            .expect("reads the head")
            .expect("the store has a chain");

        assert!(anchor.chain_seq > 0, "the anchor names a real position");
        assert_eq!(
            anchor.chain_seq,
            head.seq - 1,
            "the anchor reports the chain as it stood *before* it was itself written — a \
             record cannot contain its own hash, and claiming otherwise would be a head no \
             verification could ever reproduce"
        );
        assert_eq!(anchor.chain_head.as_str().len(), 64);
        assert!(
            !anchor.chain_head.is_genesis(),
            "a real digest, not genesis"
        );
        assert_eq!(
            anchor.unchained, 0,
            "a fresh store has no unchained history"
        );
    });
}

/// Two closes produce two anchors, and the second extends the first.
///
/// The property the cloud's refusal will be built on: an anchor that went backwards, or whose
/// head did not follow, is exactly the tampering this exists to catch.
#[test]
fn each_anchor_extends_the_one_before_it() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let mut seen: Vec<u64> = Vec::new();
        for float in [500_000, 600_000] {
            let opened = edge
                .open_shift(actor(), vnd(float))
                .await
                .expect("opens a shift");
            edge.count_shift(actor(), opened.shift_id, float)
                .await
                .expect("counts");
            edge.close_shift(actor(), opened.shift_id)
                .await
                .expect("closes");
            seen.push(
                anchor_payloads(&edge)
                    .await
                    .last()
                    .expect("an anchor per close")
                    .chain_seq,
            );
        }
        assert_eq!(seen.len(), 2);
        assert!(
            seen[1] > seen[0],
            "the second anchor must extend the first, not restate or precede it: {seen:?}"
        );
    });
}

/// Every `store.chain.anchored` payload in this store's log, in order.
async fn anchor_payloads(edge: &Edge<FakeStore>) -> Vec<StoreChainAnchored> {
    let events = edge
        .store
        .read(&EventQuery::first(
            edge.identity.store_id,
            NonZeroU32::new(500).expect("500 is not zero"),
        ))
        .await
        .expect("reads the log");
    events
        .iter()
        .filter(|envelope| envelope.event_type.as_str() == StoreChainAnchored::EVENT_TYPE.as_str())
        .filter_map(|envelope| envelope.data.decode().ok())
        .collect()
}

/// The single anchor in this store's log, when there is exactly one.
async fn anchor_payload(edge: &Edge<FakeStore>) -> Option<StoreChainAnchored> {
    anchor_payloads(edge).await.into_iter().next()
}

#[test]
fn a_shift_rolls_up_only_cash_from_settled_bills() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let opened = edge
            .open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");

        // A cash sale of 165k lands in the drawer.
        let table = TableId::new(Ulid::from_u128(400));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let bill = edge.open_bill(actor(), table).await.expect("opens a bill");
        edge.settle_bill(actor(), bill.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles in cash");

        // A card sale does not: it never touches the drawer.
        let table2 = TableId::new(Ulid::from_u128(401));
        edge.seat_table(actor(), table2, None).await.expect("seats");
        edge.add_line(actor(), table2, a_line())
            .await
            .expect("adds");
        let bill2 = edge.open_bill(actor(), table2).await.expect("opens a bill");
        let card = Payment {
            method: PaymentMethod::Card,
            tendered: vnd(165_000),
            applied_to_bill: vnd(165_000),
            tip: vnd(0),
        };
        edge.settle_bill(actor(), bill2.bill_id, vec![card], None)
            .await
            .expect("settles on card");

        // Expected drawer cash is float + cash sales only: 500k + 165k.
        edge.count_shift(actor(), opened.shift_id, 665_000)
            .await
            .expect("counts");
        let closed = edge
            .close_shift(actor(), opened.shift_id)
            .await
            .expect("closes");
        assert_eq!(closed.expected_amount, Some(vnd(665_000)));
        assert_eq!(closed.variance, Some(vnd(0)));
    });
}

#[test]
fn variance_is_negative_when_the_drawer_is_short() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let opened = edge
            .open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");
        edge.count_shift(actor(), opened.shift_id, 400_000)
            .await
            .expect("counts 100k short");
        let closed = edge
            .close_shift(actor(), opened.shift_id)
            .await
            .expect("closes");
        assert_eq!(closed.variance, Some(vnd(-100_000)));
        assert!(closed.variance.expect("variance").is_negative());
    });
}

#[test]
fn opening_a_second_shift_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        edge.open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");
        let refused = edge.open_shift(actor(), vnd(500_000)).await;
        assert!(matches!(refused, Err(super::AppError::ShiftAlreadyOpen)));
    });
}

/// An edge at a store whose `shift` node refuses selling while no shift is open (ADR-0160).
fn edge_refusing_sales_without_a_shift() -> Edge<FakeStore> {
    use pos_proto::shift::{NoShiftSelling, PublishedShift};
    Edge::new(
        FakeStore::default(),
        identity(),
        EdgeSession {
            shift: PublishedShift {
                no_shift_selling: Open::from_known(NoShiftSelling::Refuse),
                ..PublishedShift::default()
            },
            ..EdgeSession::bootstrap()
        },
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
}

#[test]
fn a_store_that_refuses_selling_without_a_shift_starts_no_sale_until_one_opens() {
    pos_fakes::executor::run_ready(async {
        let edge = edge_refusing_sales_without_a_shift();
        let table = TableId::new(Ulid::from_u128(140));

        let seated = edge.seat_table(actor(), table, None).await;
        assert!(matches!(seated, Err(super::AppError::OpenShiftRequired)));
        assert_eq!(
            edge.table_state(table),
            TableState::Free,
            "nobody was seated"
        );
        let counter = edge
            .open_counter_order(actor(), SalesChannel::Takeaway)
            .await;
        assert!(matches!(counter, Err(super::AppError::OpenShiftRequired)));

        edge.open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");
        edge.seat_table(actor(), table, None)
            .await
            .expect("seats once a shift is open");
        edge.open_counter_order(actor(), SalesChannel::Takeaway)
            .await
            .expect("starts a counter order once a shift is open");
    });
}

#[test]
fn a_store_that_refuses_selling_without_a_shift_takes_no_payment_after_its_shift_closes() {
    pos_fakes::executor::run_ready(async {
        let edge = edge_refusing_sales_without_a_shift();
        let table = TableId::new(Ulid::from_u128(141));
        let shift = edge
            .open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let bill = edge.open_bill(actor(), table).await.expect("opens a bill");
        edge.count_shift(actor(), shift.shift_id, 500_000)
            .await
            .expect("counts");
        edge.close_shift(actor(), shift.shift_id)
            .await
            .expect("closes");

        let refused = edge
            .settle_bill(actor(), bill.bill_id, vec![cash(165_000)], None)
            .await;
        assert!(matches!(refused, Err(super::AppError::OpenShiftRequired)));

        edge.open_shift(actor(), vnd(500_000))
            .await
            .expect("opens the next shift");
        edge.settle_bill(actor(), bill.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles inside a shift");
    });
}

#[test]
fn a_store_on_the_default_sells_and_takes_payment_without_a_shift_as_before() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(142));
        edge.seat_table(actor(), table, None)
            .await
            .expect("seats with no shift open");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let bill = edge.open_bill(actor(), table).await.expect("opens a bill");
        edge.settle_bill(actor(), bill.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles with no shift open");
    });
}

#[test]
fn closing_without_counting_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let opened = edge
            .open_shift(actor(), vnd(500_000))
            .await
            .expect("opens a shift");
        // The close is blind: it cannot skip the count. Open -> Close is not a legal move.
        let refused = edge.close_shift(actor(), opened.shift_id).await;
        assert!(matches!(refused, Err(super::AppError::Domain(_))));
    });
}

#[test]
fn counting_an_unknown_shift_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let refused = edge
            .count_shift(actor(), ShiftId::new(Ulid::from_u128(9)), 1)
            .await;
        assert!(matches!(refused, Err(super::AppError::UnknownShift)));
    });
}

#[test]
fn a_new_shift_opens_after_the_previous_one_closes() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let first = edge
            .open_shift(actor(), vnd(500_000))
            .await
            .expect("opens the first shift");
        edge.count_shift(actor(), first.shift_id, 500_000)
            .await
            .expect("counts");
        edge.close_shift(actor(), first.shift_id)
            .await
            .expect("closes the first shift");
        // With the first shift closed, a new one may open.
        edge.open_shift(actor(), vnd(300_000))
            .await
            .expect("opens the next shift");
    });
}

/// A drawer open past its day's end is warned about once a day, on its own record, so the
/// memory of the warning is no larger than the drawers (ADR-0167 decision 11).
#[test]
fn a_drawer_past_its_day_is_warned_about_once_a_day() {
    let mut projection = super::Projection::default();
    let day = |of| BusinessDate::from_ymd(2026, 10, of).expect("a date");
    let opened = Timestamp::from_milliseconds_since_epoch(0).expect("a valid instant");
    let shift_id = ShiftId::new(Ulid::from_u128(7));
    let float = Money::zero(CurrencyCode::VND);
    projection.open_shift_record(
        shift_id,
        super::ShiftRecord::opened(float, CurrencyCode::VND, (None, None), opened, day(6)),
    );
    assert!(
        projection.warn_day_ended(shift_id, day(7)),
        "the first time on the 7th"
    );
    assert!(!projection.warn_day_ended(shift_id, day(7)), "once a day");
    assert!(
        projection.warn_day_ended(shift_id, day(8)),
        "and again the next day"
    );
    assert!(
        !projection.warn_day_ended(ShiftId::new(Ulid::from_u128(8)), day(8)),
        "a shift it does not hold is nothing to warn about"
    );
}
