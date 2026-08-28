// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `inventory` config node ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md)):
//! the ingredients, per-item/per-modifier recipes, auto-86 thresholds, and supplier references an
//! operator authored, in the wire shape the edge parses back into the pure inventory domain.
//!
//! `pos_core::inventory` is the *runtime* form — `RecipeBook`, `StockProjection`, `Availability`
//! (`docs/pos-spec.md` §8) — with no serde. This module is the *wire* form: a serializable mirror the
//! cloud compiles from the authoring store and writes as the `inventory` key on the config tree's
//! Store layer, exactly as `campaigns`/`tax`/`floor` do. The edge's config apply parses it and calls
//! `pos_core::inventory::from_published` to build the `RecipeBook` and the per-item auto-86 thresholds
//! its fire path consumes. The two shapes are kept faithful; the conversion lives in
//! `pos_core::inventory`, the only place that can see both.
//!
//! A recipe carries ingredient ids and per-unit amounts, an ingredient carries a display name and its
//! unit, a supplier carries a name — configuration and reference data, never a customer identifier or
//! any T1 field. The full purchasing relationship (contracts, POs, invoices) stays in the ERP
//! (`docs/pos-spec.md` §19); the node keeps only the lightweight supplier reference a goods-receipt
//! names.

use serde::{Deserialize, Serialize};

use crate::enums::UnitOfMeasure;
use crate::ids::{IngredientId, MenuItemId, SupplierId};
use crate::quantity::Quantity;
use crate::text::DisplayName;
use crate::wire_enum::Open;

/// One ingredient held in stock, in wire form: its id, a display name, and the unit it is counted in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedIngredient {
    /// Its stable id.
    pub id: IngredientId,
    /// The operator-facing name, shown in the console and the recipe editor.
    pub name: DisplayName,
    /// The unit it is stocked and consumed in. Wrapped in [`Open`] so a unit token from a newer cloud
    /// round-trips rather than failing the whole node.
    pub unit: Open<UnitOfMeasure>,
}

/// One line of a recipe: an ingredient and how much of it one unit of the item (or modifier) consumes,
/// in that ingredient's own unit. Wire mirror of `pos_core::inventory::RecipeLine`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedRecipeLine {
    /// The ingredient consumed.
    pub ingredient: IngredientId,
    /// How much of it one unit of the item consumes (a [`Quantity`] in thousandths of the unit).
    pub per_unit: Quantity,
}

/// The bill of materials for one makeable thing — a menu item or a modifier — plus its auto-86
/// threshold. A modifier carries its own [`MenuItemId`] and its own recipe (the "large" size adds its
/// extra dough), so it is a recipe like any other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedRecipe {
    /// The item or modifier this recipe makes.
    pub item: MenuItemId,
    /// The ingredients and per-unit amounts. Empty means the item consumes nothing tracked and is
    /// never limited by stock.
    #[serde(default)]
    pub lines: Vec<PublishedRecipeLine>,
    /// The auto-86 threshold (§8): the item is sold while strictly more than this many can be made,
    /// and marked unavailable at or below it. Defaults to `0` — 86 only when nothing can be made.
    #[serde(default)]
    pub auto_86_threshold: i64,
}

/// A supplier a store receives goods from — a lightweight reference only (id + name). The full
/// purchasing relationship lives in the ERP (§19).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedSupplier {
    /// Its stable id.
    pub id: SupplierId,
    /// The operator-facing name.
    pub name: DisplayName,
}

/// The `inventory` config node: a store's ingredients, recipes, and supplier references, in wire form.
///
/// Lists (not maps) for the same round-trips-in-a-diff reason [`crate::menu::MenuBook`] and
/// [`crate::campaign::PublishedCampaigns`] are lists. Empty is the safe default — a store with no node
/// published tracks no stock, so every item is unlimited and nothing auto-86s, the never-blank config
/// contract keeping whatever the edge already holds if a publish is absent or unparseable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedInventory {
    #[serde(default)]
    ingredients: Vec<PublishedIngredient>,
    #[serde(default)]
    recipes: Vec<PublishedRecipe>,
    #[serde(default)]
    suppliers: Vec<PublishedSupplier>,
}

impl PublishedInventory {
    /// An empty node — no ingredient, recipe, or supplier, so nothing is stock-limited.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ingredients: Vec::new(),
            recipes: Vec::new(),
            suppliers: Vec::new(),
        }
    }

    /// A node from its parts.
    #[must_use]
    pub const fn from_parts(
        ingredients: Vec<PublishedIngredient>,
        recipes: Vec<PublishedRecipe>,
        suppliers: Vec<PublishedSupplier>,
    ) -> Self {
        Self {
            ingredients,
            recipes,
            suppliers,
        }
    }

    /// Every ingredient, in the order the node lists them.
    #[must_use]
    pub fn ingredients(&self) -> &[PublishedIngredient] {
        &self.ingredients
    }

    /// Every recipe, in the order the node lists them.
    #[must_use]
    pub fn recipes(&self) -> &[PublishedRecipe] {
        &self.recipes
    }

    /// Every supplier, in the order the node lists them.
    #[must_use]
    pub fn suppliers(&self) -> &[PublishedSupplier] {
        &self.suppliers
    }

    /// Whether the node carries nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ingredients.is_empty() && self.recipes.is_empty() && self.suppliers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PublishedIngredient, PublishedInventory, PublishedRecipe, PublishedRecipeLine,
        PublishedSupplier,
    };
    use crate::enums::UnitOfMeasure;
    use crate::ids::{IngredientId, MenuItemId, SupplierId};
    use crate::quantity::Quantity;
    use crate::text::DisplayName;
    use crate::ulid::Ulid;
    use crate::wire_enum::Open;

    fn sample() -> PublishedInventory {
        PublishedInventory::from_parts(
            vec![PublishedIngredient {
                id: IngredientId::new(Ulid::from_u128(1)),
                name: DisplayName::new("Dough"),
                unit: Open::from_known(UnitOfMeasure::Gram),
            }],
            vec![PublishedRecipe {
                item: MenuItemId::new(Ulid::from_u128(2)),
                lines: vec![PublishedRecipeLine {
                    ingredient: IngredientId::new(Ulid::from_u128(1)),
                    per_unit: Quantity::from_milli(100_000),
                }],
                auto_86_threshold: 2,
            }],
            vec![PublishedSupplier {
                id: SupplierId::new(Ulid::from_u128(3)),
                name: DisplayName::new("Anchor Dairy"),
            }],
        )
    }

    #[test]
    fn an_inventory_node_round_trips_through_json() {
        let node = sample();
        let json = serde_json::to_string(&node).expect("serialize");
        let back: PublishedInventory = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
        assert_eq!(back.ingredients().len(), 1);
        assert_eq!(back.recipes().len(), 1);
        assert_eq!(back.suppliers().len(), 1);
    }

    #[test]
    fn an_empty_node_is_the_default() {
        assert!(PublishedInventory::default().is_empty());
    }

    #[test]
    fn a_recipe_defaults_its_threshold_and_lines() {
        // A recipe document that omits `lines` and `auto_86_threshold` reloads with an empty BOM and a
        // zero threshold — the never-blank, 86-only-when-empty defaults.
        let item = MenuItemId::new(Ulid::from_u128(2));
        let json = serde_json::json!({ "item": item.to_string() }).to_string();
        let recipe: PublishedRecipe = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(recipe.item, item);
        assert!(recipe.lines.is_empty());
        assert_eq!(recipe.auto_86_threshold, 0);
    }
}
