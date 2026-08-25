// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The menu compiler ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
//!
//! The keystone of the catalog: the pure function that turns the rich, normalized authoring model
//! ([`crate::catalog`]) into the flat, per-channel [`MenuBook`] the edge reprices from. It runs in the
//! cloud at publish time; the store receives only its output.
//!
//! Two things happen here, and nothing else:
//!
//! 1. **Inheritance is folded.** A menu may inherit from a parent (a brand menu over a tenant
//!    standard, a store special over the brand). The compiler walks the chain from the requested menu
//!    up through its parents and resolves each item **most-specific-wins**: the requested menu's
//!    placement for an item overrides an ancestor's.
//! 2. **Channel is chosen.** Each surviving placement carries a price per channel; the compiler emits
//!    one [`pos_proto::MenuEntry`] into each channel's [`pos_proto::MenuCatalog`], so the same item is
//!    one price dine-in and another on delivery — exactly why the compiled node is a `MenuBook` and
//!    not one catalog. An item priced on no channel sells on none; a channel priced by no item is
//!    absent from the book.
//!
//! The output is deterministic (channels ordered by their wire token, entries by item id) so a
//! re-compile of unchanged authoring produces a byte-identical snapshot — the config tree only ships
//! a new version when something actually changed. This compiles **one** menu; which menu applies to a
//! given store is a menu-assignment concern a later slice resolves, and then calls this per channel.

use std::collections::{BTreeMap, BTreeSet};

use pos_proto::enums::SalesChannel;
use pos_proto::ids::MenuItemId;
use pos_proto::text::DisplayName;
use pos_proto::wire_enum::WireEnum;
use pos_proto::{MenuBook, MenuCatalog, MenuEntry};

use crate::catalog::{CatalogItem, Menu, MenuId, MenuPlacement};
use crate::registry::EntityStatus;

/// A refusal to compile a menu — a configuration error the operator must fix, distinct from a store
/// failure. Each names exactly what is wrong so a publish can report it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompileError {
    /// The requested menu, or one named as a parent, does not exist.
    #[error("menu {0} was not found")]
    UnknownMenu(MenuId),
    /// The inheritance chain loops back on itself.
    #[error("menu inheritance forms a cycle at {0}")]
    InheritanceCycle(MenuId),
    /// A placement prices an item that is not in the catalog — never substitute, always refuse
    /// (the same rule the store's reprice follows for an unknown item).
    #[error("a placement references item {0}, which is not in the catalog")]
    UnknownItem(MenuItemId),
}

/// Compiles one menu (with its inheritance chain) into a per-channel [`MenuBook`].
///
/// `items`, `menus` and `placements` are a tenant's whole authoring model as loaded from a
/// [`CatalogStore`](crate::catalog::CatalogStore); `root` is the menu to compile. Archived items are
/// omitted (retired, not sold); an unavailable placement compiles to a **present but 86'd** entry
/// (`available = false`), because the store distinguishes "not on the menu" from "on the menu, paused"
/// — the latter is the operator's published floor, which live stock can only lower further.
///
/// # Errors
///
/// [`CompileError`] when a menu or a parent is missing, the inheritance chain cycles, or a placement
/// prices an item the catalog does not carry.
pub fn compile_menu(
    items: &[CatalogItem],
    menus: &[Menu],
    placements: &[MenuPlacement],
    root: MenuId,
) -> Result<MenuBook, CompileError> {
    let item_by_id: BTreeMap<MenuItemId, &CatalogItem> =
        items.iter().map(|item| (item.menu_item_id, item)).collect();
    let menu_by_id: BTreeMap<MenuId, &Menu> =
        menus.iter().map(|menu| (menu.menu_id, menu)).collect();

    // The chain from the requested menu up through its parents, most-specific first.
    let mut chain: Vec<MenuId> = Vec::new();
    let mut seen: BTreeSet<MenuId> = BTreeSet::new();
    let mut current = Some(root);
    while let Some(id) = current {
        if !seen.insert(id) {
            return Err(CompileError::InheritanceCycle(id));
        }
        let menu = menu_by_id.get(&id).ok_or(CompileError::UnknownMenu(id))?;
        chain.push(id);
        current = menu.parent_menu_id;
    }

    // Resolve each item to its most-specific placement: the requested menu wins over an ancestor.
    let mut resolved: BTreeMap<MenuItemId, &MenuPlacement> = BTreeMap::new();
    for menu_id in &chain {
        for placement in placements.iter().filter(|p| p.menu_id == *menu_id) {
            resolved.entry(placement.menu_item_id).or_insert(placement);
        }
    }

    // Emit an entry per (item, priced channel). `resolved` iterates in item-id order, so a channel's
    // entries come out sorted; channels are keyed by wire token for a stable book order.
    let mut by_channel: BTreeMap<&'static str, (SalesChannel, Vec<MenuEntry>)> = BTreeMap::new();
    for (item_id, placement) in &resolved {
        let item = item_by_id
            .get(item_id)
            .ok_or(CompileError::UnknownItem(*item_id))?;
        if item.status == EntityStatus::Archived {
            continue;
        }
        for price in &placement.prices {
            if price.sales_channel.is_unrecognised() {
                continue;
            }
            let channel = price.sales_channel.known();
            if channel == <SalesChannel as WireEnum>::UNSPECIFIED {
                continue;
            }
            let entry = MenuEntry {
                menu_item_id: *item_id,
                display_name: DisplayName::new(item.name.as_str()),
                unit_price: price.unit_price,
                tax_class_id: item.tax_class_id,
                available: placement.available,
            };
            by_channel
                .entry(channel.as_wire())
                .or_insert_with(|| (channel, Vec::new()))
                .1
                .push(entry);
        }
    }

    let mut book = MenuBook::new();
    for (_wire, (channel, entries)) in by_channel {
        book = book.with(channel, MenuCatalog::from_items(entries));
    }
    Ok(book)
}

#[cfg(test)]
mod tests {
    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{CompileError, compile_menu};
    use crate::catalog::{CatalogItem, ChannelPrice, Menu, MenuId, MenuPlacement};
    use crate::registry::EntityStatus;

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn item_id(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn menu_id(n: u128) -> MenuId {
        MenuId::new(Ulid::from_u128(n))
    }

    fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    fn item(id: u128, name: &str) -> CatalogItem {
        CatalogItem {
            menu_item_id: item_id(id),
            tenant_id: tenant(),
            name: name.to_owned(),
            tax_class_id: TaxClassId::new(Ulid::from_u128(7)),
            item_category_id: None,
            item_subcategory_id: None,
            status: EntityStatus::Active,
        }
    }

    fn menu(id: u128, parent: Option<u128>) -> Menu {
        Menu {
            menu_id: menu_id(id),
            tenant_id: tenant(),
            name: format!("menu-{id}"),
            parent_menu_id: parent.map(menu_id),
            status: EntityStatus::Active,
        }
    }

    fn price(channel: SalesChannel, minor: i64) -> ChannelPrice {
        ChannelPrice {
            sales_channel: Open::from_known(channel),
            unit_price: vnd(minor),
        }
    }

    fn placement(
        menu: u128,
        item: u128,
        prices: Vec<ChannelPrice>,
        available: bool,
    ) -> MenuPlacement {
        MenuPlacement {
            tenant_id: tenant(),
            menu_id: menu_id(menu),
            menu_item_id: item_id(item),
            prices,
            available,
        }
    }

    #[test]
    fn a_flat_menu_compiles_to_per_channel_catalogs() {
        let items = [item(500, "Margherita")];
        let menus = [menu(10, None)];
        let placements = [placement(
            10,
            500,
            vec![
                price(SalesChannel::DineIn, 150_000),
                price(SalesChannel::Delivery, 180_000),
            ],
            true,
        )];

        let book = compile_menu(&items, &menus, &placements, menu_id(10)).expect("compile");

        let dine_in = book
            .catalog_for(SalesChannel::DineIn)
            .get(item_id(500))
            .expect("priced dine-in");
        assert_eq!(dine_in.unit_price, vnd(150_000));
        assert_eq!(dine_in.display_name.as_str(), "Margherita");
        assert!(dine_in.available);

        assert_eq!(
            book.catalog_for(SalesChannel::Delivery)
                .get(item_id(500))
                .expect("priced delivery")
                .unit_price,
            vnd(180_000),
            "the same item is a different price on delivery"
        );

        assert!(
            book.catalog_for(SalesChannel::Takeaway).is_empty(),
            "a channel the item is not priced on sells nothing"
        );
    }

    #[test]
    fn a_child_menu_overrides_a_parents_price_and_inherits_the_rest() {
        let items = [item(500, "Margherita"), item(501, "Marinara")];
        let menus = [menu(10, None), menu(11, Some(10))];
        let placements = [
            // Parent prices both.
            placement(10, 500, vec![price(SalesChannel::DineIn, 150_000)], true),
            placement(10, 501, vec![price(SalesChannel::DineIn, 120_000)], true),
            // Child overrides only 500.
            placement(11, 500, vec![price(SalesChannel::DineIn, 175_000)], true),
        ];

        let book = compile_menu(&items, &menus, &placements, menu_id(11)).expect("compile");
        let dine_in = book.catalog_for(SalesChannel::DineIn);
        assert_eq!(
            dine_in.get(item_id(500)).expect("overridden").unit_price,
            vnd(175_000),
            "the child's price wins over the parent's"
        );
        assert_eq!(
            dine_in.get(item_id(501)).expect("inherited").unit_price,
            vnd(120_000),
            "an item the child does not touch is inherited from the parent"
        );
    }

    #[test]
    fn an_unavailable_placement_compiles_to_an_86d_entry() {
        let items = [item(500, "Margherita")];
        let menus = [menu(10, None)];
        let placements = [placement(
            10,
            500,
            vec![price(SalesChannel::DineIn, 150_000)],
            false,
        )];

        let book = compile_menu(&items, &menus, &placements, menu_id(10)).expect("compile");
        let entry = book
            .catalog_for(SalesChannel::DineIn)
            .get(item_id(500))
            .expect("present");
        assert!(
            !entry.available,
            "a paused item is present but not for sale, not absent"
        );
    }

    #[test]
    fn an_archived_item_is_omitted_entirely() {
        let items = [CatalogItem {
            status: EntityStatus::Archived,
            ..item(500, "Retired")
        }];
        let menus = [menu(10, None)];
        let placements = [placement(
            10,
            500,
            vec![price(SalesChannel::DineIn, 150_000)],
            true,
        )];

        let book = compile_menu(&items, &menus, &placements, menu_id(10)).expect("compile");
        assert!(
            book.catalog_for(SalesChannel::DineIn)
                .get(item_id(500))
                .is_none(),
            "an archived item is not sold at all"
        );
    }

    #[test]
    fn a_placement_for_an_unknown_item_is_refused() {
        let items = [item(500, "Margherita")];
        let menus = [menu(10, None)];
        let placements = [placement(
            10,
            999,
            vec![price(SalesChannel::DineIn, 1)],
            true,
        )];

        assert_eq!(
            compile_menu(&items, &menus, &placements, menu_id(10)),
            Err(CompileError::UnknownItem(item_id(999)))
        );
    }

    #[test]
    fn an_inheritance_cycle_is_rejected() {
        let items: [CatalogItem; 0] = [];
        let menus = [menu(10, Some(11)), menu(11, Some(10))];
        assert_eq!(
            compile_menu(&items, &menus, &[], menu_id(10)),
            Err(CompileError::InheritanceCycle(menu_id(10)))
        );
    }

    #[test]
    fn an_unknown_menu_is_rejected() {
        let items: [CatalogItem; 0] = [];
        assert_eq!(
            compile_menu(&items, &[], &[], menu_id(42)),
            Err(CompileError::UnknownMenu(menu_id(42)))
        );
    }

    #[test]
    fn compiling_is_deterministic() {
        let items = [item(500, "Margherita"), item(501, "Marinara")];
        let menus = [menu(10, None)];
        let placements = [
            placement(
                10,
                501,
                vec![
                    price(SalesChannel::Delivery, 2),
                    price(SalesChannel::DineIn, 1),
                ],
                true,
            ),
            placement(10, 500, vec![price(SalesChannel::DineIn, 3)], true),
        ];
        let first = compile_menu(&items, &menus, &placements, menu_id(10)).expect("compile");
        let second = compile_menu(&items, &menus, &placements, menu_id(10)).expect("compile");
        assert_eq!(
            serde_json::to_string(&first).expect("serialise"),
            serde_json::to_string(&second).expect("serialise"),
            "the same authoring compiles to a byte-identical snapshot"
        );
    }
}
