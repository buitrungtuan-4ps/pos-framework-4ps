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
use pos_proto::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId};
use pos_proto::text::DisplayName;
use pos_proto::wire_enum::WireEnum;
use pos_proto::{
    DisplayButton, DisplayCategory as ProtoDisplayCategory, DisplayPlan,
    DisplaySubcategory as ProtoDisplaySubcategory, LayoutBook, MenuBook, MenuCatalog, MenuEntry,
};

use crate::catalog::{
    CatalogItem, DisplayCategory, DisplaySubcategory, LayoutButton, Menu, MenuId, MenuPlacement,
};
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

/// Compiles a tenant's display taxonomy and layout buttons into a per-channel [`LayoutBook`] — the
/// `layout` config node the POS / tablet / QR UI reads and the domain never does.
///
/// The layout twin of [`compile_menu`]: the same pure, deterministic move, on the presentation side.
/// Buttons are grouped `channel → display category → sub-category`, each group ordered by the button's
/// `sort` (ties broken by item id for determinism), and emitted as a [`DisplayPlan`] per channel.
///
/// It is **forgiving by design**, because a layout references two taxonomies that a person edits
/// independently: a button whose display category or sub-category is missing or archived is simply
/// **skipped** (the item just shows no button on that channel), rather than failing the whole publish
/// — the opposite of [`compile_menu`]'s refuse-on-unknown-item stance, because a stale button is a
/// presentation gap, not a pricing error. A button on an unrecognised or unspecified channel is
/// skipped too. Categories appear in ascending order of their buttons' minimum `sort`, so the plan is
/// stable across re-compiles.
#[must_use]
pub fn compile_layout_book(
    display_categories: &[DisplayCategory],
    display_subcategories: &[DisplaySubcategory],
    buttons: &[LayoutButton],
) -> LayoutBook {
    // Active taxonomy only; an archived grouping drops its buttons.
    let active_categories: BTreeMap<DisplayCategoryId, &DisplayCategory> = display_categories
        .iter()
        .filter(|category| category.status != EntityStatus::Archived)
        .map(|category| (category.display_category_id, category))
        .collect();
    let active_subcategories: BTreeMap<DisplaySubcategoryId, &DisplaySubcategory> =
        display_subcategories
            .iter()
            .filter(|subcategory| subcategory.status != EntityStatus::Archived)
            .map(|subcategory| (subcategory.display_subcategory_id, subcategory))
            .collect();

    // Group usable buttons by channel (wire token → known channel), preserving the sort key.
    let mut by_channel: BTreeMap<&'static str, (SalesChannel, Vec<&LayoutButton>)> =
        BTreeMap::new();
    for button in buttons {
        if button.sales_channel.is_unrecognised() {
            continue;
        }
        let channel = button.sales_channel.known();
        if channel == <SalesChannel as WireEnum>::UNSPECIFIED {
            continue;
        }
        if !active_categories.contains_key(&button.display_category_id) {
            continue;
        }
        // A named sub-category must exist, be active, and belong to the button's category.
        if let Some(subcategory_id) = button.display_subcategory_id {
            match active_subcategories.get(&subcategory_id) {
                Some(subcategory)
                    if subcategory.display_category_id == button.display_category_id => {}
                _ => continue,
            }
        }
        by_channel
            .entry(channel.as_wire())
            .or_insert_with(|| (channel, Vec::new()))
            .1
            .push(button);
    }

    let mut book = LayoutBook::new();
    for (_wire, (channel, mut channel_buttons)) in by_channel {
        // Deterministic order: by sort, then by item id to break ties.
        channel_buttons.sort_by(|a, b| {
            a.sort
                .cmp(&b.sort)
                .then(a.menu_item_id.cmp(&b.menu_item_id))
        });

        // Category display order = ascending minimum sort of its buttons.
        let mut category_order: Vec<DisplayCategoryId> = Vec::new();
        for button in &channel_buttons {
            if !category_order.contains(&button.display_category_id) {
                category_order.push(button.display_category_id);
            }
        }

        let mut plan = DisplayPlan::new();
        for category_id in category_order {
            let Some(category) = active_categories.get(&category_id) else {
                continue;
            };
            let group: Vec<&&LayoutButton> = channel_buttons
                .iter()
                .filter(|button| button.display_category_id == category_id)
                .collect();

            // Buttons placed directly under the category (no sub-category).
            let direct: Vec<DisplayButton> = group
                .iter()
                .filter(|button| button.display_subcategory_id.is_none())
                .map(|button| proto_button(button))
                .collect();

            // Sub-categories, in the order their first button appears.
            let mut subcategory_order: Vec<DisplaySubcategoryId> = Vec::new();
            for button in &group {
                if let Some(id) = button.display_subcategory_id
                    && !subcategory_order.contains(&id)
                {
                    subcategory_order.push(id);
                }
            }
            let subcategories: Vec<ProtoDisplaySubcategory> = subcategory_order
                .into_iter()
                .filter_map(|subcategory_id| {
                    let subcategory = active_subcategories.get(&subcategory_id)?;
                    let sub_buttons: Vec<DisplayButton> = group
                        .iter()
                        .filter(|button| button.display_subcategory_id == Some(subcategory_id))
                        .map(|button| proto_button(button))
                        .collect();
                    Some(ProtoDisplaySubcategory {
                        display_subcategory_id: subcategory_id,
                        name: DisplayName::new(subcategory.name.as_str()),
                        buttons: sub_buttons,
                    })
                })
                .collect();

            plan = plan.with(ProtoDisplayCategory {
                display_category_id: category_id,
                name: DisplayName::new(category.name.as_str()),
                buttons: direct,
                subcategories,
            });
        }
        book = book.with(channel, plan);
    }
    book
}

/// A compiled [`DisplayButton`] from an authoring [`LayoutButton`].
fn proto_button(button: &LayoutButton) -> DisplayButton {
    DisplayButton {
        menu_item_id: button.menu_item_id,
        label: DisplayName::new(button.label.as_str()),
        position: button.position,
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::display::GridPosition;
    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{
        DisplayCategoryId, DisplaySubcategoryId, MenuItemId, TaxClassId, TenantId,
    };
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{CompileError, compile_layout_book, compile_menu};
    use crate::catalog::{
        CatalogItem, ChannelPrice, DisplayCategory, DisplaySubcategory, LayoutButton, Menu, MenuId,
        MenuPlacement,
    };
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
            menu_section_id: None,
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

    fn display_category_id(n: u128) -> DisplayCategoryId {
        DisplayCategoryId::new(Ulid::from_u128(n))
    }

    fn display_subcategory_id(n: u128) -> DisplaySubcategoryId {
        DisplaySubcategoryId::new(Ulid::from_u128(n))
    }

    fn display_category(n: u128, name: &str, status: EntityStatus) -> DisplayCategory {
        DisplayCategory {
            display_category_id: display_category_id(n),
            tenant_id: tenant(),
            name: name.to_owned(),
            status,
        }
    }

    fn display_subcategory(n: u128, parent: u128, name: &str) -> DisplaySubcategory {
        DisplaySubcategory {
            display_subcategory_id: display_subcategory_id(n),
            tenant_id: tenant(),
            display_category_id: display_category_id(parent),
            name: name.to_owned(),
            status: EntityStatus::Active,
        }
    }

    fn layout_button(
        channel: SalesChannel,
        category: u128,
        subcategory: Option<u128>,
        item: u128,
        label: &str,
        position: Option<GridPosition>,
        sort: i32,
    ) -> LayoutButton {
        LayoutButton {
            tenant_id: tenant(),
            sales_channel: Open::from_known(channel),
            display_category_id: display_category_id(category),
            display_subcategory_id: subcategory.map(display_subcategory_id),
            menu_item_id: item_id(item),
            label: label.to_owned(),
            position,
            sort,
        }
    }

    #[test]
    fn a_layout_compiles_to_a_per_channel_plan_grouped_by_category_and_subcategory() {
        let categories = [display_category(10, "Pizza", EntityStatus::Active)];
        let subcategories = [display_subcategory(20, 10, "Vegetarian")];
        let buttons = [
            layout_button(
                SalesChannel::DineIn,
                10,
                None,
                500,
                "Margherita",
                Some(GridPosition { column: 0, row: 0 }),
                0,
            ),
            layout_button(
                SalesChannel::DineIn,
                10,
                Some(20),
                501,
                "Marinara",
                Some(GridPosition { column: 1, row: 0 }),
                1,
            ),
        ];

        let book = compile_layout_book(&categories, &subcategories, &buttons);
        let plan = book.plan_for(SalesChannel::DineIn);
        assert_eq!(plan.categories().len(), 1);
        let pizza = plan.categories().first().expect("a category");
        assert_eq!(pizza.name.as_str(), "Pizza");
        assert_eq!(pizza.buttons.len(), 1, "the direct button");
        assert_eq!(pizza.buttons[0].label.as_str(), "Margherita");
        assert_eq!(pizza.subcategories.len(), 1);
        assert_eq!(pizza.subcategories[0].buttons[0].label.as_str(), "Marinara");
        // A channel with no button of its own gets the empty fallback.
        assert!(book.plan_for(SalesChannel::Delivery).is_empty());
    }

    #[test]
    fn a_layout_button_under_an_archived_category_is_skipped() {
        let categories = [display_category(10, "Retired", EntityStatus::Archived)];
        let buttons = [layout_button(
            SalesChannel::DineIn,
            10,
            None,
            500,
            "Ghost",
            None,
            0,
        )];
        let book = compile_layout_book(&categories, &[], &buttons);
        assert!(
            book.plan_for(SalesChannel::DineIn).is_empty(),
            "an archived display category drops its buttons"
        );
    }
}
