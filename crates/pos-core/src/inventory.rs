// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Inventory: recipes, the stock projection, and availability.
//!
//! `docs/pos-spec.md` §8. Ingredients are consumed by a **bill of materials per item and per
//! modifier** — a large pizza is the base recipe plus the "large" modifier's extra dough. Stock is
//! deducted when a line is **fired** (that is when the kitchen consumes it), and availability is
//!
//! ```text
//! available(item) = floor( min over ingredients ( stock[i] / recipe[item][i] ) )
//! ```
//!
//! recomputed from the current projection every time, so shared ingredients propagate for free:
//! cooking something that uses ingredient *D* lowers the availability of everything else that also
//! uses *D*, whether or not that other thing was sold. When availability crosses a threshold the
//! item is **auto-86'd** (marked unavailable) and marketplaces are told.
//!
//! # Sans-I/O, like the rest of the domain
//!
//! This module holds a [`StockProjection`] in memory and computes [`Availability`] and the
//! [`StockMovement`]s a fired line consumes. It performs no I/O: the caller loads the projection,
//! asks the domain what a fire or a stocktake does to it, and persists the resulting ledger entries.
//! The cloud rebuilds the identical projection from the same consumption events for brand-level
//! views (§8), which only works because the arithmetic lives in one place.
//!
//! # Integer amounts, never floats
//!
//! Everything is [`Quantity`] in thousandths (`pos-proto`), so a recipe of "50 g" and a stock of
//! "10 kg" are exact integers and `available` is integer division. There is no float anywhere in the
//! availability path — the backbone forbids it, and money-adjacent counts must not drift.

use std::collections::BTreeMap;

use pos_proto::ids::{IngredientId, MenuItemId};
use pos_proto::inventory::PublishedInventory;
use pos_proto::money::MoneyError;
use pos_proto::quantity::Quantity;

/// One ingredient and how much of it one unit of a recipe consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecipeLine {
    /// The ingredient consumed.
    pub ingredient: IngredientId,
    /// How much of it one unit of the item (or modifier) consumes, in the ingredient's own unit.
    pub per_unit: Quantity,
}

/// The bill of materials for one makeable thing — an item or a modifier.
///
/// A modifier carries its own [`MenuItemId`] and its own `Recipe`, so "the large size adds 50 g of
/// dough" is a recipe like any other, added to the base when a line is fired.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipe {
    /// The ingredients and per-unit amounts. An empty recipe means the item consumes nothing tracked
    /// and is therefore never limited by stock.
    pub lines: Vec<RecipeLine>,
}

impl Recipe {
    /// A recipe from its lines.
    #[must_use]
    pub fn new(lines: Vec<RecipeLine>) -> Self {
        Self { lines }
    }
}

/// Every recipe, keyed by the id of the item or modifier it makes.
#[derive(Debug, Clone, Default)]
pub struct RecipeBook {
    recipes: BTreeMap<MenuItemId, Recipe>,
}

impl RecipeBook {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the recipe for an item or modifier, replacing any previous one.
    pub fn insert(&mut self, id: MenuItemId, recipe: Recipe) {
        self.recipes.insert(id, recipe);
    }

    /// The recipe for `id`, if one is recorded.
    #[must_use]
    pub fn get(&self, id: MenuItemId) -> Option<&Recipe> {
        self.recipes.get(&id)
    }
}

/// How many of an item can currently be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// The item has no inventory-tracked recipe, so stock never limits it.
    Unlimited,
    /// At most this many can be made from the current stock.
    Limited(i64),
}

impl Availability {
    /// Whether at least `threshold + 1` can be made — the sell/86 decision. An [`Availability::Unlimited`]
    /// item is always sellable; a [`Availability::Limited`] one must be strictly above the threshold.
    ///
    /// Auto-86 (§8) is the negation: an item that is *not* available at its threshold is marked
    /// unavailable and marketplaces are notified.
    #[must_use]
    pub const fn is_sellable(self, threshold: i64) -> bool {
        match self {
            Self::Unlimited => true,
            Self::Limited(count) => count > threshold,
        }
    }
}

/// The current on-hand quantity of every tracked ingredient.
///
/// Built by replaying [`StockMovement`]s (which mirror the five ledger kinds) over a starting set of
/// levels. Deterministic and `std`-only, so the edge and the cloud build the identical projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StockProjection {
    on_hand: BTreeMap<IngredientId, Quantity>,
}

impl StockProjection {
    /// An empty projection — every ingredient reads as zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an ingredient's on-hand quantity outright — for seeding from a snapshot.
    pub fn set_on_hand(&mut self, ingredient: IngredientId, quantity: Quantity) {
        self.on_hand.insert(ingredient, quantity);
    }

    /// The on-hand quantity of an ingredient, zero if it has never been seen.
    #[must_use]
    pub fn on_hand(&self, ingredient: IngredientId) -> Quantity {
        self.on_hand
            .get(&ingredient)
            .copied()
            .unwrap_or(Quantity::ZERO)
    }

    /// Applies a movement to the projection: `on_hand += movement.delta`.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if the running total leaves `i64`.
    pub fn apply(&mut self, movement: &StockMovement) -> Result<(), MoneyError> {
        let updated = self
            .on_hand(movement.ingredient)
            .checked_add(movement.delta)?;
        self.on_hand.insert(movement.ingredient, updated);
        Ok(())
    }

    /// How many whole units of `item` the current stock can make (§8's formula).
    ///
    /// The minimum, over the item's own recipe, of `floor(on_hand / per_unit)`. A recipe with no
    /// positive-amount line is [`Availability::Unlimited`]; an ingredient at or below zero yields
    /// zero for that item. Modifiers are deliberately *not* folded in — availability is about the
    /// base item a guest can order, and modifiers are optional additions priced when chosen.
    #[must_use]
    pub fn available(&self, item: MenuItemId, book: &RecipeBook) -> Availability {
        let Some(recipe) = book.get(item) else {
            return Availability::Unlimited;
        };
        let mut limit: Option<i64> = None;
        for line in &recipe.lines {
            if line.per_unit.milli <= 0 {
                continue;
            }
            let stock = self.on_hand(line.ingredient).milli;
            let makeable = if stock <= 0 {
                0
            } else {
                stock / line.per_unit.milli
            };
            limit = Some(limit.map_or(makeable, |current| current.min(makeable)));
        }
        match limit {
            Some(count) => Availability::Limited(count),
            None => Availability::Unlimited,
        }
    }
}

/// A single change to stock, tagged with the ledger kind it records.
///
/// `delta` is signed: consumption and spoilage are negative, receipts positive. The wire ledger
/// (`StockLedgerEntryKind` in `pos-proto`) has these exact five kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StockMovement {
    /// Which of the five ledger kinds this movement is.
    pub kind: StockMovementKind,
    /// The ingredient affected.
    pub ingredient: IngredientId,
    /// The signed change to on-hand stock.
    pub delta: Quantity,
}

/// The five stock-ledger kinds, mirroring `pos_proto::enums::StockLedgerEntryKind`.
///
/// Kept as a domain enum so `pos-core` does not reach into a wire vocabulary for its own bookkeeping;
/// the adapter maps between the two when it writes an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockMovementKind {
    /// Consumed by firing a line (automatic).
    Consumption,
    /// Goods received.
    Receipt,
    /// A manual correction.
    Adjustment,
    /// Spoiled or dropped raw stock (not the same as a cancelled fired line — see the module docs).
    Waste,
    /// The adjustment a physical count produces.
    Stocktake,
}

impl StockMovement {
    /// A consumption of `quantity` (reduces on-hand). Fails only on an unrepresentable negation.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if `quantity` cannot be negated within `i64`.
    pub fn consume(ingredient: IngredientId, quantity: Quantity) -> Result<Self, MoneyError> {
        Ok(Self {
            kind: StockMovementKind::Consumption,
            ingredient,
            delta: negate(quantity)?,
        })
    }

    /// A receipt of `quantity` (increases on-hand).
    #[must_use]
    pub fn receive(ingredient: IngredientId, quantity: Quantity) -> Self {
        Self {
            kind: StockMovementKind::Receipt,
            ingredient,
            delta: quantity,
        }
    }

    /// Spoiled or dropped raw stock — reduces on-hand.
    ///
    /// This is *not* the void-after-fire case: firing already consumed the stock, and §8's default is
    /// that cancelling a fired line records waste **without returning stock**, so the orchestration
    /// records a zero-effect waste entry there rather than calling this.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Overflow`] if `quantity` cannot be negated within `i64`.
    pub fn spoil(ingredient: IngredientId, quantity: Quantity) -> Result<Self, MoneyError> {
        Ok(Self {
            kind: StockMovementKind::Waste,
            ingredient,
            delta: negate(quantity)?,
        })
    }

    /// A manual adjustment by a signed `delta`.
    #[must_use]
    pub fn adjust(ingredient: IngredientId, delta: Quantity) -> Self {
        Self {
            kind: StockMovementKind::Adjustment,
            ingredient,
            delta,
        }
    }
}

/// The movements a fired line consumes: the base item's recipe plus every chosen modifier's recipe,
/// each scaled by the line quantity.
///
/// An id with no recipe in the book contributes nothing (an untracked item or a modifier that changes
/// no ingredients). The result has one movement per (ingredient, source) pair; a caller that wants
/// them merged per ingredient can fold them, but keeping them separate preserves which recipe drove
/// each consumption for the ledger.
///
/// # Errors
///
/// [`MoneyError::Overflow`] if scaling a recipe amount by the quantity leaves `i64`.
pub fn consumption_for_fire(
    base_item: MenuItemId,
    modifiers: &[MenuItemId],
    quantity: Quantity,
    book: &RecipeBook,
    rounding: pos_proto::money::Rounding,
) -> Result<Vec<StockMovement>, MoneyError> {
    let mut movements = Vec::new();
    for id in core::iter::once(base_item).chain(modifiers.iter().copied()) {
        let Some(recipe) = book.get(id) else {
            continue;
        };
        for line in &recipe.lines {
            let consumed = line.per_unit.checked_scale(quantity, rounding)?;
            if consumed.is_zero() {
                continue;
            }
            movements.push(StockMovement::consume(line.ingredient, consumed)?);
        }
    }
    Ok(movements)
}

/// The delta a stocktake records: `counted − projected_at_count_time`.
///
/// §8 is precise that the delta is measured against the projection **at the moment of counting**, not
/// at the moment the count is entered, so sales during the count do not corrupt it. The caller
/// snapshots the projected on-hand when counting starts, passes it here with the physical count, and
/// applies the returned delta (a [`StockMovementKind::Stocktake`] movement) to the *current*
/// projection — which may have moved since, correctly, because of sales in the meantime.
///
/// # Errors
///
/// [`MoneyError::Overflow`] if the difference leaves `i64`.
pub fn stocktake_movement(
    ingredient: IngredientId,
    projected_at_count_time: Quantity,
    counted: Quantity,
) -> Result<StockMovement, MoneyError> {
    let delta = counted.checked_sub(projected_at_count_time)?;
    Ok(StockMovement {
        kind: StockMovementKind::Stocktake,
        ingredient,
        delta,
    })
}

/// Builds the runtime [`RecipeBook`] and the per-item auto-86 thresholds from the published
/// `inventory` config node ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md)).
///
/// The edge calls this on config apply to populate the recipe book its fire path consumes and the
/// thresholds its availability check reads. Ingredients and suppliers in the node are
/// reference/display data the availability arithmetic does not need, so only the recipes are folded in
/// here; an item absent from the node has no recipe and is therefore [`Availability::Unlimited`], and
/// an item present but with an empty BOM is likewise never stock-limited (§8).
///
/// The threshold map carries one entry per recipe; an item with no entry falls back to `0` at the call
/// site (86 only when nothing can be made). A duplicate item id in the node keeps the last occurrence,
/// mirroring [`RecipeBook::insert`]'s replace-on-repeat.
#[must_use]
pub fn from_published(node: &PublishedInventory) -> (RecipeBook, BTreeMap<MenuItemId, i64>) {
    let mut book = RecipeBook::new();
    let mut thresholds = BTreeMap::new();
    for recipe in node.recipes() {
        let lines = recipe
            .lines
            .iter()
            .map(|line| RecipeLine {
                ingredient: line.ingredient,
                per_unit: line.per_unit,
            })
            .collect();
        book.insert(recipe.item, Recipe::new(lines));
        thresholds.insert(recipe.item, recipe.auto_86_threshold);
    }
    (book, thresholds)
}

/// Negates a quantity, honouring the `i64` range rather than wrapping.
fn negate(quantity: Quantity) -> Result<Quantity, MoneyError> {
    quantity
        .milli
        .checked_neg()
        .map(Quantity::from_milli)
        .ok_or(MoneyError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::{
        Availability, Recipe, RecipeBook, RecipeLine, StockMovement, StockProjection,
        consumption_for_fire, stocktake_movement,
    };
    use pos_proto::{IngredientId, MenuItemId, Quantity, Rounding, Ulid};

    fn ingredient(n: u128) -> IngredientId {
        IngredientId::new(Ulid::from_u128(n))
    }

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn whole(units: i64) -> Quantity {
        Quantity::from_whole(units).expect("small whole quantity")
    }

    /// One unit of each listed ingredient.
    fn recipe_of(ingredients: &[IngredientId]) -> Recipe {
        Recipe::new(
            ingredients
                .iter()
                .map(|&i| RecipeLine {
                    ingredient: i,
                    per_unit: Quantity::ONE,
                })
                .collect(),
        )
    }

    #[test]
    fn availability_is_the_binding_ingredient() {
        // Item A needs ingredients D and E; D=8, E=6 → 6 makeable, E binds.
        let dish_a = item(1);
        let (ing_d, ing_e) = (ingredient(4), ingredient(5));
        let mut book = RecipeBook::new();
        book.insert(dish_a, recipe_of(&[ing_d, ing_e]));
        let mut stock = StockProjection::new();
        stock.set_on_hand(ing_d, whole(8));
        stock.set_on_hand(ing_e, whole(6));
        assert_eq!(stock.available(dish_a, &book), Availability::Limited(6));
    }

    #[test]
    fn a_shared_ingredient_propagates() {
        // The archive fixture: A={D,E}, B={C,D}; C=10, D=8, E=6.
        // available(B) = min(C=10, D=8) = 8. Cook one A (consumes D,E): D=7,E=5.
        // available(B) = min(10, 7) = 7 — B dropped without B being sold.
        let (dish_a, dish_b) = (item(1), item(2));
        let (ing_c, ing_d, ing_e) = (ingredient(3), ingredient(4), ingredient(5));
        let mut book = RecipeBook::new();
        book.insert(dish_a, recipe_of(&[ing_d, ing_e]));
        book.insert(dish_b, recipe_of(&[ing_c, ing_d]));
        let mut stock = StockProjection::new();
        stock.set_on_hand(ing_c, whole(10));
        stock.set_on_hand(ing_d, whole(8));
        stock.set_on_hand(ing_e, whole(6));

        assert_eq!(stock.available(dish_b, &book), Availability::Limited(8));

        // Fire one A.
        let movements = consumption_for_fire(dish_a, &[], Quantity::ONE, &book, Rounding::HalfUp)
            .expect("consumption");
        for movement in &movements {
            stock.apply(movement).expect("apply");
        }

        assert_eq!(stock.on_hand(ing_d), whole(7));
        assert_eq!(stock.on_hand(ing_e), whole(5));
        assert_eq!(
            stock.available(dish_b, &book),
            Availability::Limited(7),
            "cooking A lowered B through the shared ingredient D"
        );
    }

    #[test]
    fn a_modifier_adds_its_own_recipe_when_fired() {
        // Base pizza uses 100 g dough; the "large" modifier adds 50 g. Firing one large pizza
        // consumes 150 g.
        let (pizza, large, dough) = (item(1), item(2), ingredient(9));
        let mut book = RecipeBook::new();
        book.insert(
            pizza,
            Recipe::new(vec![RecipeLine {
                ingredient: dough,
                per_unit: Quantity::from_milli(100_000),
            }]),
        );
        book.insert(
            large,
            Recipe::new(vec![RecipeLine {
                ingredient: dough,
                per_unit: Quantity::from_milli(50_000),
            }]),
        );
        let movements =
            consumption_for_fire(pizza, &[large], Quantity::ONE, &book, Rounding::HalfUp)
                .expect("consumption");
        let total: i64 = movements.iter().map(|m| m.delta.milli).sum();
        assert_eq!(total, -150_000, "base 100 g + modifier 50 g, both consumed");
    }

    #[test]
    fn an_item_with_no_recipe_is_never_limited() {
        let drink = item(7);
        let stock = StockProjection::new();
        assert_eq!(
            stock.available(drink, &RecipeBook::new()),
            Availability::Unlimited
        );
    }

    #[test]
    fn auto_86_is_availability_at_the_threshold() {
        let (item_id, flour) = (item(1), ingredient(2));
        let mut book = RecipeBook::new();
        book.insert(item_id, recipe_of(&[flour]));
        let mut stock = StockProjection::new();
        stock.set_on_hand(flour, whole(0));
        let availability = stock.available(item_id, &book);
        assert_eq!(availability, Availability::Limited(0));
        assert!(!availability.is_sellable(0), "zero on hand → auto-86");
        assert!(Availability::Unlimited.is_sellable(0));
        assert!(Availability::Limited(1).is_sellable(0));
    }

    #[test]
    fn a_stocktake_delta_is_against_the_count_time_projection() {
        // Count starts with 100 projected. During the count, sales drop on-hand to 90. The counter
        // finds 95 physically. The discrepancy is 95 − 100 = −5 (shrinkage), applied to the *current*
        // 90 → 85, so the sales during the count are preserved and only the shrinkage is booked.
        let flour = ingredient(1);
        let mut stock = StockProjection::new();
        stock.set_on_hand(flour, whole(90)); // already moved since the count began
        let projected_at_count = whole(100);
        let counted = whole(95);
        let movement = stocktake_movement(flour, projected_at_count, counted).expect("delta");
        assert_eq!(movement.delta, Quantity::from_milli(-5_000));
        stock.apply(&movement).expect("apply");
        assert_eq!(stock.on_hand(flour), whole(85));
    }

    #[test]
    fn a_receipt_raises_and_spoilage_lowers_on_hand() {
        let flour = ingredient(1);
        let mut stock = StockProjection::new();
        stock
            .apply(&StockMovement::receive(flour, whole(10)))
            .expect("receive");
        assert_eq!(stock.on_hand(flour), whole(10));
        stock
            .apply(&StockMovement::spoil(flour, whole(3)).expect("spoil"))
            .expect("apply");
        assert_eq!(stock.on_hand(flour), whole(7));
    }

    #[test]
    fn from_published_builds_the_book_and_thresholds() {
        use pos_proto::inventory::{PublishedInventory, PublishedRecipe, PublishedRecipeLine};

        let (pizza, dough) = (item(1), ingredient(2));
        let node = PublishedInventory::from_parts(
            Vec::new(),
            vec![
                PublishedRecipe {
                    item: pizza,
                    lines: vec![PublishedRecipeLine {
                        ingredient: dough,
                        per_unit: Quantity::from_milli(100_000),
                    }],
                    auto_86_threshold: 3,
                },
                // An item present with an empty BOM is never stock-limited but still carries a threshold.
                PublishedRecipe {
                    item: item(9),
                    lines: Vec::new(),
                    auto_86_threshold: 0,
                },
            ],
            Vec::new(),
        );

        let (book, thresholds) = super::from_published(&node);

        // The pizza's recipe made it across, one dough line of 100 g.
        let recipe = book.get(pizza).expect("pizza recipe");
        assert_eq!(recipe.lines.len(), 1);
        let line = recipe.lines.first().expect("one line");
        assert_eq!(line.ingredient, dough);
        assert_eq!(line.per_unit, Quantity::from_milli(100_000));
        assert_eq!(thresholds.get(&pizza), Some(&3));

        // The empty-BOM item is Unlimited (no positive line) but keeps its threshold entry.
        let mut stock = StockProjection::new();
        stock.set_on_hand(dough, Quantity::from_milli(250_000)); // 2 whole pizzas' worth
        assert_eq!(stock.available(pizza, &book), Availability::Limited(2));
        assert_eq!(stock.available(item(9), &book), Availability::Unlimited);
        assert_eq!(thresholds.get(&item(9)), Some(&0));

        // An item absent from the node has no recipe and no threshold entry.
        assert!(book.get(item(42)).is_none());
        assert_eq!(thresholds.get(&item(42)), None);
    }

    #[test]
    fn firing_more_than_one_scales_the_recipe() {
        let (item_id, cheese) = (item(1), ingredient(2));
        let mut book = RecipeBook::new();
        book.insert(
            item_id,
            Recipe::new(vec![RecipeLine {
                ingredient: cheese,
                per_unit: Quantity::from_milli(40_000),
            }]),
        );
        let movements = consumption_for_fire(item_id, &[], whole(3), &book, Rounding::HalfUp)
            .expect("consumption");
        let total: i64 = movements.iter().map(|m| m.delta.milli).sum();
        assert_eq!(total, -120_000, "40 g × 3");
    }
}
