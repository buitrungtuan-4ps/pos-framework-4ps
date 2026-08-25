// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's presentation plan ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
//!
//! # The layout half of the catalog, kept away from the price half
//!
//! A catalog has two compiled outputs and they must not entangle. The **price book**
//! ([`crate::MenuBook`]) is what the domain reprices from; the **layout** — this module — is what a
//! screen draws: which display categories to show, in what order, and where each item's button sits.
//! ADR-0066 delivers them on separate configuration nodes (`menu` and `layout`) for exactly this
//! reason: a price change relays no buttons, and a button moving reprices nothing. `pos-core` never
//! reads a `DisplayPlan`; only the POS / tablet / QR / marketplace UI does.
//!
//! # Why a display taxonomy separate from the item taxonomy
//!
//! The category a screen groups by is a *presentation* decision, not the *operational* one an item
//! reports and is taxed under. A store may show "Summer specials" as a tab while those same items
//! report under "Pizza" and "Beverage". So a [`DisplayCategory`] carries its own id
//! ([`crate::DisplayCategoryId`]), distinct from the item-master category, and an item appears in a
//! plan by a [`DisplayButton`] that names its `menu_item_id`.
//!
//! Like every configuration shape it crosses the wire in the config tree
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md)), so it is a list of rows a person can read in
//! a diff, and it tolerates unknown fields and an omitted default so a newer cloud never bricks an
//! older store.

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId};
use crate::text::DisplayName;
use crate::wire_enum::Open;

/// Where a button sits on a POS terminal's fixed grid: zero-based `column` and `row`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GridPosition {
    /// Zero-based column from the left.
    pub column: u16,
    /// Zero-based row from the top.
    pub row: u16,
}

/// One item's button in a display group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DisplayButton {
    /// The item this button orders — a `menu_item_id` the [`crate::MenuBook`] prices. Layout names
    /// the item; the price book prices it; the two meet only at the id.
    pub menu_item_id: MenuItemId,
    /// The caption to show, which may be shorter than the item's catalog name on a crowded grid.
    /// Presentation text, carried here so the UI never re-reads the price book to draw a button.
    pub label: DisplayName,
    /// The button's slot on a POS terminal's grid. `None` for a flowing layout — a tablet or a QR
    /// page — where order alone places the button. Omitted from the wire when absent (the same
    /// optional-field shape [`crate::EventEnvelope`]'s ids use), so a flowing plan stays tidy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<GridPosition>,
}

/// A display sub-category: a second grouping level under a [`DisplayCategory`], with its own buttons.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DisplaySubcategory {
    /// This sub-category's identity, distinct from any item-master category.
    pub display_subcategory_id: DisplaySubcategoryId,
    /// The name to show.
    pub name: DisplayName,
    /// Its buttons, in display order.
    #[serde(default)]
    pub buttons: Vec<DisplayButton>,
}

/// A display category: the top grouping a screen shows as a tab or section.
///
/// A **presentation** taxonomy, distinct from an item's operational category ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)):
/// a flat category has only `buttons`; a nested one groups its items into `subcategories`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DisplayCategory {
    /// This category's identity, distinct from any item-master category.
    pub display_category_id: DisplayCategoryId,
    /// The name to show.
    pub name: DisplayName,
    /// Buttons placed directly under the category — a flat category has only these.
    #[serde(default)]
    pub buttons: Vec<DisplayButton>,
    /// Nested sub-categories, in display order.
    #[serde(default)]
    pub subcategories: Vec<DisplaySubcategory>,
}

/// One sales channel's presentation plan: the display categories a screen groups by, in order.
///
/// The compiled `layout` artifact ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)) the UI reads
/// and the domain never does. A list of categories, for the round-trips-in-a-diff reason every
/// configuration shape is a list.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DisplayPlan {
    categories: Vec<DisplayCategory>,
}

impl DisplayPlan {
    /// An empty plan — a channel with nothing laid out yet, which shows no buttons until the cloud
    /// publishes one.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            categories: Vec::new(),
        }
    }

    /// A plan from its categories.
    #[must_use]
    pub const fn from_categories(categories: Vec<DisplayCategory>) -> Self {
        Self { categories }
    }

    /// Adds a category, for building a plan in code or a test.
    #[must_use]
    pub fn with(mut self, category: DisplayCategory) -> Self {
        self.categories.push(category);
        self
    }

    /// Every category, in display order.
    #[must_use]
    pub fn categories(&self) -> &[DisplayCategory] {
        &self.categories
    }

    /// Whether the plan lays out anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }
}

/// One channel's plan within a [`LayoutBook`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChannelLayout {
    /// The channel this plan lays out. `Open`, so a book from a newer cloud that has learned a
    /// channel this build has not still deserialises — the same rule [`crate::MenuBook`] follows.
    pub sales_channel: Open<SalesChannel>,
    /// The presentation plan in force on that channel.
    pub plan: DisplayPlan,
}

/// Presentation plans resolved per sales channel — the `layout` config node's shape.
///
/// The layout twin of [`crate::MenuBook`] ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)): a
/// POS terminal, a tablet, and a Grab feed each get their own plan, because the same menu is
/// presented differently on each. The cloud resolves the channel at compile time; the UI selects the
/// plan for its channel with [`LayoutBook::plan_for`].
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LayoutBook {
    channels: Vec<ChannelLayout>,
    /// The plan used for a channel with no row of its own. Empty by default, so an unconfigured
    /// channel simply shows nothing. `#[serde(default)]`, so a book that omits it still loads.
    #[serde(default)]
    fallback: DisplayPlan,
}

impl LayoutBook {
    /// An empty book — no channel laid out and no fallback.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            channels: Vec::new(),
            fallback: DisplayPlan::new(),
        }
    }

    /// A book from its channel rows, with an empty fallback.
    #[must_use]
    pub const fn from_channels(channels: Vec<ChannelLayout>) -> Self {
        Self {
            channels,
            fallback: DisplayPlan::new(),
        }
    }

    /// Adds a channel's plan.
    #[must_use]
    pub fn with(mut self, sales_channel: SalesChannel, plan: DisplayPlan) -> Self {
        self.channels.push(ChannelLayout {
            sales_channel: Open::from_known(sales_channel),
            plan,
        });
        self
    }

    /// Sets the fallback plan used for channels without a row of their own.
    #[must_use]
    pub fn with_fallback(mut self, fallback: DisplayPlan) -> Self {
        self.fallback = fallback;
        self
    }

    /// Every channel row.
    #[must_use]
    pub fn channels(&self) -> &[ChannelLayout] {
        &self.channels
    }

    /// The fallback plan.
    #[must_use]
    pub const fn fallback(&self) -> &DisplayPlan {
        &self.fallback
    }

    /// The plan to draw a channel with: its own row if the book carries one, else the fallback.
    /// Total — it always returns a plan (empty if nothing is configured), so an unconfigured channel
    /// shows nothing rather than needing a separate "no layout" branch. An unrecognised channel row
    /// never answers for a known channel, the same guard [`crate::MenuBook::catalog_for`] applies.
    #[must_use]
    pub fn plan_for(&self, sales_channel: SalesChannel) -> &DisplayPlan {
        self.channels
            .iter()
            .find(|row| {
                row.sales_channel.known() == sales_channel && !row.sales_channel.is_unrecognised()
            })
            .map_or(&self.fallback, |row| &row.plan)
    }

    /// Whether the book lays out nothing at all — no channel row and an empty fallback.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() && self.fallback.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayButton, DisplayCategory, DisplayPlan, DisplaySubcategory, GridPosition, LayoutBook,
    };
    use crate::enums::SalesChannel;
    use crate::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId};
    use crate::text::DisplayName;
    use crate::ulid::Ulid;

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn category(n: u128) -> DisplayCategoryId {
        DisplayCategoryId::new(Ulid::from_u128(n))
    }

    fn subcategory(n: u128) -> DisplaySubcategoryId {
        DisplaySubcategoryId::new(Ulid::from_u128(n))
    }

    fn button(n: u128, label: &str, position: Option<GridPosition>) -> DisplayButton {
        DisplayButton {
            menu_item_id: item(n),
            label: DisplayName::new(label),
            position,
        }
    }

    fn pizza_tab() -> DisplayCategory {
        DisplayCategory {
            display_category_id: category(10),
            name: DisplayName::new("Pizza"),
            buttons: vec![button(
                500,
                "Margherita",
                Some(GridPosition { column: 0, row: 0 }),
            )],
            subcategories: vec![DisplaySubcategory {
                display_subcategory_id: subcategory(20),
                name: DisplayName::new("Vegetarian"),
                buttons: vec![button(
                    501,
                    "Marinara",
                    Some(GridPosition { column: 1, row: 0 }),
                )],
            }],
        }
    }

    fn first_category(plan: &DisplayPlan) -> &DisplayCategory {
        plan.categories().first().expect("at least one category")
    }

    #[test]
    fn a_plan_holds_its_categories_in_order() {
        let plan = DisplayPlan::new().with(pizza_tab());
        assert!(!plan.is_empty());
        assert_eq!(plan.categories().len(), 1);
        let pizza = first_category(&plan);
        assert_eq!(pizza.name.as_str(), "Pizza");
        assert_eq!(pizza.subcategories.len(), 1);
        assert_eq!(
            pizza.buttons.first().expect("one button").position,
            Some(GridPosition { column: 0, row: 0 })
        );
    }

    #[test]
    fn a_layout_book_draws_a_channel_specifically_and_falls_back_otherwise() {
        let dine_in = DisplayPlan::new().with(pizza_tab());
        let fallback = DisplayPlan::new().with(DisplayCategory {
            display_category_id: category(99),
            name: DisplayName::new("All items"),
            buttons: vec![button(500, "Margherita", None)],
            subcategories: Vec::new(),
        });
        let book = LayoutBook::new()
            .with(SalesChannel::DineIn, dine_in)
            .with_fallback(fallback);

        assert_eq!(
            first_category(book.plan_for(SalesChannel::DineIn))
                .name
                .as_str(),
            "Pizza",
            "the channel's own plan wins"
        );
        assert_eq!(
            first_category(book.plan_for(SalesChannel::Delivery))
                .name
                .as_str(),
            "All items",
            "a channel with no row of its own takes the fallback"
        );
        assert!(!book.is_empty());
    }

    #[test]
    fn an_empty_layout_book_draws_nothing_anywhere() {
        let book = LayoutBook::new();
        assert!(book.is_empty());
        assert!(
            book.plan_for(SalesChannel::DineIn).is_empty(),
            "with no rows and no fallback, every channel gets an empty plan"
        );
    }

    #[test]
    fn a_layout_book_round_trips_through_json() {
        let book = LayoutBook::new()
            .with(SalesChannel::DineIn, DisplayPlan::new().with(pizza_tab()))
            .with_fallback(DisplayPlan::new().with(pizza_tab()));

        let json = serde_json::to_string(&book).expect("serialise");
        let back: LayoutBook = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, book);
    }

    #[test]
    fn a_flowing_button_omits_its_position_in_json() {
        // A tablet or QR button has no grid slot; `position` is `#[serde(default)]`, so the compiled
        // plan for a flowing surface omits it and still round-trips.
        let button = button(500, "Margherita", None);
        let json = serde_json::to_string(&button).expect("serialise");
        assert!(
            !json.contains("position"),
            "a None position serialises as an absent field, not null"
        );
        let back: DisplayButton = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, button);
    }

    #[test]
    fn a_layout_book_without_a_fallback_field_loads() {
        let book: LayoutBook = serde_json::from_str(r#"{"channels":[]}"#).expect("deserialise");
        assert!(
            book.is_empty(),
            "no channels and a defaulted empty fallback"
        );
    }
}
