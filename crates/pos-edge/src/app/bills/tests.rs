// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The bill's unit tests: opening, settling, tax by channel, tips, voiding, splitting and merging.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::capability::{Capability, CapabilityContext};
use pos_fakes::FakeStore;
use pos_ports::event_store::EventQuery;
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_ports::{EventStore, Transactional, TxContext};
use pos_proto::events::{BillingBillSettled, BillingPaymentCaptured};
use pos_proto::ids::{BillId, EmployeeId, OrderId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable, TaxRounding};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, EventPayload, EventType, PaymentMethod, SalesChannel, TableState};

use crate::app::tests::{a_counter_order, a_line, actor, cash, edge, identity, vnd};
use crate::app::{BuyerDetails, Edge, EdgeSession, LineDraft};
use crate::receipt::InMemoryReceipts;

/// Seats a table, sells one line at `unit_price`, opens its bill and settles it for exactly what
/// it comes to; the bill, settled.
async fn a_settled_bill_at(edge: &Edge<FakeStore>, table: TableId, unit_price: i64) -> BillId {
    edge.seat_table(actor(), table, None).await.expect("seats");
    let line = LineDraft {
        unit_price: vnd(unit_price),
        line_total: vnd(unit_price),
        ..a_line()
    };
    edge.add_line(actor(), table, line).await.expect("adds");
    let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
    let owed = edge
        .bill_totals(opened.bill_id)
        .expect("assembles")
        .total_due;
    edge.settle_bill(actor(), opened.bill_id, vec![cash(owed.amount_minor)], None)
        .await
        .expect("settles");
    opened.bill_id
}

/// ADR-0160: a store whose `locale` sets `TAX_ROUNDING_DOWN` drops the half đồng of tax a store
/// on half-up rounds up, so it settles the same bill one đồng lower, and records it so.
#[test]
fn a_store_that_rounds_tax_down_settles_half_a_unit_of_tax_one_unit_lower() {
    pos_fakes::executor::run_ready(async {
        // 10 % of 150,005 is 15,000.5.
        let recorded = |edge: &Edge<FakeStore>, bill_id: BillId| {
            let projection = edge.lock_projection();
            let receipt = projection.settled.get(&bill_id).expect("recorded");
            (receipt.tax_total, receipt.total_due)
        };
        let half_up = edge();
        let bill = a_settled_bill_at(&half_up, TableId::new(Ulid::from_u128(341)), 150_005).await;
        assert_eq!(recorded(&half_up, bill), (vnd(15_001), vnd(165_006)));

        let down = edge();
        down.apply_session(EdgeSession {
            tax_rounding: TaxRounding::Down,
            ..EdgeSession::bootstrap()
        });
        let bill = a_settled_bill_at(&down, TableId::new(Ulid::from_u128(342)), 150_005).await;
        assert_eq!(recorded(&down, bill), (vnd(15_000), vnd(165_005)));
    });
}

#[test]
fn a_bill_opens_and_settles_with_a_gapless_receipt() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(300));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");

        // Opening the bill requests it: the table moves to awaiting payment.
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
        assert_eq!(opened.state, BillState::Open);
        assert_eq!(opened.table_state, Some(TableState::AwaitingPayment));
        assert_eq!(edge.table_state(table), TableState::AwaitingPayment);

        // One 150k line at the 10% standard rate is 165k owed.
        let settled = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles");
        assert_eq!(settled.state, BillState::Settled);
        assert_eq!(settled.total_due, Some(vnd(165_000)));
        assert_eq!(
            settled.receipt_number,
            Some(1),
            "the first receipt is number one"
        );
        assert!(
            settled.print_receipt,
            "settling asks for a receipt to print"
        );
        // The table cycles to needs-cleaning, then a clean releases it.
        assert_eq!(settled.table_state, Some(TableState::NeedsCleaning));
        let cleaned = edge.clean_table(actor(), table).await.expect("cleans");
        assert_eq!(cleaned.state, TableState::Free);
    });
}

/// A corporate buyer is filed beside the log, and the settled event carries only the id.
///
/// The property [ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md) exists for: the
/// name and the 登録番号 are in the subject store, the event has a `SubjectId`, and the money
/// figures are on the event where they can never be scrubbed.
#[test]
fn a_b2b_settle_files_the_buyer_beside_the_log_and_puts_only_an_id_on_the_event() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(320));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");

        let buyer = BuyerDetails {
            name: "Kabushiki Kaisha Reiwa".to_owned(),
            tax_code: Some("T1234567890123".to_owned()),
            address: Some("1-1 Marunouchi, Chiyoda".to_owned()),
            email: None,
        };
        edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], Some(&buyer))
            .await
            .expect("settles");

        let settled = settled_payload(&edge).await;
        let subject_id = settled
            .buyer_subject_id
            .expect("the settled event names the buyer's subject");

        let record = edge
            .store
            .fetch(edge.identity.store_id, subject_id)
            .await
            .expect("the subject store answers")
            .expect("the buyer was filed in the same transaction as the settle");
        assert_eq!(
            record.fields.get("name").map(String::as_str),
            Some("Kabushiki Kaisha Reiwa")
        );
        assert_eq!(
            record.fields.get("tax_code").map(String::as_str),
            Some("T1234567890123")
        );
        assert!(
            !record.fields.contains_key("email"),
            "a field the cashier left blank is absent, not stored empty"
        );
        assert_eq!(
            settled.total_due,
            vnd(165_000),
            "the money is on the event, where erasing the buyer cannot reach it"
        );
    });
}

/// An ordinary retail sale records nobody. The overwhelming majority of bills, and they must
/// stay exactly as cheap as they were before ADR-0107.
#[test]
fn an_ordinary_sale_records_no_subject_at_all() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(321));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
        edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles");

        assert!(
            settled_payload(&edge).await.buyer_subject_id.is_none(),
            "a retail sale creates no processing activity nobody asked for"
        );
    });
}

/// The retention sweep scrubs a buyer past the store's window, and the bill still reconciles.
///
/// This is the guarantee that lets an immutable financial log and a right to erasure coexist:
/// the person goes, the takings stay.
#[test]
fn the_retention_sweep_scrubs_a_buyer_and_the_takings_still_add_up() {
    pos_fakes::executor::run_ready(async {
        let session = EdgeSession {
            // Zero days would mean "scrub on print" and the config path refuses it; one day is
            // the shortest window a store can actually publish, and the fake clock is fixed well
            // past the epoch, so a one-day window is already expired.
            retention_days: 1,
            ..EdgeSession::bootstrap()
        };
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(322));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
        let buyer = BuyerDetails {
            name: "Reliance Retail Limited".to_owned(),
            tax_code: Some("29ABCDE1234F1Z5".to_owned()),
            address: None,
            email: None,
        };
        edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], Some(&buyer))
            .await
            .expect("settles");

        // The record is collected at the settle's instant, which is `now`, so nothing is due yet.
        assert_eq!(
            edge.sweep_expired_subjects().await.expect("sweeps"),
            0,
            "a record inside the window is left alone"
        );

        // Age it past the window by rewriting its collection stamp — the clock cannot be moved
        // and the point under test is the cutoff, not the clock.
        let settled = settled_payload(&edge).await;
        let subject_id = settled.buyer_subject_id.expect("a buyer was recorded");
        let aged = SubjectRecord::new(
            Timestamp::from_milliseconds_since_epoch(0).expect("the epoch is representable"),
            buyer.fields(),
        );
        let mut tx = edge.store.begin().await.expect("begins");
        edge.store
            .record(&mut tx, edge.identity.store_id, subject_id, &aged)
            .await
            .expect("re-files the aged record");
        tx.commit().await.expect("commits");

        assert_eq!(
            edge.sweep_expired_subjects().await.expect("sweeps"),
            1,
            "a record past the window is scrubbed"
        );
        let scrubbed = edge
            .store
            .fetch(edge.identity.store_id, subject_id)
            .await
            .expect("the subject store answers")
            .expect("the row survives, scrubbed rather than deleted");
        assert!(scrubbed.is_masked());
        assert!(
            scrubbed
                .fields
                .values()
                .all(|value| value == pos_ports::subject_store::REDACTION)
        );
        assert_eq!(
            settled_payload(&edge).await.total_due,
            vnd(165_000),
            "the day's takings are unchanged by erasing the buyer"
        );
    });
}

/// The one `billing.bill.settled` in this store's log.
async fn settled_payload(edge: &Edge<FakeStore>) -> BillingBillSettled {
    let events = edge
        .store
        .read(&EventQuery::first(
            edge.identity.store_id,
            NonZeroU32::new(100).expect("100 is not zero"),
        ))
        .await
        .expect("reads the log");
    events
        .iter()
        .filter(|envelope| envelope.event_type.as_str() == BillingBillSettled::EVENT_TYPE.as_str())
        .map(|envelope| {
            envelope
                .data
                .decode::<BillingBillSettled>()
                .expect("the settled payload decodes")
        })
        .next()
        .expect("the log holds a settled bill")
}

#[test]
fn a_bill_settles_split_across_cash_and_card() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(301));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");

        // 65k cash + 100k card == 165k owed: a split tender still sums exactly to the total.
        let card = Payment {
            method: PaymentMethod::Card,
            tendered: vnd(100_000),
            applied_to_bill: vnd(100_000),
            tip: vnd(0),
        };
        let settled = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(65_000), card], None)
            .await
            .expect("settles across two tenders");
        assert_eq!(settled.state, BillState::Settled);
        assert_eq!(settled.receipt_number, Some(1));
    });
}

#[test]
fn settling_twice_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(302));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
        edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("first settle");

        // A settled bill is terminal: a second settle is refused (a refund is a new movement).
        let refused = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await;
        assert!(matches!(refused, Err(crate::app::AppError::Domain(_))));
    });
}

#[test]
fn underpaying_a_bill_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(303));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");

        // 150k applied against 165k owed does not sum to the total — the invariant refuses it.
        let refused = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(150_000)], None)
            .await;
        assert!(matches!(refused, Err(crate::app::AppError::Domain(_))));
        // Nothing settled, so no receipt was consumed and the bill is still open.
        let again = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("the still-open bill settles");
        assert_eq!(
            again.receipt_number,
            Some(1),
            "the refused attempt took no number"
        );
    });
}

#[test]
fn opening_a_bill_on_an_unseated_table_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(304));
        // A free table cannot be asked for its bill.
        let refused = edge.open_bill(actor(), table).await;
        assert!(matches!(refused, Err(crate::app::AppError::Domain(_))));
    });
}

/// The defect [ADR-0093](../../docs/adr/0093-bill-keyed-on-order.md) closes.
///
/// This flow could not be written at all before the bill was keyed on the order: `open_bill`
/// took a `TableId`, gated on that table being `Occupied` and resolved the order through
/// `order_for_table`, and a takeaway order is tableless by design (ADR-0064). So a store could
/// accept, price, queue and fire a counter order — and had no way to take money for it.
#[test]
fn a_counter_order_opens_a_bill_and_settles_with_no_table_anywhere() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let order_id = a_counter_order(&edge).await;

        // The cashier reads the figure first, and it is the same one the settle demands — the
        // order-keyed read exists because the table-keyed one cannot answer for this order.
        let owed = edge.order_totals(order_id).expect("the order assembles");
        assert_eq!(owed.total_due, vnd(165_000), "150,000 plus 10% tax");

        let opened = edge
            .open_bill_for_order(actor(), order_id)
            .await
            .expect("a tableless order opens a bill");
        assert_eq!(opened.state, BillState::Open);
        assert_eq!(
            opened.table_state, None,
            "there is no table, so there is no table state to report"
        );

        let settled = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("and the counter sale settles");
        assert_eq!(settled.state, BillState::Settled);
        assert_eq!(
            settled.receipt_number,
            Some(1),
            "a counter sale draws on the same gapless per-store receipt counter (ADR-0025)"
        );
        assert_eq!(settled.total_due, Some(vnd(165_000)));
        assert_eq!(settled.table_state, None, "and cycles no table");
    });
}

/// The defect roadmap **B1.3** closes, proved against the durable log rather than a return
/// value — the log is what the shift roll-up and the tip-out are rebuilt from, and it was the
/// half that was wrong.
///
/// Two figures were broken on every tipped sale ever captured. `tip_amount` was written as a
/// literal zero, because tips arrived as a `Vec<Money>` beside the payments and no payment knew
/// which tip was its own — so a store could take tips all night and its own log said nobody
/// tipped. And `change_given` was `tendered − applied_to_bill`, which ignores the tip: on this
/// sale it read 35,000 against a true 15,000, telling the till to hand back money the guest had
/// just left behind.
#[test]
fn a_captured_payment_records_its_own_tip_and_the_change_left_after_it() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let edge = Edge::new(
            store.clone(),
            identity(),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let order_id = a_counter_order(&edge).await;
        let opened = edge
            .open_bill_for_order(actor(), order_id)
            .await
            .expect("opens");

        // 165,000 owed. The guest hands over 200,000 and leaves 20,000.
        let tipped = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(200_000),
            applied_to_bill: vnd(165_000),
            tip: vnd(20_000),
        };
        edge.settle_bill(actor(), opened.bill_id, vec![tipped], None)
            .await
            .expect("a tipped cash sale settles");

        let captured = only_captured_payment(&store).await;
        assert_eq!(
            captured.tip_amount,
            vnd(20_000),
            "the tip the guest left, not a hardcoded zero"
        );
        assert_eq!(
            captured.change_given,
            vnd(15_000),
            "200,000 − 165,000 applied − 20,000 tip; not 35,000"
        );
        assert_eq!(captured.applied_to_bill, vnd(165_000));
        assert_eq!(captured.tendered, vnd(200_000));
    });
}

/// A store that has not turned tips on refuses to take one (§10, `tips_enabled`).
///
/// The untipped settle in the same test is the half that matters as much: the gate refuses
/// *taking the money*, never the sale, so a store with tips off trades exactly as it did.
#[test]
fn a_store_with_tips_disabled_refuses_a_tip_but_still_settles() {
    pos_fakes::executor::run_ready(async {
        let session = EdgeSession {
            capabilities: CapabilityContext::NONE.with(Capability::Tables),
            ..EdgeSession::bootstrap()
        };
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let order_id = a_counter_order(&edge).await;
        let opened = edge
            .open_bill_for_order(actor(), order_id)
            .await
            .expect("opens");

        let refused = edge
            .settle_bill(
                actor(),
                opened.bill_id,
                vec![Payment {
                    method: PaymentMethod::Cash,
                    tendered: vnd(185_000),
                    applied_to_bill: vnd(165_000),
                    tip: vnd(20_000),
                }],
                None,
            )
            .await;
        assert!(
            matches!(refused, Err(crate::app::AppError::Domain(_))),
            "a store that does not take tips refuses one, rather than pocketing it silently"
        );

        edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("and the same bill settles untipped, exactly as before");
    });
}

/// The single `billing.payment.captured` in a store's log.
///
/// Read back through the [`EventStore`] port rather than from a return value: the captured
/// payment is not in `BillView`, and the log is what the cloud's roll-ups and the store's own
/// replay actually consume.
async fn only_captured_payment(store: &FakeStore) -> BillingPaymentCaptured {
    let envelopes = store
        .read(&EventQuery::first(
            identity().store_id,
            NonZeroU32::new(1_000).expect("1000 is not zero"),
        ))
        .await
        .expect("the log reads");
    let mut captured: Vec<BillingPaymentCaptured> = envelopes
        .iter()
        .filter(|envelope| envelope.event_type.known() == Some(EventType::BillingPaymentCaptured))
        .map(|envelope| envelope.data.decode().expect("a captured payment decodes"))
        .collect();
    assert_eq!(captured.len(), 1, "one tender, one captured payment");
    captured.remove(0)
}

/// A store that taxes takeaway at 5% and dine-in at 10%, over a caller-supplied log.
///
/// [`EdgeSession::bootstrap`] leaves `sales_channel` at [`SalesChannel::DineIn`] and this
/// builder does not touch it, which is the whole point: the only way a takeaway order can come
/// out at 5% is if the rate is looked up from *that order's* channel rather than the session's.
fn edge_taxing_takeaway_at_five(store: FakeStore) -> Edge<FakeStore> {
    let standard = EdgeSession::standard_tax_class();
    let rates = TaxRateTable::new()
        .with(standard, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(standard, SalesChannel::Takeaway, TaxRate::from_percent(5));
    Edge::new(
        store,
        identity(),
        EdgeSession::bootstrap().with_tax_rates(rates),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
}

/// The defect roadmap **B1.2** closes: two orders on one store, taxed at two rates.
///
/// Before the fix both bills read `session.sales_channel` — a store-wide constant — so the
/// takeaway order was charged dine-in tax. Nothing failed and nothing logged; the only symptom
/// was 7,500 dong of tax the store did not owe, on every counter sale, for as long as it
/// traded. Reinstating the bug (passing `session.sales_channel` in `order_totals`) makes the
/// first assertion below read 165,000.
#[test]
fn each_order_is_taxed_at_the_rate_of_the_channel_it_came_in_on() {
    pos_fakes::executor::run_ready(async {
        let edge = edge_taxing_takeaway_at_five(FakeStore::default());

        // A relayed counter order: channel Takeaway, no table, on a session that says dine-in.
        let takeaway = a_counter_order(&edge).await;
        assert_eq!(
            edge.order_totals(takeaway).expect("assembles").total_due,
            vnd(157_500),
            "150,000 plus takeaway's 5% — not the session's dine-in 10%"
        );

        // The same store, the same menu item, seated at a table: dine-in's rate.
        let table = TableId::new(Ulid::from_u128(361));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let dine_in = edge.add_line(actor(), table, a_line()).await.expect("adds");
        assert_eq!(
            edge.order_totals(dine_in.order_id)
                .expect("assembles")
                .total_due,
            vnd(165_000),
            "150,000 plus dine-in's 10%, from the same store and the same session"
        );
    });
}

/// The replay arm, which is the half a restart depends on.
///
/// The channel lives in the projection, and the projection is a cache rebuilt from the log
/// ([ADR-0025](../../docs/adr/0025-event-sourced-edge.md)). If `sales.order.opened` were not
/// folded back, a box that restarted between a takeaway order being placed and its bill being
/// settled would fall back to the session channel and charge dine-in tax — the original defect,
/// surviving the fix and appearing only after a crash.
#[test]
fn a_restart_still_knows_which_channel_an_order_came_in_on() {
    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        let takeaway = {
            let edge = edge_taxing_takeaway_at_five(store.clone());
            a_counter_order(&edge).await
        };

        // "Restart": a fresh edge over the same log, then the replay.
        let edge = edge_taxing_takeaway_at_five(store.clone());
        edge.rebuild().await.expect("rebuilds from the log");
        assert_eq!(
            edge.order_totals(takeaway).expect("assembles").total_due,
            vnd(157_500),
            "the channel came back off the log, so the rate did too"
        );
    });
}

/// The rule that used to hold as a side effect of the table state machine (ADR-0093).
///
/// A table can only be `Occupied` once, so `decide_table(RequestBill)` refused a second request
/// on its way through and nothing had to say so. Keying on the order removes that accident, and
/// dropping the rule silently would mean two receipt numbers and a guest charged twice for one
/// meal — so both entry points are checked here, not just the new one.
#[test]
fn a_second_bill_on_one_order_is_refused_from_the_counter_and_from_the_floor() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();

        let order_id = a_counter_order(&edge).await;
        edge.open_bill_for_order(actor(), order_id)
            .await
            .expect("the first bill opens");
        let refused = edge.open_bill_for_order(actor(), order_id).await;
        assert!(
            matches!(refused, Err(crate::app::AppError::BillAlreadyOpen)),
            "the order index is all that stands between one bill and two on the counter"
        );

        let table = TableId::new(Ulid::from_u128(305));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        edge.open_bill(actor(), table).await.expect("opens a bill");
        let refused = edge.open_bill(actor(), table).await;
        assert!(
            matches!(refused, Err(crate::app::AppError::BillAlreadyOpen)),
            "the floor is refused too, and now says why rather than reporting an illegal table                  transition"
        );
    });
}

#[test]
fn an_order_the_edge_does_not_know_cannot_be_billed() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let refused = edge
            .open_bill_for_order(actor(), OrderId::new(Ulid::from_u128(0x00C0_FFEE)))
            .await;
        assert!(matches!(refused, Err(crate::app::AppError::UnknownOrder)));
    });
}

#[test]
fn settling_an_unknown_bill_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let refused = edge
            .settle_bill(
                actor(),
                BillId::new(Ulid::from_u128(999)),
                vec![cash(1)],
                None,
            )
            .await;
        assert!(matches!(refused, Err(crate::app::AppError::UnknownBill)));
    });
}

#[test]
fn a_store_that_prints_no_receipt_on_settle_settles_and_numbers_the_bill_without_one() {
    pos_fakes::executor::run_ready(async {
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            EdgeSession {
                printing: pos_proto::printing::PublishedPrinting {
                    receipt_printed_on_settle: Some(false),
                    ..Default::default()
                },
                ..EdgeSession::bootstrap()
            },
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(301));
        edge.seat_table(actor(), table, None).await.expect("seats");
        edge.add_line(actor(), table, a_line()).await.expect("adds");
        let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
        let settled = edge
            .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], None)
            .await
            .expect("settles");
        assert_eq!(
            settled.receipt_number,
            Some(1),
            "the bill is numbered all the same"
        );
        assert!(!settled.print_receipt, "and no receipt prints (ADR-0160)");
    });
}

/// A merge into a bill that names no lines — every bill opened before ADR-0128 recorded them —
/// used to re-lock the projection on the thread already holding it, and hang the store.
#[test]
fn merging_into_a_bill_that_names_no_lines_does_not_hang_the_store() {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(430));
            edge.seat_table(actor(), table, None).await.expect("seats");
            let mut lines = Vec::new();
            for _ in 0..2 {
                lines.push(
                    edge.add_line(actor(), table, a_line())
                        .await
                        .expect("adds")
                        .order_line_id,
                );
            }
            let bill = edge.open_bill(actor(), table).await.expect("bills");
            let parts = edge
                .split_bill(actor(), bill.bill_id, vec![vec![lines[0]], vec![lines[1]]])
                .await
                .expect("splits");
            // The shape a pre-ADR-0128 bill rebuilds to: no lines named.
            if let Some(record) = edge.lock_projection().bills.get_mut(&parts[0]) {
                record.order_line_ids.clear();
            }
            let merged = edge.merge_bills(actor(), parts[0], vec![parts[1]]).await;
            let _ = done.send(merged.is_ok());
        });
    });
    let merged = finished
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the merge returned rather than deadlocking on the projection lock");
    assert!(merged, "the merge succeeds");
}

/// A table already stuck on a bill on nothing is recovered once (ADR-0163): a manager voids the
/// empty bill, and the table is released. An edge before ADR-0163 opened that bill when asked
/// for the bill of an empty table, and nothing could settle it; the stuck log is written here
/// as that edge wrote it, and replayed by a new one.
#[test]
fn a_table_stuck_on_an_empty_bill_is_recovered_by_a_void_and_a_release() {
    use crate::app::{AppError, Approval, StaffAuth, StaffRoster};
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    use pos_core::error::DomainError;
    use pos_core::permission::{Permission, PermissionSet};
    use pos_proto::events::BillingBillOpened;
    use pos_proto::reason_codes::{PublishedReasonCodes, ReasonAction};

    fn session() -> EdgeSession {
        let pin_phc = Argon2::default()
            .hash_password_with_salt(b"4417", b"a-fixed-test-slt")
            .expect("hash")
            .to_string();
        let mut staff = StaffRoster::new();
        staff.insert(
            "MGR-1",
            StaffAuth {
                employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
                permissions: PermissionSet::EMPTY.with(Permission::VoidBill),
                permissions_with_approval: PermissionSet::EMPTY,
                discount_ceiling: None,
                pin_phc: Some(pin_phc),
            },
        );
        let mut session = EdgeSession::bootstrap();
        session.staff = staff;
        session
    }
    let over = |store: FakeStore| {
        Edge::new(
            store,
            identity(),
            session(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds")
    };
    let table = TableId::new(Ulid::from_u128(40));
    let bill_id = BillId::new(Ulid::from_u128(77));

    pos_fakes::executor::run_ready(async {
        let store = FakeStore::default();
        {
            let old = over(store.clone());
            old.seat_table(actor(), table, None).await.expect("seats");
            let order_id = old.order_for_table(table).expect("an order opened");
            let ctx = old.decision_ctx(actor()).expect("a decision context");
            old.commit_and_publish(
                &ctx,
                &BillingBillOpened {
                    bill_id,
                    order_id,
                    order_line_ids: Vec::new(),
                    fee_rules: Vec::new(),
                },
            )
            .await
            .expect("the old edge opened a bill on nothing");
        }

        let edge = over(store);
        edge.rebuild().await.expect("replays the stuck log");
        assert_eq!(edge.table_state(table), TableState::AwaitingPayment);
        let stuck = edge.release_table(actor(), table).await;
        assert!(
            matches!(stuck, Err(AppError::Domain(DomainError::Transition(_)))),
            "a table waiting for payment is not released, got {stuck:?}"
        );

        let reason = PublishedReasonCodes::framework_default()
            .for_action(ReasonAction::VoidBill)
            .next()
            .expect("the framework set covers voiding a bill")
            .id;
        let approval = Approval {
            code: "MGR-1".to_owned(),
            pin: "4417".to_owned(),
        };
        let voided = edge
            .void_bill(actor(), bill_id, reason, Some(&approval))
            .await
            .expect("a manager voids the empty bill");
        assert_eq!(voided, BillState::Voided);
        assert_eq!(edge.table_state(table), TableState::Occupied);

        edge.release_table(actor(), table)
            .await
            .expect("and the table is released");
        assert_eq!(edge.table_state(table), TableState::Free);
        assert!(edge.live_orders().is_empty());
    });
}
