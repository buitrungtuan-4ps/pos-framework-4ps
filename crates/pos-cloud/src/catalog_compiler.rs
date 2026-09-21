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
use pos_proto::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId, ModifierGroupId};
use pos_proto::text::DisplayName;
use pos_proto::wire_enum::WireEnum;
use pos_proto::{
    DisplayButton, DisplayCategory as ProtoDisplayCategory, DisplayPlan,
    DisplaySubcategory as ProtoDisplaySubcategory, LayoutBook, MenuBook, MenuCatalog, MenuEntry,
    MenuModifierGroup,
};

use crate::catalog::{
    CatalogItem, DisplayCategory, DisplaySubcategory, LayoutButton, Menu, MenuId, MenuPlacement,
    ModifierGroup,
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
    /// The menu being compiled, or one it inherits from, is archived.
    ///
    /// The compiler already omitted archived *items*, and the asymmetry was the defect: a menu an
    /// operator had retired still compiled and still published, so "archived" meant something for
    /// one entity in this model and nothing for the other. An archived ancestor is the worse half —
    /// its prices are what a store special silently rests on, and quietly dropping them would take
    /// items off a till with nothing said.
    #[error("menu {0} is archived, so it cannot be published")]
    ArchivedMenu(MenuId),
    /// A placement prices an item that is not in the catalog — never substitute, always refuse
    /// (the same rule the store's reprice follows for an unknown item).
    #[error("a placement references item {0}, which is not in the catalog")]
    UnknownItem(MenuItemId),
}

/// Whether re-parenting `menu_id` onto `proposed_parent` would make the inheritance chain loop.
///
/// The compiler already refuses a cycle, and that was the whole defence: a cycle was **authorable**
/// from the Menus screen and only fatal later, at publish, as a `422` several screens away from the
/// dropdown that caused it. An operator who set a parent got a saved menu and a working screen, and
/// learned about it when a store failed to receive a book.
///
/// So the graph is checked at the write instead, and this is the pure half of that check. It walks
/// upward from the proposed parent through the menus as they are *stored* — the only edge that
/// changes is the one being proposed, and it is the walk's starting point — and answers yes if it
/// reaches `menu_id`. A menu proposed as its own parent is the degenerate case and is caught by the
/// same test on the first step.
///
/// The `seen` set is not belt-and-braces: the stored graph may *already* contain a cycle (this
/// check did not exist until now), and a walk over one without a visited set does not return.
#[must_use]
pub fn would_cycle(menus: &[Menu], menu_id: MenuId, proposed_parent: MenuId) -> bool {
    let parent_of: BTreeMap<MenuId, Option<MenuId>> = menus
        .iter()
        .map(|menu| (menu.menu_id, menu.parent_menu_id))
        .collect();
    let mut seen: BTreeSet<MenuId> = BTreeSet::new();
    let mut current = Some(proposed_parent);
    while let Some(id) = current {
        if id == menu_id {
            return true;
        }
        if !seen.insert(id) {
            // A pre-existing loop that does not pass through `menu_id`. Not this edit's fault, and
            // not this edit's refusal — the compiler still names it at publish.
            return false;
        }
        current = parent_of.get(&id).copied().flatten();
    }
    false
}

/// Compiles one menu (with its inheritance chain) into a per-channel [`MenuBook`].
///
/// `items`, `menus`, `placements` and `groups` are a tenant's whole authoring model as loaded from a
/// [`CatalogStore`](crate::catalog::CatalogStore); `root` is the menu to compile. Archived items are
/// omitted (retired, not sold); an unavailable placement compiles to a **present but 86'd** entry
/// (`available = false`), because the store distinguishes "not on the menu" from "on the menu, paused"
/// — the latter is the operator's published floor, which live stock can only lower further.
///
/// An archived **menu** is refused rather than omitted, and the asymmetry with archived items is
/// deliberate: an item missing from a book is a menu that is still coherent, whereas a menu that is
/// not there at all has no coherent compilation, and silently emitting an empty book would take a
/// store's whole till layout away without a word.
///
/// # Modifier groups
///
/// [ADR-0127](../../../docs/adr/0127-modifier-groups-reach-the-edge.md) makes this the place
/// attachment is **inverted**: the operator pins one "Size" group to forty pizzas, and the till asks
/// the opposite question once per tap — *what must I ask about this item?* — so each entry is emitted
/// carrying the ids of the groups attached to it, and the groups themselves are carried once on the
/// catalog rather than forty times inside the entries.
///
/// Three things are dropped on the way down, each per channel, because a group is only a rule about
/// things this channel actually sells:
///
///   * an **archived** group is not published at all (ADR-0127 decision 2, the same posture an
///     archived item gets);
///   * a **member this channel does not price** is not offered on it — a modifier is an ordinary item
///     (ADR-0066 entity 4), so an id with no entry beside it is a button the till cannot price, and
///     delivery legitimately prices fewer sizes than dine-in;
///   * a group left with **no member, or fewer than `min_select` of them**, is not published on that
///     channel and its id is stripped from the entries there. Publishing a rule that cannot be
///     satisfied would not make the store ask a better question — it would take the item off sale on
///     that channel entirely, which is a worse answer than the one the till gives today. The check is
///     forgiving for [`compile_layout_book`]'s reason and not [`CompileError::UnknownItem`]'s: an
///     unpriced member is a pricing gap in one channel, not a reference to something that is not
///     there.
///
/// The group's `display_name_translations` are empty because the authoring model has no per-locale
/// name for a group yet; the field is carried so adding them later changes no shape.
///
/// # Errors
///
/// [`CompileError`] when a menu or a parent is missing, any menu in the chain is archived, the
/// inheritance chain cycles, or a placement prices an item the catalog does not carry.
pub fn compile_menu(
    items: &[CatalogItem],
    menus: &[Menu],
    placements: &[MenuPlacement],
    groups: &[ModifierGroup],
    root: MenuId,
) -> Result<MenuBook, CompileError> {
    let item_by_id: BTreeMap<MenuItemId, &CatalogItem> =
        items.iter().map(|item| (item.menu_item_id, item)).collect();
    let menu_by_id: BTreeMap<MenuId, &Menu> =
        menus.iter().map(|menu| (menu.menu_id, menu)).collect();

    // The inversion, built once: item -> the groups pinned to it. A `BTreeSet` so an entry's ids come
    // out in id order however the operator listed the attachments, which is what keeps a re-compile
    // of unchanged authoring byte-identical.
    let mut groups_by_item: BTreeMap<MenuItemId, BTreeSet<ModifierGroupId>> = BTreeMap::new();
    let mut group_by_id: BTreeMap<ModifierGroupId, &ModifierGroup> = BTreeMap::new();
    for group in groups
        .iter()
        .filter(|group| group.status != EntityStatus::Archived)
    {
        group_by_id.insert(group.modifier_group_id, group);
        for attached in &group.attached_item_ids {
            groups_by_item
                .entry(*attached)
                .or_default()
                .insert(group.modifier_group_id);
        }
    }

    // The chain from the requested menu up through its parents, most-specific first.
    let mut chain: Vec<MenuId> = Vec::new();
    let mut seen: BTreeSet<MenuId> = BTreeSet::new();
    let mut current = Some(root);
    while let Some(id) = current {
        if !seen.insert(id) {
            return Err(CompileError::InheritanceCycle(id));
        }
        let menu = menu_by_id.get(&id).ok_or(CompileError::UnknownMenu(id))?;
        // Every menu in the chain, not just the root. A store special inheriting from an archived
        // brand menu is the case that matters: the special looks fine, and the prices it does not
        // restate come from a menu nobody meant to be live.
        if menu.status == EntityStatus::Archived {
            return Err(CompileError::ArchivedMenu(id));
        }
        chain.push(id);
        current = menu.parent_menu_id;
    }

    // Group placements by menu_id first to avoid O(chain_depth * placements) scanning.
    let mut placements_by_menu: BTreeMap<MenuId, Vec<&MenuPlacement>> = BTreeMap::new();
    for placement in placements {
        placements_by_menu
            .entry(placement.menu_id)
            .or_default()
            .push(placement);
    }

    // Resolve each item to its most-specific placement: the requested menu wins over an ancestor.
    let mut resolved: BTreeMap<MenuItemId, &MenuPlacement> = BTreeMap::new();
    for menu_id in &chain {
        if let Some(menu_placements) = placements_by_menu.get(menu_id) {
            for placement in menu_placements {
                resolved.entry(placement.menu_item_id).or_insert(placement);
            }
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
                display_name_translations: item
                    .name_translations
                    .iter()
                    .map(|(locale, translated)| {
                        (locale.clone(), DisplayName::new(translated.as_str()))
                    })
                    .collect(),
                unit_price: price.unit_price,
                tax_class_id: item.tax_class_id,
                available: placement.available,
                modifier_group_ids: groups_by_item
                    .get(item_id)
                    .map(|attached| attached.iter().copied().collect())
                    .unwrap_or_default(),
            };
            by_channel
                .entry(channel.as_wire())
                .or_insert_with(|| (channel, Vec::new()))
                .1
                .push(entry);
        }
    }

    let mut book = MenuBook::new();
    for (_wire, (channel, mut entries)) in by_channel {
        let published = publishable_groups(&mut entries, &group_by_id);
        let mut catalog = MenuCatalog::from_items(entries);
        for group in published {
            catalog = catalog.with_modifier_group(group);
        }
        book = book.with(channel, catalog);
    }
    Ok(book)
}

/// One channel's publishable modifier groups, with the ids of the ones it cannot offer stripped from
/// `entries` as it goes.
///
/// Separate from [`compile_menu`] because it runs *after* the channel's last entry has been emitted,
/// and for the reason it has to: which groups survive depends on which members this channel prices,
/// which is not known while the entries are still being built. See that function's **Modifier
/// groups** section for what is dropped and why.
fn publishable_groups(
    entries: &mut [MenuEntry],
    group_by_id: &BTreeMap<ModifierGroupId, &ModifierGroup>,
) -> Vec<MenuModifierGroup> {
    let priced: BTreeSet<MenuItemId> = entries.iter().map(|entry| entry.menu_item_id).collect();
    let attached: BTreeSet<ModifierGroupId> = entries
        .iter()
        .flat_map(|entry| entry.modifier_group_ids.iter().copied())
        .collect();

    let published: Vec<MenuModifierGroup> = attached
        .iter()
        .filter_map(|group_id| group_by_id.get(group_id).copied())
        .filter_map(|group| {
            let member_menu_item_ids: Vec<MenuItemId> = group
                .member_item_ids
                .iter()
                .copied()
                .filter(|member| priced.contains(member))
                .collect();
            // `u16::MAX` rather than a refusal: a group with more than 65 535 members is not a
            // selection rule anyone authored, and saturating here only ever makes the rule look
            // *more* satisfiable, which is the direction the comparison below already allows.
            let offerable = u16::try_from(member_menu_item_ids.len()).unwrap_or(u16::MAX);
            if member_menu_item_ids.is_empty() || offerable < group.min_select {
                return None;
            }
            Some(MenuModifierGroup {
                modifier_group_id: group.modifier_group_id,
                display_name: DisplayName::new(group.name.as_str()),
                display_name_translations: BTreeMap::new(),
                min_select: group.min_select,
                max_select: group.max_select,
                member_menu_item_ids,
            })
        })
        .collect();

    // The id goes wherever the rule went, so an entry never names a group this channel's catalog does
    // not carry. `MenuCatalog::groups_for` skips a dangling id rather than failing, and that is the
    // safety net for a partial sync, not licence to publish a book that disagrees with itself.
    let carried: BTreeSet<ModifierGroupId> = published
        .iter()
        .map(|group| group.modifier_group_id)
        .collect();
    for entry in entries {
        entry.modifier_group_ids.retain(|id| carried.contains(id));
    }
    published
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
    use std::collections::BTreeMap;

    use pos_proto::display::GridPosition;
    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{
        DisplayCategoryId, DisplaySubcategoryId, MenuItemId, ModifierGroupId, TaxClassId, TenantId,
    };
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{CompileError, compile_layout_book, compile_menu, would_cycle};
    use crate::catalog::{
        CatalogItem, ChannelPrice, DisplayCategory, DisplaySubcategory, LayoutButton, Menu, MenuId,
        MenuPlacement, ModifierGroup,
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
            name_translations: BTreeMap::new(),
            tax_class_id: TaxClassId::new(Ulid::from_u128(7)),
            item_category_id: None,
            item_subcategory_id: None,
            course_id: None,
            image_ref: None,
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

        let book = compile_menu(&items, &menus, &placements, &[], menu_id(10)).expect("compile");

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
    fn per_locale_item_names_travel_onto_the_compiled_entry() {
        // The item's per-locale names compile onto the `MenuEntry`, with the default `name` still the
        // fallback (ADR-0074). Selection happens later, at the edge, from the store's language.
        let mut translated = item(500, "Margherita");
        translated
            .name_translations
            .insert("vi".to_owned(), "Bánh Margherita".to_owned());
        let items = [translated];
        let menus = [menu(10, None)];
        let placements = [placement(
            10,
            500,
            vec![price(SalesChannel::DineIn, 150_000)],
            true,
        )];

        let book = compile_menu(&items, &menus, &placements, &[], menu_id(10)).expect("compile");
        let entry = book
            .catalog_for(SalesChannel::DineIn)
            .get(item_id(500))
            .expect("priced dine-in")
            .clone();
        assert_eq!(
            entry.display_name.as_str(),
            "Margherita",
            "default is the fallback"
        );
        assert_eq!(entry.localized_name("vi").as_str(), "Bánh Margherita");
        assert_eq!(
            entry.localized_name("en").as_str(),
            "Margherita",
            "an untranslated locale falls back to the default name"
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

        let book = compile_menu(&items, &menus, &placements, &[], menu_id(11)).expect("compile");
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

        let book = compile_menu(&items, &menus, &placements, &[], menu_id(10)).expect("compile");
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

        let book = compile_menu(&items, &menus, &placements, &[], menu_id(10)).expect("compile");
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
            compile_menu(&items, &menus, &placements, &[], menu_id(10)),
            Err(CompileError::UnknownItem(item_id(999)))
        );
    }

    #[test]
    fn an_inheritance_cycle_is_rejected() {
        let items: [CatalogItem; 0] = [];
        let menus = [menu(10, Some(11)), menu(11, Some(10))];
        assert_eq!(
            compile_menu(&items, &menus, &[], &[], menu_id(10)),
            Err(CompileError::InheritanceCycle(menu_id(10)))
        );
    }

    #[test]
    fn an_unknown_menu_is_rejected() {
        let items: [CatalogItem; 0] = [];
        assert_eq!(
            compile_menu(&items, &[], &[], &[], menu_id(42)),
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
        let first = compile_menu(&items, &menus, &placements, &[], menu_id(10)).expect("compile");
        let second = compile_menu(&items, &menus, &placements, &[], menu_id(10)).expect("compile");
        assert_eq!(
            serde_json::to_string(&first).expect("serialise"),
            serde_json::to_string(&second).expect("serialise"),
            "the same authoring compiles to a byte-identical snapshot"
        );
    }

    fn modifier_group_id(n: u128) -> ModifierGroupId {
        ModifierGroupId::new(Ulid::from_u128(n))
    }

    fn group(
        id: u128,
        name: &str,
        min_select: u16,
        max_select: u16,
        members: &[u128],
        attached: &[u128],
    ) -> ModifierGroup {
        ModifierGroup {
            modifier_group_id: modifier_group_id(id),
            tenant_id: tenant(),
            name: name.to_owned(),
            min_select,
            max_select,
            member_item_ids: members.iter().copied().map(item_id).collect(),
            attached_item_ids: attached.iter().copied().map(item_id).collect(),
            status: EntityStatus::Active,
        }
    }

    /// Two pizzas, both sizes, all four priced dine-in. Enough authoring for every test below to vary
    /// one thing and leave the rest alone.
    fn pizzas_and_sizes() -> ([CatalogItem; 4], [Menu; 1], Vec<MenuPlacement>) {
        let items = [
            item(500, "Margherita"),
            item(501, "Marinara"),
            item(600, "25cm"),
            item(601, "30cm"),
        ];
        let menus = [menu(10, None)];
        let placements = vec![
            placement(10, 500, vec![price(SalesChannel::DineIn, 150_000)], true),
            placement(10, 501, vec![price(SalesChannel::DineIn, 140_000)], true),
            placement(10, 600, vec![price(SalesChannel::DineIn, 0)], true),
            placement(10, 601, vec![price(SalesChannel::DineIn, 40_000)], true),
        ];
        (items, menus, placements)
    }

    #[test]
    fn a_group_is_inverted_onto_every_item_it_is_attached_to() {
        // ADR-0127 decision 3: the operator pins one group to two pizzas; the till asks the opposite
        // question, so each entry names the group and the catalog carries the rule exactly once.
        let (items, menus, placements) = pizzas_and_sizes();
        let groups = [group(900, "Size", 1, 1, &[600, 601], &[500, 501])];

        let book =
            compile_menu(&items, &menus, &placements, &groups, menu_id(10)).expect("compile");
        let dine_in = book.catalog_for(SalesChannel::DineIn);

        assert_eq!(
            dine_in.modifier_groups().len(),
            1,
            "one rule, not one copy per pizza"
        );
        let published = dine_in.modifier_groups().first().expect("the group");
        assert_eq!(published.modifier_group_id, modifier_group_id(900));
        assert_eq!(published.display_name.as_str(), "Size");
        assert!(published.required(), "min_select 1 is what required means");
        assert_eq!(published.max_select, 1);
        assert_eq!(
            published.member_menu_item_ids,
            vec![item_id(600), item_id(601)]
        );
        assert!(
            published.display_name_translations.is_empty(),
            "authoring has no per-locale group name yet"
        );

        for pizza in [500, 501] {
            assert_eq!(
                dine_in
                    .groups_for(item_id(pizza))
                    .first()
                    .expect("the pizza asks")
                    .modifier_group_id,
                modifier_group_id(900)
            );
        }
        assert!(
            dine_in.groups_for(item_id(600)).is_empty(),
            "a size is an ordinary item and attaches nothing"
        );
    }

    #[test]
    fn an_entry_names_its_groups_in_id_order_however_they_were_authored() {
        // The order an operator attached them in is not a fact the book should carry: a re-compile of
        // unchanged authoring has to stay byte-identical, so the inversion sorts.
        let (items, menus, placements) = pizzas_and_sizes();
        let groups = [
            group(902, "Extras", 0, 1, &[601], &[500]),
            group(901, "Size", 1, 1, &[600, 601], &[500]),
        ];

        let book =
            compile_menu(&items, &menus, &placements, &groups, menu_id(10)).expect("compile");
        assert_eq!(
            book.catalog_for(SalesChannel::DineIn)
                .get(item_id(500))
                .expect("the pizza")
                .modifier_group_ids,
            vec![modifier_group_id(901), modifier_group_id(902)]
        );
    }

    #[test]
    fn an_archived_group_is_not_published() {
        // ADR-0127 decision 2: the compiled view has no `status` to reason about, so a retired group
        // simply does not cross — the same posture an archived item gets.
        let (items, menus, placements) = pizzas_and_sizes();
        let mut retired = group(900, "Size", 1, 1, &[600, 601], &[500]);
        retired.status = EntityStatus::Archived;

        let book =
            compile_menu(&items, &menus, &placements, &[retired], menu_id(10)).expect("compile");
        let dine_in = book.catalog_for(SalesChannel::DineIn);

        assert!(dine_in.modifier_groups().is_empty());
        assert!(
            dine_in
                .get(item_id(500))
                .expect("the pizza still sells")
                .modifier_group_ids
                .is_empty(),
            "and no entry is left naming it"
        );
    }

    #[test]
    fn a_member_a_channel_does_not_price_is_not_offered_on_it() {
        // A modifier is an ordinary item, so it is on a channel only if it is priced there. Delivery
        // sells one size; publishing the other would give the till a button it cannot price.
        let items = [
            item(500, "Margherita"),
            item(600, "25cm"),
            item(601, "30cm"),
        ];
        let menus = [menu(10, None)];
        let placements = [
            placement(
                10,
                500,
                vec![
                    price(SalesChannel::DineIn, 150_000),
                    price(SalesChannel::Delivery, 180_000),
                ],
                true,
            ),
            placement(
                10,
                600,
                vec![
                    price(SalesChannel::DineIn, 0),
                    price(SalesChannel::Delivery, 0),
                ],
                true,
            ),
            placement(10, 601, vec![price(SalesChannel::DineIn, 40_000)], true),
        ];
        let groups = [group(900, "Size", 1, 1, &[600, 601], &[500])];

        let book =
            compile_menu(&items, &menus, &placements, &groups, menu_id(10)).expect("compile");

        assert_eq!(
            book.catalog_for(SalesChannel::DineIn)
                .modifier_groups()
                .first()
                .expect("dine-in asks")
                .member_menu_item_ids,
            vec![item_id(600), item_id(601)]
        );
        assert_eq!(
            book.catalog_for(SalesChannel::Delivery)
                .modifier_groups()
                .first()
                .expect("delivery still asks")
                .member_menu_item_ids,
            vec![item_id(600)],
            "only the size delivery prices"
        );
    }

    #[test]
    fn a_group_a_channel_cannot_satisfy_is_dropped_there() {
        // Delivery prices the pizza and neither size. Publishing a `min_select = 1` rule with nothing
        // to select would not make the store ask a better question — it would take the pizza off
        // delivery altogether, which is worse than the answer the till gives today.
        let items = [item(500, "Margherita"), item(600, "25cm")];
        let menus = [menu(10, None)];
        let placements = [
            placement(
                10,
                500,
                vec![
                    price(SalesChannel::DineIn, 150_000),
                    price(SalesChannel::Delivery, 180_000),
                ],
                true,
            ),
            placement(10, 600, vec![price(SalesChannel::DineIn, 0)], true),
        ];
        let groups = [group(900, "Size", 1, 1, &[600], &[500])];

        let book =
            compile_menu(&items, &menus, &placements, &groups, menu_id(10)).expect("compile");

        assert_eq!(
            book.catalog_for(SalesChannel::DineIn)
                .modifier_groups()
                .len(),
            1,
            "dine-in prices a size, so dine-in asks"
        );
        let delivery = book.catalog_for(SalesChannel::Delivery);
        assert!(delivery.modifier_groups().is_empty());
        assert!(
            delivery
                .get(item_id(500))
                .expect("the pizza still sells on delivery")
                .modifier_group_ids
                .is_empty(),
            "the id goes with the rule, so the book stays self-consistent"
        );
    }

    #[test]
    fn a_group_attached_to_nothing_this_menu_sells_is_not_published() {
        // A group is a rule about this menu's items. One pinned only to an item this menu does not
        // place is not part of it, and carrying it down would be noise the till scans past.
        let (items, menus, placements) = pizzas_and_sizes();
        let groups = [group(900, "Size", 1, 1, &[600, 601], &[999])];

        let book =
            compile_menu(&items, &menus, &placements, &groups, menu_id(10)).expect("compile");
        assert!(
            book.catalog_for(SalesChannel::DineIn)
                .modifier_groups()
                .is_empty()
        );
    }

    #[test]
    fn an_optional_group_with_nothing_left_to_offer_is_dropped_too() {
        // `min_select = 0` makes the rule satisfiable by choosing nothing, so the survivor count does
        // not trip the comparison — but a picker with no buttons is still a picker nobody can use.
        let (items, menus, placements) = pizzas_and_sizes();
        let groups = [group(900, "Extras", 0, 2, &[999], &[500])];

        let book =
            compile_menu(&items, &menus, &placements, &groups, menu_id(10)).expect("compile");
        assert!(
            book.catalog_for(SalesChannel::DineIn)
                .modifier_groups()
                .is_empty()
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

    // --- an archived menu is refused, and a cycle is caught before it is stored ----------------

    #[test]
    fn an_archived_menu_is_refused_rather_than_compiled_empty() {
        // The asymmetry this closes: archived *items* were already skipped, and an archived menu
        // published anyway. An operator who retires a menu and then publishes it was, until now,
        // shipping it.
        let items = [item(1, "Margherita")];
        let mut retired = menu(100, None);
        retired.status = EntityStatus::Archived;
        let placements = [placement(
            100,
            1,
            vec![price(SalesChannel::DineIn, 100_000)],
            true,
        )];
        assert_eq!(
            compile_menu(&items, &[retired], &placements, &[], menu_id(100)),
            Err(CompileError::ArchivedMenu(menu_id(100)))
        );
    }

    #[test]
    fn an_archived_parent_is_refused_rather_than_silently_dropping_its_prices() {
        // The worse half. The child looks live and compiles; the prices it does not restate come
        // from the parent, so skipping an archived ancestor would take items off a till with
        // nothing said anywhere.
        let items = [item(1, "Margherita"), item(2, "Marinara")];
        let mut parent = menu(100, None);
        parent.status = EntityStatus::Archived;
        let child = menu(101, Some(100));
        let placements = [
            placement(100, 1, vec![price(SalesChannel::DineIn, 100_000)], true),
            placement(101, 2, vec![price(SalesChannel::DineIn, 90_000)], true),
        ];
        assert_eq!(
            compile_menu(&items, &[parent, child], &placements, &[], menu_id(101)),
            Err(CompileError::ArchivedMenu(menu_id(100)))
        );
    }

    #[test]
    fn an_active_chain_still_compiles() {
        // The guard above must not refuse the ordinary case, which is the whole point of the
        // inheritance model: a store special over a brand standard.
        let items = [item(1, "Margherita"), item(2, "Marinara")];
        let parent = menu(100, None);
        let child = menu(101, Some(100));
        let placements = [
            placement(100, 1, vec![price(SalesChannel::DineIn, 100_000)], true),
            placement(101, 2, vec![price(SalesChannel::DineIn, 90_000)], true),
        ];
        let book = compile_menu(&items, &[parent, child], &placements, &[], menu_id(101))
            .expect("an all-active chain compiles");
        for id in [item_id(1), item_id(2)] {
            assert!(
                book.catalog_for(SalesChannel::DineIn).get(id).is_some(),
                "both the parent's and the child's placements reach the book"
            );
        }
    }

    #[test]
    fn re_parenting_onto_a_descendant_is_a_cycle() {
        // 100 <- 101 <- 102. Making 100 inherit from 102 closes the loop.
        let menus = [menu(100, None), menu(101, Some(100)), menu(102, Some(101))];
        assert!(would_cycle(&menus, menu_id(100), menu_id(102)));
        assert!(would_cycle(&menus, menu_id(101), menu_id(102)));
    }

    #[test]
    fn a_menu_proposed_as_its_own_parent_is_a_cycle() {
        let menus = [menu(100, None)];
        assert!(would_cycle(&menus, menu_id(100), menu_id(100)));
    }

    #[test]
    fn re_parenting_onto_an_unrelated_menu_is_not_a_cycle() {
        let menus = [menu(100, None), menu(101, Some(100)), menu(200, None)];
        assert!(!would_cycle(&menus, menu_id(101), menu_id(200)));
    }

    #[test]
    fn a_pre_existing_loop_elsewhere_does_not_hang_the_walk() {
        // 200 and 201 already point at each other — data this check did not exist to prevent. The
        // walk must terminate, and must not blame the edit being made.
        let mut a = menu(200, Some(201));
        let b = menu(201, Some(200));
        a.parent_menu_id = Some(menu_id(201));
        let menus = [menu(100, None), a, b];
        assert!(!would_cycle(&menus, menu_id(100), menu_id(200)));
    }
}
