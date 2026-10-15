// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The inventory publish (`PUT /admin/config/inventory`): a tenant's authored inventory, compiled
//! into a store's `inventory` node, alone or for a batch fan-out.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::inventory::{PublishedIngredient, PublishedRecipe, PublishedSupplier};
use serde::Deserialize;

use super::inventory_error_response;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{PublishedConfig, publish_config_nodes};
use crate::inventory::{InventoryStore, InventoryStoreError, to_node as inventory_to_node};
use crate::version::records;

// --- Inventory publish (`/admin/config/inventory`, ADR-0079, Track M6) -------------------------

/// The collaborators the inventory-publish route needs: the inventory store the ingredients, recipes,
/// and suppliers are read from, the config-tree store the `inventory` node is written onto, plus the
/// admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigInventoryState<Inv, Cfg, A, C> {
    inventory: Inv,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `PUT /admin/config/inventory` body: the `(tenant, store)` to push the tenant's authored inventory
/// to. Like every other node publish, the ingredients/recipes/suppliers come from what is authored, not
/// the body.
#[derive(Debug, Clone, Deserialize)]
struct PublishInventoryRequest {
    tenant_id: String,
    store_id: String,
}

/// A compact audit summary of an inventory publish — how many of each kind went into the node, never
/// the recipe amounts (T2 proprietary process live in the inventory store).
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::http) struct InventoryPublishSummary {
    ingredients: usize,
    recipes: usize,
    suppliers: usize,
}

/// Reads a tenant's three authored inventory lists in one place, short-circuiting on the first store
/// failure. Split out of [`admin_publish_inventory`] to keep that handler within its line budget.
async fn list_inventory_parts<Inv>(
    inventory: &Inv,
    tenant_id: TenantId,
) -> Result<
    (
        Vec<PublishedIngredient>,
        Vec<PublishedRecipe>,
        Vec<PublishedSupplier>,
    ),
    InventoryStoreError,
>
where
    Inv: InventoryStore,
{
    // A publish wants the records, not the versions the read saw: the node is assembled from what is
    // authored now, and a conditional write against any one row would say nothing about the set.
    Ok((
        records(inventory.list_ingredients(tenant_id).await?),
        records(inventory.list_recipes(tenant_id).await?),
        records(inventory.list_suppliers(tenant_id).await?),
    ))
}

/// Builds the inventory-publish sub-router ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md), M6).
///
/// One route: assemble the tenant's authored ingredients, recipes, and suppliers into a
/// `PublishedInventory`, write it as the store's `inventory` config node, and version it through the
/// config tree — the same node-merge the campaigns/tax/floor/menu publishes use, so the other
/// Store-level keys survive. Behind [`ConsolePermission::PublishConfig`]. The edge applies the node to
/// build its `RecipeBook` and per-item auto-86 thresholds (§8).
pub fn config_inventory_router<Inv, Cfg, A, C>(
    inventory: Inv,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/inventory",
            axum::routing::put(admin_publish_inventory::<Inv, Cfg, A, C>),
        )
        .with_state(ConfigInventoryState {
            inventory,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Assembles a tenant's authored inventory into a `PublishedInventory`, writes it as the store's
/// `inventory` node, and versions it — the same load→merge→publish→version shape as the other node
/// publishes.
/// Compiles a tenant's authored inventory into the store's `inventory` node, with a summary.
///
/// Shared with the batch fan-out ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §3):
/// the node is the same at every member, so a batch compiles it once and writes it *N* times.
pub(in crate::http) async fn inventory_nodes<Inv>(
    inventory: &Inv,
    tenant_id: TenantId,
) -> Result<(Vec<(String, serde_json::Value)>, InventoryPublishSummary), Response>
where
    Inv: InventoryStore,
{
    let (ingredients, recipes, suppliers) = match list_inventory_parts(inventory, tenant_id).await {
        Ok(parts) => parts,
        Err(error) => return Err(inventory_error_response(&error)),
    };
    let summary = InventoryPublishSummary {
        ingredients: ingredients.len(),
        recipes: recipes.len(),
        suppliers: suppliers.len(),
    };
    let Ok(inventory_value) =
        serde_json::to_value(inventory_to_node(ingredients, recipes, suppliers))
    else {
        tracing::error!("could not serialise an inventory node");
        return Err(service_unavailable("inventory"));
    };
    Ok((vec![("inventory".to_owned(), inventory_value)], summary))
}

async fn admin_publish_inventory<Inv, Cfg, A, C>(
    State(state): State<ConfigInventoryState<Inv, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishInventoryRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    // Set the `inventory` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys (`menu`, `tax`, `campaigns`, `permissions`, `floor`, capability flags).
    let (nodes, summary) = match inventory_nodes(&state.inventory, tenant_id).await {
        Ok(compiled) => compiled,
        Err(refusal) => return refusal,
    };

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
        audit_action(
            &state.audit,
            &state.clock,
            &context,
            Some(tenant_id),
            "config.inventory.publish",
            "store",
            &store_id.to_string(),
            None,
            serde_json::to_value(summary).ok(),
        )
        .await;
        (
            StatusCode::OK,
            Json(PublishedConfig {
                config_version_id: id.to_string(),
            }),
        )
            .into_response()
    }
}
