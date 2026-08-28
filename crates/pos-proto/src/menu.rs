// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's authoritative menu catalog ([ADR-0063](../../../docs/adr/0063-store-menu-catalog.md)).
//!
//! # Why the store needs a price book at all
//!
//! The dine-in path prices on the *device*: a waiter's tablet holds the menu it synced, so the edge
//! records the amounts the device captured and never reprices ([ADR-0026](../../../docs/adr/0026-port-shapes.md),
//! `docs/pos-spec.md` §14.2). An **inbound** order — a marketplace order, `POST /v1/orders`, a QR
//! guest — has no device in the loop: it arrives as `(menu_item_id, quantity)` and nothing else. So
//! the store server itself must be able to turn that into a priced line, or the `OrderIn` contract's
//! rule 2 ("the store's price wins") and rule 3 ("an unknown item is refused, never substituted")
//! have nothing to stand on. This catalog is that price book.
//!
//! # Why the type is here and not in `pos-core`
//!
//! Same two reasons the [`crate::locale::TaxRateTable`] is here. `pos-core` reprices from the catalog
//! and must not depend on anything downstream of `pos-proto`
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md)); and the catalog crosses the wire — the
//! cloud publishes it to stores inside the configuration tree
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md)), so it needs the same forward-compatible
//! serialisation as every other configuration shape. The *logic* that consumes it — repricing — is
//! domain, and lives in `pos_core::menu`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::ids::{MenuItemId, TaxClassId};
use crate::money::Money;
use crate::text::DisplayName;
use crate::wire_enum::Open;

/// One sellable item in the store's catalog: what it is called, what it costs, and how it is taxed.
///
/// The fields are exactly those a `sales.order_line.added` event captures for a line, because that is
/// what repricing produces — the catalog is the source the store reads to fill them for an inbound
/// order the way a device fills them for a dine-in one.
///
/// A modifier is itself a `MenuEntry`: choosing "extra cheese" adds that entry's `unit_price` to the
/// line, so a modifier that the store does not sell is refused exactly as an unknown base item is
/// ("modifiers are optional additions priced when chosen", `pos-core` inventory §8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MenuEntry {
    /// The item this prices — the identifier an inbound order names.
    pub menu_item_id: MenuItemId,
    /// The name to show the guest, captured onto the line so it never re-reads the live menu. Always
    /// present, and the fallback for a locale [`display_name_translations`](Self::display_name_translations)
    /// does not carry.
    pub display_name: DisplayName,
    /// The name in each locale the item is translated into, keyed by locale code (`"vi"`, `"en"`, …)
    /// ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), Track M4). The store's display
    /// language selects one at the edge with [`localized_name`](Self::localized_name); a locale absent
    /// here falls back to [`display_name`](Self::display_name), so an item with no translations behaves
    /// exactly as it did before. Additive and `#[serde(default)]`: an older edge that predates the
    /// field ignores it, and a book that omits it still loads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub display_name_translations: BTreeMap<String, DisplayName>,
    /// The store's price per unit. Integer [`Money`]; the caller's quote never overrides it.
    pub unit_price: Money,
    /// The tax class, which the channel-keyed [`crate::locale::TaxRateTable`] turns into a rate.
    pub tax_class_id: TaxClassId,
    /// Whether the item can be sold right now. An item present but 86'd (out of stock, or an operator
    /// has paused it) is refused rather than promised. Absent in the document means available: an
    /// item on the menu is sellable unless something says otherwise.
    #[serde(default = "available_by_default")]
    pub available: bool,
}

/// The default for [`MenuEntry::available`] when a document omits it: an item on the menu is for sale.
const fn available_by_default() -> bool {
    true
}

impl MenuEntry {
    /// A sellable catalog entry.
    #[must_use]
    pub fn new(
        menu_item_id: MenuItemId,
        display_name: DisplayName,
        unit_price: Money,
        tax_class_id: TaxClassId,
    ) -> Self {
        Self {
            menu_item_id,
            display_name,
            display_name_translations: BTreeMap::new(),
            unit_price,
            tax_class_id,
            available: true,
        }
    }

    /// The same entry marked 86'd — present in the catalog but not for sale right now.
    #[must_use]
    pub fn out_of_stock(mut self) -> Self {
        self.available = false;
        self
    }

    /// The same entry with its per-locale names set (ADR-0074). [`display_name`](Self::display_name)
    /// stays the fallback; each translation overrides it for its locale at the edge.
    #[must_use]
    pub fn with_name_translations(mut self, translations: BTreeMap<String, DisplayName>) -> Self {
        self.display_name_translations = translations;
        self
    }

    /// The name to show in `language`: the translation for that locale if the entry carries one, else
    /// [`display_name`](Self::display_name). Total and never-blank — an untranslated item shows its
    /// default name rather than nothing.
    #[must_use]
    pub fn localized_name(&self, language: &str) -> &DisplayName {
        self.display_name_translations
            .get(language)
            .unwrap_or(&self.display_name)
    }
}

/// The store's price book: the items it sells and what they cost.
///
/// A list of rows rather than a map, for the same reason [`crate::locale::TaxRateTable`] is — it
/// survives JSON round-tripping through the configuration tree, and it is a shape a person can read
/// in a diff ([ADR-0010](../../../docs/adr/0010-naming-standard.md)). Lookups are a linear scan; a
/// store's menu is a couple of hundred items and an order a handful of lines, so the scan is never
/// the cost that matters, and a `Vec` keeps the wire form honest.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MenuCatalog {
    items: Vec<MenuEntry>,
}

impl MenuCatalog {
    /// An empty catalog — a store with no menu published yet, which sells nothing to an inbound
    /// channel until one is (a safe default: it never guesses a price).
    #[must_use]
    pub const fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// A catalog from its rows.
    #[must_use]
    pub const fn from_items(items: Vec<MenuEntry>) -> Self {
        Self { items }
    }

    /// Adds an entry, for building a catalog in code or a test.
    #[must_use]
    pub fn with(mut self, entry: MenuEntry) -> Self {
        self.items.push(entry);
        self
    }

    /// Every entry.
    #[must_use]
    pub fn items(&self) -> &[MenuEntry] {
        &self.items
    }

    /// The entry for an item, or `None` if the store does not carry it.
    #[must_use]
    pub fn get(&self, menu_item_id: MenuItemId) -> Option<&MenuEntry> {
        self.items
            .iter()
            .find(|entry| entry.menu_item_id == menu_item_id)
    }

    /// Whether the catalog carries anything at all — false for a store whose menu is not yet
    /// published.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many items the catalog carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The same catalog with every entry's [`display_name`](MenuEntry::display_name) resolved to
    /// `language` (ADR-0074) — the store's display language applied once, at the edge, so the priced
    /// line and receipt read in the store's language. An entry with no translation for `language`
    /// keeps its default name (never-blank). The lookup by id is unchanged, so repricing a base item
    /// or a modifier reads the localized name uniformly.
    #[must_use]
    pub fn localized(&self, language: &str) -> Self {
        Self::from_items(
            self.items
                .iter()
                .map(|entry| {
                    let mut localized = entry.clone();
                    localized.display_name = entry.localized_name(language).clone();
                    localized
                })
                .collect(),
        )
    }
}

/// One channel's price book within a [`MenuBook`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChannelCatalog {
    /// The channel this catalog prices. `Open`, so a book published by a newer cloud that has learned
    /// a channel this build has not still deserialises — the same rule [`crate::locale::TaxRateRow`]
    /// follows for the rate table.
    pub sales_channel: Open<SalesChannel>,
    /// The price book in force on that channel.
    pub catalog: MenuCatalog,
}

/// The store's price book, resolved per sales channel.
///
/// [ADR-0063](../../../docs/adr/0063-store-menu-catalog.md) gave the store one flat [`MenuCatalog`];
/// [ADR-0066](../../../docs/adr/0066-cloud-catalog.md) makes the compiled `menu` node a `MenuBook`, so
/// the same item can be one price dine-in and another on delivery **without reshaping** the tested
/// [`MenuEntry`] / reprice contract. The cloud resolves the channel at compile time and emits one
/// catalog per channel; the edge selects the catalog for an inbound order's channel with
/// [`MenuBook::catalog_for`] and reprices from it exactly as before.
///
/// A list of rows plus a `fallback`, for the same round-trips-in-a-diff reason [`MenuCatalog`] and
/// [`crate::locale::TaxRateTable`] are lists. A single-channel store is the degenerate case: one row,
/// or just the fallback.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MenuBook {
    channels: Vec<ChannelCatalog>,
    /// The catalog used for a channel that has no row of its own. Empty by default — a store with no
    /// fallback simply sells nothing on an unconfigured channel, the same safe default an empty
    /// [`MenuCatalog`] already is. `#[serde(default)]` so a book that omits it still loads.
    #[serde(default)]
    fallback: MenuCatalog,
}

impl MenuBook {
    /// An empty book — no channel priced and no fallback, so it sells nothing anywhere until the
    /// cloud publishes one. The safe default, like [`MenuCatalog::new`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            channels: Vec::new(),
            fallback: MenuCatalog::new(),
        }
    }

    /// A book from its channel rows, with an empty fallback.
    #[must_use]
    pub const fn from_channels(channels: Vec<ChannelCatalog>) -> Self {
        Self {
            channels,
            fallback: MenuCatalog::new(),
        }
    }

    /// Adds (or, in code, appends) a channel's catalog.
    #[must_use]
    pub fn with(mut self, sales_channel: SalesChannel, catalog: MenuCatalog) -> Self {
        self.channels.push(ChannelCatalog {
            sales_channel: Open::from_known(sales_channel),
            catalog,
        });
        self
    }

    /// Sets the fallback catalog used for channels without a row of their own.
    #[must_use]
    pub fn with_fallback(mut self, fallback: MenuCatalog) -> Self {
        self.fallback = fallback;
        self
    }

    /// Every channel row.
    #[must_use]
    pub fn channels(&self) -> &[ChannelCatalog] {
        &self.channels
    }

    /// The fallback catalog.
    #[must_use]
    pub const fn fallback(&self) -> &MenuCatalog {
        &self.fallback
    }

    /// The catalog to price a channel from: the channel's own row if the book carries one, otherwise
    /// the fallback. Total — it always returns a catalog (empty if nothing is configured), so the
    /// caller reprices uniformly and an unpriced channel refuses every line as `UnknownItem` rather
    /// than needing a separate "no catalog" branch. An unrecognised channel row never answers for a
    /// known channel, the same guard [`crate::locale::TaxRateTable::rate_for`] applies.
    #[must_use]
    pub fn catalog_for(&self, sales_channel: SalesChannel) -> &MenuCatalog {
        self.channels
            .iter()
            .find(|row| {
                row.sales_channel.known() == sales_channel && !row.sales_channel.is_unrecognised()
            })
            .map_or(&self.fallback, |row| &row.catalog)
    }

    /// Whether the book prices nothing at all — no channel row and an empty fallback.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() && self.fallback.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{MenuBook, MenuCatalog, MenuEntry};
    use crate::enums::SalesChannel;
    use crate::ids::{MenuItemId, TaxClassId};
    use crate::money::{CurrencyCode, Money};
    use crate::text::DisplayName;
    use crate::ulid::Ulid;

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    fn food_class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(1))
    }

    fn margherita() -> MenuEntry {
        MenuEntry::new(
            item(500),
            DisplayName::new("Margherita"),
            vnd(150_000),
            food_class(),
        )
    }

    #[test]
    fn a_catalog_finds_an_item_it_carries_and_not_one_it_does_not() {
        let catalog = MenuCatalog::new().with(margherita());
        assert_eq!(catalog.len(), 1);
        assert!(!catalog.is_empty());

        let found = catalog.get(item(500)).expect("the store carries it");
        assert_eq!(found.unit_price, vnd(150_000));
        assert_eq!(found.display_name.as_str(), "Margherita");

        assert!(
            catalog.get(item(999)).is_none(),
            "an item the store does not carry is not found — the caller refuses it, never guesses"
        );
    }

    #[test]
    fn an_empty_catalog_is_the_no_menu_published_state() {
        let catalog = MenuCatalog::new();
        assert!(catalog.is_empty());
        assert!(catalog.get(item(500)).is_none());
    }

    #[test]
    fn an_entry_is_available_unless_marked_out_of_stock() {
        assert!(
            margherita().available,
            "an item on the menu is for sale by default"
        );
        assert!(
            !margherita().out_of_stock().available,
            "an 86'd item is not"
        );
    }

    #[test]
    fn a_catalog_round_trips_through_json() {
        let catalog = MenuCatalog::new().with(margherita()).with(
            MenuEntry::new(
                item(501),
                DisplayName::new("Coke"),
                vnd(30_000),
                food_class(),
            )
            .out_of_stock(),
        );

        let json = serde_json::to_string(&catalog).expect("serialise");
        let back: MenuCatalog = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, catalog);
    }

    #[test]
    fn available_defaults_to_true_when_the_document_omits_it() {
        // A cloud that publishes only the fields it needs must not accidentally 86 an item by
        // leaving `available` out: an item present on the menu is for sale.
        let json = format!(
            r#"{{"items":[{{"menu_item_id":"{}","display_name":"Pho","unit_price":{{"currency_code":"VND","amount_minor":80000}},"tax_class_id":"{}"}}]}}"#,
            item(500),
            food_class()
        );
        let catalog: MenuCatalog = serde_json::from_str(&json).expect("deserialise");
        assert!(
            catalog.get(item(500)).expect("present").available,
            "an omitted `available` field means the item is for sale"
        );
    }

    #[test]
    fn an_unknown_field_does_not_make_the_catalog_unusable() {
        // A newer cloud adds a field a store has not learned, to an entry. The store must still apply
        // the menu, or it stops being manageable — the same forward-compatibility rule the event
        // envelope and the locale pack follow. The future field is placed on the entry (not inside the
        // strict `Money` shape, which is a different contract that rejects unknown fields).
        let json = format!(
            r#"{{"items":[{{"a_field_from_the_future":true,"menu_item_id":"{}","display_name":"Margherita","unit_price":{{"currency_code":"VND","amount_minor":150000}},"tax_class_id":"{}","available":true}}]}}"#,
            item(500),
            food_class()
        );
        let back: MenuCatalog = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(
            back,
            MenuCatalog::new().with(margherita()),
            "an unknown field must not make a menu catalog unusable, nor change what it means"
        );
    }

    fn priced(price: i64) -> MenuEntry {
        MenuEntry::new(
            item(500),
            DisplayName::new("Margherita"),
            vnd(price),
            food_class(),
        )
    }

    #[test]
    fn a_menu_book_prices_a_channel_specifically_and_falls_back_otherwise() {
        // Dine-in has its own price; delivery has none, so it takes the fallback. This is the whole
        // point of the book: one item, different money per channel, with the tested reprice contract
        // untouched.
        let book = MenuBook::new()
            .with(
                SalesChannel::DineIn,
                MenuCatalog::new().with(priced(150_000)),
            )
            .with_fallback(MenuCatalog::new().with(priced(170_000)));

        assert_eq!(
            book.catalog_for(SalesChannel::DineIn)
                .get(item(500))
                .expect("dine-in")
                .unit_price,
            vnd(150_000),
            "the channel's own catalog wins"
        );
        assert_eq!(
            book.catalog_for(SalesChannel::Delivery)
                .get(item(500))
                .expect("fallback")
                .unit_price,
            vnd(170_000),
            "a channel with no row of its own takes the fallback"
        );
        assert!(!book.is_empty());
    }

    #[test]
    fn an_empty_menu_book_prices_nothing_anywhere() {
        let book = MenuBook::new();
        assert!(book.is_empty());
        assert!(
            book.catalog_for(SalesChannel::DineIn).is_empty(),
            "with no rows and no fallback, every channel gets an empty catalog and refuses every line"
        );
    }

    #[test]
    fn a_menu_book_round_trips_through_json() {
        let book = MenuBook::new()
            .with(
                SalesChannel::DineIn,
                MenuCatalog::new().with(priced(150_000)),
            )
            .with(
                SalesChannel::Delivery,
                MenuCatalog::new().with(priced(180_000)),
            )
            .with_fallback(MenuCatalog::new().with(priced(160_000)));

        let json = serde_json::to_string(&book).expect("serialise");
        let back: MenuBook = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, book);
    }

    #[test]
    fn a_menu_book_without_a_fallback_field_loads() {
        // The fallback is `#[serde(default)]`, so a book that publishes only its channels still
        // applies — the same forward-compatibility the catalog and locale pack follow.
        let book: MenuBook = serde_json::from_str(r#"{"channels":[]}"#).expect("deserialise");
        assert!(
            book.is_empty(),
            "no channels and a defaulted empty fallback"
        );
    }

    fn translated() -> MenuEntry {
        margherita().with_name_translations(BTreeMap::from([
            ("vi".to_owned(), DisplayName::new("Bánh Margherita")),
            ("ja".to_owned(), DisplayName::new("マルゲリータ")),
        ]))
    }

    #[test]
    fn localized_name_picks_the_locale_and_falls_back_to_the_default() {
        let entry = translated();
        assert_eq!(entry.localized_name("vi").as_str(), "Bánh Margherita");
        assert_eq!(entry.localized_name("ja").as_str(), "マルゲリータ");
        assert_eq!(
            entry.localized_name("en").as_str(),
            "Margherita",
            "a locale the item is not translated into falls back to the default name"
        );
    }

    #[test]
    fn localizing_a_catalog_resolves_every_entry_or_keeps_its_default() {
        // The store's display language applied once: a translated item shows its localized name, an
        // untranslated one keeps its default, and the lookup by id is unchanged.
        let catalog = MenuCatalog::new().with(translated()).with(MenuEntry::new(
            item(501),
            DisplayName::new("Coke"),
            vnd(30_000),
            food_class(),
        ));
        let vietnamese = catalog.localized("vi");
        assert_eq!(
            vietnamese
                .get(item(500))
                .expect("translated item")
                .display_name
                .as_str(),
            "Bánh Margherita"
        );
        assert_eq!(
            vietnamese
                .get(item(501))
                .expect("untranslated item")
                .display_name
                .as_str(),
            "Coke",
            "an item with no translation keeps its default name"
        );
    }

    #[test]
    fn translations_round_trip_and_an_entry_without_them_omits_the_field() {
        // The translations map is additive and `skip_serializing_if` empty, so an entry without any
        // translations serialises exactly as before (no new key) while a translated one round-trips.
        let with = MenuCatalog::new().with(translated());
        let json = serde_json::to_string(&with).expect("serialise");
        assert_eq!(
            serde_json::from_str::<MenuCatalog>(&json).expect("deserialise"),
            with
        );

        let without =
            serde_json::to_string(&MenuCatalog::new().with(margherita())).expect("serialise");
        assert!(
            !without.contains("display_name_translations"),
            "an entry with no translations does not carry the field on the wire"
        );
    }
}
