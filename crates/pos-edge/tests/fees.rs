// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge charges a bill's fees
//! ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)).
//!
//! A bill freezes the rules in force on its channel when it opens and is computed from those until
//! it settles (decision 3), so a publish during a meal reaches the next bill and not the one on the
//! table, and a restart folds the same rules back from the log. A split part keeps the rules its
//! source froze, with a fee per bill shared out rather than charged again on every part
//! (decision 6); a merge keeps the rules of the bill the cashier holds; a bill opened again after a
//! void freezes what is in force then.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::BillTotals;
use pos_core::decision::Actor;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    Approval, Edge, EdgeSession, InMemoryReceipts, LineDraft, StaffAuth, StaffRoster, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::events::BillingBillOpened;
use pos_proto::fees::{FrozenFee, PublishedFees};
use pos_proto::ids::{
    BillId, DeviceId, EmployeeId, FeeId, MenuItemId, OrderLineId, StoreId, TableId, TaxClassId,
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

/// A `fees` node of `rules`, parsed as the edge parses a published one.
fn fees(rules: &[Value]) -> PublishedFees {
    serde_json::from_str(&json!({ "fees": rules }).to_string()).expect("a fees node")
}

/// A store that prices one dish at 10% and charges `fees`, with one manager who may void a bill.
fn session(fees: PublishedFees) -> EdgeSession {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Bún chả"),
        vnd(UNIT_PRICE),
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
            permissions: PermissionSet::EMPTY.with(Permission::VoidBill),
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

/// The rules every `billing.bill.opened` in the log froze, in the order the bills opened.
async fn frozen_on_open(store: &FakeStore) -> Vec<Vec<FrozenFee>> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(200).expect("a positive limit"));
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "billing.bill.opened")
        .map(|envelope| {
            envelope
                .data
                .decode::<BillingBillOpened>()
                .expect("an opened bill decodes")
                .fee_rules
        })
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
/// did. A merge keeps the rules of the bill the cashier holds, and with them its share.
#[test]
fn a_split_shares_a_fee_per_bill_and_a_merge_keeps_the_holders_rules() {
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

        // Folded back together, the bill the cashier holds keeps its own rules: its share of the
        // cover charge, and the service charge on every line it now owes.
        let survivor = edge
            .merge_bills(server(), *one, vec![*two])
            .await
            .expect("merges");
        assert_eq!(
            charged(&totals(&edge, survivor)),
            [("COVER", 3_333), ("SERVICE", 7_500)]
        );
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
