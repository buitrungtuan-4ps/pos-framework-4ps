// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge enforces a modifier group's selection rule
//! ([ADR-0127](../../../docs/adr/0127-modifier-groups-reach-the-edge.md) decision 5).
//!
//! The asymmetry this closes: the **write** path has been complete since `sales.order_line.added`
//! was written — a line carries `modifier_menu_item_ids`, a fire consumes the base recipe plus one
//! per modifier (`docs/pos-spec.md` §8), and `POST /api/tables/{id}/lines` accepts the ids. The
//! **read** path did not exist, so a till could record that a pizza was large and could not ask what
//! sizes there were.
//!
//! Decision 5 is the half worth testing: the *edge* validates. A required choice that a till which
//! forgot to ask can skip is required in name only, and by the time the kitchen reads the ticket the
//! line has already been priced without the size it was sold at.
//!
//! The case that is **not** a refusal is as important as the ones that are: an item attaching no
//! group is unconfigured, not empty-handed, and every store is publishing none today.

use std::sync::Arc;

use pos_core::decision::Actor;
use pos_edge::{AppError, Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{
    DeviceId, EmployeeId, MenuItemId, ModifierGroupId, StoreId, TableId, TaxClassId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry, MenuModifierGroup};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn menu_item(n: u128) -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(n))
}

fn group(n: u128) -> ModifierGroupId {
    ModifierGroupId::new(Ulid::from_u128(n))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(42))
}

fn class() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

/// The pizza (attaching a required Size and an optional Extras), its two sizes, one topping, and a
/// plain item attaching nothing at all.
fn session_with_groups() -> EdgeSession {
    let entry = |id: u128, name: &str, price: i64| {
        MenuEntry::new(menu_item(id), DisplayName::new(name), vnd(price), class())
    };
    let menu = MenuCatalog::new()
        .with(entry(500, "Margherita", 149_000).with_modifier_groups(vec![group(700), group(701)]))
        .with(entry(501, "Iced tea", 39_000))
        .with(entry(201, "Size — 25cm", 0))
        .with(entry(202, "Size — 30cm", 40_000))
        .with(entry(210, "Extra cheese", 25_000))
        .with_modifier_group(MenuModifierGroup {
            modifier_group_id: group(700),
            display_name: DisplayName::new("Size"),
            display_name_translations: std::collections::BTreeMap::new(),
            min_select: 1,
            max_select: 1,
            member_menu_item_ids: vec![menu_item(201), menu_item(202)],
        })
        .with_modifier_group(MenuModifierGroup {
            modifier_group_id: group(701),
            display_name: DisplayName::new("Extras"),
            display_name_translations: std::collections::BTreeMap::new(),
            min_select: 0,
            max_select: 1,
            member_menu_item_ids: vec![menu_item(210)],
        });
    let rates = TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
    EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates)
}

fn edge_over(session: EdgeSession) -> Edge<FakeStore> {
    Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

/// A line on `menu_item_id`, choosing `modifiers`.
fn a_line(menu_item_id: MenuItemId, modifiers: Vec<MenuItemId>) -> pos_edge::LineDraft {
    pos_edge::LineDraft {
        menu_item_id,
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: vnd(149_000),
        line_total: vnd(149_000),
        tax_class_id: class(),
        tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: modifiers,
        note_present: false,
    }
}

async fn seated(edge: &Edge<FakeStore>) {
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
}

/// A required group is required, and the refusal is the **edge's** — not the till's good manners.
#[test]
fn a_pizza_cannot_be_sold_without_the_size_the_store_requires() {
    run_ready(async {
        let edge = edge_over(session_with_groups());
        seated(&edge).await;

        let refused = edge
            .add_line(server(), table(), a_line(menu_item(500), Vec::new()))
            .await;
        assert!(
            matches!(refused, Err(AppError::ModifierSelectionInvalid)),
            "a pizza with no size chosen must be refused, got {refused:?}"
        );

        // With a size, it sells.
        edge.add_line(
            server(),
            table(),
            a_line(menu_item(500), vec![menu_item(202)]),
        )
        .await
        .expect("one size chosen satisfies the rule");
    });
}

/// `max_select` is the other half of the same number, and one rule broken is enough.
#[test]
fn two_sizes_is_above_the_maximum_and_refused() {
    run_ready(async {
        let edge = edge_over(session_with_groups());
        seated(&edge).await;

        let refused = edge
            .add_line(
                server(),
                table(),
                a_line(menu_item(500), vec![menu_item(201), menu_item(202)]),
            )
            .await;
        assert!(matches!(refused, Err(AppError::ModifierSelectionInvalid)));
    });
}

/// An optional group is optional — the pizza sells with a size and no cheese, and with both.
#[test]
fn an_optional_group_may_be_skipped() {
    run_ready(async {
        let edge = edge_over(session_with_groups());
        seated(&edge).await;

        edge.add_line(
            server(),
            table(),
            a_line(menu_item(500), vec![menu_item(201)]),
        )
        .await
        .expect("no cheese is a valid answer");

        edge.add_line(
            server(),
            table(),
            a_line(menu_item(500), vec![menu_item(201), menu_item(210)]),
        )
        .await
        .expect("a size and the cheese");
    });
}

/// The second kind of mistake: a modifier belonging to no group this item attaches. Without this a
/// device could name any item id at all, and the kitchen would be told to add it and the guest
/// charged for it.
#[test]
fn a_modifier_no_group_offers_is_refused() {
    run_ready(async {
        let edge = edge_over(session_with_groups());
        seated(&edge).await;

        let refused = edge
            .add_line(
                server(),
                table(),
                // A size, so every group's rule is satisfied — and an iced tea smuggled in beside it.
                a_line(menu_item(500), vec![menu_item(201), menu_item(501)]),
            )
            .await;
        assert!(
            matches!(refused, Err(AppError::ModifierSelectionInvalid)),
            "an item no attached group offers must be refused, got {refused:?}"
        );
    });
}

/// An item attaching **no** group asks nothing, which is most of a menu.
#[test]
fn an_item_with_no_groups_sells_with_no_questions() {
    run_ready(async {
        let edge = edge_over(session_with_groups());
        seated(&edge).await;

        edge.add_line(server(), table(), a_line(menu_item(501), Vec::new()))
            .await
            .expect("an iced tea has nothing to ask");
    });
}

/// **The compatibility case, and the one that matters most today.**
///
/// Every store is publishing no groups at all. `add_line` has accepted `modifier_menu_item_ids`
/// since long before groups existed, so refusing a modifier on an item that attaches nothing would
/// break lines that work right now, on every store, until the console got round to attaching one.
/// An item with no groups is *unconfigured*, not empty-handed, and the edge enforces only what has
/// been published.
#[test]
fn a_store_that_has_published_no_groups_still_takes_modifiers() {
    run_ready(async {
        let plain = EdgeSession::bootstrap()
            .with_menu(MenuCatalog::new().with(MenuEntry::new(
                menu_item(500),
                DisplayName::new("Margherita"),
                vnd(149_000),
                class(),
            )))
            .with_tax_rates(TaxRateTable::new().with(
                class(),
                SalesChannel::DineIn,
                TaxRate::from_percent(10),
            ));
        let edge = edge_over(plain);
        seated(&edge).await;

        edge.add_line(
            server(),
            table(),
            a_line(menu_item(500), vec![menu_item(210)]),
        )
        .await
        .expect("a modifier on a store with no groups published is not this edge's business");
    });
}
