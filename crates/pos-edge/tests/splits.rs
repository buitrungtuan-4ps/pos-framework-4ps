// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Splitting and merging a bill, over the real edge
//! ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md)).
//!
//! `billing.bill.split` and `billing.bill.merged` were defined in `pos-proto` with their exact
//! shapes since the schema was written, and **nothing ever emitted either**. A table of six who
//! wanted to pay separately was six tables opened as a workaround, or one bill and arithmetic done
//! on paper beside the till.
//!
//! Two structural things stood in the way and both are gone: a bill covers *lines* now rather than
//! an order, so there is something to partition; and `BillState` has somewhere for a split source
//! and a merged bill to go, so neither can be charged twice.
//!
//! These cases hold the properties the record promises: the parts owe what the source owed, the
//! source owes nothing and cannot be settled, a partition that does not add up is refused, and a
//! store that restarts rebuilds all of it from its own log.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::{DomainError, PartitionFault};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{
    BillId, DeviceId, EmployeeId, MenuItemId, OrderLineId, StoreId, TableId, TaxClassId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{SalesChannel, TableState};

/// Each line is 50,000 at 10%, so a bill's arithmetic is readable by eye.
const UNIT_PRICE: i64 = 50_000;

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn item() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

fn session() -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Bún chả"),
        vnd(UNIT_PRICE),
        class(),
    ));
    let rates = TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

fn edge_over(store: FakeStore) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session(),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn a_line() -> LineDraft {
    LineDraft {
        menu_item_id: item(),
        display_name: DisplayName::new("Bún chả"),
        quantity: Quantity::ONE,
        unit_price: vnd(UNIT_PRICE),
        line_total: vnd(UNIT_PRICE),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// Seats a table, sells `count` things, and opens the bill for them.
async fn a_bill_of(edge: &Edge<FakeStore>, count: usize) -> (BillId, Vec<OrderLineId>) {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    let mut lines = Vec::new();
    for _ in 0..count {
        let view = edge
            .add_line(server(), table(), a_line())
            .await
            .expect("adds");
        lines.push(view.order_line_id);
    }
    let bill = edge
        .open_bill(server(), table())
        .await
        .expect("opens the bill");
    (bill.bill_id, lines)
}

/// Cash for exactly `minor`.
fn cash(minor: i64) -> Vec<pos_core::billing::Payment> {
    vec![pos_core::billing::Payment {
        method: pos_proto::PaymentMethod::Cash,
        tendered: vnd(minor),
        applied_to_bill: vnd(minor),
        tip: Money::zero(CurrencyCode::VND),
    }]
}

/// Every event type the store holds, in order.
async fn logged(store: &FakeStore) -> Vec<String> {
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(200).expect("a positive limit"),
    );
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

/// The headline: a table of four splits two and two, and **each part owes its own half**.
///
/// Before this, what a bill owed was assembled from every line of its order, so two bills on one
/// order would each have owed all of it — the guest would have been charged twice for the same
/// meal. That is the whole reason a bill had to learn which lines it covers before a split was
/// possible at all.
#[test]
fn a_split_gives_each_part_its_own_lines_and_its_own_total() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let (source, lines) = a_bill_of(&edge, 4).await;

        // 4 × 50,000 at 10% is 220,000 before anybody splits anything.
        assert_eq!(
            edge.bill_totals(source).expect("totals").total_due,
            vnd(220_000)
        );

        let parts = edge
            .split_bill(
                server(),
                source,
                vec![vec![lines[0], lines[1]], vec![lines[2], lines[3]]],
            )
            .await
            .expect("a partition of the bill's own lines");
        assert_eq!(parts.len(), 2);

        for part in &parts {
            assert_eq!(
                edge.bill_totals(*part).expect("totals").total_due,
                vnd(110_000),
                "each part owes two lines and the tax on them"
            );
        }

        // And the log says which lines went into which, so the arithmetic can be checked from the
        // store's own record rather than from a live projection (decision 9).
        let events = logged(&store).await;
        assert_eq!(
            events
                .iter()
                .filter(|kind| *kind == "billing.bill.opened")
                .count(),
            3,
            "the source plus one per part: {events:?}"
        );
        assert!(events.contains(&"billing.bill.split".to_owned()));
    });
}

/// The source is terminal, so the same food cannot be charged twice.
///
/// This is what `SPLIT` being a state rather than projection bookkeeping buys: the refusal sits
/// where `SETTLED`'s refusal already is, and no permission or PIN can get past it.
#[test]
fn a_split_source_owes_nothing_and_can_never_be_settled() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (source, lines) = a_bill_of(&edge, 2).await;
        edge.split_bill(server(), source, vec![vec![lines[0]], vec![lines[1]]])
            .await
            .expect("splits");

        let refused = edge
            .settle_bill(
                server(),
                source,
                vec![pos_core::billing::Payment {
                    method: pos_proto::PaymentMethod::Cash,
                    tendered: vnd(110_000),
                    applied_to_bill: vnd(110_000),
                    tip: Money::zero(CurrencyCode::VND),
                }],
                None,
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(pos_edge::AppError::Domain(DomainError::Transition(_)))
            ),
            "a split source is terminal, got {refused:?}"
        );

        // And it cannot be split again either — there is nothing left on it to partition.
        let again = edge
            .split_bill(server(), source, vec![vec![lines[0]], vec![lines[1]]])
            .await;
        assert!(matches!(
            again,
            Err(pos_edge::AppError::Domain(DomainError::Transition(_)))
        ));
    });
}

/// A proposed split that is not a partition is refused, and says which rule it broke.
#[test]
fn a_split_that_does_not_add_up_is_refused_over_the_edge() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (source, lines) = a_bill_of(&edge, 3).await;

        // A line left behind would be food nobody is charged for.
        let left_behind = edge
            .split_bill(server(), source, vec![vec![lines[0]], vec![lines[1]]])
            .await;
        assert!(
            matches!(
                left_behind,
                Err(pos_edge::AppError::Domain(DomainError::NotAPartition {
                    reason: PartitionFault::LinesDoNotPartition
                }))
            ),
            "got {left_behind:?}"
        );

        // A line in two parts would charge the same food twice.
        let twice = edge
            .split_bill(
                server(),
                source,
                vec![vec![lines[0], lines[1]], vec![lines[1], lines[2]]],
            )
            .await;
        assert!(matches!(
            twice,
            Err(pos_edge::AppError::Domain(DomainError::NotAPartition {
                reason: PartitionFault::LinesDoNotPartition
            }))
        ));

        // The bill is untouched by either refusal, and still owes all three lines.
        assert_eq!(
            edge.bill_totals(source).expect("totals").total_due,
            vnd(165_000)
        );
    });
}

/// Two bills on one table fold into one, and the survivor owes both.
///
/// The target keeps its identity because it is the bill the cashier is standing in front of, so a
/// merge is not a split in reverse.
#[test]
fn a_merge_folds_bills_into_the_one_the_cashier_is_holding() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (source, lines) = a_bill_of(&edge, 4).await;
        let parts = edge
            .split_bill(
                server(),
                source,
                vec![vec![lines[0], lines[1]], vec![lines[2], lines[3]]],
            )
            .await
            .expect("splits");

        let survivor = edge
            .merge_bills(server(), parts[0], vec![parts[1]])
            .await
            .expect("two open bills on one table fold together");
        assert_eq!(survivor, parts[0], "the target keeps its identity");
        assert_eq!(
            edge.bill_totals(survivor).expect("totals").total_due,
            vnd(220_000),
            "the survivor owes what the source owed before anybody split it"
        );

        // The absorbed bill is terminal, so its guest cannot be charged a second time.
        let refused = edge.merge_bills(server(), survivor, vec![parts[1]]).await;
        assert!(matches!(
            refused,
            Err(pos_edge::AppError::Domain(DomainError::Transition(_)))
        ));
    });
}

/// A merge into itself is refused, which is the one arithmetic mistake this record exists to make
/// impossible: the bill's own lines would be counted twice.
#[test]
fn a_bill_cannot_be_merged_into_itself() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (bill, _) = a_bill_of(&edge, 2).await;
        let refused = edge.merge_bills(server(), bill, vec![bill]).await;
        assert!(
            matches!(refused, Err(pos_edge::AppError::BillsOnDifferentTables)),
            "got {refused:?}"
        );
        assert_eq!(
            edge.bill_totals(bill).expect("totals").total_due,
            vnd(110_000),
            "and the bill still owes what it did"
        );
    });
}

/// A store that restarts rebuilds the split from its own log.
///
/// This is the property decision 9 was added for: without the line set in `bill.opened`, the log
/// would say which bills a split produced and never which lines went into which, so a store
/// replaying after a restart could not rebuild which bill owed what — and the parts' totals would
/// come back wrong, or as the whole order each.
#[test]
fn a_split_is_rebuilt_from_the_log() {
    run_ready(async {
        let store = FakeStore::default();
        let parts = {
            let edge = edge_over(store.clone());
            let (source, lines) = a_bill_of(&edge, 4).await;
            edge.split_bill(
                server(),
                source,
                vec![vec![lines[0]], vec![lines[1], lines[2], lines[3]]],
            )
            .await
            .expect("splits")
        };

        // A second edge over the same log: everything it knows, it learned by replaying.
        let rebuilt = edge_over(store);
        rebuilt.rebuild().await.expect("replays the log");

        assert_eq!(
            rebuilt.bill_totals(parts[0]).expect("totals").total_due,
            vnd(55_000),
            "the one-line part"
        );
        assert_eq!(
            rebuilt.bill_totals(parts[1]).expect("totals").total_due,
            vnd(165_000),
            "and the three-line part"
        );
    });
}

/// **Every part settles**, and the table waits for the last one.
///
/// Settling the first part used to move the shared table to `NEEDS_CLEANING`, and every other
/// part was then refused by the table machine (`NEEDS_CLEANING` has no settle), so a table that
/// split could take only one person's money. The order has to stay owing until the last part is
/// paid, and a restart has to agree at every step.
#[test]
fn every_part_of_a_split_settles_and_the_table_waits_for_the_last() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let (source, lines) = a_bill_of(&edge, 2).await;
        let parts = edge
            .split_bill(server(), source, vec![vec![lines[0]], vec![lines[1]]])
            .await
            .expect("splits");

        let first = edge
            .settle_bill(server(), parts[0], cash(55_000), None)
            .await
            .expect("the first part settles");
        assert_eq!(first.table_state, Some(TableState::AwaitingPayment));
        assert_eq!(edge.live_orders().len(), 1, "half the table still owes");
        let rebuilt = edge_over(store.clone());
        rebuilt.rebuild().await.expect("replays the log");
        assert_eq!(rebuilt.table_state(table()), TableState::AwaitingPayment);
        assert_eq!(rebuilt.live_orders().len(), 1);

        let last = edge
            .settle_bill(server(), parts[1], cash(55_000), None)
            .await
            .expect("and so does the second");
        assert_eq!(last.table_state, Some(TableState::NeedsCleaning));
        assert!(edge.live_orders().is_empty(), "the whole table has paid");
        let rebuilt = edge_over(store);
        rebuilt.rebuild().await.expect("replays the log");
        assert_eq!(rebuilt.table_state(table()), TableState::NeedsCleaning);
        assert!(rebuilt.live_orders().is_empty());
    });
}

/// A split merged back and settled is **finished**. The order pointed at the last part opened,
/// which the merge absorbed, so the paid order stayed on every till's live list.
#[test]
fn a_split_merged_back_and_settled_leaves_the_live_list() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let (source, lines) = a_bill_of(&edge, 2).await;
        let parts = edge
            .split_bill(server(), source, vec![vec![lines[0]], vec![lines[1]]])
            .await
            .expect("splits");
        edge.merge_bills(server(), parts[0], vec![parts[1]])
            .await
            .expect("merges back");

        let settled = edge
            .settle_bill(server(), parts[0], cash(110_000), None)
            .await
            .expect("settles the merged bill");
        assert_eq!(settled.table_state, Some(TableState::NeedsCleaning));
        assert!(edge.live_orders().is_empty());

        let rebuilt = edge_over(store);
        rebuilt.rebuild().await.expect("replays the log");
        assert!(rebuilt.live_orders().is_empty());
        assert_eq!(rebuilt.table_state(table()), TableState::NeedsCleaning);
    });
}

/// **A split table is read part by part, and as a whole.** Each part answers for itself by its own
/// id, the table answers with what its open parts owe together, and the live read names every part
/// still open.
///
/// Before these reads, the table's check answered with the newest open part alone, so a table split
/// two ways quoted half of what it owed. And the live read named only that newest part, so a till
/// that reloaded mid-split paid it, found the table still awaiting payment, and had no id to settle
/// the rest with — asking for another bill is refused while one is open.
#[test]
fn a_split_table_is_read_part_by_part_and_as_a_whole() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let (source, lines) = a_bill_of(&edge, 3).await;
        let parts = edge
            .split_bill(
                server(),
                source,
                vec![vec![lines[0]], vec![lines[1], lines[2]]],
            )
            .await
            .expect("splits");

        let first = edge.bill_check(parts[0]).expect("the first part reads");
        assert_eq!(first.state, pos_proto::BillState::Open);
        assert_eq!(
            first.order_line_ids,
            vec![lines[0]],
            "it covers its own line"
        );
        assert_eq!(first.totals.total_due, vnd(55_000));
        let second = edge.bill_check(parts[1]).expect("the second part reads");
        assert_eq!(second.order_line_ids, vec![lines[1], lines[2]]);
        assert_eq!(second.totals.total_due, vnd(110_000));
        assert_eq!(
            edge.bill_check(source).expect("the source reads").state,
            pos_proto::BillState::Split,
            "and the source says it owes nothing more"
        );

        assert_eq!(
            edge.check_totals(table())
                .expect("the table reads")
                .total_due,
            vnd(165_000),
            "the table owes both parts, not the newest one"
        );
        assert_eq!(
            edge.live_orders()[0].open_bill_ids,
            parts,
            "both parts are open, oldest first"
        );

        edge.settle_bill(server(), parts[0], cash(55_000), None)
            .await
            .expect("the first part settles");
        assert_eq!(
            edge.check_totals(table())
                .expect("the table reads")
                .total_due,
            vnd(110_000),
            "and the table owes what is left"
        );
        assert_eq!(edge.live_orders()[0].open_bill_ids, vec![parts[1]]);

        // A store that restarts answers the same.
        let rebuilt = edge_over(store);
        rebuilt.rebuild().await.expect("replays the log");
        assert_eq!(
            rebuilt
                .check_totals(table())
                .expect("the table reads")
                .total_due,
            vnd(110_000)
        );
        assert_eq!(rebuilt.live_orders()[0].open_bill_ids, vec![parts[1]]);
        assert_eq!(
            rebuilt.bill_check(parts[1]).expect("reads").order_line_ids,
            vec![lines[1], lines[2]]
        );
    });
}

/// An ordinary bill is one open bill, and the table's check is that bill's — the sum of one part is
/// the part, so nothing a table that never splits reads has changed.
#[test]
fn an_unsplit_table_reads_its_one_bill() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (bill, lines) = a_bill_of(&edge, 2).await;
        assert_eq!(edge.live_orders()[0].open_bill_ids, vec![bill]);
        assert_eq!(
            edge.check_totals(table()).expect("the table reads"),
            edge.bill_totals(bill).expect("the bill reads"),
        );
        assert_eq!(
            edge.bill_check(bill).expect("reads").order_line_ids,
            lines,
            "a bill covers every line the order had when it opened"
        );
        assert!(matches!(
            edge.bill_check(BillId::new(Ulid::from_u128(999))),
            Err(pos_edge::AppError::UnknownBill)
        ));
    });
}
