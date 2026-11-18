// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bill taking its payments one at a time
//! ([ADR-0220](../../../docs/adr/0220-a-bill-takes-its-payments-one-at-a-time.md)): each recorded
//! as it is taken, the last settling, and nothing done to a part-paid bill that would lose what was
//! paid.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, Edge, EdgeSession, InMemoryReceipts, StaffAuth, StaffRoster, StoreIdentity,
    TillScope,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::billing::PublishedBilling;
use pos_proto::ids::{BillId, DeviceId, EmployeeId, MenuItemId, ReasonCodeId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{PaymentMethod, SalesChannel};

const MANAGER_CODE: &str = "MGR-1";
const MANAGER_PIN: &str = "4417";

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

fn goodwill() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_301))
}

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), b"a-fixed-test-slt")
        .expect("hash")
        .to_string()
}

/// A store at 10 % with a discount reason and a manager who may discount past the ceiling, its
/// `billing` node as given.
fn session(billing: PublishedBilling) -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let mut session = EdgeSession::bootstrap().with_tax_rates(TaxRateTable::new().with(
        class,
        SalesChannel::DineIn,
        TaxRate::from_percent(10),
    ));
    session.reason_codes = PublishedReasonCodes::from_parts(vec![PublishedReasonCode::new(
        goodwill(),
        ReasonCode::new("GOODWILL"),
        DisplayName::new("Goodwill"),
        vec![ReasonAction::Discount, ReasonAction::VoidBill],
    )]);
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::EMPTY
                .with(Permission::OverrideDiscountCeiling)
                .with(Permission::VoidBill),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    session.staff = staff;
    session.billing = billing;
    session
}

fn edge_with(store: FakeStore, billing: PublishedBilling) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session(billing),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

/// Seats a table, sells one 150,000 dish and opens its bill: 165,000 with tax.
async fn a_bill(edge: &Edge<FakeStore>) -> BillId {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    edge.add_line(
        server(),
        table(),
        pos_edge::LineDraft {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
            display_name: DisplayName::new("Fake dish"),
            quantity: Quantity::ONE,
            unit_price: vnd(150_000),
            line_total: vnd(150_000),
            tax_class_id: EdgeSession::standard_tax_class(),
            tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
            seat: None,
            course_id: None,
            modifier_menu_item_ids: Vec::new(),
            note_present: false,
        },
    )
    .await
    .expect("adds");
    edge.open_bill(server(), table())
        .await
        .expect("opens the bill")
        .bill_id
}

fn cash(applied: i64) -> Payment {
    Payment {
        method: PaymentMethod::Cash,
        tendered: vnd(applied),
        applied_to_bill: vnd(applied),
        tip: vnd(0),
    }
}

async fn logged(store: &FakeStore) -> Vec<String> {
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(100).expect("a positive limit"),
    );
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| envelope.event_type.as_str().to_owned())
        .collect()
}

#[test]
fn each_payment_is_recorded_as_it_is_taken_and_the_last_settles() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_with(store.clone(), PublishedBilling::default());
        let bill = a_bill(&edge).await;

        let first = edge
            .take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(100_000),
                Some(2),
                None,
            )
            .await
            .expect("takes a part");
        assert_eq!(
            (first.paid_total, first.remaining),
            (vnd(100_000), vnd(65_000))
        );
        assert!(first.settled.is_none());
        assert_eq!(
            logged(&store).await.last().map(String::as_str),
            Some("billing.payment.captured"),
            "recorded at once, the bill still open"
        );
        // Every till reads it, from the log as well as live: how, how much, and the split.
        let order = edge.order_for_bill(bill).expect("the bill's order");
        let replayed = edge_with(store.clone(), PublishedBilling::default());
        replayed.rebuild().await.expect("replays");
        for reader in [&edge, &replayed] {
            let payments = reader.payments_towards(order, Some(bill));
            assert_eq!(payments.len(), 1, "{payments:?}");
            assert_eq!(
                (payments[0].method, payments[0].amount, payments[0].split_ways),
                (PaymentMethod::Cash, vnd(100_000), Some(2))
            );
        }

        let over = edge
            .take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(65_001),
                None,
                None,
            )
            .await;
        assert!(
            matches!(over, Err(AppError::PaymentExceedsRemaining)),
            "{over:?}"
        );

        let last = edge
            .take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(65_000),
                Some(2),
                None,
            )
            .await
            .expect("the last settles");
        let settled = last.settled.expect("settled");
        assert_eq!(settled.receipt_number, Some(1));
        assert_eq!(last.remaining, vnd(0));
        assert!(
            edge.payments_towards(order, Some(bill)).is_empty(),
            "a settled bill lists nothing to collect against"
        );
    });
}

#[test]
fn a_store_that_takes_a_bill_in_one_payment_refuses_a_part_and_settles_the_whole() {
    run_ready(async {
        let edge = edge_with(
            FakeStore::default(),
            PublishedBilling {
                part_payments: Some(false),
                ..PublishedBilling::default()
            },
        );
        let bill = a_bill(&edge).await;
        let part = edge
            .take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(100_000),
                None,
                None,
            )
            .await;
        assert!(matches!(part, Err(AppError::PartPaymentsOff)), "{part:?}");
        let whole = edge
            .take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(165_000),
                None,
                None,
            )
            .await
            .expect("one payment settles");
        assert!(whole.settled.is_some());
    });
}

#[test]
fn a_part_paid_bill_is_not_split_merged_voided_reduced_below_what_is_paid_or_closed_under() {
    run_ready(async {
        let edge = edge_with(FakeStore::default(), PublishedBilling::default());
        let shift = edge
            .open_shift(server(), vnd(0))
            .await
            .expect("opens a shift")
            .shift_id;
        let bill = a_bill(&edge).await;
        edge.take_payment_at(
            server(),
            TillScope::default(),
            bill,
            cash(100_000),
            None,
            None,
        )
        .await
        .expect("takes a part");

        let split = edge.split_bill(server(), bill, vec![]).await;
        assert!(matches!(split, Err(AppError::BillPartPaid)), "{split:?}");
        let approval = Approval {
            code: MANAGER_CODE.to_owned(),
            pin: MANAGER_PIN.to_owned(),
        };
        let below = edge
            .discount_bill(server(), bill, vnd(100_000), goodwill(), Some(&approval))
            .await;
        assert!(matches!(below, Err(AppError::BillPartPaid)), "{below:?}");
        edge.count_shift(server(), shift, 100_000)
            .await
            .expect("counts");
        let close = edge.close_shift(server(), shift).await;
        assert!(matches!(close, Err(AppError::BillPartPaid)), "{close:?}");

        edge.take_payment_at(
            server(),
            TillScope::default(),
            bill,
            cash(65_000),
            None,
            None,
        )
        .await
        .expect_err("a counted drawer takes no more cash");
    });
}

#[test]
fn a_part_payment_is_rebuilt_from_the_log() {
    run_ready(async {
        let store = FakeStore::default();
        let bill = {
            let edge = edge_with(store.clone(), PublishedBilling::default());
            let bill = a_bill(&edge).await;
            edge.take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(100_000),
                None,
                None,
            )
            .await
            .expect("takes a part");
            bill
        };
        let restarted = edge_with(store, PublishedBilling::default());
        restarted.rebuild().await.expect("replays the log");
        let over = restarted
            .take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(165_000),
                None,
                None,
            )
            .await;
        assert!(
            matches!(over, Err(AppError::PaymentExceedsRemaining)),
            "{over:?}"
        );
        let last = restarted
            .take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(65_000),
                None,
                None,
            )
            .await
            .expect("settles what remains");
        assert!(last.settled.is_some());
    });
}

#[test]
fn voiding_a_part_paid_bill_gives_each_payment_back_live_and_after_a_restart() {
    run_ready(async {
        let store = FakeStore::default();
        let shift = {
            let edge = edge_with(store.clone(), PublishedBilling::default());
            let shift = edge
                .open_shift(server(), vnd(0))
                .await
                .expect("opens a shift")
                .shift_id;
            let bill = a_bill(&edge).await;
            edge.take_payment_at(
                server(),
                TillScope::default(),
                bill,
                cash(100_000),
                None,
                None,
            )
            .await
            .expect("takes a part");
            let approval = Approval {
                code: MANAGER_CODE.to_owned(),
                pin: MANAGER_PIN.to_owned(),
            };
            let voided = edge
                .void_bill_at(
                    server(),
                    TillScope::default(),
                    bill,
                    goodwill(),
                    Some(&approval),
                )
                .await
                .expect("voids, giving the payment back");
            assert_eq!(voided.cash_returned, Some(vnd(100_000)));
            assert!(voided.open_drawer());
            shift
        };
        // After a restart the drawer still expects nothing: the cash went in and came back out.
        let restarted = edge_with(store, PublishedBilling::default());
        restarted.rebuild().await.expect("replays the log");
        restarted
            .count_shift(server(), shift, 0)
            .await
            .expect("counts");
        let closed = restarted
            .close_shift(server(), shift)
            .await
            .expect("no part-paid bill holds the drawer open");
        assert_eq!(closed.expected_amount, Some(vnd(0)), "{closed:?}");
        assert_eq!(closed.variance, Some(vnd(0)), "{closed:?}");
    });
}
