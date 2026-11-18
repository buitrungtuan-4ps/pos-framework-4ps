// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Giving a line away
//! ([ADR-0219](../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)
//! decision 1): the whole line, once, with a reason published for a comp and a manager who may
//! comp, and the tax follows the lowered base.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, Edge, EdgeSession, InMemoryReceipts, StaffAuth, StaffRoster, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::SalesChannel;
use pos_proto::ids::{
    BillId, DeviceId, EmployeeId, MenuItemId, OrderLineId, ReasonCodeId, StoreId, TableId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

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

/// A reason the store publishes for a comp.
fn spilled() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_201))
}

/// A reason published only for a discount.
fn goodwill() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_202))
}

fn approval() -> Approval {
    Approval {
        code: MANAGER_CODE.to_owned(),
        pin: MANAGER_PIN.to_owned(),
    }
}

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), b"a-fixed-test-slt")
        .expect("hash")
        .to_string()
}

/// A store at 10 % with a comp reason, a discount reason and a manager who may comp.
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let mut session = EdgeSession::bootstrap().with_tax_rates(TaxRateTable::new().with(
        class,
        SalesChannel::DineIn,
        TaxRate::from_percent(10),
    ));
    session.reason_codes = PublishedReasonCodes::from_parts(vec![
        PublishedReasonCode::new(
            spilled(),
            ReasonCode::new("SPILLED"),
            DisplayName::new("Spilled"),
            vec![ReasonAction::Comp],
        ),
        PublishedReasonCode::new(
            goodwill(),
            ReasonCode::new("GOODWILL"),
            DisplayName::new("Goodwill"),
            vec![ReasonAction::Discount],
        ),
    ]);
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::EMPTY.with(Permission::ApplyComp),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    session.staff = staff;
    session
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

fn a_line(item: u128, price: i64) -> pos_edge::LineDraft {
    pos_edge::LineDraft {
        menu_item_id: MenuItemId::new(Ulid::from_u128(item)),
        display_name: DisplayName::new("Fake dish"),
        quantity: Quantity::ONE,
        unit_price: vnd(price),
        line_total: vnd(price),
        tax_class_id: EdgeSession::standard_tax_class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// Seats a table, sells a 150,000 and a 50,000 dish, and opens the bill: 220,000 with tax.
async fn a_bill(edge: &Edge<FakeStore>) -> (BillId, OrderLineId) {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    edge.add_line(server(), table(), a_line(500, 150_000))
        .await
        .expect("adds");
    let comped = edge
        .add_line(server(), table(), a_line(501, 50_000))
        .await
        .expect("adds")
        .order_line_id;
    let bill = edge
        .open_bill(server(), table())
        .await
        .expect("opens the bill")
        .bill_id;
    (bill, comped)
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
fn a_comp_takes_the_whole_line_and_its_tax_off_with_a_managers_approval() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let (bill, line) = a_bill(&edge).await;

        let alone = edge.comp_line(server(), bill, line, spilled(), None).await;
        assert!(
            matches!(alone, Err(AppError::ApprovalRequired)),
            "{alone:?}"
        );

        let totals = edge
            .comp_line(server(), bill, line, spilled(), Some(&approval()))
            .await
            .expect("a manager approves the comp");
        assert_eq!(totals.comp_total, vnd(50_000));
        assert_eq!(totals.tax_total, vnd(15_000));
        assert_eq!(totals.total_due, vnd(165_000));

        let events = logged(&store).await;
        let tail = &events[events.len() - 2..];
        assert_eq!(
            tail,
            ["billing.comp.applied", "security.permission.overridden"]
        );
    });
}

#[test]
fn a_line_is_comped_once_for_a_comp_reason_and_only_on_its_bill() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let (bill, line) = a_bill(&edge).await;

        let wrong_reason = edge
            .comp_line(server(), bill, line, goodwill(), Some(&approval()))
            .await;
        assert!(
            matches!(wrong_reason, Err(AppError::ReasonCodeNotValid)),
            "{wrong_reason:?}"
        );
        let stranger = OrderLineId::new(Ulid::from_u128(77));
        let unknown = edge
            .comp_line(server(), bill, stranger, spilled(), Some(&approval()))
            .await;
        assert!(matches!(unknown, Err(AppError::UnknownLine)), "{unknown:?}");

        edge.comp_line(server(), bill, line, spilled(), Some(&approval()))
            .await
            .expect("comps");
        let again = edge
            .comp_line(server(), bill, line, spilled(), Some(&approval()))
            .await;
        assert!(matches!(again, Err(AppError::AlreadyComped)), "{again:?}");
    });
}

#[test]
fn a_comp_is_rebuilt_from_the_log_and_still_refuses_a_second() {
    run_ready(async {
        let store = FakeStore::default();
        let (bill, line) = {
            let edge = edge_over(store.clone());
            let (bill, line) = a_bill(&edge).await;
            edge.comp_line(server(), bill, line, spilled(), Some(&approval()))
                .await
                .expect("comps");
            (bill, line)
        };

        let restarted = edge_over(store);
        restarted.rebuild().await.expect("replays the log");
        let totals = restarted
            .bill_totals(bill)
            .expect("the rebuilt bill prices");
        assert_eq!(totals.total_due, vnd(165_000));
        let again = restarted
            .comp_line(server(), bill, line, spilled(), Some(&approval()))
            .await;
        assert!(matches!(again, Err(AppError::AlreadyComped)), "{again:?}");
    });
}
