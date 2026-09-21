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

/// The server's own badge code, so a ceiling can be published against their identity.
const SERVER_CODE: &str = "SRV-1";
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
            discount_ceiling: None,
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

/// The same store, with a ceiling published for the server's role — what a tenant that has
/// configured one looks like.
///
/// Added to the roster under the server's own code, because the roster is keyed by the badge code a
/// person types and the ceiling is looked up by the identity their session carries. `permissions`
/// stays what it was: the point is that the *ceiling* is what changed, not what they are granted.
fn session_with_a_server_ceiling(ceiling: Money) -> EdgeSession {
    let mut session = session();
    session.staff.insert(
        SERVER_CODE,
        StaffAuth {
            employee_id: Some(server().employee_id),
            permissions: PermissionSet::EMPTY.with(Permission::ApplyDiscount),
            discount_ceiling: Some(ceiling),
            pin_phc: Some(hash_of("0000")),
        },
    );
    session
}

fn edge_over(store: FakeStore) -> Edge<FakeStore> {
    edge_with(store, session())
}

fn edge_with(store: FakeStore, session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        store,
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
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

/// The payload of every `security.permission.overridden` the store holds, as JSON.
///
/// Read as JSON rather than as the typed event because the point of the assertion is the *figure on
/// the wire* — what an auditor reading the log gets — and a typed read would prove the edge can
/// deserialise what it just serialised.
async fn overrides(store: &FakeStore) -> Vec<serde_json::Value> {
    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(1)),
        NonZeroU32::new(100).expect("a positive limit"),
    );
    store
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "security.permission.overridden")
        .filter_map(|envelope| serde_json::to_value(&envelope.data).ok())
        .collect()
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

/// The ceiling published, and the whole point of publishing one: **a server discounts on their own**.
///
/// Nothing about the permission set changed between this and
/// [`a_server_cannot_discount_while_no_ceiling_is_published`] — the same person, holding the same
/// `billing.discount.apply`, with no manager anywhere near the till. What changed is that their role
/// now carries a figure, which is exactly the promise ADR-0070's node made and had no field for.
#[test]
fn a_server_discounts_under_a_published_ceiling_without_a_manager() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_with(store.clone(), session_with_a_server_ceiling(vnd(30_000)));
        let bill = a_bill(&edge).await;

        let totals = edge
            .discount_bill(server(), bill, vnd(20_000), goodwill(), None)
            .await
            .expect("20,000 is under the published 30,000 ceiling");

        // 150,000 less 20,000 is 130,000, and the tax follows the reduced base: 13,000, not 15,000.
        assert_eq!(totals.discount_total, vnd(20_000));
        assert_eq!(totals.total_due, vnd(143_000));

        // And no override was recorded, because none was needed. A ceiling that still made the till
        // ask for a manager would be a ceiling in name only.
        let events = logged(&store).await;
        assert!(
            !events
                .iter()
                .any(|kind| kind == "security.permission.overridden"),
            "a discount inside the ceiling recorded an override: {events:?}"
        );
    });
}

/// A discount **at** the ceiling goes through, and a single minor unit over it does not.
///
/// The boundary is the interesting part of any limit, and `over_ceiling` is written as
/// `ceiling - amount` being negative — so equal is allowed. This pins that reading: change it to a
/// `<=` and this case says so.
#[test]
fn the_ceiling_is_inclusive_and_one_unit_over_it_is_not() {
    run_ready(async {
        // A store each: one table holds one open bill, so the two halves of a boundary have to be
        // asked of the same starting state rather than of a table the first half already moved on.
        let at_the_ceiling = edge_with(
            FakeStore::default(),
            session_with_a_server_ceiling(vnd(30_000)),
        );
        let exactly = a_bill(&at_the_ceiling).await;
        at_the_ceiling
            .discount_bill(server(), exactly, vnd(30_000), goodwill(), None)
            .await
            .expect("a discount of exactly the ceiling is within it");

        let past_it = edge_with(
            FakeStore::default(),
            session_with_a_server_ceiling(vnd(30_000)),
        );
        let over = a_bill(&past_it).await;
        let refused = past_it
            .discount_bill(server(), over, vnd(30_001), goodwill(), None)
            .await;
        assert!(
            matches!(
                refused,
                Err(AppError::Domain(DomainError::PermissionDenied {
                    permission: "billing.discount.override_ceiling"
                }))
            ),
            "one unit over the ceiling should want a manager, got {refused:?}"
        );
    });
}

/// Over a published ceiling, `exceeded_by` carries **the excess**, not the whole discount.
///
/// With no ceiling the two are the same number and the field could not tell them apart. With one
/// published they differ, which is what makes the figure worth recording: an auditor asking "how far
/// past their allowance did this go?" gets 5,000 rather than 35,000.
#[test]
fn an_override_records_how_far_over_the_ceiling_it_went() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = edge_with(store.clone(), session_with_a_server_ceiling(vnd(30_000)));
        let bill = a_bill(&edge).await;

        edge.discount_bill(server(), bill, vnd(35_000), goodwill(), Some(&approval()))
            .await
            .expect("a manager's PIN authorises going over");

        let recorded = overrides(&store).await;
        assert_eq!(recorded.len(), 1, "the override is recorded once");
        assert_eq!(
            recorded[0]["exceeded_by"]["amount_minor"], 5_000,
            "the figure is the excess over the ceiling, not the whole discount: {:?}",
            recorded[0]
        );
    });
}
