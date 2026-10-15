// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/layout-buttons`: the buttons a till's layout shows.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId, TenantId};
use pos_proto::wire_enum::Open;

use super::requests::{CreateLayoutButtonRequest, SetLayoutButtonRequest};
use super::{CatalogState, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogStore, LayoutButton};
use crate::routes::{
    RegistryTenantQuery, already_exists, if_match, not_found, parse_optional_ulid,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::version::{CreateOutcome, UpdateOutcome, Versioned};

/// A super-admin lists a tenant's layout buttons across all channels.
pub(super) async fn admin_list_layout_buttons<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_layout_buttons(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<Versioned<LayoutButton>>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// Assembles a [`LayoutButton`] from a request plus the channel and item that identify it.
///
/// Shared by the create and update routes so the record is assembled in one place: the two differ
/// only in where the identity comes from (a body on `POST`, the path on `PUT`) and in what the
/// write does with it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns, the shape `parse_ulid_fields` already carries this expectation for"
)]
fn build_layout_button(
    request: SetLayoutButtonRequest,
    sales_channel: &str,
    menu_item_id: &str,
) -> Result<LayoutButton, Response> {
    let (tenant_id, menu_item_id, display_category_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("menu_item_id", menu_item_id),
        ("display_category_id", &request.display_category_id),
    ]) {
        Ok([tenant_id, menu_item_id, display_category_id]) => (
            TenantId::new(tenant_id),
            MenuItemId::new(menu_item_id),
            DisplayCategoryId::new(display_category_id),
        ),
        Err(refusal) => return Err(refusal),
    };
    let Ok(display_subcategory_id) = parse_optional_ulid(
        request.display_subcategory_id.as_deref(),
        DisplaySubcategoryId::new,
    ) else {
        return Err(ulid_refusal(&["display_subcategory_id"]));
    };
    // A grid slot exists only when both column and row are given; otherwise the button flows by order.
    let position = match (request.grid_column, request.grid_row) {
        (Some(column), Some(row)) => Some(GridPosition { column, row }),
        _ => None,
    };
    Ok(LayoutButton {
        tenant_id,
        sales_channel: Open::<SalesChannel>::parse(sales_channel),
        display_category_id,
        display_subcategory_id,
        menu_item_id,
        label: request.label,
        position,
        sort: request.sort,
    })
}

/// A super-admin places an item's button on a channel's layout, refusing if it is already there.
///
/// The route this replaced was a single `PUT` upsert. A button's identity is entirely
/// caller-supplied — `(tenant, channel, item)` — so "place this button" silently replaced the
/// label, grid slot and sort order of one already on that channel, and the audit trail recorded
/// the overwrite as a `set` with no `before`.
pub(super) async fn admin_create_layout_button<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateLayoutButtonRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (sales_channel, menu_item_id) =
        (request.sales_channel.clone(), request.menu_item_id.clone());
    let record = match build_layout_button(request.into_write(), &sales_channel, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    match state.catalog.create_layout_button(&record).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "layout_button.create",
                "layout_button",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("layout button"),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin changes a button's presentation on a channel, only at the version they read.
pub(super) async fn admin_update_layout_button<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((sales_channel, menu_item_id)): Path<(String, String)>,
    Json(request): Json<SetLayoutButtonRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let record = match build_layout_button(request, &sales_channel, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_layout_button(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            // A layout button's identity is (tenant, channel, item); the item id is its entity key,
            // and the full row (channel, category, position) is recorded as `after`.
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "layout_button.update",
                "layout_button",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("layout button"),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin removes an item's button from a channel's layout (tenant named on the query).
pub(super) async fn admin_remove_layout_button<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((sales_channel, menu_item_id)): Path<(String, String)>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, menu_item_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("menu_item_id", &menu_item_id),
    ]) {
        Ok([tenant_id, menu_item_id]) => (TenantId::new(tenant_id), MenuItemId::new(menu_item_id)),
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .remove_layout_button(
            tenant_id,
            Open::<SalesChannel>::parse(&sales_channel),
            menu_item_id,
        )
        .await
    {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "layout_button.remove",
                "layout_button",
                &menu_item_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("layout button"),
        Err(error) => catalog_error_response(&error),
    }
}
