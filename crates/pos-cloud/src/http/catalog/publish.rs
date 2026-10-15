// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/catalog/publish`: a menu compiled and written to a store, with its fees.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::fees::PublishedFees;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::catalog_error_response;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{
    CatalogItem, CatalogStore, Course, DisplayCategory, DisplaySubcategory, Menu, MenuId,
    ModifierGroup,
};
use crate::catalog_compiler::{compile_layout_book, compile_menu};
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::fees::FeeRuleStore;
use crate::http::fees::{FeePublishResult, TenantFeeRules};
use crate::http::publish_config_nodes_with;
use crate::registry::RegistryStore;
use crate::version::records;

// --- Catalog publish (`/admin/catalog/publish`, ADR-0066) ---------------------------------------

/// The collaborators the publish route needs: the catalog to compile from, the fee rules and the
/// registry the store's `fees` node is resolved from, the config-tree store to publish into, plus
/// the admin and clock the session guard and version-id minting use.
#[derive(Clone)]
struct CatalogPublishState<Cat, F, Rg, Cfg, A, C> {
    catalog: Cat,
    fees: F,
    registry: Rg,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the catalog publish sub-router ([ADR-0066](../../../../../docs/adr/0066-cloud-catalog.md)).
///
/// The step that turns authored catalog into what a store actually pulls: compile a menu into a
/// per-channel [`pos_proto::MenuBook`] and write it onto the `menu` node of the store's **Store**
/// config layer, so it rides the config tree to the store like every other configuration change
/// ([ADR-0033](../../../../../docs/adr/0033-config-tree.md)) — no new channel. It is a separate sub-router
/// because it needs the config-tree store the CRUD routes do not.
///
/// The store's `fees` node goes in the same version
/// ([ADR-0159](../../../../../docs/adr/0159-a-fee-is-configuration.md)): a fee rule naming a category is
/// compiled into the items the category holds, so the publish that adds an item to a store is the
/// one that brings its fees along. That is what the fee rules and the registry are for.
pub fn catalog_publish_router<Cat, F, Rg, Cfg, A, C>(
    catalog: Cat,
    fees: F,
    registry: Rg,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    F: FeeRuleStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/publish",
            post(admin_publish_menu::<Cat, F, Rg, Cfg, A, C>),
        )
        .with_state(CatalogPublishState {
            catalog,
            fees,
            registry,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// A super-admin selects the (tenant, store, menu) to compile and publish.
#[expect(
    clippy::struct_field_names,
    reason = "tenant_id/store_id/menu_id are the wire field names; the shared _id postfix is the ULID naming convention (docs/naming-and-api.md), not a smell"
)]
#[derive(Debug, Clone, Deserialize)]
struct PublishMenuRequest {
    /// The tenant that owns the catalog and the store (a 26-character ULID).
    tenant_id: String,
    /// The store whose `menu` config node receives the compiled book (a ULID).
    store_id: String,
    /// The menu to compile — its inheritance chain and placements (a ULID).
    menu_id: String,
}

/// Compiles a tenant's catalog into the `menu` and `layout` nodes one publish writes.
///
/// Both keys move together and neither names a store, which is what makes a menu the headline
/// batchable node ([ADR-0122](../../../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §4):
/// a batch compiles the book once and writes the identical document to every member.
pub(in crate::http) async fn menu_nodes<Cat>(
    catalog: &Cat,
    tenant_id: TenantId,
    menu_id: MenuId,
) -> Result<Vec<(String, serde_json::Value)>, Response>
where
    Cat: CatalogStore,
{
    // Load the tenant's authoring model. Placements are gathered across every menu; the compiler
    // filters to the requested menu's inheritance chain, so extra rows are harmless.
    // The compiler takes the authoring records themselves; the version each was read at is a
    // writer's concern (ADR-0094) and would only be noise in a compiled book.
    let items: Vec<CatalogItem> = match catalog.list_items(tenant_id).await {
        Ok(items) => items.into_iter().map(|item| item.record).collect(),
        Err(error) => return Err(catalog_error_response(&error)),
    };
    let menus: Vec<Menu> = match catalog.list_menus(tenant_id).await {
        Ok(menus) => menus.into_iter().map(|menu| menu.record).collect(),
        Err(error) => return Err(catalog_error_response(&error)),
    };
    let mut placements = Vec::new();
    for menu in &menus {
        match catalog.list_placements(tenant_id, menu.menu_id).await {
            Ok(rows) => placements.extend(records(rows)),
            Err(error) => return Err(catalog_error_response(&error)),
        }
    }
    // Modifier groups are tenant-wide, not per menu (ADR-0066 entity 5): one "Size" group is pinned to
    // items, and the compiler keeps only the ones this menu's channels can offer (ADR-0127).
    let groups: Vec<ModifierGroup> = match catalog.list_modifier_groups(tenant_id).await {
        Ok(rows) => rows.into_iter().map(|row| row.record).collect(),
        Err(error) => return Err(catalog_error_response(&error)),
    };

    // Courses are tenant-wide too (ADR-0130): the item declares which one it goes out on, and the
    // compiler publishes only the courses this menu's channels actually serve, in service order.
    let courses: Vec<Course> = match catalog.list_courses(tenant_id).await {
        Ok(rows) => rows.into_iter().map(|row| row.record).collect(),
        Err(error) => return Err(catalog_error_response(&error)),
    };

    // Compile the price book. A refusal here is a configuration error the operator must fix, not a
    // store failure — which is why a batch treats it as refusing the *batch* and not one member.
    let book = match compile_menu(&items, &menus, &placements, &groups, &courses, menu_id) {
        Ok(book) => book,
        Err(error) => return Err(api_error(ErrorStatus::Unprocessable, error.to_string())),
    };
    let Ok(book_value) = serde_json::to_value(&book) else {
        tracing::error!("could not serialise a compiled menu book");
        return Err(service_unavailable("catalog"));
    };

    // Compile the presentation layout alongside the price book (ADR-0066): the display taxonomy plus
    // the tenant's layout buttons resolve to a per-channel `LayoutBook`, delivered on a separate
    // `layout` node so a button moving reprices nothing. The layout compiler is forgiving (a stale
    // button is skipped), so this never fails a publish that the price compile accepted.
    let display_categories: Vec<DisplayCategory> =
        match catalog.list_display_categories(tenant_id).await {
            Ok(rows) => rows.into_iter().map(|row| row.record).collect(),
            Err(error) => return Err(catalog_error_response(&error)),
        };
    let display_subcategories: Vec<DisplaySubcategory> =
        match catalog.list_display_subcategories(tenant_id).await {
            Ok(rows) => rows.into_iter().map(|row| row.record).collect(),
            Err(error) => return Err(catalog_error_response(&error)),
        };
    let layout_buttons = match catalog.list_layout_buttons(tenant_id).await {
        Ok(rows) => records(rows),
        Err(error) => return Err(catalog_error_response(&error)),
    };
    let layout = compile_layout_book(&display_categories, &display_subcategories, &layout_buttons);
    let Ok(layout_value) = serde_json::to_value(&layout) else {
        tracing::error!("could not serialise a compiled layout book");
        return Err(service_unavailable("catalog"));
    };
    Ok(vec![
        ("menu".to_owned(), book_value),
        ("layout".to_owned(), layout_value),
    ])
}

/// What a menu publish answers: the version it produced, and how the store's fees went with it.
#[derive(Debug, Clone, serde::Serialize)]
struct PublishedMenu {
    /// The new config version id (a ULID).
    config_version_id: String,
    /// The store's `fees` node, composed on the tree the menu was written onto, with a fee
    /// publish's outcomes: applied in the same version, unchanged, refused with each rule the store
    /// cannot apply and why, or failed. Only an applied one wrote anything.
    fees: FeePublishResult,
}

/// A super-admin publishes a menu to a store: compile the price book and the presentation layout →
/// write the `MenuBook` and `LayoutBook` to the store's `menu` and `layout` config nodes, with the
/// store's `fees` node beside them where it changes → version it through the config tree.
///
/// The fees are composed inside the write's retry, on the tree each attempt reads, with the menu
/// being written: what the store is sent is checked against what it will hold. A store that cannot
/// apply one of its rules still gets its menu; the answer says which rule and why, and the store
/// keeps the fees it has, as a fee publish leaves it.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every node \
              publish's closure carries — and this closure never takes the Err branch"
)]
async fn admin_publish_menu<Cat, F, Rg, Cfg, A, C>(
    State(state): State<CatalogPublishState<Cat, F, Rg, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishMenuRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    F: FeeRuleStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id, menu_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("menu_id", &request.menu_id),
    ]) {
        Ok([tenant_id, store_id, menu_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            MenuId::new(menu_id),
        ),
        Err(refusal) => return refusal,
    };

    // Load the store's tree (or start one), set the `menu` and `layout` keys on its Store layer, and
    // re-publish that layer. The Store layer is index 2 in the Tenant→Brand→Store→Device order
    // (`ConfigLevel::ORDER`); writing the whole layer back preserves any other Store-level keys there.
    let nodes = match menu_nodes(&state.catalog, tenant_id, menu_id).await {
        Ok(nodes) => nodes,
        Err(refusal) => return refusal,
    };
    let tenant_fees =
        TenantFeeRules::load(&state.fees, &state.registry, &state.catalog, tenant_id).await;
    let store_fees = tenant_fees
        .as_ref()
        .and_then(|fees| fees.for_store(store_id));

    let mut composed = None;
    let id = match publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        |tree| {
            let mut writing = nodes.clone();
            if let Some(fees) = &store_fees {
                let on_tree = fees.on_tree(tree, &writing);
                if let Some(node) = on_tree.node() {
                    writing.push((PublishedFees::NODE.to_owned(), node.clone()));
                }
                composed = Some(on_tree);
            }
            Ok(writing)
        },
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    let fees = composed.map_or_else(
        || FeePublishResult::of(store_id, "FEE_PUBLISH_FAILED"),
        |on_tree| on_tree.result(store_id, Some(id)),
    );
    {
        // Records that a menu was compiled and published to a store — the menu compiled and the
        // config version it produced, keyed to the store. The compiled book itself rides the
        // config tree; the config version history (a later G2 slice) is where it is diffed.
        audit_action(
            &state.audit,
            &state.clock,
            &context,
            Some(tenant_id),
            "catalog.publish",
            "store",
            &store_id.to_string(),
            None,
            Some(serde_json::json!({
                "menu_id": menu_id.to_string(),
                "config_version_id": id.to_string(),
            })),
        )
        .await;
        (
            StatusCode::OK,
            Json(PublishedMenu {
                config_version_id: id.to_string(),
                fees,
            }),
        )
            .into_response()
    }
}
