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

use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
use pos_proto::inventory::{
    PublishedIngredient, PublishedInventory, PublishedRecipe, PublishedSupplier,
};

/// Persists and reads a tenant's authored inventory — ingredients, recipes, and supplier references.
///
/// Every method is tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every other
/// cloud table. Each `create_*` inserts one record and refuses a taken id; each `update_*` replaces one
/// only at the version the caller read it at
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)); each `delete_*` removes
/// one, and removing an absent record is not an error.
pub trait InventoryStore {
    /// Every ingredient a tenant has authored, id order (a ULID, so creation order — stable for a diff).
    ///
    /// Each row carries the version it was read at: the console edits an ingredient from this list,
    /// and that token is what [`update_ingredient`](Self::update_ingredient) demands back.
    fn list_ingredients(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<PublishedIngredient>>, InventoryStoreError>> + Send;

    /// Inserts an ingredient, refusing if one already holds its id.
    fn create_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient: &PublishedIngredient,
    ) -> impl Future<Output = Result<CreateOutcome, InventoryStoreError>> + Send;

    /// Replaces an ingredient, only at the version the caller read it at.
    fn update_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient: &PublishedIngredient,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, InventoryStoreError>> + Send;

    /// Removes an ingredient by id. Removing one that does not exist is not an error.
    fn delete_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;

    /// Every recipe a tenant has authored, keyed by the item or modifier it makes, each with the
    /// version it was read at.
    fn list_recipes(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<PublishedRecipe>>, InventoryStoreError>> + Send;

    /// Inserts a recipe, refusing if the item or modifier it makes already has one.
    ///
    /// This is the half that was losing data: a recipe is keyed by the item it makes, and the item
    /// id comes from the caller, so "add a recipe" for an item that already had one silently
    /// replaced its bill of materials.
    fn create_recipe(
        &self,
        tenant_id: TenantId,
        recipe: &PublishedRecipe,
    ) -> impl Future<Output = Result<CreateOutcome, InventoryStoreError>> + Send;

    /// Replaces a recipe's bill of materials and threshold, only at the version the caller read it at.
    fn update_recipe(
        &self,
        tenant_id: TenantId,
        recipe: &PublishedRecipe,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, InventoryStoreError>> + Send;

    /// Removes a recipe by the item it makes. Removing one that does not exist is not an error.
    fn delete_recipe(
        &self,
        tenant_id: TenantId,
        item: MenuItemId,
    ) -> impl Future<Output = Result<(), InventoryStoreError>> + Send;

    /// Every supplier a tenant has authored, id order, each with the version it was read at.
    fn list_suppliers(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<PublishedSupplier>>, InventoryStoreError>> + Send;

    /// Inserts a supplier, refusing if one already holds its id.
    fn create_supplier(
        &self,
        tenant_id: TenantId,
        supplier: &PublishedSupplier,
    ) -> impl Future<Output = Result<CreateOutcome, InventoryStoreError>> + Send;

    /// Replaces a supplier, only at the version the caller read it at.
    fn update_supplier(
        &self,
        tenant_id: TenantId,
        supplier: &PublishedSupplier,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, InventoryStoreError>> + Send;

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
    use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned, records};

    /// An in-memory `InventoryStore` for the seam's tests, tenant-scoped exactly like the real thing so
    /// a test can prove isolation.
    #[derive(Default)]
    struct FakeInventory {
        ingredients: Mutex<Vec<(TenantId, PublishedIngredient, Version)>>,
        recipes: Mutex<Vec<(TenantId, PublishedRecipe, Version)>>,
        suppliers: Mutex<Vec<(TenantId, PublishedSupplier, Version)>>,
        next_version: Mutex<u64>,
    }

    impl FakeInventory {
        /// The fake's stand-in for `xmin` (ADR-0094): a token that changes on every successful
        /// write, which is the only property the seam contract needs.
        fn mint(&self) -> Version {
            let mut next = self.next_version.lock().expect("lock");
            *next += 1;
            Version::new(next.to_string())
        }
    }

    impl InventoryStore for FakeInventory {
        async fn list_ingredients(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<PublishedIngredient>>, InventoryStoreError> {
            Ok(self
                .ingredients
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _row, _at)| *owner == tenant_id)
                .map(|(_owner, item, at)| Versioned::new(item.clone(), at.clone()))
                .collect())
        }

        async fn create_ingredient(
            &self,
            tenant_id: TenantId,
            ingredient: &PublishedIngredient,
        ) -> Result<CreateOutcome, InventoryStoreError> {
            let mut rows = self.ingredients.lock().expect("lock");
            if rows
                .iter()
                .any(|(owner, existing, _at)| *owner == tenant_id && existing.id == ingredient.id)
            {
                return Ok(CreateOutcome::AlreadyExists);
            }
            let version = self.mint();
            rows.push((tenant_id, ingredient.clone(), version.clone()));
            Ok(CreateOutcome::Created(version))
        }

        async fn update_ingredient(
            &self,
            tenant_id: TenantId,
            ingredient: &PublishedIngredient,
            expected: &Version,
        ) -> Result<UpdateOutcome, InventoryStoreError> {
            let version = self.mint();
            let mut rows = self.ingredients.lock().expect("lock");
            let Some(row) = rows
                .iter_mut()
                .find(|(owner, existing, _at)| *owner == tenant_id && existing.id == ingredient.id)
            else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.2 != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.1 = ingredient.clone();
            row.2 = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn delete_ingredient(
            &self,
            tenant_id: TenantId,
            ingredient_id: IngredientId,
        ) -> Result<(), InventoryStoreError> {
            self.ingredients
                .lock()
                .expect("lock")
                .retain(|(owner, item, _at)| !(*owner == tenant_id && item.id == ingredient_id));
            Ok(())
        }

        async fn list_recipes(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<PublishedRecipe>>, InventoryStoreError> {
            Ok(self
                .recipes
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _row, _at)| *owner == tenant_id)
                .map(|(_owner, item, at)| Versioned::new(item.clone(), at.clone()))
                .collect())
        }

        async fn create_recipe(
            &self,
            tenant_id: TenantId,
            recipe: &PublishedRecipe,
        ) -> Result<CreateOutcome, InventoryStoreError> {
            let mut rows = self.recipes.lock().expect("lock");
            if rows
                .iter()
                .any(|(owner, existing, _at)| *owner == tenant_id && existing.item == recipe.item)
            {
                return Ok(CreateOutcome::AlreadyExists);
            }
            let version = self.mint();
            rows.push((tenant_id, recipe.clone(), version.clone()));
            Ok(CreateOutcome::Created(version))
        }

        async fn update_recipe(
            &self,
            tenant_id: TenantId,
            recipe: &PublishedRecipe,
            expected: &Version,
        ) -> Result<UpdateOutcome, InventoryStoreError> {
            let version = self.mint();
            let mut rows = self.recipes.lock().expect("lock");
            let Some(row) = rows
                .iter_mut()
                .find(|(owner, existing, _at)| *owner == tenant_id && existing.item == recipe.item)
            else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.2 != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.1 = recipe.clone();
            row.2 = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn delete_recipe(
            &self,
            tenant_id: TenantId,
            item: MenuItemId,
        ) -> Result<(), InventoryStoreError> {
            self.recipes
                .lock()
                .expect("lock")
                .retain(|(owner, recipe, _at)| !(*owner == tenant_id && recipe.item == item));
            Ok(())
        }

        async fn list_suppliers(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<PublishedSupplier>>, InventoryStoreError> {
            Ok(self
                .suppliers
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _row, _at)| *owner == tenant_id)
                .map(|(_owner, item, at)| Versioned::new(item.clone(), at.clone()))
                .collect())
        }

        async fn create_supplier(
            &self,
            tenant_id: TenantId,
            supplier: &PublishedSupplier,
        ) -> Result<CreateOutcome, InventoryStoreError> {
            let mut rows = self.suppliers.lock().expect("lock");
            if rows
                .iter()
                .any(|(owner, existing, _at)| *owner == tenant_id && existing.id == supplier.id)
            {
                return Ok(CreateOutcome::AlreadyExists);
            }
            let version = self.mint();
            rows.push((tenant_id, supplier.clone(), version.clone()));
            Ok(CreateOutcome::Created(version))
        }

        async fn update_supplier(
            &self,
            tenant_id: TenantId,
            supplier: &PublishedSupplier,
            expected: &Version,
        ) -> Result<UpdateOutcome, InventoryStoreError> {
            let version = self.mint();
            let mut rows = self.suppliers.lock().expect("lock");
            let Some(row) = rows
                .iter_mut()
                .find(|(owner, existing, _at)| *owner == tenant_id && existing.id == supplier.id)
            else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.2 != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.1 = supplier.clone();
            row.2 = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn delete_supplier(
            &self,
            tenant_id: TenantId,
            supplier_id: SupplierId,
        ) -> Result<(), InventoryStoreError> {
            self.suppliers
                .lock()
                .expect("lock")
                .retain(|(owner, item, _at)| !(*owner == tenant_id && item.id == supplier_id));
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
    async fn ingredient_create_refuses_a_taken_id_and_update_needs_its_version() {
        let store = FakeInventory::default();
        store
            .create_ingredient(other_tenant(), &ingredient(99, "Neighbour flour"))
            .await
            .expect("neighbour");
        let first = match store
            .create_ingredient(tenant(), &ingredient(10, "Dough"))
            .await
            .expect("create")
        {
            CreateOutcome::Created(version) => version,
            CreateOutcome::AlreadyExists => panic!("the id was free"),
        };
        // A second create at a taken id is refused and changes nothing. This was an upsert before
        // ADR-0095 split the seam, so the rename below used to arrive through the create path.
        assert_eq!(
            store
                .create_ingredient(tenant(), &ingredient(10, "Dough (renamed)"))
                .await
                .expect("the comparison must not raise"),
            CreateOutcome::AlreadyExists
        );
        let listed = store.list_ingredients(tenant()).await.expect("list");
        assert_eq!(listed.len(), 1, "a refused create adds no row");
        let row = listed.first().expect("one");
        assert_eq!(
            row.record.name,
            DisplayName::new("Dough"),
            "and leaves the row it refused to overwrite alone"
        );
        // The list is where the console edits from, so it is where the token `update_ingredient`
        // demands has to come from. Without a version per row the update is unreachable after a
        // page reload.
        assert_eq!(
            row.etag, first,
            "a list row carries the version it was read at"
        );

        // The rename goes through update, at the version the create minted; the used version is
        // then stale.
        assert!(matches!(
            store
                .update_ingredient(tenant(), &ingredient(10, "Dough (renamed)"), &first)
                .await
                .expect("the update"),
            UpdateOutcome::Updated(_)
        ));
        assert_eq!(
            store
                .update_ingredient(tenant(), &ingredient(10, "Dough (again)"), &first)
                .await
                .expect("the comparison must not raise"),
            UpdateOutcome::VersionMismatch
        );
        let listed = store.list_ingredients(tenant()).await.expect("list");
        assert_eq!(listed.len(), 1, "an update adds no row");
        let row = listed.first().expect("one");
        assert_eq!(row.record.name, DisplayName::new("Dough (renamed)"));
        assert_ne!(row.etag, first, "and the version moves with the write");
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
    async fn a_recipe_create_refuses_an_item_that_already_has_one_and_to_node_assembles_all_three()
    {
        let store = FakeInventory::default();
        let first = match store
            .create_recipe(tenant(), &recipe(1, 10, 2))
            .await
            .expect("create")
        {
            CreateOutcome::Created(version) => version,
            CreateOutcome::AlreadyExists => panic!("the item had no recipe"),
        };
        // A recipe is keyed by the item it makes, and that key comes from the caller — so this is
        // the case that used to lose data: a second create replaced the bill of materials of the
        // recipe already there. It is now refused, and the stored recipe is left alone.
        assert_eq!(
            store
                .create_recipe(tenant(), &recipe(1, 10, 5))
                .await
                .expect("the comparison must not raise"),
            CreateOutcome::AlreadyExists
        );
        let recipes = store.list_recipes(tenant()).await.expect("list");
        assert_eq!(recipes.len(), 1, "a refused create adds no row");
        assert_eq!(
            recipes.first().expect("one").record.auto_86_threshold,
            2,
            "and does not touch the threshold it refused to overwrite"
        );

        // Changing the threshold goes through update, at the version the create minted.
        assert!(matches!(
            store
                .update_recipe(tenant(), &recipe(1, 10, 5), &first)
                .await
                .expect("the update"),
            UpdateOutcome::Updated(_)
        ));
        let recipes = store.list_recipes(tenant()).await.expect("list");
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes.first().expect("one").record.auto_86_threshold, 5);

        store
            .create_supplier(
                tenant(),
                &PublishedSupplier {
                    id: SupplierId::new(Ulid::from_u128(7)),
                    name: DisplayName::new("Anchor"),
                },
            )
            .await
            .expect("supplier");

        // A publish assembles the node from the records; the versions the read carried are a
        // writer's concern and `records` is where they stop.
        let node = to_node(
            records(store.list_ingredients(tenant()).await.expect("ing")),
            records(recipes),
            records(store.list_suppliers(tenant()).await.expect("sup")),
        );
        assert_eq!(node.recipes().len(), 1);
        assert_eq!(node.suppliers().len(), 1);
    }
}
