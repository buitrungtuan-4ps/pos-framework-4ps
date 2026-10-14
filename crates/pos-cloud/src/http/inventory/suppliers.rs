// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin/inventory/suppliers` handlers: list, read, create, update and delete.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{SupplierId, TenantId};

use super::{
    InventoryState, SupplierAuditSummary, SupplierRequest, build_supplier,
    inventory_entropy_unavailable, inventory_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{
    RegistryTenantQuery, already_exists, if_match, not_found, version_mismatch, versioned_created,
    versioned_ok,
};
use crate::inventory::InventoryStore;
use crate::version::{CreateOutcome, UpdateOutcome};

/// A super-admin lists a tenant's authored suppliers.
pub(super) async fn admin_list_suppliers<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    match state.inventory.list_suppliers(tenant_id).await {
        Ok(suppliers) => (StatusCode::OK, Json(suppliers)).into_response(),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin reads one supplier by id.
pub(super) async fn admin_get_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(supplier_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, supplier_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("supplier_id", &supplier_id),
    ]) {
        Ok([tenant_id, supplier_id]) => (TenantId::new(tenant_id), SupplierId::new(supplier_id)),
        Err(refusal) => return refusal,
    };
    match state.inventory.get_supplier(tenant_id, supplier_id).await {
        Ok(Some(row)) => versioned_ok(row.record, &row.etag),
        Ok(None) => not_found("supplier"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin creates a supplier; the server mints its id.
pub(super) async fn admin_create_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Json(request): Json<SupplierRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(supplier_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(SupplierId::new)
    else {
        return inventory_entropy_unavailable();
    };
    let supplier = match build_supplier(&request, supplier_id) {
        Ok(supplier) => supplier,
        Err(refusal) => return refusal.into_response(),
    };
    match state.inventory.create_supplier(tenant_id, &supplier).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.supplier.create",
                "supplier",
                &supplier_id.to_string(),
                None,
                serde_json::to_value(SupplierAuditSummary::of(&supplier)).ok(),
            )
            .await;
            versioned_created(supplier, &version)
        }
        // Unreachable while the id is minted here rather than sent by the caller, and answered
        // anyway — the seam cannot know that, and silently overwriting is what this slice removed.
        Ok(CreateOutcome::AlreadyExists) => already_exists("supplier"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin updates a supplier in place, by the path id.
pub(super) async fn admin_update_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(supplier_id): Path<String>,
    Json(request): Json<SupplierRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, supplier_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("supplier_id", &supplier_id),
    ]) {
        Ok([tenant_id, supplier_id]) => (TenantId::new(tenant_id), SupplierId::new(supplier_id)),
        Err(refusal) => return refusal,
    };
    let before = match state.inventory.get_supplier(tenant_id, supplier_id).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("supplier");
    };
    let supplier = match build_supplier(&request, supplier_id) {
        Ok(supplier) => supplier,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .update_supplier(tenant_id, &supplier, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.supplier.update",
                "supplier",
                &supplier_id.to_string(),
                serde_json::to_value(SupplierAuditSummary::of(&before)).ok(),
                serde_json::to_value(SupplierAuditSummary::of(&supplier)).ok(),
            )
            .await;
            versioned_ok(supplier, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("supplier"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin deletes a supplier by id.
pub(super) async fn admin_delete_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(supplier_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, supplier_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("supplier_id", &supplier_id),
    ]) {
        Ok([tenant_id, supplier_id]) => (TenantId::new(tenant_id), SupplierId::new(supplier_id)),
        Err(refusal) => return refusal,
    };
    let before = match state.inventory.get_supplier(tenant_id, supplier_id).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("supplier");
    };
    match state
        .inventory
        .delete_supplier(tenant_id, supplier_id)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.supplier.delete",
                "supplier",
                &supplier_id.to_string(),
                serde_json::to_value(SupplierAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}
