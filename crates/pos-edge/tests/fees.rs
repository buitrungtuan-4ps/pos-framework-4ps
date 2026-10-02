// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge charges a bill's fees
//! ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)).
//!
//! A bill freezes the rules in force on its channel when it opens and is computed from those until
//! it settles (decision 3), so a publish during a meal reaches the next bill and not the one on the
//! table, and a restart folds the same rules back from the log. A split part keeps the rules its
//! source froze, with a fee per bill shared out rather than charged again on every part
//! (decision 6). A merge keeps the rules of the bill the cashier holds and charges a fee per bill
//! once, at the sum of the merged bills' shares capped at its whole, so merging a split back
//! together restores it. A bill opened again after a void freezes what is in force then. A fee
//! waived on a bill charges nothing on it, through every split, merge and restart (decision 5).

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::BillTotals;
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, Edge, EdgeSession, InMemoryReceipts, LineDraft, OrderLineChoice, StaffAuth,
    StaffRoster, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::events::{
    BillFeeLine, BillTaxLine, BillingBillOpened, BillingBillSettled, BillingFeeWaived,
    SecurityPermissionOverridden,
};
use pos_proto::fees::{FeeCode, FrozenFee, PublishedFees};
use pos_proto::ids::{
    BillId, DeviceId, EmployeeId, FeeId, MenuItemId, OrderLineId, ReasonCodeId, StoreId, TableId,
    TaxClassId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{PublishedReasonCodes, ReasonAction};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, SalesChannel};
use serde_json::{Value, json};

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

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

fn table(n: u128) -> TableId {
    TableId::new(Ulid::from_u128(40 + n))
}

fn fee_id(n: u128) -> FeeId {
    FeeId::new(Ulid::from_u128(n))
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

/// A service charge of `percent` on every channel, taxed the way its lines are.
fn service(percent: i64) -> Value {
    json!({
        "fee_id": fee_id(1).to_string(),
        "code": "SERVICE",
        "display_name": "Service charge",
        "kind": "FEE_KIND_PERCENT",
        "rate": { "numerator": percent, "denominator": 100 },
    })
}

/// A cover charge of 10,000 a bill.
fn cover() -> Value {
    json!({
        "fee_id": fee_id(2).to_string(),
        "code": "COVER",
        "display_name": "Cover charge",
        "kind": "FEE_KIND_AMOUNT_PER_BILL",
        "amount": { "currency_code": "VND", "amount_minor": 10_000 },
    })
}

/// A drink at 10,000, beside the dish.
fn drink() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(501))
}

/// A cover charge of 10,000 a bill on the dish alone, not on a drink.
fn food_cover() -> Value {
    let mut rule = cover();
    rule["item_scope"] = json!("FEE_ITEMS_INCLUDE");
    rule["menu_item_ids"] = json!([item().to_string()]);
    rule
}

/// A `fees` node of `rules`, parsed as the edge parses a published one.
fn fees(rules: &[Value]) -> PublishedFees {
    serde_json::from_str(&json!({ "fees": rules }).to_string()).expect("a fees node")
}

/// A store that prices one dish at 10% and charges `fees`, with one manager who may void a bill
/// and waive a fee.
fn session(fees: PublishedFees) -> EdgeSession {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    let menu = MenuCatalog::new()
        .with(MenuEntry::new(
            item(),
            DisplayName::new("Bún chả"),
            vnd(UNIT_PRICE),
            class(),
        ))
        .with(MenuEntry::new(
            drink(),
            DisplayName::new("Trà đá"),
            vnd(10_000),
            class(),
        ));
    let rates = TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
    let pin_phc = Argon2::default()
        .hash_password_with_salt(b"4417", b"a-fixed-test-slt")
        .expect("hash")
        .to_string();
    let mut staff = StaffRoster::new();
    staff.insert(
        "MGR-1",
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::EMPTY
                .with(Permission::VoidBill)
                .with(Permission::WaiveFee),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(pin_phc),
        },
    );
    EdgeSession {
        fees,
        ..EdgeSession::bootstrap()
            .with_menu(menu)
            .with_tax_rates(rates)
            .with_staff(staff)
    }
}

fn edge_over(store: FakeStore, fees: PublishedFees) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(store_id()),
        session(fees),
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

/// A drink, as the till drafts one.
fn a_drink() -> LineDraft {
    LineDraft {
        menu_item_id: drink(),
        display_name: DisplayName::new("Trà đá"),
        unit_price: vnd(10_000),
        line_total: vnd(10_000),
        ..a_line()
    }
}

/// Seats `table`, sells `count` dishes on it, and returns their lines.
async fn a_table_of(edge: &Edge<FakeStore>, table: TableId, count: usize) -> Vec<OrderLineId> {
    edge.seat_table(server(), table, None).await.expect("seats");
    let mut lines = Vec::new();
    for _ in 0..count {
        let view = edge
            .add_line(server(), table, a_line())
            .await
            .expect("adds");
        lines.push(view.order_line_id);
    }
    lines
}

/// Every payload of `event_type` in the log, in order.
async fn logged<P: serde::de::DeserializeOwned>(store: &FakeStore, event_type: &str) -> Vec<P> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(200).expect("a positive limit"));
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == event_type)
        .map(|envelope| envelope.data.decode::<P>().expect("the payload decodes"))
        .collect()
}

/// The rules every `billing.bill.opened` in the log froze, in the order the bills opened.
async fn frozen_on_open(store: &FakeStore) -> Vec<Vec<FrozenFee>> {
    logged::<BillingBillOpened>(store, "billing.bill.opened")
        .await
        .into_iter()
        .map(|opened| opened.fee_rules)
        .collect()
}

/// What each fee on a bill charged, by code, in rule order.
fn charged(totals: &BillTotals) -> Vec<(&str, i64)> {
    totals
        .fee_lines
        .iter()
        .map(|line| (line.code.as_str(), line.amount.amount_minor))
        .collect()
}

fn totals(edge: &Edge<FakeStore>, bill_id: BillId) -> BillTotals {
    edge.bill_totals(bill_id).expect("the bill's totals")
}

/// A packaging fee for takeaway alone and a paused rule are not in force on a dine-in bill; the
/// service charge is, and the bill keeps it at the rate it opened with whatever is published
/// during the meal, and after a restart too.
#[test]
fn a_bill_keeps_the_fees_in_force_when_it_opened() {
    run_ready(async {
        let store = FakeStore::default();
        let packaging = json!({
            "fee_id": fee_id(3).to_string(),
            "code": "PACKAGING",
            "display_name": "Packaging",
            "kind": "FEE_KIND_AMOUNT_PER_UNIT",
            "amount": { "currency_code": "VND", "amount_minor": 2_000 },
            "channels": ["SALES_CHANNEL_TAKEAWAY"],
        });
        let paused = json!({
            "fee_id": fee_id(4).to_string(),
            "code": "PAUSED",
            "display_name": "Paused",
            "kind": "FEE_KIND_PERCENT",
            "rate": { "numerator": 3, "denominator": 100 },
            "active": false,
        });
        let published = fees(&[service(5), packaging, paused]);
        let edge = edge_over(store.clone(), published.clone());
        a_table_of(&edge, table(1), 2).await;

        // Before the bill opens the check is priced with the rules a bill would freeze now: 5% of
        // 100,000, and 10% on 105,000.
        assert_eq!(
            edge.check_totals(table(1)).expect("check").total_due,
            vnd(115_500)
        );
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let opened = frozen_on_open(&store).await;
        assert_eq!(opened, vec![published.in_force(SalesChannel::DineIn)]);
        let codes: Vec<&str> = opened
            .iter()
            .flatten()
            .map(|rule| rule.code.as_str())
            .collect();
        assert_eq!(codes, ["SERVICE"]);

        let open = totals(&edge, bill);
        assert_eq!(charged(&open), [("SERVICE", 5_000)]);
        assert_eq!(open.service_charge, vnd(5_000));
        assert_eq!(open.fee_lines.first().map(|line| line.tax), Some(vnd(500)));
        assert_eq!(open.total_due, vnd(115_500));

        // A publish during the meal: the bill on the table keeps 5%, and the next table is quoted
        // the new rate before its bill opens.
        edge.apply_session(session(fees(&[service(10)])));
        assert_eq!(totals(&edge, bill).total_due, vnd(115_500));
        a_table_of(&edge, table(2), 1).await;
        assert_eq!(
            edge.check_totals(table(2)).expect("check").total_due,
            vnd(60_500),
            "50,000, 10% of it, and 10% on 55,000"
        );

        // A restart folds the frozen rules back from the log, whatever the session says now.
        let restarted = edge_over(store, fees(&[service(10)]));
        restarted.rebuild().await.expect("rebuilds");
        assert_eq!(totals(&restarted, bill), open);
    });
}

/// ADR-0159 decision 6: a split part keeps its source's rules, with the cover charge shared 1:2 as
/// the parts' lines are rather than charged on both, so the parts owe together what the whole bill
/// did. Merged back together they owe the whole bill again, cover and all, after a restart too.
#[test]
fn a_split_shares_a_fee_per_bill_and_merging_the_parts_back_restores_it() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), fees(&[cover(), service(5)]));
        let lines = a_table_of(&edge, table(1), 3).await;
        let source = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let whole = totals(&edge, source);
        assert_eq!(charged(&whole), [("COVER", 10_000), ("SERVICE", 7_500)]);
        assert_eq!(whole.total_due, vnd(184_250));

        let parts = edge
            .split_bill(server(), source, vec![vec![lines[0]], lines[1..].to_vec()])
            .await
            .expect("splits");
        let [one, two] = parts.as_slice() else {
            panic!("two parts: {parts:?}");
        };
        // The cover charge's allocation is recorded on each part's opening, beside the rest of
        // the source's rules.
        let shares: Vec<Vec<(String, Option<Money>)>> = frozen_on_open(&store)
            .await
            .into_iter()
            .skip(1)
            .map(|rules| {
                rules
                    .into_iter()
                    .map(|rule| (rule.code.as_str().to_owned(), rule.amount))
                    .collect()
            })
            .collect();
        assert_eq!(
            shares,
            vec![
                vec![
                    ("COVER".to_owned(), Some(vnd(3_333))),
                    ("SERVICE".to_owned(), None)
                ],
                vec![
                    ("COVER".to_owned(), Some(vnd(6_667))),
                    ("SERVICE".to_owned(), None)
                ],
            ]
        );
        let (first, second) = (totals(&edge, *one), totals(&edge, *two));
        assert_eq!(charged(&first), [("COVER", 3_333), ("SERVICE", 2_500)]);
        assert_eq!(charged(&second), [("COVER", 6_667), ("SERVICE", 5_000)]);
        assert_eq!(
            first
                .total_due
                .checked_add(second.total_due)
                .expect("in range"),
            whole.total_due,
            "each part rounds its own tax, and here they come to the whole"
        );

        // Folded back together, the bill the cashier holds keeps its own rules, and the cover
        // charge is the sum of the two shares: the whole bill again.
        let survivor = edge
            .merge_bills(server(), *one, vec![*two])
            .await
            .expect("merges");
        assert_eq!(totals(&edge, survivor), whole);

        let restarted = edge_over(store, fees(&[cover(), service(5)]));
        restarted.rebuild().await.expect("rebuilds");
        assert_eq!(totals(&restarted, survivor), whole);
    });
}

/// What the cover charge came to on a bill, if it charged one.
fn cover_on(edge: &Edge<FakeStore>, bill_id: BillId) -> Option<i64> {
    totals(edge, bill_id)
        .fee_lines
        .iter()
        .find(|line| line.code.as_str() == "COVER")
        .map(|line| line.amount.amount_minor)
}

/// Merging some of a split's parts charges the sum of their shares of a fee per bill, whichever
/// part the cashier holds, and a restart folds the same answer from the log.
#[test]
fn merging_some_parts_charges_the_sum_of_their_shares() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), fees(&[cover()]));
        let lines = a_table_of(&edge, table(1), 3).await;
        let source = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let parts = edge
            .split_bill(
                server(),
                source,
                lines.iter().map(|line| vec![*line]).collect(),
            )
            .await
            .expect("splits three ways");
        let shares: Vec<Option<i64>> = parts.iter().map(|part| cover_on(&edge, *part)).collect();
        assert_eq!(shares, [Some(3_333), Some(3_333), Some(3_334)]);

        let survivor = edge
            .merge_bills(server(), parts[2], vec![parts[0]])
            .await
            .expect("merges two of three");
        assert_eq!(cover_on(&edge, survivor), Some(6_667));
        assert_eq!(cover_on(&edge, parts[1]), Some(3_333));

        let restarted = edge_over(store, fees(&[cover()]));
        restarted.rebuild().await.expect("rebuilds");
        assert_eq!(cover_on(&restarted, survivor), Some(6_667));
    });
}

/// Two counter bills that were never one bill each carry the cover; merged, the bill charges it
/// once.
#[test]
fn merging_bills_that_were_never_one_charges_a_fee_per_bill_once() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), fees(&[cover()]));
        let mut bills = Vec::new();
        for _ in 0..2 {
            let order = edge
                .open_counter_order(server(), SalesChannel::DineIn)
                .await
                .expect("the counter opens an order")
                .order_id;
            let dish = OrderLineChoice {
                menu_item_id: item(),
                quantity: Quantity::ONE,
                modifier_menu_item_ids: Vec::new(),
                seat: None,
                course_id: None,
                note_present: false,
            };
            edge.add_line_to_order(server(), order, dish)
                .await
                .expect("adds a dish");
            let bill = edge
                .open_bill_for_order(server(), order)
                .await
                .expect("opens its bill")
                .bill_id;
            assert_eq!(cover_on(&edge, bill), Some(10_000));
            bills.push(bill);
        }
        let survivor = edge
            .merge_bills(server(), bills[0], vec![bills[1]])
            .await
            .expect("two counter bills merge");
        assert_eq!(cover_on(&edge, survivor), Some(10_000), "once, not twice");
    });
}

/// The cashier holds the drinks' part, which dropped the cover on the food, and absorbs the food's:
/// the cover is still charged, on the food it was always for.
#[test]
fn a_fee_per_bill_survives_a_merge_into_a_part_that_dropped_it() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), fees(&[food_cover()]));
        edge.seat_table(server(), table(1), None)
            .await
            .expect("seats");
        let tea = edge
            .add_line(server(), table(1), a_drink())
            .await
            .expect("adds a drink")
            .order_line_id;
        let dish = edge
            .add_line(server(), table(1), a_line())
            .await
            .expect("adds a dish")
            .order_line_id;
        let source = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let whole = totals(&edge, source);
        assert_eq!(charged(&whole), [("COVER", 10_000)]);

        let parts = edge
            .split_bill(server(), source, vec![vec![tea], vec![dish]])
            .await
            .expect("splits the drink off");
        assert_eq!(cover_on(&edge, parts[0]), None, "no food, no cover");
        let survivor = edge
            .merge_bills(server(), parts[0], vec![parts[1]])
            .await
            .expect("the drinks' part takes the food's back");
        assert_eq!(totals(&edge, survivor), whole);
    });
}

/// A bill opened again after a void freezes what is in force then, not what the voided one held.
#[test]
fn a_bill_opened_again_after_a_void_freezes_what_is_in_force_then() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), fees(&[service(5)]));
        a_table_of(&edge, table(1), 1).await;
        let first = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        assert_eq!(charged(&totals(&edge, first)), [("SERVICE", 2_500)]);

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
            .void_bill(server(), first, reason, Some(&approval))
            .await
            .expect("a manager voids the bill");
        assert_eq!(voided, BillState::Voided);

        edge.apply_session(session(fees(&[service(10)])));
        let again = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens again")
            .bill_id;
        assert_eq!(charged(&totals(&edge, again)), [("SERVICE", 5_000)]);
        let rates: Vec<Option<Ratio>> = frozen_on_open(&store)
            .await
            .iter()
            .map(|rules| rules.first().and_then(|rule| rule.rate))
            .collect();
        assert_eq!(rates, [Ratio::percent(5).ok(), Ratio::percent(10).ok()]);
    });
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

/// The settle records each fee under the name the bill froze, and the tax per class, from the
/// totals the guest paid (ADR-0159 decision 4). A rename published during the meal changes neither.
#[test]
fn a_settled_bill_records_each_fee_and_its_tax_per_class() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), fees(&[service(5)]));
        a_table_of(&edge, table(1), 2).await;
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let mut renamed = service(5);
        renamed["display_name"] = json!("Phí phục vụ");
        edge.apply_session(session(fees(&[renamed])));
        edge.settle_bill(server(), bill, cash(115_500), None)
            .await
            .expect("settles for what the check said");

        let settled: Vec<BillingBillSettled> = logged(&store, "billing.bill.settled").await;
        let [settled] = settled.as_slice() else {
            panic!("one settled bill: {settled:?}");
        };
        assert_eq!(
            settled.fee_lines,
            [BillFeeLine {
                fee_id: fee_id(1),
                code: FeeCode::new("SERVICE"),
                display_name: DisplayName::new("Service charge"),
                amount: vnd(5_000),
                tax: vnd(500),
            }]
        );
        assert_eq!(settled.service_charge, vnd(5_000), "the sum of every fee");
        assert_eq!(
            settled.tax_lines,
            [BillTaxLine {
                tax_class_id: class(),
                taxable_base: vnd(105_000),
                rate_basis_points: 1_000,
                tax: vnd(10_500),
            }]
        );
        assert_eq!(settled.tax_total, vnd(10_500));
    });
}

/// A bill with no fee records no fee line, and still records its tax per class: every bill has a
/// class, so every settle from this release carries `tax_lines`.
#[test]
fn a_bill_without_fees_records_its_tax_per_class_and_no_fee_line() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), PublishedFees::default());
        a_table_of(&edge, table(1), 1).await;
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        edge.settle_bill(server(), bill, cash(55_000), None)
            .await
            .expect("settles");
        let settled: Vec<BillingBillSettled> = logged(&store, "billing.bill.settled").await;
        let [settled] = settled.as_slice() else {
            panic!("one settled bill: {settled:?}");
        };
        assert!(settled.fee_lines.is_empty());
        assert_eq!(settled.service_charge, vnd(0));
        assert_eq!(
            settled.tax_lines,
            [BillTaxLine {
                tax_class_id: class(),
                taxable_base: vnd(50_000),
                rate_basis_points: 1_000,
                tax: vnd(5_000),
            }]
        );
        assert!(frozen_on_open(&store).await.iter().all(Vec::is_empty));
    });
}

/// A copy of the receipt prints the fees and the tax per class the settle recorded (ADR-0164,
/// ADR-0159 decision 4): neither a fee published at another rate since nor a rate changed since
/// moves a figure on it.
#[test]
fn a_copy_prints_the_fees_and_tax_the_settle_recorded() {
    run_ready(async {
        let edge = edge_over(FakeStore::default(), fees(&[service(5)]));
        a_table_of(&edge, table(1), 2).await;
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        edge.settle_bill(server(), bill, cash(115_500), None)
            .await
            .expect("settles");

        let mut since = session(fees(&[service(10)]));
        since.tax_rates =
            TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(8));
        edge.apply_session(since);
        let copy = edge.reprint_receipt(server(), bill).await.expect("a copy");
        assert_eq!(charged(&copy.totals), [("SERVICE", 5_000)]);
        let rates: Vec<(u32, Money)> = copy
            .totals
            .tax_lines
            .iter()
            .map(|line| (line.rate_basis_points, line.tax))
            .collect();
        assert_eq!(rates, [(1_000, vnd(10_500))]);
        assert_eq!(copy.totals.total_due, vnd(115_500));
    });
}

/// The service charge of `percent`, which staff may waive on a bill (ADR-0159 decision 5).
fn waivable_service(percent: i64) -> Value {
    let mut rule = service(percent);
    rule["waivable"] = json!(true);
    rule
}

/// The framework's reason for putting it right for a guest, which a waive may cite.
fn putting_it_right() -> ReasonCodeId {
    PublishedReasonCodes::framework_default()
        .codes()
        .iter()
        .find(|reason| reason.code.as_str() == "SERVICE_RECOVERY")
        .filter(|reason| reason.is_valid_for(ReasonAction::WaiveFee))
        .map(|reason| reason.id)
        .expect("a framework reason for a waive")
}

/// The manager's badge and PIN, approving a waive as they approve a void.
fn the_manager() -> Approval {
    Approval {
        code: "MGR-1".to_owned(),
        pin: "4417".to_owned(),
    }
}

/// ADR-0159 decision 5: a waived fee charges nothing and is taxed nothing, the cover beside it is
/// charged as before, and the log holds the waive and the approval that let it through. The
/// settle records no line for it.
#[test]
fn a_waived_fee_is_charged_and_taxed_nothing_and_the_waive_is_on_the_record() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), fees(&[waivable_service(10), cover()]));
        a_table_of(&edge, table(1), 2).await;
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        // 100,000 of food, 10,000 of service and 10,000 of cover, and 10% on all of it.
        let before = totals(&edge, bill);
        assert_eq!(charged(&before), [("SERVICE", 10_000), ("COVER", 10_000)]);
        assert_eq!(before.total_due, vnd(132_000));
        let waivable: Vec<bool> = before.fee_lines.iter().map(|fee| fee.waivable).collect();
        assert_eq!(waivable, [true, false]);

        let after = edge
            .waive_fee(
                server(),
                bill,
                fee_id(1),
                putting_it_right(),
                Some(&the_manager()),
            )
            .await
            .expect("a manager waives the service charge");
        assert_eq!(charged(&after), [("COVER", 10_000)]);
        assert_eq!(after.service_charge, vnd(10_000));
        assert_eq!(after.tax_total, vnd(11_000));
        assert_eq!(after.total_due, vnd(121_000));
        assert_eq!(
            totals(&edge, bill),
            after,
            "the bill reads as the waive answered"
        );

        let waived: Vec<BillingFeeWaived> = logged(&store, "billing.fee.waived").await;
        assert_eq!(
            waived,
            [BillingFeeWaived {
                bill_id: bill,
                fee_id: fee_id(1),
                reason_code_id: putting_it_right(),
            }]
        );
        let approvals: Vec<SecurityPermissionOverridden> =
            logged(&store, "security.permission.overridden").await;
        let approved: Vec<&str> = approvals
            .iter()
            .map(|approval| approval.permission_key.as_str())
            .collect();
        assert_eq!(approved, ["billing.fee.waive"]);

        edge.settle_bill(server(), bill, cash(121_000), None)
            .await
            .expect("settles for what the waive left");
        let settled: Vec<BillingBillSettled> = logged(&store, "billing.bill.settled").await;
        let codes: Vec<&str> = settled
            .iter()
            .flat_map(|settle| &settle.fee_lines)
            .map(|line| line.code.as_str())
            .collect();
        assert_eq!(codes, ["COVER"]);
    });
}

/// Only a waivable fee the bill charges is waived, for a reason published for waiving, with an
/// approver where the store asks for one. Each refusal writes nothing.
#[test]
fn a_fee_is_waived_only_where_its_rule_its_reason_and_its_approver_allow() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), fees(&[waivable_service(10), cover()]));
        a_table_of(&edge, table(1), 2).await;
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let manager = the_manager();

        let not_waivable = edge
            .waive_fee(
                server(),
                bill,
                fee_id(2),
                putting_it_right(),
                Some(&manager),
            )
            .await;
        assert!(
            matches!(
                not_waivable,
                Err(AppError::Domain(DomainError::FeeNotWaivable))
            ),
            "{not_waivable:?}"
        );
        let not_on_bill = edge
            .waive_fee(
                server(),
                bill,
                fee_id(9),
                putting_it_right(),
                Some(&manager),
            )
            .await;
        assert!(
            matches!(
                not_on_bill,
                Err(AppError::Domain(DomainError::FeeNotOnBill))
            ),
            "{not_on_bill:?}"
        );
        let for_a_void = PublishedReasonCodes::framework_default()
            .codes()
            .iter()
            .find(|reason| !reason.is_valid_for(ReasonAction::WaiveFee))
            .map(|reason| reason.id)
            .expect("a framework reason that is not for a waive");
        let wrong_reason = edge
            .waive_fee(server(), bill, fee_id(1), for_a_void, Some(&manager))
            .await;
        assert!(
            matches!(wrong_reason, Err(AppError::ReasonCodeNotValid)),
            "{wrong_reason:?}"
        );
        // A store that does not enforce each person's own set asks a holder's PIN every time.
        let unapproved = edge
            .waive_fee(server(), bill, fee_id(1), putting_it_right(), None)
            .await;
        assert!(
            matches!(unapproved, Err(AppError::ApprovalRequired)),
            "{unapproved:?}"
        );
        assert!(
            logged::<BillingFeeWaived>(&store, "billing.fee.waived")
                .await
                .is_empty(),
            "a refusal writes nothing"
        );
    });
}

/// A fee is waived once, and only while its bill is open: a second waive finds it no longer on
/// the bill, and a settled bill owes nothing to waive.
#[test]
fn a_fee_is_waived_once_and_only_on_an_open_bill() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone(), fees(&[waivable_service(10), cover()]));
        a_table_of(&edge, table(1), 2).await;
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let manager = the_manager();
        edge.waive_fee(
            server(),
            bill,
            fee_id(1),
            putting_it_right(),
            Some(&manager),
        )
        .await
        .expect("waives");
        let twice = edge
            .waive_fee(
                server(),
                bill,
                fee_id(1),
                putting_it_right(),
                Some(&manager),
            )
            .await;
        assert!(
            matches!(twice, Err(AppError::Domain(DomainError::FeeNotOnBill))),
            "a waived fee is no longer on the bill: {twice:?}"
        );

        edge.settle_bill(server(), bill, cash(121_000), None)
            .await
            .expect("settles");
        let settled = edge
            .waive_fee(
                server(),
                bill,
                fee_id(1),
                putting_it_right(),
                Some(&manager),
            )
            .await;
        assert!(
            matches!(settled, Err(AppError::Domain(DomainError::Transition(_)))),
            "a settled bill owes nothing to waive: {settled:?}"
        );
        assert_eq!(
            logged::<BillingFeeWaived>(&store, "billing.fee.waived")
                .await
                .len(),
            1
        );
    });
}

/// A waive follows the fee's id (ADR-0159 decision 5): each part of a split of the bill charges
/// nothing for it while sharing the cover, the parts merged back charge the cover whole and still
/// nothing for it, and a restart folds the same bill back from the log.
#[test]
fn a_waive_follows_the_fee_through_a_split_a_merge_and_a_restart() {
    run_ready(async {
        let store = FakeStore::default();
        let published = fees(&[waivable_service(10), cover()]);
        let edge = edge_over(store.clone(), published.clone());
        let lines = a_table_of(&edge, table(1), 2).await;
        let source = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let waived = edge
            .waive_fee(
                server(),
                source,
                fee_id(1),
                putting_it_right(),
                Some(&the_manager()),
            )
            .await
            .expect("waives");

        let parts = edge
            .split_bill(server(), source, vec![vec![lines[0]], vec![lines[1]]])
            .await
            .expect("splits");
        let [one, two] = parts.as_slice() else {
            panic!("two parts: {parts:?}");
        };
        assert_eq!(charged(&totals(&edge, *one)), [("COVER", 5_000)]);
        assert_eq!(charged(&totals(&edge, *two)), [("COVER", 5_000)]);
        let refused = edge
            .waive_fee(
                server(),
                *one,
                fee_id(1),
                putting_it_right(),
                Some(&the_manager()),
            )
            .await;
        assert!(
            matches!(refused, Err(AppError::Domain(DomainError::FeeNotOnBill))),
            "a part keeps the bill's waive: {refused:?}"
        );

        let merged = edge
            .merge_bills(server(), *one, vec![*two])
            .await
            .expect("merges");
        assert_eq!(
            totals(&edge, merged),
            waived,
            "the bill the waive left, whole again"
        );

        let restarted = edge_over(store, published);
        restarted.rebuild().await.expect("rebuilds");
        assert_eq!(totals(&restarted, merged), waived);
    });
}

/// Where the store decides with each person's own set (ADR-0158), a person who holds the waive
/// directly waives alone, with no approval on the record.
#[test]
fn a_person_who_holds_the_waive_directly_waives_alone() {
    run_ready(async {
        let store = FakeStore::default();
        let mut enforced = session(fees(&[waivable_service(10)]));
        enforced.permissions_enforced = true;
        let mut staff = StaffRoster::new();
        staff.insert(
            "MGR-1",
            StaffAuth {
                employee_id: Some(server().employee_id),
                permissions: PermissionSet::EMPTY
                    .with(Permission::ManageTables)
                    .with(Permission::AddLine)
                    .with(Permission::OpenBill)
                    .with(Permission::WaiveFee),
                permissions_with_approval: PermissionSet::EMPTY,
                discount_ceiling: None,
                pin_phc: None,
            },
        );
        enforced.staff = staff;
        let edge = Edge::new(
            store.clone(),
            StoreIdentity::for_store(store_id()),
            enforced,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator");
        a_table_of(&edge, table(1), 1).await;
        let bill = edge
            .open_bill(server(), table(1))
            .await
            .expect("opens")
            .bill_id;
        let after = edge
            .waive_fee(server(), bill, fee_id(1), putting_it_right(), None)
            .await
            .expect("a direct holder needs nobody's PIN");
        assert!(after.fee_lines.is_empty());
        assert!(
            logged::<SecurityPermissionOverridden>(&store, "security.permission.overridden")
                .await
                .is_empty(),
            "acting on a permission you hold is not an override"
        );
    });
}
