// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/tax-classes`: list, create and update a tenant's tax classes.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{TaxClassId, TenantId};

use super::requests::{CreateTaxClassRequest, UpdateTaxClassRequest};
use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogItem, CatalogStore, CatalogStoreError, TaxClass};
use crate::registry::EntityStatus;
use crate::routes::{
    RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    unprocessable_violations, version_mismatch, versioned_created, versioned_ok,
};
use crate::version::UpdateOutcome;

/// A super-admin lists a tenant's tax classes.
pub(super) async fn admin_list_tax_classes<Cat, A, C>(
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
    match state.catalog.list_tax_classes(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates a tax class; the id is minted here and returned once in the created record.
pub(super) async fn admin_create_tax_class<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateTaxClassRequest>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(tax_class_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(TaxClassId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = TaxClass {
        tax_class_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_tax_class(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "tax_class.create",
                "tax_class",
                &record.tax_class_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => catalog_error_response(&error),
    }
}

/// How many of a tenant's **active** items still point at a taxonomy row, for the archive guard.
///
/// # Why archiving needed a guard at all
///
/// Archiving a tax class or a category was an unconditional write, and the two ends of that were
/// already disagreeing with each other in the tree: the console's item editor
/// (`Items.tsx`) *rewrites* a dangling taxonomy reference when it next saves the item, so a
/// reference to an archived class did not stay dangling so much as quietly become something else.
/// A tax class is the expensive one — `pos_core::billing` raises `TaxRateNotConfigured` when no
/// rate matches a line's class and channel, and the store learns that at the payment screen, with
/// a customer waiting.
///
/// Counting only **active** items is the deliberate part. An archived item is already not sold, so
/// its reference costs nothing and holding a tax class hostage to a retired item would make the
/// guard the thing operators route around.
pub(super) async fn active_items_referencing<Cat>(
    catalog: &Cat,
    tenant_id: TenantId,
    references: impl Fn(&CatalogItem) -> bool,
) -> Result<usize, CatalogStoreError>
where
    Cat: CatalogStore,
{
    let rows = catalog.list_items(tenant_id).await?;
    Ok(rows
        .into_iter()
        .map(|row| row.record)
        .filter(|item| item.status == EntityStatus::Active && references(item))
        .count())
}

/// The `422` an archive gets when active items still reference the row.
///
/// It names the count rather than the items: a class held by two hundred items is a different
/// problem from one held by two, and an operator deciding what to do next needs the size before
/// the list. The list itself is one search away on the Items screen, which is where the fix
/// happens anyway.
pub(super) fn still_referenced(kind: &str, count: usize) -> Response {
    unprocessable_violations(&[format!(
        "this {kind} cannot be archived: {count} active {} still use it",
        if count == 1 { "item" } else { "items" }
    )])
}

/// A super-admin renames a tax class and/or sets its status.
pub(super) async fn admin_update_tax_class<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(tax_class_id): Path<String>,
    Json(request): Json<UpdateTaxClassRequest>,
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
    let (tax_class_id, tenant_id) = match parse_ulid_fields([
        ("tax_class_id", &tax_class_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([tax_class_id, tenant_id]) => (TaxClassId::new(tax_class_id), TenantId::new(tenant_id)),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = TaxClass {
        tax_class_id,
        tenant_id,
        name: request.name,
        status,
    };
    // Refuse the archive while active items still point here (see `active_items_referencing`).
    if status == EntityStatus::Archived {
        match active_items_referencing(&state.catalog, tenant_id, |item| {
            item.tax_class_id == tax_class_id
        })
        .await
        {
            Ok(0) => {}
            Ok(count) => return still_referenced("tax class", count),
            Err(error) => return catalog_error_response(&error),
        }
    }
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_tax_class(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "tax_class.update",
                "tax_class",
                &record.tax_class_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("tax class"),
        Err(error) => catalog_error_response(&error),
    }
}
