// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The inventory authoring seam (Track M6, [ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md)).
//!
//! Where a tenant's ingredients, per-item/modifier recipes (with their auto-86 thresholds), and
//! supplier references live between edits. Inventory is authored per **tenant** — a menu item's recipe
//! is the same across a brand's stores — and a publish (a later slice) assembles the three lists with
//! [`to_node`] into the `inventory` config node a store applies to build its `RecipeBook` and
//! thresholds (§8).
//!
//! The authored records *are* the wire types ([`PublishedIngredient`], [`PublishedRecipe`],
//! [`PublishedSupplier`]) — the fields an operator sets are exactly the fields the edge reads — so
//! there is no separate cloud-domain shape to keep in sync, exactly as the campaign seam works. Each
//! record is its own row keyed by `(tenant, kind, id)`; CRUD is per-record, not the wholesale replace
//! tax rates use, because a tenant edits one ingredient or one recipe at a time. The `store-postgres`
//! impl holds each as `jsonb`, tenant-scoped and RLS-isolated like the rest of the config data.

use core::future::Future;

use pos_proto::ids::{IngredientId, MenuItemId, SupplierId, TenantId};
use pos_proto::inventory::{
    PublishedIngredient, PublishedInventory, PublishedRecipe, PublishedSupplier,
};

/// Persists and reads a tenant's authored inventory — ingredients, recipes, and supplier references.
///
/// Every method is tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every other
/// cloud table. Each `upsert_*` creates or replaces one record by its id; each `delete_*` removes one
/// and removing an absent record is not an error.
pub trait InventoryStore {
    /// Every ingredient a tenant has authored, id order (a ULID, so creation order — stable for a diff).
    fn list_ingredients(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<PublishedIngredient>, InventoryStoreError>> + Send;

    /// Creates an ingredient, or replaces the one that already has its id.
    fn upsert_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient: &PublishedIngredient,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;

    /// Removes an ingredient by id. Removing one that does not exist is not an error.
    fn delete_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;

    /// Every recipe a tenant has authored, keyed by the item or modifier it makes.
    fn list_recipes(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<PublishedRecipe>, InventoryStoreError>> + Send;

    /// Creates a recipe, or replaces the one for the same item/modifier (its BOM lines and threshold).
    fn upsert_recipe(
        &self,
        tenant_id: TenantId,
        recipe: &PublishedRecipe,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;

    /// Removes a recipe by the item it makes. Removing one that does not exist is not an error.
    fn delete_recipe(
        &self,
        tenant_id: TenantId,
        item: MenuItemId,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;

    /// Every supplier a tenant has authored, id order.
    fn list_suppliers(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<PublishedSupplier>, InventoryStoreError>> + Send;

    /// Creates a supplier, or replaces the one that already has its id.
    fn upsert_supplier(
        &self,
        tenant_id: TenantId,
        supplier: &PublishedSupplier,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;

    /// Removes a supplier by id. Removing one that does not exist is not an error.
    fn delete_supplier(
        &self,
        tenant_id: TenantId,
        supplier_id: SupplierId,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;
}

/// Assembles a tenant's authored ingredients, recipes, and suppliers into the wire
/// [`PublishedInventory`] node a publish writes — the same "assemble authored rows into the node" shape
/// every other node uses (e.g. `campaigns::to_node`).
#[must_use]
pub fn to_node(
    ingredients: Vec<PublishedIngredient>,
    recipes: Vec<PublishedRecipe>,
    suppliers: Vec<PublishedSupplier>,
) -> PublishedInventory {
    PublishedInventory::from_parts(ingredients, recipes, suppliers)
}

/// A failure of the inventory store itself — the database is unreachable, or a stored value could not
/// be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the inventory store failed: {0}")]
pub struct InventoryStoreError(String);

impl InventoryStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::enums::UnitOfMeasure;
    use pos_proto::ids::{IngredientId, MenuItemId, SupplierId, TenantId};
    use pos_proto::inventory::{
        PublishedIngredient, PublishedRecipe, PublishedRecipeLine, PublishedSupplier,
    };
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{InventoryStore, InventoryStoreError, to_node};

    /// An in-memory `InventoryStore` for the seam's tests, tenant-scoped exactly like the real thing so
    /// a test can prove isolation.
    #[derive(Default)]
    struct FakeInventory {
        ingredients: Mutex<Vec<(TenantId, PublishedIngredient)>>,
        recipes: Mutex<Vec<(TenantId, PublishedRecipe)>>,
        suppliers: Mutex<Vec<(TenantId, PublishedSupplier)>>,
    }

    impl InventoryStore for FakeInventory {
        async fn list_ingredients(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<PublishedIngredient>, InventoryStoreError> {
            Ok(self
                .ingredients
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _)| *owner == tenant_id)
                .map(|(_, item)| item.clone())
                .collect())
        }

        async fn upsert_ingredient(
            &self,
            tenant_id: TenantId,
            ingredient: &PublishedIngredient,
        ) -> Result<(), InventoryStoreError> {
            let mut rows = self.ingredients.lock().expect("lock");
            rows.retain(|(owner, existing)| !(*owner == tenant_id && existing.id == ingredient.id));
            rows.push((tenant_id, ingredient.clone()));
            Ok(())
        }

        async fn delete_ingredient(
            &self,
            tenant_id: TenantId,
            ingredient_id: IngredientId,
        ) -> Result<(), InventoryStoreError> {
            self.ingredients
                .lock()
                .expect("lock")
                .retain(|(owner, item)| !(*owner == tenant_id && item.id == ingredient_id));
            Ok(())
        }

        async fn list_recipes(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<PublishedRecipe>, InventoryStoreError> {
            Ok(self
                .recipes
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _)| *owner == tenant_id)
                .map(|(_, item)| item.clone())
                .collect())
        }

        async fn upsert_recipe(
            &self,
            tenant_id: TenantId,
            recipe: &PublishedRecipe,
        ) -> Result<(), InventoryStoreError> {
            let mut rows = self.recipes.lock().expect("lock");
            rows.retain(|(owner, existing)| !(*owner == tenant_id && existing.item == recipe.item));
            rows.push((tenant_id, recipe.clone()));
            Ok(())
        }

        async fn delete_recipe(
            &self,
            tenant_id: TenantId,
            item: MenuItemId,
        ) -> Result<(), InventoryStoreError> {
            self.recipes
                .lock()
                .expect("lock")
                .retain(|(owner, recipe)| !(*owner == tenant_id && recipe.item == item));
            Ok(())
        }

        async fn list_suppliers(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<PublishedSupplier>, InventoryStoreError> {
            Ok(self
                .suppliers
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _)| *owner == tenant_id)
                .map(|(_, item)| item.clone())
                .collect())
        }

        async fn upsert_supplier(
            &self,
            tenant_id: TenantId,
            supplier: &PublishedSupplier,
        ) -> Result<(), InventoryStoreError> {
            let mut rows = self.suppliers.lock().expect("lock");
            rows.retain(|(owner, existing)| !(*owner == tenant_id && existing.id == supplier.id));
            rows.push((tenant_id, supplier.clone()));
            Ok(())
        }

        async fn delete_supplier(
            &self,
            tenant_id: TenantId,
            supplier_id: SupplierId,
        ) -> Result<(), InventoryStoreError> {
            self.suppliers
                .lock()
                .expect("lock")
                .retain(|(owner, item)| !(*owner == tenant_id && item.id == supplier_id));
            Ok(())
        }
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn other_tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(2))
    }

    fn ingredient(n: u128, name: &str) -> PublishedIngredient {
        PublishedIngredient {
            id: IngredientId::new(Ulid::from_u128(n)),
            name: DisplayName::new(name),
            unit: Open::from_known(UnitOfMeasure::Gram),
        }
    }

    fn recipe(item: u128, ingredient_id: u128, threshold: i64) -> PublishedRecipe {
        PublishedRecipe {
            item: MenuItemId::new(Ulid::from_u128(item)),
            lines: vec![PublishedRecipeLine {
                ingredient: IngredientId::new(Ulid::from_u128(ingredient_id)),
                per_unit: Quantity::from_milli(100_000),
            }],
            auto_86_threshold: threshold,
        }
    }

    #[tokio::test]
    async fn ingredient_crud_stays_tenant_scoped() {
        let store = FakeInventory::default();
        store
            .upsert_ingredient(other_tenant(), &ingredient(99, "Neighbour flour"))
            .await
            .expect("neighbour");
        store
            .upsert_ingredient(tenant(), &ingredient(10, "Dough"))
            .await
            .expect("create");
        store
            .upsert_ingredient(tenant(), &ingredient(10, "Dough (renamed)"))
            .await
            .expect("update");
        let listed = store.list_ingredients(tenant()).await.expect("list");
        assert_eq!(listed.len(), 1, "upsert replaces by id, not appends");
        assert_eq!(
            listed.first().expect("one").name,
            DisplayName::new("Dough (renamed)")
        );
        store
            .delete_ingredient(tenant(), IngredientId::new(Ulid::from_u128(10)))
            .await
            .expect("delete");
        assert!(
            store
                .list_ingredients(tenant())
                .await
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            store
                .list_ingredients(other_tenant())
                .await
                .expect("list")
                .len(),
            1,
            "the neighbour is untouched throughout"
        );
    }

    #[tokio::test]
    async fn recipe_upsert_is_keyed_by_item_and_to_node_assembles_all_three() {
        let store = FakeInventory::default();
        store
            .upsert_recipe(tenant(), &recipe(1, 10, 2))
            .await
            .expect("create");
        // Upsert of the same item replaces its BOM/threshold rather than adding a row.
        store
            .upsert_recipe(tenant(), &recipe(1, 10, 5))
            .await
            .expect("update");
        let recipes = store.list_recipes(tenant()).await.expect("list");
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes.first().expect("one").auto_86_threshold, 5);

        store
            .upsert_supplier(
                tenant(),
                &PublishedSupplier {
                    id: SupplierId::new(Ulid::from_u128(7)),
                    name: DisplayName::new("Anchor"),
                },
            )
            .await
            .expect("supplier");

        let node = to_node(
            store.list_ingredients(tenant()).await.expect("ing"),
            recipes,
            store.list_suppliers(tenant()).await.expect("sup"),
        );
        assert_eq!(node.recipes().len(), 1);
        assert_eq!(node.suppliers().len(), 1);
    }
}
