// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Wiring the [`InventoryStore`] seam to its `store-postgres` table
//! ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md)).
//!
//! Moved out of `pos-cloud`'s `persistence.rs` with the trait (ADR-0170 decision 4): a trait can be
//! implemented for another crate's type only in the trait's own crate. The SQL stays in
//! `store-postgres`.

use pos_cloud_config::persistence::{create_outcome, update_outcome};
use pos_proto::ids::{IngredientId, MenuItemId, SupplierId, TenantId};
use pos_proto::inventory::{PublishedIngredient, PublishedRecipe, PublishedSupplier};
use store_postgres::{InventoryRow, PostgresInventory};

use crate::inventory::{InventoryStore, InventoryStoreError};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

/// The `kind` discriminators the `inventory_items` table stores the three record kinds under.
const INVENTORY_KIND_INGREDIENT: &str = "ingredient";
const INVENTORY_KIND_RECIPE: &str = "recipe";
const INVENTORY_KIND_SUPPLIER: &str = "supplier";

/// Decodes one stored inventory document (a wire record), failing loudly on JSON this cloud wrote that
/// no longer parses — that is store corruption, not an absence.
fn decode_inventory<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, InventoryStoreError> {
    serde_json::from_str(json).map_err(|error| {
        InventoryStoreError::new(format!("a stored inventory record is not valid: {error}"))
    })
}

/// One stored inventory row as the seam returns it: the decoded record paired with the version the
/// read saw, which is the token the matching `update_*` demands back (ADR-0095). Generic over the
/// three record kinds because the row shape is one table's.
fn versioned_inventory<T: serde::de::DeserializeOwned>(
    row: &InventoryRow,
) -> Result<Versioned<T>, InventoryStoreError> {
    Ok(Versioned::new(
        decode_inventory(&row.doc_json)?,
        Version::new(row.version.clone()),
    ))
}

impl InventoryStore for PostgresInventory {
    async fn list_ingredients(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedIngredient>>, InventoryStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string(), INVENTORY_KIND_INGREDIENT)
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_inventory).collect()
    }

    async fn get_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> Result<Option<Versioned<PublishedIngredient>>, InventoryStoreError> {
        let row = self
            .fetch_one(
                &tenant_id.to_string(),
                INVENTORY_KIND_INGREDIENT,
                &ingredient_id.to_string(),
            )
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_inventory).transpose()
    }

    async fn create_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient: &PublishedIngredient,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(ingredient).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize an ingredient: {error}"))
        })?;
        self.insert(
            &tenant_id.to_string(),
            INVENTORY_KIND_INGREDIENT,
            &ingredient.id.to_string(),
            &json,
        )
        .await
        .map(create_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn update_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient: &PublishedIngredient,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(ingredient).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize an ingredient: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            INVENTORY_KIND_INGREDIENT,
            &ingredient.id.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn delete_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> Result<(), InventoryStoreError> {
        self.delete(
            &tenant_id.to_string(),
            INVENTORY_KIND_INGREDIENT,
            &ingredient_id.to_string(),
        )
        .await
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn list_recipes(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedRecipe>>, InventoryStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string(), INVENTORY_KIND_RECIPE)
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_inventory).collect()
    }

    async fn get_recipe(
        &self,
        tenant_id: TenantId,
        item: MenuItemId,
    ) -> Result<Option<Versioned<PublishedRecipe>>, InventoryStoreError> {
        let row = self
            .fetch_one(
                &tenant_id.to_string(),
                INVENTORY_KIND_RECIPE,
                &item.to_string(),
            )
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_inventory).transpose()
    }

    async fn create_recipe(
        &self,
        tenant_id: TenantId,
        recipe: &PublishedRecipe,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(recipe).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a recipe: {error}"))
        })?;
        self.insert(
            &tenant_id.to_string(),
            INVENTORY_KIND_RECIPE,
            &recipe.item.to_string(),
            &json,
        )
        .await
        .map(create_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn update_recipe(
        &self,
        tenant_id: TenantId,
        recipe: &PublishedRecipe,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(recipe).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a recipe: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            INVENTORY_KIND_RECIPE,
            &recipe.item.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn delete_recipe(
        &self,
        tenant_id: TenantId,
        item: MenuItemId,
    ) -> Result<(), InventoryStoreError> {
        self.delete(
            &tenant_id.to_string(),
            INVENTORY_KIND_RECIPE,
            &item.to_string(),
        )
        .await
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn list_suppliers(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedSupplier>>, InventoryStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string(), INVENTORY_KIND_SUPPLIER)
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_inventory).collect()
    }

    async fn get_supplier(
        &self,
        tenant_id: TenantId,
        supplier_id: SupplierId,
    ) -> Result<Option<Versioned<PublishedSupplier>>, InventoryStoreError> {
        let row = self
            .fetch_one(
                &tenant_id.to_string(),
                INVENTORY_KIND_SUPPLIER,
                &supplier_id.to_string(),
            )
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_inventory).transpose()
    }

    async fn create_supplier(
        &self,
        tenant_id: TenantId,
        supplier: &PublishedSupplier,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(supplier).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a supplier: {error}"))
        })?;
        self.insert(
            &tenant_id.to_string(),
            INVENTORY_KIND_SUPPLIER,
            &supplier.id.to_string(),
            &json,
        )
        .await
        .map(create_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn update_supplier(
        &self,
        tenant_id: TenantId,
        supplier: &PublishedSupplier,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(supplier).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a supplier: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            INVENTORY_KIND_SUPPLIER,
            &supplier.id.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn delete_supplier(
        &self,
        tenant_id: TenantId,
        supplier_id: SupplierId,
    ) -> Result<(), InventoryStoreError> {
        self.delete(
            &tenant_id.to_string(),
            INVENTORY_KIND_SUPPLIER,
            &supplier_id.to_string(),
        )
        .await
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }
}
