// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a settled bill carries out for its receipt to print
//! ([ADR-0129](../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)).
//!
//! The document went from the seller's address straight to `Subtotal`: a guest got a total with no
//! way to check it and a tax authority got a document naming no goods, in a country whose Decree
//! 123/2020 Art. 10 lists the per-line contents an invoice must have. The store knew all along —
//! `sales.order_line.added` has carried the name and the unit price since it was written, as
//! `docs/pos-spec.md` §14.2's line snapshot — and the projection discarded both on the way in.
//!
//! These cases hold the three properties that make the printed document trustworthy: the rows are
//! the ones that were sold, they sum to the subtotal printed under them, and they say what the guest
//! agreed to rather than what the menu says today.

use std::sync::Arc;

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    Edge, EdgeSession, InMemoryReceipts, LineDraft, StaffAuth, StaffRoster, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, OrderLineId, ReasonCodeId, StoreId, TableId, TaxClassId,
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
use pos_proto::{PaymentMethod, SalesChannel};

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn item(seed: u128) -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(seed))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

/// The reason a void cites. Voiding an *unfired* line needs no manager, which is what lets this
/// suite stay about the receipt rather than about ADR-0115's step-up.
fn keyed_wrong() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(9_001))
}

/// A store that prices two dine-in items at 10 %, and holds the one reason a void needs.
///
/// `named` is the spelling the menu carries *now*, so a case can rename an item after the sale and
/// watch the receipt keep the old one.
fn session_naming(named: &str) -> EdgeSession {
    let menu = MenuCatalog::new()
        .with(MenuEntry::new(
            item(500),
            DisplayName::new(named),
            vnd(150_000),
            class(),
        ))
        .with(MenuEntry::new(
            item(501),
            DisplayName::new("Phở bò đặc biệt"),
            vnd(99_000),
            class(),
        ));
    let rates = TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
    let mut staff = StaffRoster::new();
    staff.insert(
        "SRV-1",
        StaffAuth {
            employee_id: Some(server().employee_id),
            permissions: PermissionSet::EMPTY.with(Permission::VoidFiredLine),
            pin_phc: None,
        },
    );
    let mut session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    session.reason_codes = PublishedReasonCodes::from_parts(vec![PublishedReasonCode::new(
        keyed_wrong(),
        ReasonCode::new("KEYED_WRONG"),
        DisplayName::new("Keyed in error"),
        vec![ReasonAction::VoidLine],
    )]);
    session.staff = staff;
    session
}

fn edge() -> Edge<FakeStore> {
    Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session_naming("Margherita"),
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

/// One line as a till would send it: the snapshot §14.2 demands, captured at add time.
fn draft(menu_item_id: MenuItemId, name: &str, milli: i64, unit: i64, total: i64) -> LineDraft {
    LineDraft {
        menu_item_id,
        display_name: DisplayName::new(name),
        quantity: Quantity::from_milli(milli),
        unit_price: vnd(unit),
        line_total: vnd(total),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

fn cash(minor: i64) -> Payment {
    Payment {
        method: PaymentMethod::Cash,
        tendered: vnd(minor),
        applied_to_bill: vnd(minor),
        tip: vnd(0),
    }
}

/// Seats the table and adds the two lines every case starts from, returning their ids.
async fn two_lines(edge: &Edge<FakeStore>) -> (OrderLineId, OrderLineId) {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    let pizza = edge
        .add_line(
            server(),
            table(),
            draft(item(500), "Margherita", 1_000, 150_000, 150_000),
        )
        .await
        .expect("adds the pizza")
        .order_line_id;
    let pho = edge
        .add_line(
            server(),
            table(),
            draft(item(501), "Phở bò đặc biệt", 2_000, 99_000, 198_000),
        )
        .await
        .expect("adds the phở")
        .order_line_id;
    (pizza, pho)
}

#[test]
fn a_settled_bill_carries_a_row_per_line_and_they_sum_to_its_subtotal() {
    // ADR-0129 decision 1. The identity is the point: the rows are the very records the tax classes
    // were folded from, so a receipt that prints them under a subtotal cannot disagree with it.
    run_ready(async {
        let edge = edge();
        two_lines(&edge).await;
        let opened = edge.open_bill(server(), table()).await.expect("opens");

        // 348,000 at 10 % is 382,800.
        let settled = edge
            .settle_bill(server(), opened.bill_id, vec![cash(382_800)], None)
            .await
            .expect("settles");

        let names: Vec<&str> = settled
            .lines
            .iter()
            .map(|line| line.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["Margherita", "Phở bò đặc biệt"]);

        let pho = settled.lines.get(1).expect("the second row");
        assert_eq!(pho.quantity, Quantity::from_milli(2_000));
        assert_eq!(pho.unit_price, vnd(99_000), "the đơn giá Art. 10 asks for");
        assert_eq!(pho.line_total, vnd(198_000), "and the thành tiền beside it");

        let totals = settled.totals.as_ref().expect("a settled bill has totals");
        let summed = settled
            .lines
            .iter()
            .try_fold(Money::zero(CurrencyCode::VND), |running, line| {
                running.checked_add(line.line_total)
            })
            .expect("the rows sum");
        assert_eq!(
            summed, totals.subtotal,
            "the rows add up to the subtotal printed under them"
        );
    });
}

#[test]
fn a_voided_line_is_not_on_the_receipt() {
    // ADR-0129 decision 4. A void is owed nothing, so it is taxed nothing; printing it would give a
    // document whose rows do not sum to its own subtotal, which is worse than a terse one. Where an
    // auditor looks for the void is the log, and ADR-0115's reason code is what answers there.
    run_ready(async {
        let edge = edge();
        let (pizza, _pho) = two_lines(&edge).await;
        edge.void_line(server(), pizza, keyed_wrong(), None)
            .await
            .expect("voids the pizza");
        let opened = edge.open_bill(server(), table()).await.expect("opens");

        // Only the phở is owed: 198,000 at 10 % is 217,800.
        let settled = edge
            .settle_bill(server(), opened.bill_id, vec![cash(217_800)], None)
            .await
            .expect("settles");

        let names: Vec<&str> = settled
            .lines
            .iter()
            .map(|line| line.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["Phở bò đặc biệt"], "the voided row is gone");
        assert_eq!(
            settled.lines.first().expect("the row").line_total,
            settled.totals.as_ref().expect("totals").subtotal,
            "and what is left still sums to the subtotal"
        );
    });
}

#[test]
fn the_receipt_says_what_the_guest_agreed_to_and_not_what_the_menu_says_now() {
    // ADR-0129 decision 3, which is §14.2 applied to the printed document: a line never re-reads the
    // live menu, so renaming an item — or repricing it — after the sale cannot alter a settled bill.
    // This is the one way a receipt row differs from a `CounterOrderLine`, which resolves against the
    // published menu on purpose because a screen showing an open order wants today's spelling.
    run_ready(async {
        let edge = edge();
        two_lines(&edge).await;

        // The cloud publishes a rename between the order and the payment.
        edge.apply_session(session_naming("Margherita (classic)"));

        let opened = edge.open_bill(server(), table()).await.expect("opens");
        let settled = edge
            .settle_bill(server(), opened.bill_id, vec![cash(382_800)], None)
            .await
            .expect("settles");

        assert_eq!(
            settled
                .lines
                .first()
                .expect("the pizza's row")
                .display_name
                .as_str(),
            "Margherita",
            "the name captured at add time, not the one published since"
        );
    });
}
