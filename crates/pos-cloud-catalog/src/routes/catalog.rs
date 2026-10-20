// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Catalog authoring: the items, tax classes, categories, layout buttons, courses, modifier groups,
//! menus and placements a tenant authors under `/admin/catalog/*`, and the menu publish
//! ([ADR-0066](../../../../docs/adr/0066-cloud-catalog.md)).
//!
//! The state, the router and the store-error helpers are here; each kind's handlers are in a
//! module of their own.

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use courses::{admin_create_course, admin_list_courses, admin_update_course};
use display_categories::{
    admin_create_display_category, admin_create_display_subcategory, admin_list_display_categories,
    admin_list_display_subcategories, admin_update_display_category,
    admin_update_display_subcategory,
};
use item_categories::{
    admin_create_item_category, admin_create_item_subcategory, admin_list_item_categories,
    admin_list_item_subcategories, admin_update_item_category, admin_update_item_subcategory,
};
use item_files::{admin_export_items, admin_import_items_apply, admin_import_items_dry_run};
use items::{admin_create_item, admin_list_items, admin_update_item};
use layout_buttons::{
    admin_create_layout_button, admin_list_layout_buttons, admin_remove_layout_button,
    admin_update_layout_button,
};
use menus::{
    admin_create_menu, admin_create_menu_section, admin_list_menu_sections, admin_list_menus,
    admin_update_menu, admin_update_menu_section,
};
use modifier_groups::{
    admin_create_modifier_group, admin_list_modifier_groups, admin_update_modifier_group,
};
use placements::{
    admin_create_placement, admin_list_placements, admin_remove_placement, admin_update_placement,
};
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use tax_classes::{admin_create_tax_class, admin_list_tax_classes, admin_update_tax_class};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::catalog::{CatalogStore, CatalogStoreError};

mod courses;
mod display_categories;
mod item_categories;
mod item_files;
mod items;
mod layout_buttons;
mod menus;
mod modifier_groups;
mod placements;
mod publish;
mod requests;
mod tax_classes;

pub use items::page_shaping_needs_a_limit_refusal;
pub use publish::catalog_publish_router;
pub use publish::menu_nodes;
pub use requests::clean_name_translations;
// The handlers the console's OpenAPI document names.
pub use courses::{
    __path_admin_create_course, __path_admin_list_courses, __path_admin_update_course,
};
pub use display_categories::{
    __path_admin_create_display_category, __path_admin_create_display_subcategory,
    __path_admin_list_display_categories, __path_admin_list_display_subcategories,
    __path_admin_update_display_category, __path_admin_update_display_subcategory,
};
pub use item_categories::{
    __path_admin_create_item_category, __path_admin_create_item_subcategory,
    __path_admin_list_item_categories, __path_admin_list_item_subcategories,
    __path_admin_update_item_category, __path_admin_update_item_subcategory,
};

// --- Catalog authoring (`/admin/catalog/...`, ADR-0066) -----------------------------------------

/// The collaborators the catalog routes need, stated independently of `pos-cloud`'s `CloudApp`,
/// like its `RegistryState`: the catalog authoring store, plus the admin and clock every session
/// guard uses.
#[derive(Clone)]
struct CatalogState<Cat, A, C> {
    catalog: Cat,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the catalog authoring sub-router ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
///
/// The write surface for the menu source of truth an operator edits — items, menus (with an
/// inheritance edge), and the per-channel placements a menu is compiled from. Every route is behind
/// the super-admin session guard and names a tenant the admin-is-global way
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): a `?tenant_id=` query for the
/// reads and deletes, the request body for a create or upsert. A placement is addressed by its
/// `(menu_id, menu_item_id)` pair: `PUT` upserts it, `DELETE` removes it. The routes live under
/// `/admin/catalog/`, a fresh prefix that never collides with the registry or device surfaces.
pub fn catalog_router<Cat, A, C>(
    catalog: Cat,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .merge(item_routes::<Cat, A, C>())
        .merge(taxonomy_routes::<Cat, A, C>())
        .merge(till_routes::<Cat, A, C>())
        .merge(menu_routes::<Cat, A, C>())
        .with_state(CatalogState {
            catalog,
            admin,
            clock,
            audit,
        })
}

/// The items, their spreadsheet export and import, and the tax classes they are taxed at.
fn item_routes<Cat, A, C>() -> Router<CatalogState<Cat, A, C>>
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/items",
            get(admin_list_items::<Cat, A, C>).post(admin_create_item::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/items/{menu_item_id}",
            axum::routing::patch(admin_update_item::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/export/items",
            get(admin_export_items::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/import/items/dry-run",
            post(admin_import_items_dry_run::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/import/items/apply",
            post(admin_import_items_apply::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/tax-classes",
            get(admin_list_tax_classes::<Cat, A, C>).post(admin_create_tax_class::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/tax-classes/{tax_class_id}",
            axum::routing::patch(admin_update_tax_class::<Cat, A, C>),
        )
}

/// The item and display categories and their subcategories.
fn taxonomy_routes<Cat, A, C>() -> Router<CatalogState<Cat, A, C>>
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/item-categories",
            get(admin_list_item_categories::<Cat, A, C>)
                .post(admin_create_item_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/item-categories/{item_category_id}",
            axum::routing::patch(admin_update_item_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/item-subcategories",
            get(admin_list_item_subcategories::<Cat, A, C>)
                .post(admin_create_item_subcategory::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/item-subcategories/{item_subcategory_id}",
            axum::routing::patch(admin_update_item_subcategory::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-categories",
            get(admin_list_display_categories::<Cat, A, C>)
                .post(admin_create_display_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-categories/{display_category_id}",
            axum::routing::patch(admin_update_display_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-subcategories",
            get(admin_list_display_subcategories::<Cat, A, C>)
                .post(admin_create_display_subcategory::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-subcategories/{display_subcategory_id}",
            axum::routing::patch(admin_update_display_subcategory::<Cat, A, C>),
        )
}

/// What the till lays out: its buttons, courses and modifier groups.
fn till_routes<Cat, A, C>() -> Router<CatalogState<Cat, A, C>>
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/layout-buttons",
            get(admin_list_layout_buttons::<Cat, A, C>)
                .post(admin_create_layout_button::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/layout-buttons/{sales_channel}/{menu_item_id}",
            axum::routing::put(admin_update_layout_button::<Cat, A, C>)
                .delete(admin_remove_layout_button::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/courses",
            get(admin_list_courses::<Cat, A, C>).post(admin_create_course::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/courses/{course_id}",
            axum::routing::patch(admin_update_course::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/modifier-groups",
            get(admin_list_modifier_groups::<Cat, A, C>)
                .post(admin_create_modifier_group::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/modifier-groups/{modifier_group_id}",
            axum::routing::patch(admin_update_modifier_group::<Cat, A, C>),
        )
}

/// The menus, their sections, and the placements a menu is compiled from.
fn menu_routes<Cat, A, C>() -> Router<CatalogState<Cat, A, C>>
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/menus",
            get(admin_list_menus::<Cat, A, C>).post(admin_create_menu::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}",
            axum::routing::patch(admin_update_menu::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/sections",
            get(admin_list_menu_sections::<Cat, A, C>).post(admin_create_menu_section::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/sections/{menu_section_id}",
            axum::routing::patch(admin_update_menu_section::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/placements",
            get(admin_list_placements::<Cat, A, C>).post(admin_create_placement::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/placements/{menu_item_id}",
            axum::routing::put(admin_update_placement::<Cat, A, C>)
                .delete(admin_remove_placement::<Cat, A, C>),
        )
}

/// Maps a catalog store failure to a retryable `503`, logging the detail rather than leaking it.
pub fn catalog_error_response(error: &CatalogStoreError) -> Response {
    tracing::error!(%error, "a catalog store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the catalog service is unavailable",
    )
}

/// The `503` returned when OS entropy is unavailable to mint a catalog id.
fn catalog_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a catalog id");
    api_error(
        ErrorStatus::Unavailable,
        "the catalog service is unavailable",
    )
}
