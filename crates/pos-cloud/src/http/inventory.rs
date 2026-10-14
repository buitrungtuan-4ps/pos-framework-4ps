// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Inventory authoring: the ingredients, recipes and suppliers a tenant authors under
//! `/admin/inventory/*`, and their publish to a store as its `inventory` node
//! ([ADR-0079](../../../../docs/adr/0079-inventory-and-suppliers.md)).
//!
//! The state, the request bodies and their checks, and the router are here; each list's
//! handlers are in a module of their own, and the publish in [`publish`].

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::get;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::enums::UnitOfMeasure;
use pos_proto::ids::{IngredientId, MenuItemId, SupplierId};
use pos_proto::inventory::{
    PublishedIngredient, PublishedRecipe, PublishedRecipeLine, PublishedSupplier,
};
use pos_proto::text::DisplayName;
use pos_proto::wire_enum::Open;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::http::FieldRefusal;
use crate::inventory::{InventoryStore, InventoryStoreError};

mod ingredients;
mod publish;
mod recipes;
mod suppliers;

use ingredients::{
    admin_create_ingredient, admin_delete_ingredient, admin_get_ingredient, admin_list_ingredients,
    admin_update_ingredient,
};
pub use publish::config_inventory_router;
pub(super) use publish::inventory_nodes;
use recipes::{
    admin_create_recipe, admin_delete_recipe, admin_get_recipe, admin_list_recipes,
    admin_update_recipe,
};
use suppliers::{
    admin_create_supplier, admin_delete_supplier, admin_get_supplier, admin_list_suppliers,
    admin_update_supplier,
};

// --- Inventory authoring (`/admin/inventory/*`, ADR-0079, Track M6) ----------------------------

/// The state the inventory routes share: the inventory store they read and write, and the
/// admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct InventoryState<Inv, A, C> {
    inventory: Inv,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A create/update body for an ingredient. The id is **server-owned** — minted on create, the path id
/// on update — so a client never supplies or forges it. `name` and `unit` are the wire ingredient's own
/// fields, reused rather than re-declared.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct IngredientRequest {
    tenant_id: String,
    name: String,
    unit: Open<UnitOfMeasure>,
}

/// A `POST /admin/inventory/recipes` body: a recipe plus the item it makes.
///
/// The item id is in the body here because the collection URI has no room for it, and the `PUT`
/// keeps taking it from the path. A struct of its own rather than an `Option` on `RecipeRequest`,
/// so neither route carries a field the other requires — and flat rather than
/// `#[serde(flatten)]` for the same reason as [`CreateLayoutButtonRequest`].
#[derive(Debug, Clone, Deserialize)]
struct CreateRecipeRequest {
    item_id: String,
    tenant_id: String,
    #[serde(default)]
    lines: Vec<PublishedRecipeLine>,
    #[serde(default)]
    auto_86_threshold: i64,
}

impl CreateRecipeRequest {
    /// The recipe half, as the shared handler body takes it.
    fn into_write(self) -> RecipeRequest {
        RecipeRequest {
            tenant_id: self.tenant_id,
            lines: self.lines,
            auto_86_threshold: self.auto_86_threshold,
        }
    }
}

/// The body of a recipe write. The item it makes is the URL key on the `PUT` — a recipe references
/// an existing menu item or modifier, so its id is client-owned, unlike an ingredient's
/// server-minted id. `lines` (the bill of materials) and `auto_86_threshold` are the wire recipe's
/// own fields.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct RecipeRequest {
    tenant_id: String,
    #[serde(default)]
    lines: Vec<PublishedRecipeLine>,
    #[serde(default)]
    auto_86_threshold: i64,
}

/// A create/update body for a supplier. The id is **server-owned**, exactly like an ingredient's.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SupplierRequest {
    tenant_id: String,
    name: String,
}

/// A compact audit summary of an ingredient — reference data (id, name, unit), never a T1 field.
#[derive(Debug, Clone, serde::Serialize)]
struct IngredientAuditSummary {
    ingredient_id: String,
    name: String,
    unit: String,
}

impl IngredientAuditSummary {
    fn of(ingredient: &PublishedIngredient) -> Self {
        Self {
            ingredient_id: ingredient.id.to_string(),
            name: ingredient.name.as_str().to_owned(),
            unit: ingredient.unit.as_wire().to_owned(),
        }
    }
}

/// A compact audit summary of a recipe: the item, how many BOM lines it has, and its threshold — enough
/// to see *that* a recipe changed, without reproducing the per-ingredient amounts (the recipe itself is
/// proprietary process, T2) in the queryable audit trail. The BOM lives in the inventory store; the
/// trail records that it moved.
#[derive(Debug, Clone, serde::Serialize)]
struct RecipeAuditSummary {
    item: String,
    line_count: usize,
    auto_86_threshold: i64,
}

impl RecipeAuditSummary {
    fn of(recipe: &PublishedRecipe) -> Self {
        Self {
            item: recipe.item.to_string(),
            line_count: recipe.lines.len(),
            auto_86_threshold: recipe.auto_86_threshold,
        }
    }
}

/// A compact audit summary of a supplier — reference data (id, name).
#[derive(Debug, Clone, serde::Serialize)]
struct SupplierAuditSummary {
    supplier_id: String,
    name: String,
}

impl SupplierAuditSummary {
    fn of(supplier: &PublishedSupplier) -> Self {
        Self {
            supplier_id: supplier.id.to_string(),
            name: supplier.name.as_str().to_owned(),
        }
    }
}

/// Builds the inventory sub-router ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md), M6).
///
/// Per-record CRUD over a tenant's ingredients, recipes, and supplier references. `GET` (list and
/// by-id) is behind [`ConsolePermission::Read`]; the writes are behind
/// [`ConsolePermission::ManageInventory`] and audited by summary — the ingredient/supplier reference
/// data in full, a recipe by its item, line count, and threshold only (never the BOM amounts, which are
/// proprietary process). An ingredient's and a supplier's id is server-minted; a recipe's key is the
/// menu item it makes, so a recipe is a `PUT` upsert keyed by the path id. Publishing the composed
/// `inventory` node to a store is a separate route that reuses `PublishConfig` (a later slice).
pub fn inventory_router<Inv, A, C>(
    inventory: Inv,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/inventory/ingredients",
            get(admin_list_ingredients::<Inv, A, C>).post(admin_create_ingredient::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/ingredients/{ingredient_id}",
            get(admin_get_ingredient::<Inv, A, C>)
                .put(admin_update_ingredient::<Inv, A, C>)
                .delete(admin_delete_ingredient::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/recipes",
            get(admin_list_recipes::<Inv, A, C>).post(admin_create_recipe::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/recipes/{item_id}",
            get(admin_get_recipe::<Inv, A, C>)
                .put(admin_update_recipe::<Inv, A, C>)
                .delete(admin_delete_recipe::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/suppliers",
            get(admin_list_suppliers::<Inv, A, C>).post(admin_create_supplier::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/suppliers/{supplier_id}",
            get(admin_get_supplier::<Inv, A, C>)
                .put(admin_update_supplier::<Inv, A, C>)
                .delete(admin_delete_supplier::<Inv, A, C>),
        )
        .with_state(InventoryState {
            inventory,
            admin,
            clock,
            audit,
        })
}

/// Validates an ingredient request and builds the ingredient with the given (server-owned) id. Returns
/// the fault message for a `400` rather than the whole response, so the error type stays small.
pub(super) fn build_ingredient(
    request: &IngredientRequest,
    id: IngredientId,
) -> Result<PublishedIngredient, FieldRefusal> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(FieldRefusal::new("name is required", "name", "REQUIRED"));
    }
    if request.unit.is_unspecified() || request.unit.is_unrecognised() {
        // `INVALID_ENUM_VALUE`, not `REQUIRED`: `unit` carries no `#[serde(default)]`, so an absent
        // key never reaches here — what does is a present token that is unrecognised or explicitly
        // `UNSPECIFIED`, and both are enum-value faults.
        return Err(FieldRefusal::new(
            "an ingredient names an unknown unit of measure",
            "unit",
            "INVALID_ENUM_VALUE",
        ));
    }
    Ok(PublishedIngredient {
        id,
        name: DisplayName::new(name),
        unit: request.unit.clone(),
    })
}

/// Validates a recipe request and builds the recipe for the given (client-owned, path) item.
pub(super) fn build_recipe(
    request: &RecipeRequest,
    item: MenuItemId,
) -> Result<PublishedRecipe, FieldRefusal> {
    if request.auto_86_threshold < 0 {
        return Err(FieldRefusal::new(
            "an auto-86 threshold cannot be negative",
            "auto_86_threshold",
            "OUT_OF_RANGE",
        ));
    }
    if request
        .lines
        .iter()
        .any(|line| line.per_unit.as_milli() <= 0)
    {
        return Err(FieldRefusal::new(
            "a recipe line must consume a positive amount",
            "lines",
            "OUT_OF_RANGE",
        ));
    }
    Ok(PublishedRecipe {
        item,
        lines: request.lines.clone(),
        auto_86_threshold: request.auto_86_threshold,
    })
}

/// Validates a supplier request and builds the supplier with the given (server-owned) id.
pub(super) fn build_supplier(
    request: &SupplierRequest,
    id: SupplierId,
) -> Result<PublishedSupplier, FieldRefusal> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(FieldRefusal::new("name is required", "name", "REQUIRED"));
    }
    Ok(PublishedSupplier {
        id,
        name: DisplayName::new(name),
    })
}

/// The `503` for when the OS entropy needed to mint an inventory id is unavailable.
fn inventory_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint an inventory id");
    api_error(
        ErrorStatus::Unavailable,
        "the inventory service is unavailable",
    )
}

/// Maps an inventory store failure to a retryable `503`, logging the detail rather than leaking it.
fn inventory_error_response(error: &InventoryStoreError) -> Response {
    tracing::error!(%error, "an inventory store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the inventory service is unavailable",
    )
}
