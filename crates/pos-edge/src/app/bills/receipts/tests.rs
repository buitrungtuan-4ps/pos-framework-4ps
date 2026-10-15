// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The receipts' unit tests: a receipt's copies, today's takings and receipts, and the buyer a copy
//! reads back.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::{BillTotals, TaxLine};
use pos_fakes::FakeStore;
use pos_ports::event_store::EventQuery;
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_ports::{EventStore, Transactional, TxContext};
use pos_proto::events::BillingReceiptReprinted;
use pos_proto::ids::{BillId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::{EventPayload, SalesChannel};

use crate::app::tests::{a_line, actor, cash, edge, identity, vnd};
use crate::app::{BuyerDetails, Edge, EdgeSession};
use crate::receipt::InMemoryReceipts;

/// The reprint tests' bill: one pizza on `table`, paid in cash, with `buyer` on it if any.
async fn a_settled_pizza(
    edge: &Edge<FakeStore>,
    table: TableId,
    buyer: Option<&BuyerDetails>,
) -> BillId {
    edge.seat_table(actor(), table, None).await.expect("seats");
    edge.add_line(actor(), table, a_line()).await.expect("adds");
    let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
    edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], buyer)
        .await
        .expect("settles");
    opened.bill_id
}

/// How many `billing.receipt.reprinted` the store holds.
async fn copies_logged(edge: &Edge<FakeStore>) -> usize {
    edge.store
        .read(&EventQuery::first(
            edge.identity.store_id,
            NonZeroU32::new(100).expect("100 is not zero"),
        ))
        .await
        .expect("reads the log")
        .iter()
        .filter(|envelope| {
            envelope.event_type.as_str() == BillingReceiptReprinted::EVENT_TYPE.as_str()
        })
        .count()
}

/// A receipt as the settle recorded it, for the tests of what a copy prints.
fn a_settled_receipt(
    receipt_number: u64,
    business_date: BusinessDate,
    settle_ms: i64,
) -> crate::app::SettledReceipt {
    crate::app::SettledReceipt {
        receipt_number,
        subtotal: vnd(150_000),
        reduction_total: vnd(20_000),
        service_charge: vnd(0),
        tax_total: vnd(13_000),
        rounding_adjustment: vnd(0),
        total_due: vnd(143_000),
        fee_lines: Vec::new(),
        tax_lines: Vec::new(),
        buyer_subject_id: None,
        business_date,
        settle_time: Timestamp::from_milliseconds_since_epoch(settle_ms).expect("a valid instant"),
        copies: 0,
    }
}

fn a_day(day: u8) -> BusinessDate {
    BusinessDate::from_ymd(2026, 9, day).expect("a real date")
}

/// A bill settled before the settle recorded its lines (ADR-0159 decision 4) is copied as it
/// was: its per-rate lines computed again while they give what it recorded, and its recorded
/// tax as one total once they do not.
#[test]
fn a_copy_of_a_settle_that_recorded_no_lines_computes_them_again() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let bill_id = a_settled_pizza(&edge, TableId::new(Ulid::from_u128(331)), None).await;
        {
            // What an older settle event folds to.
            let mut projection = edge.lock_projection();
            let receipt = projection.settled.get_mut(&bill_id).expect("recorded");
            receipt.fee_lines.clear();
            receipt.tax_lines.clear();
        }
        let copy = edge
            .reprint_receipt(actor(), bill_id)
            .await
            .expect("a copy");
        assert_eq!(copy.totals.tax_lines.len(), 1, "the rates still give it");

        let mut session = (*edge.session()).clone();
        session.tax_rates = TaxRateTable::new().with(
            EdgeSession::standard_tax_class(),
            SalesChannel::DineIn,
            TaxRate::from_percent(8),
        );
        edge.apply_session(session);
        let copy = edge
            .reprint_receipt(actor(), bill_id)
            .await
            .expect("a copy");
        assert!(copy.totals.tax_lines.is_empty(), "no line at 8%");
        assert_eq!(copy.totals.tax_total, vnd(15_000));
    });
}

/// The till's reads name a fee in the store's display language where the store's current rule
/// translates it, and as the bill froze it otherwise (ADR-0159 decision 4). What the settle
/// records keeps the frozen name, whatever language a screen is in.
#[test]
fn a_screen_names_a_fee_in_its_language_and_the_record_keeps_the_frozen_name() {
    use pos_core::billing::FeeLine;
    use pos_proto::events::BillFeeLine;
    use pos_proto::fees::{FeeCode, PublishedFees};
    use pos_proto::ids::FeeId;

    let rules: PublishedFees = serde_json::from_str(
        r#"{"fees": [
            {"fee_id": "00000000000000000000000001", "code": "SERVICE",
             "display_name": "Service fee", "display_name_translations": {"vi": "Phí phục vụ"}}
        ]}"#,
    )
    .expect("a fees node");
    let session = EdgeSession {
        fees: rules,
        ..EdgeSession::bootstrap()
    };
    let fee = |seed: u128, code: &str, name: &str, minor: i64| FeeLine {
        fee_id: FeeId::new(Ulid::from_u128(seed)),
        code: FeeCode::new(code),
        display_name: DisplayName::new(name),
        amount: vnd(minor),
        class_shares: Vec::new(),
        tax: vnd(0),
        tax_components: Vec::new(),
        waivable: false,
    };
    // The first rule is still published and translated; the second is no longer published.
    let totals = BillTotals {
        subtotal: vnd(100_000),
        discount_total: vnd(0),
        comp_total: vnd(0),
        service_charge: vnd(15_000),
        fee_lines: vec![
            fee(1, "SERVICE", "Service charge", 5_000),
            fee(2, "COVER", "Cover", 10_000),
        ],
        tax_lines: Vec::new(),
        tax_total: vnd(0),
        rounding_adjustment: vnd(0),
        total_due: vnd(115_000),
    };
    let names = |records: Vec<BillFeeLine>| -> Vec<String> {
        records
            .iter()
            .map(|record| record.display_name.as_str().to_owned())
            .collect()
    };
    assert_eq!(
        names(crate::app::fee_records_in(&totals, &session, Some("vi"))),
        ["Phí phục vụ", "Cover"]
    );
    assert_eq!(
        names(crate::app::fee_records_in(&totals, &session, Some("en"))),
        ["Service charge", "Cover"],
        "a language the rule does not translate into keeps the frozen name"
    );
    assert_eq!(
        names(crate::app::fee_records_in(&totals, &session, None)),
        ["Service charge", "Cover"]
    );
    assert_eq!(
        names(crate::app::fee_records(&totals)),
        ["Service charge", "Cover"]
    );
}

/// Only today's receipts are copied at the till (ADR-0164 decision 3). One settled on an
/// earlier business day is refused and left out of today's list, and nothing is counted.
#[test]
fn a_receipt_from_an_earlier_day_is_not_copied_at_the_till() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let bill_id = a_settled_pizza(&edge, TableId::new(Ulid::from_u128(330)), None).await;
        assert_eq!(edge.settled_bills().expect("the list reads").len(), 1);

        // The record as the box holds it on a later day: the clock cannot be moved, and the
        // point under test is the business day, not the clock.
        edge.lock_projection()
            .settled
            .get_mut(&bill_id)
            .expect("the settle was recorded")
            .business_date = BusinessDate::from_ymd(2000, 1, 1).expect("a real date");

        let refused = edge.reprint_receipt(actor(), bill_id).await;
        assert!(
            matches!(refused, Err(crate::app::AppError::ReceiptFromAnotherDay)),
            "got {refused:?}"
        );
        assert!(
            edge.settled_bills().expect("the list reads").is_empty(),
            "an earlier day's bill is not in today's list"
        );
        assert_eq!(
            copies_logged(&edge).await,
            0,
            "a refused copy is not counted"
        );
    });
}

/// A copy prints the settle's own figures (ADR-0164 decision 5). The recomputation lends its
/// per-rate lines only when it agrees with every recorded figure, and its discount and comp
/// split only when that split still adds up to the recorded reduction. Whichever it is, the
/// paper adds up to what the guest paid.
#[test]
fn a_copy_prints_what_the_settle_recorded_whatever_the_rates_say_now() {
    let settled = a_settled_receipt(7, a_day(30), 1);
    let recomputed = |discount: i64, comp: i64, tax: i64| BillTotals {
        subtotal: vnd(150_000),
        discount_total: vnd(discount),
        comp_total: vnd(comp),
        service_charge: vnd(0),
        fee_lines: Vec::new(),
        tax_lines: vec![TaxLine {
            tax_class_id: EdgeSession::standard_tax_class(),
            taxable_base: vnd(150_000 - discount - comp),
            rate_basis_points: 1_000,
            tax: vnd(tax),
            components: Vec::new(),
        }],
        tax_total: vnd(tax),
        rounding_adjustment: vnd(0),
        total_due: vnd(150_000 - discount - comp + tax),
    };
    let adds_up = |totals: &BillTotals| {
        totals.subtotal.amount_minor
            - totals.discount_total.amount_minor
            - totals.comp_total.amount_minor
            + totals.service_charge.amount_minor
            + totals.tax_total.amount_minor
            + totals.rounding_adjustment.amount_minor
            == totals.total_due.amount_minor
    };

    // The rates have not moved: the recomputation is the receipt, per-rate line and all.
    let unchanged = recomputed(15_000, 5_000, 13_000);
    assert_eq!(settled.totals(Some(&unchanged)), unchanged);

    // The rate has moved: the recorded tax, as one total, with the split that still holds.
    let moved = settled.totals(Some(&recomputed(15_000, 5_000, 10_400)));
    assert!(
        moved.tax_lines.is_empty(),
        "no line at a rate that did not apply"
    );
    assert_eq!(moved.tax_total, vnd(13_000));
    assert_eq!(
        (moved.discount_total, moved.comp_total),
        (vnd(15_000), vnd(5_000))
    );
    assert_eq!(moved.total_due, vnd(143_000));

    // A split that no longer adds up is printed as one reduction rather than guessed at.
    let regrouped = settled.totals(Some(&recomputed(10_000, 0, 14_000)));
    assert_eq!(
        (regrouped.discount_total, regrouped.comp_total),
        (vnd(20_000), vnd(0))
    );

    // Nothing could be recomputed: the recorded figures alone.
    let alone = settled.totals(None);
    assert_eq!(
        (alone.discount_total, alone.comp_total),
        (vnd(20_000), vnd(0))
    );
    assert!(alone.tax_lines.is_empty());

    for totals in [&unchanged, &moved, &regrouped, &alone] {
        assert!(adds_up(totals), "{totals:?} does not add up");
    }
}

/// A tax line prints the named parts its settle recorded, whatever the table names now
/// (ADR-0168 decision 4). A line that recorded none, as every line settled before the settle
/// recorded them, takes them as a copy always did: from the line computed again where it
/// charged the class the same tax at the same rate, and otherwise not at all.
#[test]
fn a_copy_prints_the_parts_a_line_recorded_and_recovers_them_only_where_it_recorded_none() {
    use pos_core::billing::TaxComponentLine;
    use pos_proto::events::{BillTaxComponent, BillTaxLine};
    use pos_proto::locale::TaxComponentName;

    let name = |name: &str| TaxComponentName::new(name).expect("a token");
    let part = |half: &str| TaxComponentLine {
        name: name(half),
        rate_basis_points: 500,
        tax: vnd(6_500),
    };
    let recomputed = |tax: i64, halves: [&str; 2]| BillTotals {
        subtotal: vnd(150_000),
        discount_total: vnd(20_000),
        comp_total: vnd(0),
        service_charge: vnd(0),
        fee_lines: Vec::new(),
        tax_lines: vec![TaxLine {
            tax_class_id: EdgeSession::standard_tax_class(),
            taxable_base: vnd(130_000),
            rate_basis_points: 1_000,
            tax: vnd(tax),
            components: halves.iter().map(|half| part(half)).collect(),
        }],
        tax_total: vnd(tax),
        rounding_adjustment: vnd(0),
        total_due: vnd(130_000 + tax),
    };
    let mut settled = a_settled_receipt(7, a_day(30), 1);
    settled.tax_lines = vec![BillTaxLine {
        tax_class_id: EdgeSession::standard_tax_class(),
        taxable_base: vnd(130_000),
        rate_basis_points: 1_000,
        tax: vnd(13_000),
        components: ["CGST", "SGST"]
            .iter()
            .map(|half| BillTaxComponent {
                name: name(half),
                rate_basis_points: 500,
                tax: vnd(6_500),
            })
            .collect(),
    }];
    let parts = |totals: BillTotals| -> Vec<TaxComponentLine> {
        totals
            .tax_lines
            .into_iter()
            .flat_map(|line| line.components)
            .collect()
    };

    // Recorded: SGST, though the table now names UTGST at the same rate.
    let today = recomputed(13_000, ["CGST", "UTGST"]);
    assert_eq!(
        parts(settled.totals(Some(&today))),
        [part("CGST"), part("SGST")]
    );
    assert_eq!(
        parts(settled.totals(None)),
        [part("CGST"), part("SGST")],
        "with nothing computed again"
    );

    // Recorded none: today's, where they give the recorded tax, as before; else none.
    if let Some(line) = settled.tax_lines.first_mut() {
        line.components.clear();
    }
    assert_eq!(
        parts(settled.totals(Some(&today))),
        [part("CGST"), part("UTGST")]
    );
    assert!(parts(settled.totals(Some(&recomputed(12_000, ["CGST", "UTGST"])))).is_empty());
}

/// Today's takings are every bill the day settled, at what its guests paid, and none of another
/// day's (ADR-0160 decision 2).
#[test]
fn today_s_takings_are_every_bill_settled_today_and_none_of_another_day() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let earlier = a_settled_pizza(&edge, TableId::new(Ulid::from_u128(341)), None).await;
        a_settled_pizza(&edge, TableId::new(Ulid::from_u128(342)), None).await;
        let taken = edge.takings_today().expect("today's takings");
        assert_eq!((taken.takings_amount, taken.bill_count), (vnd(330_000), 2));

        // One of them settled on another day, as a log folded after the cutoff holds it.
        edge.lock_projection()
            .settled
            .get_mut(&earlier)
            .expect("recorded")
            .business_date = BusinessDate::from_ymd(2000, 1, 1).expect("a real date");
        let taken = edge.takings_today().expect("today's takings");
        assert_eq!((taken.takings_amount, taken.bill_count), (vnd(165_000), 1));
    });
}

/// Today's list is bounded and newest first, with no other day's receipt in it (ADR-0164). Two
/// settled in the same instant list the later number first.
#[test]
fn today_s_receipts_are_listed_newest_first_and_bounded() {
    let mut projection = crate::app::Projection::default();
    let bill = |n: u64| BillId::new(Ulid::from_u128(u128::from(n)));
    let bound = u64::try_from(crate::app::MAX_SETTLED_LISTED).expect("a small bound");
    for n in 1..=bound + 1 {
        let ms = i64::try_from(n).expect("a small count");
        projection.record_settled(bill(n), a_settled_receipt(n, a_day(30), 1_000 + ms));
    }
    // The same instant as the newest, one number later.
    projection.record_settled(
        bill(bound + 2),
        a_settled_receipt(
            bound + 2,
            a_day(30),
            1_000 + i64::try_from(bound + 1).expect("small"),
        ),
    );
    // Yesterday's, settled at the latest instant of all.
    projection.record_settled(bill(9_999), a_settled_receipt(9_999, a_day(29), 9_999_999));

    let listed = projection.settled_on(a_day(30), crate::app::MAX_SETTLED_LISTED);
    let numbers: Vec<u64> = listed
        .iter()
        .map(|(_, receipt)| receipt.receipt_number)
        .collect();
    assert_eq!(numbers.len(), crate::app::MAX_SETTLED_LISTED);
    assert_eq!(
        numbers.first().copied(),
        Some(bound + 2),
        "a tie on the instant lists the later number first"
    );
    assert_eq!(numbers.get(1).copied(), Some(bound + 1));
    assert!(
        !numbers.contains(&1) && !numbers.contains(&2),
        "the oldest give way to the bound"
    );
    assert!(
        !numbers.contains(&9_999),
        "yesterday's receipt is not today's"
    );
}

/// The count only goes up: a replay that meets the copies out of order ends where the live box
/// did, and a copy of a bill with no settle on record changes nothing.
#[test]
fn a_receipt_s_copies_are_counted_by_the_highest_number_seen() {
    let mut projection = crate::app::Projection::default();
    let bill = BillId::new(Ulid::from_u128(5));
    projection.record_settled(bill, a_settled_receipt(1, a_day(30), 1));
    projection.record_copy(bill, 3);
    projection.record_copy(bill, 2);
    assert_eq!(
        projection.settled.get(&bill).map(|receipt| receipt.copies),
        Some(3)
    );

    let unknown = BillId::new(Ulid::from_u128(6));
    projection.record_copy(unknown, 1);
    assert!(!projection.settled.contains_key(&unknown));
}

/// A copy prints its buyer again from the subject store (ADR-0107). Once the retention sweep
/// has scrubbed the record there is no buyer left to print, and the copy still prints.
#[test]
fn a_copy_reads_its_buyer_back_until_the_record_is_scrubbed() {
    pos_fakes::executor::run_ready(async {
        let session = EdgeSession {
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
        let buyer = BuyerDetails {
            name: "Example Trading Co.".to_owned(),
            tax_code: Some("0000000000".to_owned()),
            address: Some("1 Example Street".to_owned()),
            email: None,
        };
        let bill_id =
            a_settled_pizza(&edge, TableId::new(Ulid::from_u128(331)), Some(&buyer)).await;

        let copy = edge
            .reprint_receipt(actor(), bill_id)
            .await
            .expect("a copy");
        let subject_id = copy.buyer_subject_id.expect("the copy names its buyer");
        let read = edge
            .buyer_of(subject_id)
            .await
            .expect("the subject store answers");
        assert!(
            read.as_ref() == Some(&buyer),
            "the copy prints the buyer the original did"
        );

        // Aged past the window and swept, as the retention test does.
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
        assert_eq!(edge.sweep_expired_subjects().await.expect("sweeps"), 1);

        assert!(
            edge.buyer_of(subject_id)
                .await
                .expect("the subject store answers")
                .is_none(),
            "a scrubbed buyer is not printed again"
        );
        let next = edge
            .reprint_receipt(actor(), bill_id)
            .await
            .expect("the receipt is still copied");
        assert_eq!(next.copy_number, 2);
    });
}
