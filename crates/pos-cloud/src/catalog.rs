// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud catalog authoring seam ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
//!
//! The rich, normalized *source of truth* an operator edits — items, menus that inherit, and the
//! per-channel prices an item carries in a menu. It is deliberately distinct from two things it feeds:
//! the [`config_tree`](crate::config_tree), which carries only the *compiled* output a store pulls,
//! and the [`registry`](crate::registry), which is identity and naming. The compiler
//! (a later slice) resolves this model per `(store × channel)` into the flat
//! [`pos_proto::MenuBook`] the edge reprices from; nothing here crosses the wire to a store.
//!
//! This slice lands the item master, menus (with inheritance), and menu placements — the inputs a
//! `MenuBook` is compiled from. Modifier groups, the display taxonomy and layouts are later slices of
//! the same seam. Prices are a **T2** asset (a pricing model): authored and compiled in the cloud,
//! never written to a log or an event.
//!
//! The seam is a trait so it runs against an in-memory fake in tests and a `store-postgres` table in
//! the cloud, exactly as [`RegistryStore`](crate::registry::RegistryStore) does — the same
//! create/list/update shape, tenant-scoped and RLS-isolated, entities archived rather than
//! hard-deleted so a compiled menu that referenced one still resolves.

use core::fmt;
use core::future::Future;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId, TaxClassId, TenantId};
use pos_proto::money::Money;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::media::MediaId;
use crate::paging::{Page, PageRequest};
use crate::registry::EntityStatus;
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

/// A menu's identifier — a ULID minted at creation. A menu is an authoring concept that never
/// crosses the wire (the compiled [`pos_proto::MenuBook`] has no menu id), so its id is defined here
/// beside the seam rather than in [`pos_proto::ids`], the same way a `BrandId` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct MenuId(Ulid);

impl MenuId {
    /// Wraps a ULID as a menu id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for MenuId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An item category's identifier — a ULID minted at creation. The operational taxonomy is a
/// cloud-authoring concept (reporting/tax-default/kitchen grouping); like [`MenuId`], its id is
/// defined beside the seam rather than in [`pos_proto::ids`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ItemCategoryId(Ulid);

impl ItemCategoryId {
    /// Wraps a ULID as an item-category id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for ItemCategoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An item sub-category's identifier — a ULID minted at creation, nested under an [`ItemCategoryId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ItemSubcategoryId(Ulid);

impl ItemSubcategoryId {
    /// Wraps a ULID as an item-sub-category id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for ItemSubcategoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An item category — the operational taxonomy for reporting, tax defaults and kitchen grouping
/// (ADR-0066 entity 2). This is **not** the presentation taxonomy a screen groups by (that is a
/// [display category](DisplayCategory-like) delivered in the layout, entity 11); a product-mix report
/// groups by *this*.
#[derive(Debug, Clone, Serialize)]
pub struct ItemCategory {
    /// The category id.
    pub item_category_id: ItemCategoryId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name (`Pizza`, `Beverage`, `Dessert`).
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// An item sub-category — a refinement of an [`ItemCategory`] (ADR-0066 entity 3).
#[derive(Debug, Clone, Serialize)]
pub struct ItemSubcategory {
    /// The sub-category id.
    pub item_subcategory_id: ItemSubcategoryId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The parent category this refines.
    pub item_category_id: ItemCategoryId,
    /// The human name (`Thin crust`, `Soft drink`).
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A tax class — a named bucket an item belongs to, whose rate the store's channel-keyed
/// [`pos_proto::locale::TaxRateTable`] resolves at reprice time (ADR-0066 entity 10; D6).
///
/// The class itself is country-agnostic (`alcohol`, `standard`, `takeaway-reduced`); the *rate* for
/// each `(tax_class, channel)` lives in the store's locale pack, not here. This entity exists so the
/// operator picks a tax class **by name** when authoring an item, instead of pasting a
/// [`TaxClassId`] ULID — the same "kill the ULID" move [ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)
/// made for tenants and stores. Its id is the [`TaxClassId`] an item's `tax_class_id` references.
#[derive(Debug, Clone, Serialize)]
pub struct TaxClass {
    /// The tax-class id — the [`TaxClassId`] an item references and the rate table is keyed by.
    pub tax_class_id: TaxClassId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name (`Standard 10%`, `Alcohol`, `Takeaway reduced`).
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// An item in the catalog — the product master.
///
/// Its id is a [`MenuItemId`], the same identifier the compiled [`pos_proto::MenuEntry`] names and an
/// inbound order references: the item authored here is the item priced downstream. This slice carries
/// the fields a `MenuEntry` needs (name, tax class); its operational category, recipe/BOM link and
/// the rest arrive with later slices.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogItem {
    /// The item's id — shared with the compiled menu entry and any inbound order that names it.
    pub menu_item_id: MenuItemId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name (the default caption, before a layout overrides it per channel). Always
    /// present, and the fallback for a locale [`name_translations`](Self::name_translations) omits.
    pub name: String,
    /// The item's name in each locale it is translated into, keyed by locale code (`"vi"`, `"en"`, …)
    /// ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), Track M4). The compiler carries
    /// these onto the [`pos_proto::MenuEntry`]; the store's display language selects one at the edge,
    /// falling back to [`name`](Self::name). Additive — an item with none behaves as before.
    pub name_translations: BTreeMap<String, String>,
    /// The tax class, which the store's channel-keyed rate table turns into a rate at reprice time.
    pub tax_class_id: TaxClassId,
    /// The operational category this item reports under, or `None` if unclassified (entity 2).
    pub item_category_id: Option<ItemCategoryId>,
    /// The operational sub-category, refining the category, or `None` (entity 3).
    pub item_subcategory_id: Option<ItemSubcategoryId>,
    /// The item's photo — a [`MediaId`] into the media library (ADR-0075), or `None`. Authoring/display
    /// only; it does not cross to the edge. A dangling ref (the asset was deleted) shows a placeholder.
    pub image_ref: Option<MediaId>,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A menu — a named set of placements that may **inherit** from a parent menu.
///
/// Inheritance is how a brand menu overrides a tenant standard and a store special overrides the
/// brand: the compiler folds a parent's placements under a child's, most-specific-wins. A cycle in
/// `parent_menu_id` is a configuration error the compiler rejects; the seam only stores the edge.
#[derive(Debug, Clone, Serialize)]
pub struct Menu {
    /// The menu id.
    pub menu_id: MenuId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name.
    pub name: String,
    /// The menu this one inherits from, or `None` for a root menu.
    pub parent_menu_id: Option<MenuId>,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A menu section's identifier — a ULID minted at creation. A section is an authoring grouping within
/// a menu (ADR-0066 entity 7); like [`MenuId`] it never crosses the wire (the compiled `MenuBook` is a
/// flat list of entries with no sections), so its id lives beside the seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct MenuSectionId(Ulid);

impl MenuSectionId {
    /// Wraps a ULID as a menu-section id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for MenuSectionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A menu section — an authoring grouping within a menu (ADR-0066 entity 7), e.g. `Starters`, `Mains`,
/// `Desserts` on a single menu.
///
/// A section is where the operator organises a menu's placements for authoring and printing; a
/// [`MenuPlacement`] carries an optional `menu_section_id` naming the section it sits under. Sections
/// are **authoring-only**: the compiled `MenuBook` is a flat set of entries, so a section changes what
/// the operator sees while authoring, never what the edge is served — the same posture a
/// [`ModifierGroup`] holds until a `pos-proto` extension carries it.
#[derive(Debug, Clone, Serialize)]
pub struct MenuSection {
    /// The section id.
    pub menu_section_id: MenuSectionId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The menu this section belongs to.
    pub menu_id: MenuId,
    /// The human name (`Starters`, `Mains`, `Desserts`).
    pub name: String,
    /// Where the section sorts within its menu, ascending. Ties break by id.
    pub sort: i32,
    /// Active or archived.
    pub status: EntityStatus,
}

/// One channel's price for a [`MenuPlacement`].
///
/// This is where dine-in ≠ takeaway ≠ delivery pricing is authored; the compiler emits one channel's
/// prices into that channel's [`pos_proto::MenuCatalog`] within the store's `MenuBook`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelPrice {
    /// The channel this price applies on. `Open`, the same shape [`pos_proto::locale::TaxRateRow`]
    /// uses, so authoring stored under a channel a later build learns still round-trips.
    pub sales_channel: Open<SalesChannel>,
    /// The price per unit on that channel. Integer [`Money`].
    pub unit_price: Money,
}

/// An item placed in a menu, with its per-channel prices — the row a compiled `MenuEntry` is made
/// from. Its identity is the pair `(menu_id, menu_item_id)`: an item appears in a menu at most once.
#[derive(Debug, Clone, Serialize)]
pub struct MenuPlacement {
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The menu this placement belongs to.
    pub menu_id: MenuId,
    /// The item placed.
    pub menu_item_id: MenuItemId,
    /// The section this placement sits under, or `None` for an unsectioned placement. Authoring-only
    /// (ADR-0066 entity 7): it groups the row for the operator, and does not reach the compiled book.
    pub menu_section_id: Option<MenuSectionId>,
    /// The item's price per channel in this menu. A channel with no row here falls to the menu's
    /// parent (inheritance) or, failing that, is not sold on that channel.
    pub prices: Vec<ChannelPrice>,
    /// Whether the item is for sale in this menu right now — the operator's *published* floor, which
    /// the edge's live-stock auto-86 (`pos-core` §8) can only push further down, never re-raise.
    pub available: bool,
}

/// A modifier group's identifier — a ULID minted at creation. A modifier group is an authoring
/// concept (a compiled `MenuEntry` does not yet carry modifiers — that is a later `pos-proto`
/// extension), so its id lives beside the seam like [`MenuId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ModifierGroupId(Ulid);

impl ModifierGroupId {
    /// Wraps a ULID as a modifier-group id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for ModifierGroupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A modifier group — a set of modifier choices with a min/max selection rule, attached to items
/// (ADR-0066 entities 4 and 5).
///
/// A **modifier is itself an item** (a [`MenuItemId`] priced in the same money, ADR-0063), so this
/// entity needs no separate modifier type: `member_item_ids` are the items offered as choices, and
/// `attached_item_ids` are the items this group modifies (a pizza's "Size", "Extra toppings"). The
/// selection rule is `min_select..=max_select`. This is **authoring only** today — the compiled
/// [`pos_proto::MenuEntry`] carries no modifier reference yet; wiring modifiers to the edge is a
/// `pos-proto`/ADR-0063 extension and its own resolver slice.
#[derive(Debug, Clone, Serialize)]
pub struct ModifierGroup {
    /// The group id.
    pub modifier_group_id: ModifierGroupId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name (`Size`, `Extra toppings`).
    pub name: String,
    /// The minimum number of choices a guest must select (0 for optional).
    pub min_select: u16,
    /// The maximum number of choices a guest may select.
    pub max_select: u16,
    /// The items offered as choices in this group — each a modifier, i.e. an item.
    pub member_item_ids: Vec<MenuItemId>,
    /// The items this group is attached to (the products that show this modifier set).
    pub attached_item_ids: Vec<MenuItemId>,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A display category — the **presentation** taxonomy a screen groups by (ADR-0066 entity 11).
///
/// Deliberately distinct from an [`ItemCategory`] (the operational taxonomy): a screen may show a
/// "Summer specials" tab whose items report under "Pizza". Its id is a [`DisplayCategoryId`] — the
/// same id the compiled [`pos_proto::DisplayCategory`] carries, the way an item's id crosses into the
/// compiled `MenuEntry`.
#[derive(Debug, Clone, Serialize)]
pub struct DisplayCategory {
    /// The display-category id — carried into the compiled `DisplayPlan`.
    pub display_category_id: DisplayCategoryId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The name a screen shows as a tab or section.
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A display sub-category — a second grouping level under a [`DisplayCategory`] (ADR-0066 entity 11).
#[derive(Debug, Clone, Serialize)]
pub struct DisplaySubcategory {
    /// The display-sub-category id — carried into the compiled plan.
    pub display_subcategory_id: DisplaySubcategoryId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The parent display category.
    pub display_category_id: DisplayCategoryId,
    /// The name a screen shows.
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// One item's button in a per-channel layout — the authoring row a compiled [`pos_proto::DisplayButton`]
/// is made from (ADR-0066 entity 12).
///
/// A "layout" is the set of these rows for a channel: the compiler groups them by
/// `(sales_channel → display category → sub-category)`, orders each group by `sort`, and emits a
/// [`pos_proto::DisplayPlan`] per channel into a [`pos_proto::LayoutBook`]. Its identity is
/// `(tenant, sales_channel, menu_item_id)` — an item has at most one button per channel. Presentation
/// only: it names an item's `menu_item_id` and where its button sits, never a price.
#[derive(Debug, Clone, Serialize)]
pub struct LayoutButton {
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The channel this button lays out. `Open`, like [`ChannelPrice::sales_channel`].
    pub sales_channel: Open<SalesChannel>,
    /// The display category the button sits under.
    pub display_category_id: DisplayCategoryId,
    /// The sub-category within that category, or `None` for a button directly under the category.
    pub display_subcategory_id: Option<DisplaySubcategoryId>,
    /// The item this button orders — a `menu_item_id` the compiled `MenuBook` prices.
    pub menu_item_id: MenuItemId,
    /// The caption to show (may be shorter than the item's catalog name on a crowded grid).
    pub label: String,
    /// The button's grid slot on a POS terminal, or `None` for a flowing layout (tablet, QR).
    pub position: Option<GridPosition>,
    /// The display order within its group; lower comes first.
    pub sort: i32,
}

/// Persists and reads the catalog authoring model.
///
/// **Nine record-shaped entities** — item, tax class, item category and sub-category, display
/// category and sub-category, modifier group, menu, menu section — have the registry's shape:
/// `create` (a freshly-minted record, returning the [`Version`] it starts at), `list` (scoped to its
/// tenant, each row carrying the version it was read at), and `update` (applied only if the stored
/// version still equals the one the caller expected —
/// [ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
///
/// **Two carry a caller-supplied key** — a placement is identified by its `(menu_id, menu_item_id)`
/// pair and a layout button by its `(sales_channel, menu_item_id)` slot — and were single upserts
/// until [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md) split them the
/// same way: `create_*` inserts and refuses a key already taken, `update_*` replaces only at the
/// version the caller read. An upsert cannot tell those two intentions apart, so "add this item to
/// the menu" silently replaced the prices of one already on it. Their `list_*` carries a version per
/// row for the same reason the record-shaped reads do: it is the only place an editor can obtain the
/// token `update_*` demands.
///
/// All reads and writes are tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant, like
/// every other cloud table.
pub trait CatalogStore {
    /// Inserts an item.
    fn create_item(
        &self,
        item: &CatalogItem,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's items.
    ///
    /// Every item, unpaged, and permanently so
    /// ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)): five of the six console consumers
    /// of `GET /admin/items` fill a picker rather than a table, and the menu compiler needs the whole
    /// item master or it compiles a menu missing whatever fell off the page.
    fn list_items(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<CatalogItem>>, CatalogStoreError>> + Send;

    /// One page of a tenant's items, newest first, with how many the tenant has.
    ///
    /// Beside [`list_items`](Self::list_items), not replacing it: the item master runs to thousands
    /// for a chain, and the Items table is the one consumer that wants a window rather than the set.
    ///
    /// The order is `created_at DESC, menu_item_id DESC` — total, per ADR-0098 decision 9.
    /// `created_at` alone is not: it defaults to `now()`, which is *transaction* time, so items
    /// imported in one transaction share it exactly and a window over the tie could return a row on
    /// two pages or on neither.
    fn list_items_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<Versioned<CatalogItem>>, CatalogStoreError>> + Send;

    /// Renames an item, sets its tax class, and/or sets its status, within its tenant. Returns
    /// whether a row was found and changed.
    fn update_item(
        &self,
        item: &CatalogItem,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts a tax class.
    fn create_tax_class(
        &self,
        tax_class: &TaxClass,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's tax classes.
    fn list_tax_classes(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<TaxClass>>, CatalogStoreError>> + Send;

    /// Renames a tax class and/or sets its status, within its tenant. Applies only at `expected`.
    fn update_tax_class(
        &self,
        tax_class: &TaxClass,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts an item category.
    fn create_item_category(
        &self,
        category: &ItemCategory,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's item categories.
    fn list_item_categories(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<ItemCategory>>, CatalogStoreError>> + Send;

    /// Renames an item category and/or sets its status. Applies only at `expected`.
    fn update_item_category(
        &self,
        category: &ItemCategory,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts an item sub-category under a parent category.
    fn create_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's item sub-categories.
    fn list_item_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<ItemSubcategory>>, CatalogStoreError>> + Send;

    /// Renames an item sub-category, (re)parents it, and/or sets its status. Applies only at
    /// `expected`.
    fn update_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts a display category.
    fn create_display_category(
        &self,
        category: &DisplayCategory,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's display categories.
    fn list_display_categories(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<DisplayCategory>>, CatalogStoreError>> + Send;

    /// Renames a display category and/or sets its status. Applies only at `expected`.
    fn update_display_category(
        &self,
        category: &DisplayCategory,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts a display sub-category under a parent display category.
    fn create_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's display sub-categories.
    fn list_display_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<DisplaySubcategory>>, CatalogStoreError>> + Send;

    /// Renames a display sub-category, (re)parents it, and/or sets its status. Applies only at
    /// `expected`.
    fn update_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts a layout button, refusing if one already holds its
    /// `(tenant, sales_channel, menu_item_id)` identity.
    ///
    /// The identity is entirely caller-supplied, so the `set_layout_button` this replaced could —
    /// and did — silently overwrite the label, grid position and sort order of a button already
    /// placed on that channel for that item.
    fn create_layout_button(
        &self,
        button: &LayoutButton,
    ) -> impl Future<Output = Result<CreateOutcome, CatalogStoreError>> + Send;

    /// Replaces a layout button's presentation, only at the version the caller read it at.
    fn update_layout_button(
        &self,
        button: &LayoutButton,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Lists a tenant's layout buttons across all channels, each with the version it was read at.
    fn list_layout_buttons(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<LayoutButton>>, CatalogStoreError>> + Send;

    /// Removes a layout button by its `(tenant, sales_channel, menu_item_id)` identity. Returns
    /// whether a row was found and removed.
    fn remove_layout_button(
        &self,
        tenant_id: TenantId,
        sales_channel: Open<SalesChannel>,
        menu_item_id: MenuItemId,
    ) -> impl Future<Output = Result<bool, CatalogStoreError>> + Send;

    /// Inserts a modifier group.
    fn create_modifier_group(
        &self,
        group: &ModifierGroup,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's modifier groups.
    fn list_modifier_groups(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<ModifierGroup>>, CatalogStoreError>> + Send;

    /// Renames a modifier group, sets its selection rule, members, attachments and/or status. Returns
    /// whether a row changed.
    fn update_modifier_group(
        &self,
        group: &ModifierGroup,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts a menu.
    fn create_menu(
        &self,
        menu: &Menu,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a tenant's menus.
    fn list_menus(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<Menu>>, CatalogStoreError>> + Send;

    /// Renames a menu, (re)sets its parent, and/or sets its status, within its tenant. Returns
    /// whether a row changed.
    fn update_menu(
        &self,
        menu: &Menu,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts a menu section.
    fn create_menu_section(
        &self,
        section: &MenuSection,
    ) -> impl Future<Output = Result<Version, CatalogStoreError>> + Send;

    /// Lists a menu's sections, within its tenant.
    fn list_menu_sections(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> impl Future<Output = Result<Vec<Versioned<MenuSection>>, CatalogStoreError>> + Send;

    /// Renames a menu section, sets its sort and/or status. Applies only at `expected`.
    fn update_menu_section(
        &self,
        section: &MenuSection,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Inserts a placement, refusing if its `(menu_id, menu_item_id)` pair is already on the menu.
    ///
    /// The pair is entirely caller-supplied, so the `set_placement` this replaced could — and did —
    /// silently overwrite the prices, section and availability of an item already on that menu.
    fn create_placement(
        &self,
        placement: &MenuPlacement,
    ) -> impl Future<Output = Result<CreateOutcome, CatalogStoreError>> + Send;

    /// Replaces a placement's section, prices and availability, only at the version the caller read.
    fn update_placement(
        &self,
        placement: &MenuPlacement,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CatalogStoreError>> + Send;

    /// Lists a menu's placements, within its tenant, each with the version it was read at.
    fn list_placements(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> impl Future<Output = Result<Vec<Versioned<MenuPlacement>>, CatalogStoreError>> + Send;

    /// Removes an item from a menu. Returns whether a row was found and removed.
    fn remove_placement(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
        menu_item_id: MenuItemId,
    ) -> impl Future<Output = Result<bool, CatalogStoreError>> + Send;
}

/// A failure of the catalog store itself — the database is unreachable.
#[derive(Debug, thiserror::Error)]
#[error("the catalog store failed: {0}")]
pub struct CatalogStoreError(String);

impl CatalogStoreError {
    /// Wraps a message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{
        CatalogItem, CatalogStore, CatalogStoreError, ChannelPrice, DisplayCategory,
        DisplaySubcategory, ItemCategory, ItemSubcategory, LayoutButton, Menu, MenuId,
        MenuPlacement, MenuSection, ModifierGroup, TaxClass,
    };
    use crate::paging::{Page, PageRequest};
    use crate::registry::EntityStatus;
    use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

    /// An in-memory `CatalogStore` for the domain tests here and, later, the compiler's. Tenant-scoped
    /// like the real thing; every list filters by tenant so a test can prove isolation.
    #[derive(Default)]
    struct FakeCatalog {
        items: Mutex<Vec<Versioned<CatalogItem>>>,
        tax_classes: Mutex<Vec<Versioned<TaxClass>>>,
        categories: Mutex<Vec<Versioned<ItemCategory>>>,
        subcategories: Mutex<Vec<Versioned<ItemSubcategory>>>,
        display_categories: Mutex<Vec<Versioned<DisplayCategory>>>,
        display_subcategories: Mutex<Vec<Versioned<DisplaySubcategory>>>,
        layout_buttons: Mutex<Vec<Versioned<LayoutButton>>>,
        modifier_groups: Mutex<Vec<Versioned<ModifierGroup>>>,
        menus: Mutex<Vec<Versioned<Menu>>>,
        menu_sections: Mutex<Vec<Versioned<MenuSection>>>,
        placements: Mutex<Vec<Versioned<MenuPlacement>>>,
        next_version: Mutex<u64>,
    }

    impl FakeCatalog {
        /// The fake's stand-in for `xmin` (ADR-0094): a token that changes on every
        /// successful write, which is the only property the seam contract needs.
        fn mint(&self) -> Version {
            let mut next = self.next_version.lock().expect("lock");
            *next += 1;
            Version::new(next.to_string())
        }

        /// The tenant's items, in insertion order.
        ///
        /// Shared by the whole-set and paged reads so the page cannot filter differently from the
        /// set it claims to be a window onto.
        fn tenant_items(&self, tenant_id: TenantId) -> Vec<Versioned<CatalogItem>> {
            self.items
                .lock()
                .expect("lock")
                .iter()
                .filter(|item| item.record.tenant_id == tenant_id)
                .cloned()
                .collect()
        }
    }

    impl CatalogStore for FakeCatalog {
        async fn create_item(&self, item: &CatalogItem) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.items
                .lock()
                .expect("lock")
                .push(Versioned::new(item.clone(), version.clone()));
            Ok(version)
        }

        async fn list_items(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<CatalogItem>>, CatalogStoreError> {
            Ok(self.tenant_items(tenant_id))
        }

        async fn list_items_page(
            &self,
            tenant_id: TenantId,
            page: PageRequest,
        ) -> Result<Page<Versioned<CatalogItem>>, CatalogStoreError> {
            let matching = self.tenant_items(tenant_id);
            let total = u32::try_from(matching.len()).unwrap_or(u32::MAX);
            let items = matching
                .into_iter()
                .skip(page.offset() as usize)
                .take(page.limit() as usize)
                .collect();
            Ok(Page::new(items, total))
        }

        async fn update_item(
            &self,
            item: &CatalogItem,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut items = self.items.lock().expect("lock");
            let Some(row) = items.iter_mut().find(|row| {
                row.record.menu_item_id == item.menu_item_id
                    && row.record.tenant_id == item.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&item.name);
            row.record
                .name_translations
                .clone_from(&item.name_translations);
            row.record.tax_class_id = item.tax_class_id;
            row.record.item_category_id = item.item_category_id;
            row.record.item_subcategory_id = item.item_subcategory_id;
            row.record.image_ref = item.image_ref;
            row.record.status = item.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_tax_class(
            &self,
            tax_class: &TaxClass,
        ) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.tax_classes
                .lock()
                .expect("lock")
                .push(Versioned::new(tax_class.clone(), version.clone()));
            Ok(version)
        }

        async fn list_tax_classes(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<TaxClass>>, CatalogStoreError> {
            Ok(self
                .tax_classes
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_tax_class(
            &self,
            tax_class: &TaxClass,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.tax_classes.lock().expect("lock");
            let Some(row) = rows.iter_mut().find(|row| {
                row.record.tax_class_id == tax_class.tax_class_id
                    && row.record.tenant_id == tax_class.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&tax_class.name);
            row.record.status = tax_class.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_item_category(
            &self,
            category: &ItemCategory,
        ) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.categories
                .lock()
                .expect("lock")
                .push(Versioned::new(category.clone(), version.clone()));
            Ok(version)
        }

        async fn list_item_categories(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<ItemCategory>>, CatalogStoreError> {
            Ok(self
                .categories
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_item_category(
            &self,
            category: &ItemCategory,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.categories.lock().expect("lock");
            let Some(row) = rows.iter_mut().find(|row| {
                row.record.item_category_id == category.item_category_id
                    && row.record.tenant_id == category.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&category.name);
            row.record.status = category.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_item_subcategory(
            &self,
            subcategory: &ItemSubcategory,
        ) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.subcategories
                .lock()
                .expect("lock")
                .push(Versioned::new(subcategory.clone(), version.clone()));
            Ok(version)
        }

        async fn list_item_subcategories(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<ItemSubcategory>>, CatalogStoreError> {
            Ok(self
                .subcategories
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_item_subcategory(
            &self,
            subcategory: &ItemSubcategory,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.subcategories.lock().expect("lock");
            let Some(row) = rows.iter_mut().find(|row| {
                row.record.item_subcategory_id == subcategory.item_subcategory_id
                    && row.record.tenant_id == subcategory.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&subcategory.name);
            row.record.item_category_id = subcategory.item_category_id;
            row.record.status = subcategory.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_display_category(
            &self,
            category: &DisplayCategory,
        ) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.display_categories
                .lock()
                .expect("lock")
                .push(Versioned::new(category.clone(), version.clone()));
            Ok(version)
        }

        async fn list_display_categories(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<DisplayCategory>>, CatalogStoreError> {
            Ok(self
                .display_categories
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_display_category(
            &self,
            category: &DisplayCategory,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.display_categories.lock().expect("lock");
            let Some(row) = rows.iter_mut().find(|row| {
                row.record.display_category_id == category.display_category_id
                    && row.record.tenant_id == category.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&category.name);
            row.record.status = category.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_display_subcategory(
            &self,
            subcategory: &DisplaySubcategory,
        ) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.display_subcategories
                .lock()
                .expect("lock")
                .push(Versioned::new(subcategory.clone(), version.clone()));
            Ok(version)
        }

        async fn list_display_subcategories(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<DisplaySubcategory>>, CatalogStoreError> {
            Ok(self
                .display_subcategories
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_display_subcategory(
            &self,
            subcategory: &DisplaySubcategory,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.display_subcategories.lock().expect("lock");
            let Some(row) = rows.iter_mut().find(|row| {
                row.record.display_subcategory_id == subcategory.display_subcategory_id
                    && row.record.tenant_id == subcategory.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&subcategory.name);
            row.record.display_category_id = subcategory.display_category_id;
            row.record.status = subcategory.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_layout_button(
            &self,
            button: &LayoutButton,
        ) -> Result<CreateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.layout_buttons.lock().expect("lock");
            if rows.iter().any(|row| {
                row.record.tenant_id == button.tenant_id
                    && row.record.sales_channel == button.sales_channel
                    && row.record.menu_item_id == button.menu_item_id
            }) {
                return Ok(CreateOutcome::AlreadyExists);
            }
            rows.push(Versioned::new(button.clone(), version.clone()));
            Ok(CreateOutcome::Created(version))
        }

        async fn update_layout_button(
            &self,
            button: &LayoutButton,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.layout_buttons.lock().expect("lock");
            let Some(row) = rows.iter_mut().find(|row| {
                row.record.tenant_id == button.tenant_id
                    && row.record.sales_channel == button.sales_channel
                    && row.record.menu_item_id == button.menu_item_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record = button.clone();
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn list_layout_buttons(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<LayoutButton>>, CatalogStoreError> {
            Ok(self
                .layout_buttons
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn remove_layout_button(
            &self,
            tenant_id: TenantId,
            sales_channel: Open<SalesChannel>,
            menu_item_id: MenuItemId,
        ) -> Result<bool, CatalogStoreError> {
            let mut rows = self.layout_buttons.lock().expect("lock");
            let before = rows.len();
            rows.retain(|row| {
                !(row.record.tenant_id == tenant_id
                    && row.record.sales_channel == sales_channel
                    && row.record.menu_item_id == menu_item_id)
            });
            Ok(rows.len() != before)
        }

        async fn create_modifier_group(
            &self,
            group: &ModifierGroup,
        ) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.modifier_groups
                .lock()
                .expect("lock")
                .push(Versioned::new(group.clone(), version.clone()));
            Ok(version)
        }

        async fn list_modifier_groups(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<ModifierGroup>>, CatalogStoreError> {
            Ok(self
                .modifier_groups
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_modifier_group(
            &self,
            group: &ModifierGroup,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut rows = self.modifier_groups.lock().expect("lock");
            let Some(row) = rows.iter_mut().find(|row| {
                row.record.modifier_group_id == group.modifier_group_id
                    && row.record.tenant_id == group.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record = group.clone();
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_menu(&self, menu: &Menu) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.menus
                .lock()
                .expect("lock")
                .push(Versioned::new(menu.clone(), version.clone()));
            Ok(version)
        }

        async fn list_menus(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<Menu>>, CatalogStoreError> {
            Ok(self
                .menus
                .lock()
                .expect("lock")
                .iter()
                .filter(|menu| menu.record.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_menu(
            &self,
            menu: &Menu,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut menus = self.menus.lock().expect("lock");
            let Some(row) = menus.iter_mut().find(|row| {
                row.record.menu_id == menu.menu_id && row.record.tenant_id == menu.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&menu.name);
            row.record.parent_menu_id = menu.parent_menu_id;
            row.record.status = menu.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_menu_section(
            &self,
            section: &MenuSection,
        ) -> Result<Version, CatalogStoreError> {
            let version = self.mint();
            self.menu_sections
                .lock()
                .expect("lock")
                .push(Versioned::new(section.clone(), version.clone()));
            Ok(version)
        }

        async fn list_menu_sections(
            &self,
            tenant_id: TenantId,
            menu_id: MenuId,
        ) -> Result<Vec<Versioned<MenuSection>>, CatalogStoreError> {
            Ok(self
                .menu_sections
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id && row.record.menu_id == menu_id)
                .cloned()
                .collect())
        }

        async fn update_menu_section(
            &self,
            section: &MenuSection,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut sections = self.menu_sections.lock().expect("lock");
            let Some(row) = sections.iter_mut().find(|row| {
                row.record.menu_section_id == section.menu_section_id
                    && row.record.tenant_id == section.tenant_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record.name.clone_from(&section.name);
            row.record.sort = section.sort;
            row.record.status = section.status;
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn create_placement(
            &self,
            placement: &MenuPlacement,
        ) -> Result<CreateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut placements = self.placements.lock().expect("lock");
            if placements.iter().any(|row| {
                row.record.tenant_id == placement.tenant_id
                    && row.record.menu_id == placement.menu_id
                    && row.record.menu_item_id == placement.menu_item_id
            }) {
                return Ok(CreateOutcome::AlreadyExists);
            }
            placements.push(Versioned::new(placement.clone(), version.clone()));
            Ok(CreateOutcome::Created(version))
        }

        async fn update_placement(
            &self,
            placement: &MenuPlacement,
            expected: &Version,
        ) -> Result<UpdateOutcome, CatalogStoreError> {
            let version = self.mint();
            let mut placements = self.placements.lock().expect("lock");
            let Some(row) = placements.iter_mut().find(|row| {
                row.record.tenant_id == placement.tenant_id
                    && row.record.menu_id == placement.menu_id
                    && row.record.menu_item_id == placement.menu_item_id
            }) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.etag != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.record = placement.clone();
            row.etag = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn list_placements(
            &self,
            tenant_id: TenantId,
            menu_id: MenuId,
        ) -> Result<Vec<Versioned<MenuPlacement>>, CatalogStoreError> {
            Ok(self
                .placements
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.record.tenant_id == tenant_id && row.record.menu_id == menu_id)
                .cloned()
                .collect())
        }

        async fn remove_placement(
            &self,
            tenant_id: TenantId,
            menu_id: MenuId,
            menu_item_id: MenuItemId,
        ) -> Result<bool, CatalogStoreError> {
            let mut placements = self.placements.lock().expect("lock");
            let before = placements.len();
            placements.retain(|row| {
                !(row.record.tenant_id == tenant_id
                    && row.record.menu_id == menu_id
                    && row.record.menu_item_id == menu_item_id)
            });
            Ok(placements.len() != before)
        }
    }

    fn tenant(n: u128) -> TenantId {
        TenantId::new(Ulid::from_u128(n))
    }

    fn item_id(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn menu_id(n: u128) -> MenuId {
        MenuId::new(Ulid::from_u128(n))
    }

    fn tax_class(n: u128) -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(n))
    }

    fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    fn item(tenant_n: u128, id_n: u128, name: &str) -> CatalogItem {
        CatalogItem {
            menu_item_id: item_id(id_n),
            tenant_id: tenant(tenant_n),
            name: name.to_owned(),
            name_translations: BTreeMap::new(),
            tax_class_id: tax_class(1),
            item_category_id: None,
            item_subcategory_id: None,
            image_ref: None,
            status: EntityStatus::Active,
        }
    }

    #[tokio::test]
    async fn items_are_created_listed_and_updated_within_a_tenant() {
        let store = FakeCatalog::default();
        store
            .create_item(&item(1, 500, "Margherita"))
            .await
            .expect("create");
        store
            .create_item(&item(1, 501, "Marinara"))
            .await
            .expect("create");
        store
            .create_item(&item(2, 999, "Someone else's"))
            .await
            .expect("create");

        let mine = store.list_items(tenant(1)).await.expect("list");
        assert_eq!(mine.len(), 2, "another tenant's item is not listed");

        let renamed = CatalogItem {
            name: "Margherita (classic)".to_owned(),
            ..item(1, 500, "unused")
        };
        // The version the row was read at is what makes the write conditional (ADR-0094); an update
        // against a version the row no longer holds is refused rather than applied.
        let at = mine
            .iter()
            .find(|row| row.record.menu_item_id == item_id(500))
            .expect("present")
            .etag
            .clone();
        // The token itself is opaque — asserting a particular one would couple this to the fake's
        // counter, which is exactly what the seam says a caller may not do. What matters is that the
        // write applied and came back with *a* version.
        assert!(matches!(
            store.update_item(&renamed, &at).await.expect("update"),
            UpdateOutcome::Updated(_),
        ));
        let found = store
            .list_items(tenant(1))
            .await
            .expect("list")
            .into_iter()
            .find(|row| row.record.menu_item_id == item_id(500))
            .expect("present");
        assert_eq!(found.record.name, "Margherita (classic)");
        assert_ne!(found.etag, at, "an applied update moves the version");

        // Replaying the version just replaced is the lost update, refused.
        assert_eq!(
            store.update_item(&renamed, &at).await.expect("update"),
            UpdateOutcome::VersionMismatch,
        );

        // An unknown id updates nothing, and says so differently.
        assert_eq!(
            store
                .update_item(&item(1, 12345, "ghost"), &at)
                .await
                .expect("update"),
            UpdateOutcome::NotFound,
        );
    }

    #[tokio::test]
    async fn a_menu_can_inherit_from_a_parent() {
        let store = FakeCatalog::default();
        let standard = Menu {
            menu_id: menu_id(10),
            tenant_id: tenant(1),
            name: "Standard".to_owned(),
            parent_menu_id: None,
            status: EntityStatus::Active,
        };
        let grab = Menu {
            menu_id: menu_id(11),
            tenant_id: tenant(1),
            name: "Grab".to_owned(),
            parent_menu_id: Some(menu_id(10)),
            status: EntityStatus::Active,
        };
        store.create_menu(&standard).await.expect("create");
        store.create_menu(&grab).await.expect("create");

        let menus = store.list_menus(tenant(1)).await.expect("list");
        assert_eq!(menus.len(), 2);
        let child = menus
            .iter()
            .find(|m| m.record.menu_id == menu_id(11))
            .expect("present");
        assert_eq!(
            child.record.parent_menu_id,
            Some(menu_id(10)),
            "the inheritance edge is stored"
        );
    }

    #[tokio::test]
    async fn a_placement_create_refuses_a_pair_already_on_the_menu_and_a_reprice_needs_its_version()
    {
        let store = FakeCatalog::default();
        let placement = |price: i64| MenuPlacement {
            tenant_id: tenant(1),
            menu_id: menu_id(10),
            menu_item_id: item_id(500),
            menu_section_id: None,
            prices: vec![ChannelPrice {
                sales_channel: Open::from_known(SalesChannel::DineIn),
                unit_price: vnd(price),
            }],
            available: true,
        };
        let first = match store
            .create_placement(&placement(150_000))
            .await
            .expect("create")
        {
            CreateOutcome::Created(version) => version,
            CreateOutcome::AlreadyExists => panic!("the pair was free"),
        };

        // A placement's identity is the caller-supplied `(menu, item)` pair, so before ADR-0095 split
        // this seam a second "add this item to the menu" silently repriced the one already on it —
        // and because the per-channel prices are the price-change journal (ADR-0069, G2), the
        // overwrite was recorded as a set with no `before` to compare against. Now it is refused.
        assert_eq!(
            store
                .create_placement(&placement(160_000))
                .await
                .expect("the comparison must not raise"),
            CreateOutcome::AlreadyExists
        );
        let rows = store
            .list_placements(tenant(1), menu_id(10))
            .await
            .expect("list");
        assert_eq!(rows.len(), 1, "a refused create adds no row");
        let row = rows.first().expect("one placement");
        assert_eq!(
            row.record.prices.first().expect("one price").unit_price,
            vnd(150_000),
            "and does not reprice the placement it refused to overwrite"
        );
        assert_eq!(
            row.etag, first,
            "the list carries the version, which is where a reprice gets the token it must send"
        );

        // The reprice goes through update, at the version the read carried.
        let repriced = match store
            .update_placement(&placement(160_000), &first)
            .await
            .expect("the update")
        {
            UpdateOutcome::Updated(version) => version,
            other => panic!("expected the reprice to apply, got {other:?}"),
        };
        let rows = store
            .list_placements(tenant(1), menu_id(10))
            .await
            .expect("list again");
        assert_eq!(rows.len(), 1, "an update does not add a row");
        let row = rows.first().expect("one placement");
        assert_eq!(
            row.record.prices.first().expect("one price").unit_price,
            vnd(160_000)
        );
        assert_eq!(row.etag, repriced, "and the version moves with the write");

        // Replaying the spent version is the lost update, refused.
        assert_eq!(
            store
                .update_placement(&placement(170_000), &first)
                .await
                .expect("the comparison must not raise"),
            UpdateOutcome::VersionMismatch
        );

        assert!(
            store
                .remove_placement(tenant(1), menu_id(10), item_id(500))
                .await
                .expect("remove")
        );
        assert!(
            store
                .list_placements(tenant(1), menu_id(10))
                .await
                .expect("list")
                .is_empty()
        );
        assert!(
            !store
                .remove_placement(tenant(1), menu_id(10), item_id(500))
                .await
                .expect("remove"),
            "removing an absent placement changes nothing"
        );

        // And the pair is free again, so a create at it now succeeds.
        assert!(matches!(
            store
                .create_placement(&placement(150_000))
                .await
                .expect("create after the remove"),
            CreateOutcome::Created(_)
        ));
    }
}
