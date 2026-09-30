// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A settled bill's receipt is printed again as a marked copy, and every copy is counted
//! ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
//!
//! `docs/pos-spec.md` §11 item 4 asked for reprints "marked COPY, counted, and permissioned", and
//! nothing did any of it: the permission was checked by nothing, no route printed a receipt twice,
//! and no event recorded a copy. These cases hold what the till now relies on: a copy keeps the
//! receipt's number and the settle's figures, each copy is its own event, a bill with no receipt has
//! nothing to copy, and a restart keeps the count.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_edge::{AppError, Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{BillId, DeviceId, EmployeeId, MenuItemId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{PaymentMethod, SalesChannel};

fn cashier() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn pizza() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn table(number: u128) -> TableId {
    TableId::new(Ulid::from_u128(40 + number))
}

/// A pizza, taxed for dine-in at `percent`.
fn session_taxed_at(percent: u32) -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class,
    ));
    let rates =
        TaxRateTable::new().with(class, SalesChannel::DineIn, TaxRate::from_percent(percent));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

fn edge_over(store: FakeStore, session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

fn a_pizza() -> LineDraft {
    LineDraft {
        menu_item_id: pizza(),
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

/// Seats a table, rings a pizza, and pays it in cash exactly: the bill and its receipt number.
async fn a_settled_bill(edge: &Edge<FakeStore>, number: u128) -> (BillId, u64) {
    edge.seat_table(cashier(), table(number), None)
        .await
        .expect("seats");
    edge.add_line(cashier(), table(number), a_pizza())
        .await
        .expect("rings a pizza");
    let bill = edge
        .open_bill(cashier(), table(number))
        .await
        .expect("opens the bill")
        .bill_id;
    let due = edge
        .check_totals(table(number))
        .expect("the check reads")
        .total_due;
    let settled = edge
        .settle_bill(
            cashier(),
            bill,
            vec![Payment {
                method: PaymentMethod::Cash,
                tendered: due,
                applied_to_bill: due,
                tip: vnd(0),
            }],
            None,
        )
        .await
        .expect("settles");
    (
        bill,
        settled.receipt_number.expect("a settle takes a number"),
    )
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

/// **A copy keeps the receipt's number and the settle's figures, and each copy is counted.** The
/// first press is copy 1, the next copy 2; each is its own event, and neither takes a new number.
#[test]
fn a_settled_receipt_is_copied_under_its_own_number_and_every_copy_is_counted() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), session_taxed_at(10));
        let (bill, receipt_number) = a_settled_bill(&edge, 1).await;

        let first = edge.reprint_receipt(cashier(), bill).await.expect("a copy");
        assert_eq!(
            first.receipt_number, receipt_number,
            "the original's number"
        );
        assert_eq!(first.copy_number, 1);
        assert_eq!(
            first.totals.total_due,
            vnd(165_000),
            "one pizza and its tax"
        );
        assert_eq!(first.totals.tax_total, vnd(15_000));
        assert_eq!(
            first.totals.tax_lines.len(),
            1,
            "the rates have not changed, so the copy prints the per-rate line"
        );
        assert_eq!(first.lines.len(), 1);
        assert_eq!(first.lines[0].display_name.as_str(), "Margherita");

        let second = edge
            .reprint_receipt(cashier(), bill)
            .await
            .expect("another copy");
        assert_eq!(second.copy_number, 2);
        assert_eq!(second.receipt_number, receipt_number);
        assert_ne!(
            second.job_id, first.job_id,
            "each copy is its own print job"
        );

        let events = logged(&store).await;
        let count = |kind: &str| events.iter().filter(|event| *event == kind).count();
        assert_eq!(count("billing.receipt.reprinted"), 2);
        assert_eq!(count("billing.bill.settled"), 1, "no second sale");

        let listed = edge.settled_bills().expect("the list reads");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].copies, 2);
    });
}

/// **A bill with no receipt has nothing to copy**: one still open is refused, and one the edge does
/// not know is refused as unknown. Neither writes anything.
#[test]
fn a_bill_that_has_not_settled_has_no_receipt_to_copy() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), session_taxed_at(10));
        edge.seat_table(cashier(), table(1), None)
            .await
            .expect("seats");
        edge.add_line(cashier(), table(1), a_pizza())
            .await
            .expect("rings a pizza");
        let open = edge
            .open_bill(cashier(), table(1))
            .await
            .expect("opens the bill")
            .bill_id;
        let before = logged(&store).await.len();

        let refused = edge.reprint_receipt(cashier(), open).await;
        assert!(
            matches!(refused, Err(AppError::NotSettled)),
            "got {refused:?}"
        );
        let unknown = edge
            .reprint_receipt(cashier(), BillId::new(Ulid::from_u128(999)))
            .await;
        assert!(
            matches!(unknown, Err(AppError::UnknownBill)),
            "got {unknown:?}"
        );
        assert_eq!(logged(&store).await.len(), before, "nothing was written");
        assert!(
            edge.settled_bills().expect("the list reads").is_empty(),
            "an open bill is not listed"
        );
    });
}

/// **Copying needs `billing.receipt.reprint`.** Without it the copy is refused and nothing is
/// counted.
#[test]
fn copying_a_receipt_needs_the_reprint_permission() {
    run_ready(async {
        let store = FakeStore::default();
        let mut without = session_taxed_at(10);
        without.granted = Permission::ALL
            .iter()
            .copied()
            .filter(|permission| *permission != Permission::ReprintReceipt)
            .collect();
        let edge = edge_over(store.clone(), without);
        let (bill, _) = a_settled_bill(&edge, 1).await;
        let before = logged(&store).await.len();

        let refused = edge.reprint_receipt(cashier(), bill).await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied { .. }))
            ),
            "got {refused:?}"
        );
        assert_eq!(logged(&store).await.len(), before, "nothing was counted");
    });
}

/// **The count survives a restart.** The settle and the copies are folded from the log, so a box
/// that restarts after two copies prints copy 3 next, and still lists the bill.
#[test]
fn the_copies_are_counted_across_a_restart() {
    run_ready(async {
        let store = FakeStore::default();
        let bill = {
            let edge = edge_over(store.clone(), session_taxed_at(10));
            let (bill, _) = a_settled_bill(&edge, 1).await;
            for _ in 0..2 {
                edge.reprint_receipt(cashier(), bill).await.expect("a copy");
            }
            bill
        };

        let edge = edge_over(store, session_taxed_at(10));
        edge.rebuild().await.expect("replays the log");
        let listed = edge.settled_bills().expect("the list reads");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].copies, 2);
        let third = edge
            .reprint_receipt(cashier(), bill)
            .await
            .expect("a copy after the restart");
        assert_eq!(third.copy_number, 3);
    });
}

/// **Today's bills, newest first.** Two tables settle one after the other; the list names the later
/// one first, each with its own receipt number and what was paid.
#[test]
fn today_s_settled_bills_are_listed_newest_first() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session_taxed_at(10));
        let (earlier, earlier_number) = a_settled_bill(&edge, 1).await;
        let (later, later_number) = a_settled_bill(&edge, 2).await;

        let listed = edge.settled_bills().expect("the list reads");
        let order: Vec<BillId> = listed.iter().map(|bill| bill.bill_id).collect();
        assert_eq!(order, vec![later, earlier]);
        assert_eq!(listed[0].receipt_number, later_number);
        assert_eq!(listed[1].receipt_number, earlier_number);
        assert_eq!(listed[0].table_id, Some(table(2)));
        assert_eq!(listed[0].total_due, vnd(165_000));
        assert_eq!(listed[0].copies, 0);
    });
}

/// **A copy never prints a figure the settle did not record.** The store's rate moves from 10% to
/// 8% after the bill was paid; the copy keeps the 10% the guest paid, and, since the per-rate lines
/// the settle did not record would now come out at 8%, prints the tax as the one recorded total.
#[test]
fn a_copy_after_a_rate_change_prints_what_the_settle_recorded() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), session_taxed_at(10));
        let (bill, _) = a_settled_bill(&edge, 1).await;
        edge.apply_session(session_taxed_at(8));

        let copy = edge.reprint_receipt(cashier(), bill).await.expect("a copy");
        assert_eq!(copy.totals.tax_total, vnd(15_000), "the 10% the guest paid");
        assert_eq!(copy.totals.total_due, vnd(165_000));
        assert!(
            copy.totals.tax_lines.is_empty(),
            "no per-rate line at a rate that did not apply"
        );
    });
}
