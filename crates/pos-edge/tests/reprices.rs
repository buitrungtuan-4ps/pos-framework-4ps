// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Overriding a line's price
//! ([ADR-0219](../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)
//! decision 2): before the bill opens, above zero and at most the store's multiple of the price the
//! line was added at, with a reason published for an override and a manager who may override.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    AppError, Approval, Edge, EdgeSession, InMemoryReceipts, StaffAuth, StaffRoster, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::SalesChannel;
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, OrderLineId, ReasonCodeId, StoreId, TableId,
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

/// A reason the store publishes for a price override.
fn agreed() -> ReasonCodeId {
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

/// A store at 10 % with an override reason, a discount reason and a manager who may override, at
/// three times the price.
fn session() -> EdgeSession {
    let class = EdgeSession::standard_tax_class();
    let mut session = EdgeSession::bootstrap().with_tax_rates(TaxRateTable::new().with(
        class,
        SalesChannel::DineIn,
        TaxRate::from_percent(10),
    ));
    session.reason_codes = PublishedReasonCodes::from_parts(vec![
        PublishedReasonCode::new(
            agreed(),
            ReasonCode::new("PRICE_AGREED"),
            DisplayName::new("Price agreed"),
            vec![ReasonAction::PriceOverride],
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
            permissions: PermissionSet::EMPTY.with(Permission::OverridePrice),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    session.staff = staff;
    session.billing.price_override_max_multiple = Some(3);
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

fn a_line_draft(item: u128, price: i64) -> pos_edge::LineDraft {
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

/// Seats a table and sells two of a 50,000 dish: the line to override.
async fn a_line(edge: &Edge<FakeStore>) -> OrderLineId {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    let mut two = a_line_draft(501, 50_000);
    two.quantity = Quantity::from_milli(2_000);
    two.line_total = vnd(100_000);
    edge.add_line(server(), table(), two)
        .await
        .expect("adds")
        .order_line_id
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
fn a_manager_overrides_a_price_and_the_total_and_tax_follow() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let line = a_line(&edge).await;

        let alone = edge
            .reprice_line(server(), line, vnd(40_000), agreed(), None)
            .await;
        assert!(
            matches!(alone, Err(AppError::ApprovalRequired)),
            "{alone:?}"
        );

        let repriced = edge
            .reprice_line(server(), line, vnd(40_000), agreed(), Some(&approval()))
            .await
            .expect("a manager approves the override");
        assert_eq!(repriced.previous_unit_price, vnd(50_000));
        assert_eq!(repriced.line_total, vnd(80_000));
        let bill = edge
            .open_bill(server(), table())
            .await
            .expect("opens the bill")
            .bill_id;
        assert_eq!(
            edge.bill_totals(bill).expect("totals").total_due,
            vnd(88_000)
        );

        let events = logged(&store).await;
        assert!(
            events.contains(&"sales.order_line.repriced".to_owned()),
            "{events:?}"
        );
        assert!(
            events.contains(&"security.permission.overridden".to_owned()),
            "{events:?}"
        );
    });
}

#[test]
fn an_override_is_refused_out_of_bounds_for_another_reason_and_once_the_bill_opens() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let line = a_line(&edge).await;

        // Three times 50,000, the price it was added at, even after an override lowered it.
        edge.reprice_line(server(), line, vnd(10_000), agreed(), Some(&approval()))
            .await
            .expect("overrides");
        let over = edge
            .reprice_line(server(), line, vnd(150_001), agreed(), Some(&approval()))
            .await;
        assert!(
            matches!(
                over,
                Err(AppError::Domain(DomainError::PriceOutOfRange {
                    ceiling_minor: 150_000
                }))
            ),
            "{over:?}"
        );
        let wrong_reason = edge
            .reprice_line(server(), line, vnd(60_000), goodwill(), Some(&approval()))
            .await;
        assert!(
            matches!(wrong_reason, Err(AppError::ReasonCodeNotValid)),
            "{wrong_reason:?}"
        );

        edge.open_bill(server(), table())
            .await
            .expect("opens the bill");
        let billed = edge
            .reprice_line(server(), line, vnd(60_000), agreed(), Some(&approval()))
            .await;
        assert!(
            matches!(billed, Err(AppError::BillAlreadyOpen)),
            "{billed:?}"
        );
    });
}

#[test]
fn an_override_is_rebuilt_from_the_log() {
    run_ready(async {
        let store = FakeStore::default();
        let line = {
            let edge = edge_over(store.clone());
            let line = a_line(&edge).await;
            edge.reprice_line(server(), line, vnd(40_000), agreed(), Some(&approval()))
                .await
                .expect("overrides");
            line
        };
        let restarted = edge_over(store);
        restarted.rebuild().await.expect("replays the log");
        // The ceiling is still three times the price it was added at, not the price it was set to.
        let over = restarted
            .reprice_line(server(), line, vnd(150_001), agreed(), Some(&approval()))
            .await;
        assert!(matches!(over, Err(AppError::Domain(_))), "{over:?}");
        let bill = restarted
            .open_bill(server(), table())
            .await
            .expect("opens the bill")
            .bill_id;
        assert_eq!(
            restarted.bill_totals(bill).expect("totals").total_due,
            vnd(88_000)
        );
    });
}
