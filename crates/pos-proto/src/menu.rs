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

use serde::{Deserialize, Serialize};

use crate::ids::{MenuItemId, TaxClassId};
use crate::money::Money;
use crate::text::DisplayName;

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
    /// The name to show the guest, captured onto the line so it never re-reads the live menu.
    pub display_name: DisplayName,
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
}

#[cfg(test)]
mod tests {
    use super::{MenuCatalog, MenuEntry};
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
}
