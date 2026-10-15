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
use pos_proto::ids::{
    CourseId, DisplayCategoryId, DisplaySubcategoryId, MenuItemId, ModifierGroupId,
};
use pos_proto::money::Money;
use pos_proto::text::DisplayName;
use pos_proto::wire_enum::WireEnum;
use pos_proto::{
    DisplayButton, DisplayCategory as ProtoDisplayCategory, DisplayPlan,
    DisplaySubcategory as ProtoDisplaySubcategory, LayoutBook, MenuBook, MenuCatalog, MenuCourse,
    MenuEntry, MenuModifierGroup,
};

use crate::catalog::{
    CatalogItem, Course, DisplayCategory, DisplaySubcategory, LayoutButton, Menu, MenuId,
    MenuPlacement, ModifierGroup,
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
/// # Courses
///
/// [ADR-0130](../../../docs/adr/0130-a-course-is-something-the-catalog-names.md) carries the same
/// shape for a second entity: the item declares its course, the courses themselves ride once on the
/// catalog, and the till groups an order into starters, mains and desserts without a second read.
/// **In service order**, ties broken by id — the order is the entity's whole meaning, and a caller
/// that had to sort it again would eventually not.
///
/// Two things are dropped, mirroring the groups above and deliberately not inventing a third rule:
///
///   * an **archived** course is not published, and an entry naming one is emitted with no course
///     rather than refused;
///   * a course **no entry on this channel names** is not published either, so a channel's book never
///     lists a course nothing on it goes out on.
///
/// The `course_id` goes wherever the course went, exactly as a group's id does, so an entry never
/// names a course its own catalog does not carry. [`MenuCatalog::course_for`] answering `None` for a
/// dangling id is the safety net for a partial sync, not licence to publish a book that disagrees
/// with itself.
///
/// The course's `display_name_translations` are empty for the group's reason: the authoring model has
/// no per-locale name for a course yet, and the field is carried so adding one changes no shape.
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
    courses: &[Course],
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

    // Active courses only. An archived one is not published and an entry that still names it is
    // emitted with no course — the posture an archived group already gets, rather than a third rule.
    let course_by_id: BTreeMap<CourseId, &Course> = courses
        .iter()
        .filter(|course| course.status != EntityStatus::Archived)
        .map(|course| (course.course_id, course))
        .collect();

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
            let entry = compiled_entry(
                item,
                price.unit_price,
                placement.available,
                groups_by_item.get(item_id),
                &course_by_id,
            );
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
        let served = publishable_courses(&entries, &course_by_id);
        let mut catalog = MenuCatalog::from_items(entries);
        for group in published {
            catalog = catalog.with_modifier_group(group);
        }
        for course in served {
            catalog = catalog.with_course(course);
        }
        book = book.with(channel, catalog);
    }
    Ok(book)
}

/// One channel's publishable courses, in service order, ties broken by id.
///
/// Only the courses this channel's entries actually name: a book that listed "Dessert" on a delivery
/// channel that prices no dessert would put an empty heading on the till, and the till would have to
/// learn to hide it. Takes `entries` by shared reference where [`publishable_groups`] takes them
/// mutably, because an entry's `course_id` was already resolved against the active courses as it was
/// built — a course cannot become unpublishable later the way a group can when its last priced member
/// turns out to be missing.
fn publishable_courses(
    entries: &[MenuEntry],
    course_by_id: &BTreeMap<CourseId, &Course>,
) -> Vec<MenuCourse> {
    let named: BTreeSet<CourseId> = entries.iter().filter_map(|entry| entry.course_id).collect();
    let mut published: Vec<MenuCourse> = named
        .iter()
        .filter_map(|course_id| course_by_id.get(course_id).copied())
        .map(|course| {
            MenuCourse::new(
                course.course_id,
                DisplayName::new(course.name.as_str()),
                course.sort,
            )
        })
        .collect();
    // The sequence is the point. `named` is already in id order, so sorting by `sort` alone would
    // leave ties in id order anyway — the key is spelled out because a reader should not have to
    // know that to trust a re-compile is byte-identical.
    published.sort_by_key(|course| (course.sort, course.course_id));
    published
}

/// One authoring item, priced for one channel, as the compiled book carries it.
///
/// Extracted from [`compile_menu`]'s emit loop because it is the one place the authoring model and
/// the wire shape meet: every field a store reads about an item is decided here, and the loop around
/// it is only about *which* (item, channel) pairs exist.
///
/// The ids it carries are resolved against what this tenant actually has — a group pinned to the item
/// and a course the tenant still carries — so a channel's later pass has only to drop what the
/// channel cannot serve, never to discover a reference that was never valid.
fn compiled_entry(
    item: &CatalogItem,
    unit_price: Money,
    available: bool,
    attached: Option<&BTreeSet<ModifierGroupId>>,
    course_by_id: &BTreeMap<CourseId, &Course>,
) -> MenuEntry {
    MenuEntry {
        menu_item_id: item.menu_item_id,
        display_name: DisplayName::new(item.name.as_str()),
        display_name_translations: item
            .name_translations
            .iter()
            .map(|(locale, translated)| (locale.clone(), DisplayName::new(translated.as_str())))
            .collect(),
        unit_price,
        tax_class_id: item.tax_class_id,
        available,
        modifier_group_ids: attached
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default(),
        // An archived course and a course the tenant does not carry at all are the same answer here,
        // so the two cannot drift into different behaviour later.
        course_id: item
            .course_id
            .filter(|course_id| course_by_id.contains_key(course_id)),
    }
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
mod tests;
