// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Taking money off a bill, and the ceiling nobody has published
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md), roadmap B2.2).
//!
//! `billing.discount.applied` has been defined in `pos-proto`, with exactly the shape this needs,
//! since the schema was written. `pos_core::billing::assemble` has taken a `bill_discount` and
//! allocated it across tax classes for just as long, and `docs/ui-ux.md` §3 has promised the right
//! column would show "lines, discounts, service charge, tax". **Nothing ever emitted the event**
//! and the edge passed `Money::zero` in, so the allocation ran on every bill in the country and
//! could only ever produce the same answer.
//!
//! The half worth reading is the ceiling. `billing.discount.apply` is granted to a **server** and
//! is *not* PIN-flagged — its own description is "apply a discount up to the role's configured
//! ceiling" — and **no store publishes a ceiling**: nothing in the cloud authors one and no config
//! node carries one. Reading that absence as "no limit" would have handed every server an unbounded
//! till the day this shipped. It is read as zero, so a discount needs a manager until a store says
//! otherwise, and the day a ceiling is published a server's small discount starts working with no
//! code change.

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
    DeviceId, EmployeeId, MenuItemId, ReasonCodeId, StoreId, TableId, TaxClassId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

const MANAGER_CODE: &str = "MGR-1";
const MANAGER_PIN: &str = "4417";

/// The signed-in server. They hold `ApplyDiscount` — which every server role does by default — and
/// **not** the override. That pair is the whole subject of this file.
fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn manager_id() -> EmployeeId {
    EmployeeId::new(Ulid::from_u128(11))
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

/// A reason the store publishes **for a discount**.
fn goodwill() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_101))
}

/// A reason published only for voiding a line — the near miss that proves the list is per-action.
fn keyed_wrong() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_102))
}

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

fn approval() -> Approval {
    Approval {
        code: MANAGER_CODE.to_owned(),
        pin: MANAGER_PIN.to_owned(),
    }
}

/// A store selling one 150,000 item at 10%, with a discount reason, a server who may discount and a
/// manager who may exceed the ceiling.
fn session() -> EdgeSession {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Margherita"),
        vnd(150_000),
        class(),
    ));
    let rates = TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
    let reasons = PublishedReasonCodes::from_parts(vec![
        PublishedReasonCode::new(
            goodwill(),
            ReasonCode::new("GOODWILL"),
            DisplayName::new("Goodwill"),
            vec![ReasonAction::Discount],
        ),
        PublishedReasonCode::new(
            keyed_wrong(),
            ReasonCode::new("KEYED_WRONG"),
            DisplayName::new("Keyed in error"),
            vec![ReasonAction::VoidLine],
        ),
    ]);
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(manager_id()),
            permissions: PermissionSet::EMPTY.with(Permission::OverrideDiscountCeiling),
            pin_phc: Some(hash_of(MANAGER_PIN)),
        },
    );
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.reason_codes = reasons;
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

fn a_line() -> pos_edge::LineDraft {
    pos_edge::LineDraft {
        menu_item_id: item(),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(150_000),
        line_total: vnd(150_000),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// Seats a table, sells one thing, and opens the bill for it.
async fn a_bill(edge: &Edge<FakeStore>) -> pos_proto::ids::BillId {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    edge.add_line(server(), table(), a_line())
        .await
        .expect("adds");
    edge.open_bill(server(), table())
        .await
        .expect("opens the bill")
        .bill_id
}

/// Every event type the store holds, in order.
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

/// The headline: no ceiling is published, so a server alone cannot discount at all — and the
/// refusal names the override rather than the discount, which is what tells the till to fetch a
/// manager instead of telling the server they may not discount.
#[test]
fn a_server_cannot_discount_while_no_ceiling_is_published() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let bill = a_bill(&edge).await;

        let refused = edge
            .discount_bill(server(), bill, vnd(20_000), goodwill(), None)
            .await;

        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied {
                    permission: "billing.discount.override_ceiling"
                }))
            ),
            "expected the ceiling override to be named, got {refused:?}"
        );
    });
}

/// With a manager's PIN it goes through, the guest's total moves by the discount **plus its tax**,
/// and the approval is its own auditable event carrying the amount that exceeded the ceiling.
#[test]
fn a_manager_may_discount_and_the_tax_follows_the_reduced_base() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_over(store.clone());
        let bill = a_bill(&edge).await;

        // 150,000 at 10% is 165,000 before anything comes off.
        let before = edge.bill_totals(bill).expect("totals");
        assert_eq!(before.total_due, vnd(165_000));

        let totals = edge
            .discount_bill(server(), bill, vnd(20_000), goodwill(), Some(&approval()))
            .await
            .expect("a manager's PIN authorises the discount");

        // The base drops to 130,000 and the tax with it — 13,000, not the 15,000 it was. A
        // discount that left the tax alone would overcharge the guest and misstate the VAT return.
        assert_eq!(totals.discount_total, vnd(20_000));
        assert_eq!(totals.tax_total, vnd(13_000));
        assert_eq!(totals.total_due, vnd(143_000));

        let events = logged(&store).await;
        assert!(
            events.contains(&"billing.discount.applied".to_owned()),
            "the discount is recorded: {events:?}"
        );
        assert!(
            events.contains(&"security.permission.overridden".to_owned()),
            "the manager's override is its own event: {events:?}"
        );
    });
}

/// The reason list is per-action, exactly as a void's is: a reason the store published for voiding
/// a line is not a reason to knock money off a bill.
#[test]
fn a_reason_published_for_something_else_is_refused() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let bill = a_bill(&edge).await;

        let refused = edge
            .discount_bill(
                server(),
                bill,
                vnd(20_000),
                keyed_wrong(),
                Some(&approval()),
            )
            .await;

        assert!(
            matches!(refused, Err(AppError::VoidReasonNotValid)),
            "expected the reason to be refused, got {refused:?}"
        );
    });
}

/// A guest cannot be owed money by being sold food. The sum is what is checked, so the second
/// discount is refused although it would have been legal on its own.
#[test]
fn reductions_may_not_take_the_bill_below_nothing() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let bill = a_bill(&edge).await;

        edge.discount_bill(server(), bill, vnd(100_000), goodwill(), Some(&approval()))
            .await
            .expect("100,000 off a 150,000 base is fine");

        let refused = edge
            .discount_bill(server(), bill, vnd(100_000), goodwill(), Some(&approval()))
            .await;

        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::ReductionExceedsBill {
                    reduction_minor: 200_000,
                    reducible_minor: 150_000,
                }))
            ),
            "expected the pair to be refused, got {refused:?}"
        );
    });
}

/// A discount must survive a restart, or the store settles at full price against a guest holding a
/// receipt that says otherwise. This is the replay fold, driven the way a reboot drives it.
#[test]
fn a_discount_is_rebuilt_from_the_log() {
    run_ready(async {
        let store = FakeStore::default();
        let bill = {
            let edge = edge_over(store.clone());
            let bill = a_bill(&edge).await;
            edge.discount_bill(server(), bill, vnd(20_000), goodwill(), Some(&approval()))
                .await
                .expect("discounts");
            bill
        };

        // A second edge over the same store: the projection is empty until it reads the log.
        let restarted = edge_over(store.clone());
        restarted.rebuild().await.expect("replays the log");

        let totals = restarted
            .bill_totals(bill)
            .expect("the rebuilt bill prices");
        assert_eq!(
            totals.total_due,
            vnd(143_000),
            "the discount survived the restart"
        );
    });
}

/// The running check a till reads goes through the bill once one is open, so the figure on the
/// screen and the figure the settle proves are the same one. Before this it read the order, which
/// knows nothing about a reduction — the operator would have quoted the guest full price.
#[test]
fn the_running_check_shows_the_discount() {
    run_ready(async {
        let edge = edge_over(FakeStore::default());
        let bill = a_bill(&edge).await;
        edge.discount_bill(server(), bill, vnd(20_000), goodwill(), Some(&approval()))
            .await
            .expect("discounts");

        let check = edge
            .check_totals(table())
            .expect("the table's running check");
        assert_eq!(check.total_due, vnd(143_000));
        assert_eq!(check.discount_total, vnd(20_000));
    });
}
