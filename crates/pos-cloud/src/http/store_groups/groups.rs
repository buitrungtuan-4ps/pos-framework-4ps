// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/store-groups`: list, create and update a tenant's groups, and set their members.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header::ETAG;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::{
    CreateStoreGroupRequest, MAX_STORE_GROUP_MEMBERS, SetStoreGroupMembersRequest, StoreGroupState,
    StoreGroupView, UpdateStoreGroupRequest, store_group_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::people::people_error_response;
use crate::http::{
    FieldRefusal, RegistryTenantQuery, entity_status_refusal, if_match, not_found,
    parse_entity_status, registry_error_response, unprocessable_violations, version_mismatch,
    versioned_created, versioned_ok,
};
use crate::registry::{EntityStatus, RegistryStore, StoreRecord};
use crate::store_groups::{StoreGroup, StoreGroupId, StoreGroupStore};
use crate::version::{UpdateOutcome, Versioned};

#[utoipa::path(
    get,
    path = "/admin/store-groups",
    params(("tenant_id" = String, Query, description = "The tenant whose cohorts to list (a 26-character ULID)")),
    responses(
        (status = 200, description = "The tenant's store groups, creation order, each with its \
                                      membership. Archived groups are included: an operator needs to \
                                      see a retired cohort in order to bring it back"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The store-group store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "store groups",
)]
/// A super-admin lists a tenant's store groups, each with its membership.
pub(super) async fn admin_list_store_groups<Grp, Reg, Cfg, A, C>(
    State(state): State<StoreGroupState<Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
    let rows = match state.groups.list_groups(tenant_id).await {
        Ok(rows) => rows,
        Err(error) => return store_group_error_response(&error),
    };
    let mut views: Vec<StoreGroupView> = Vec::with_capacity(rows.len());
    for row in rows {
        let members = match state
            .groups
            .list_members(tenant_id, row.record.group_id)
            .await
        {
            Ok(members) => members,
            Err(error) => return store_group_error_response(&error),
        };
        views.push(StoreGroupView {
            group_id: row.record.group_id.to_string(),
            tenant_id: row.record.tenant_id.to_string(),
            name: row.record.name,
            status: row.record.status,
            store_ids: members.iter().map(ToString::to_string).collect(),
            etag: row.etag.as_str().to_owned(),
        });
    }
    (StatusCode::OK, Json(views)).into_response()
}

#[utoipa::path(
    post,
    path = "/admin/store-groups",
    responses(
        (status = 201, description = "The group, with the ETag to hand back on its first edit. It \
                                      holds no stores yet — membership is a second call, so naming \
                                      a cohort is never itself an act with a fleet-wide reach"),
        (status = 400, description = "tenant_id is absent or not a ULID, or name is empty", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The store-group store is unreachable, or OS entropy is unavailable to mint an id", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "store groups",
)]
/// A super-admin creates an empty store group.
///
/// Empty, deliberately: a group that started holding every store would make *creating* one an act
/// with a fleet-wide blast radius, and an operator naming a cohort has not yet said what is in it.
pub(super) async fn admin_create_store_group<Grp, Reg, Cfg, A, C>(
    State(state): State<StoreGroupState<Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateStoreGroupRequest>,
) -> Response
where
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
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
    let name = request.name.trim();
    if name.is_empty() {
        return FieldRefusal::new("name is required", "name", "REQUIRED").into_response();
    }
    let Some(group_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(StoreGroupId::new)
    else {
        return service_unavailable("store groups");
    };
    let record = StoreGroup {
        group_id,
        tenant_id,
        name: name.to_owned(),
        status: EntityStatus::Active,
    };
    match state.groups.create_group(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "store_group.create",
                "store_group",
                &group_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => store_group_error_response(&error),
    }
}

#[utoipa::path(
    patch,
    path = "/admin/store-groups/{group_id}",
    params(
        ("group_id" = String, Path, description = "The group to rename or (un)archive (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the group was read at (ADR-0094). Required"),
    ),
    responses(
        (status = 200, description = "The group as it now stands, with its new ETag"),
        (status = 400, description = "group_id or tenant_id is not a ULID, name is empty, or status is not active/archived", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such group in this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "If-Match is absent, or names a version the group has moved past", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The store-group store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "store groups",
)]
/// A super-admin renames a group and/or archives or restores it.
pub(super) async fn admin_update_store_group<Grp, Reg, Cfg, A, C>(
    State(state): State<StoreGroupState<Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    Json(request): Json<UpdateStoreGroupRequest>,
) -> Response
where
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (group_id, tenant_id) =
        match parse_ulid_fields([("group_id", &group_id), ("tenant_id", &request.tenant_id)]) {
            Ok([group_id, tenant_id]) => (StoreGroupId::new(group_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    let name = request.name.trim();
    if name.is_empty() {
        return FieldRefusal::new("name is required", "name", "REQUIRED").into_response();
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = StoreGroup {
        group_id,
        tenant_id,
        name: name.to_owned(),
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.groups.update_group(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "store_group.update",
                "store_group",
                &group_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("store group"),
        Err(error) => store_group_error_response(&error),
    }
}

#[utoipa::path(
    put,
    path = "/admin/store-groups/{group_id}/members",
    params(
        ("group_id" = String, Path, description = "The group whose membership to replace (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the group was read at (ADR-0094/0095 shape C). Required"),
    ),
    responses(
        (status = 200, description = "The membership as stored, deduplicated, with the group's new ETag, and `stores`: \
                                      when an assignment names the group, how the `permissions` publish went at each open store \
                                      that joined or left it (ADR-0158), and empty otherwise"),
        (status = 400, description = "group_id, tenant_id or a store id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such group in this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "If-Match is absent, or names a version the group has moved past", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "More than 200 stores, or an id that names no store of this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The store-group store, the registry or the people store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "store groups",
)]
/// A super-admin replaces a group's membership.
///
/// Every id is checked against the tenant's own stores before anything is written. An id that names
/// no store of this tenant is a `422` rather than a stored row, because the failure it otherwise
/// produces is invisible: the cohort looks right on screen, and every batch quietly reports one more
/// `skipped` store than the operator expects, forever.
///
/// A group an assignment names changes who works at each store that joins or leaves it, so each
/// such store that is open is published its `permissions` node once the membership is saved
/// ([ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 7), and the answer's `stores` says how each went. The membership before the write, and
/// whether an assignment names the group, are read first, so a failed read refuses a write that has
/// not happened.
pub(super) async fn admin_set_store_group_members<Grp, Reg, Cfg, A, C>(
    State(state): State<StoreGroupState<Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    Json(request): Json<SetStoreGroupMembersRequest>,
) -> Response
where
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (group_id, tenant_id) =
        match parse_ulid_fields([("group_id", &group_id), ("tenant_id", &request.tenant_id)]) {
            Ok([group_id, tenant_id]) => (StoreGroupId::new(group_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    if request.store_ids.len() > MAX_STORE_GROUP_MEMBERS {
        return unprocessable_violations(&[format!(
            "a store group holds at most {MAX_STORE_GROUP_MEMBERS} stores; split this cohort in two"
        )]);
    }
    let mut members: Vec<StoreId> = Vec::with_capacity(request.store_ids.len());
    for id in &request.store_ids {
        let Ok([parsed]) = parse_ulid_fields([("store_ids", id)]) else {
            return ulid_refusal(&["store_ids"]);
        };
        let store_id = StoreId::new(parsed);
        // Deduplicated here rather than left to the table's primary key: the operator's list is what
        // the response echoes back, and a duplicate that silently vanished on write would make the
        // screen and the database disagree about what was saved.
        if !members.contains(&store_id) {
            members.push(store_id);
        }
    }
    let known = match state.registry.list_stores(tenant_id).await {
        Ok(rows) => rows,
        Err(error) => return registry_error_response(&error),
    };
    if let Some(stranger) = members
        .iter()
        .find(|id| !known.iter().any(|row| row.record.store_id == **id))
    {
        return unprocessable_violations(&[format!(
            "store {stranger} is not a store of this tenant, so it cannot be in one of its groups"
        )]);
    }
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let moved = match staff_moved_by(&state, tenant_id, group_id, &known, &members).await {
        Ok(moved) => moved,
        Err(refusal) => return refusal,
    };
    match state
        .groups
        .set_members(tenant_id, group_id, &members, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            let membership: Vec<String> = members.iter().map(ToString::to_string).collect();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "store_group.members",
                "store_group",
                &group_id.to_string(),
                None,
                serde_json::to_value(&membership).ok(),
            )
            .await;
            let stores = state
                .staff
                .publish_to(&context, tenant_id, group_id, &moved)
                .await;
            (
                StatusCode::OK,
                [(ETAG, version.as_str().to_owned())],
                Json(serde_json::json!({
                    "store_ids": membership,
                    "etag": version.as_str(),
                    "stores": stores,
                })),
            )
                .into_response()
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("store group"),
        Err(error) => store_group_error_response(&error),
    }
}

/// The open stores whose staff a membership write changes — each that joins or leaves a group some
/// assignment names (ADR-0158 decision 3) — read before the write. None when no assignment names
/// the group.
async fn staff_moved_by<Grp, Reg, Cfg, A, C>(
    state: &StoreGroupState<Grp, Reg, Cfg, A, C>,
    tenant_id: TenantId,
    group_id: StoreGroupId,
    stores: &[Versioned<StoreRecord>],
    members: &[StoreId],
) -> Result<BTreeSet<StoreId>, Response>
where
    Grp: StoreGroupStore,
{
    let before = state
        .groups
        .list_members(tenant_id, group_id)
        .await
        .map_err(|error| store_group_error_response(&error))?;
    let named = state
        .staff
        .names_group(tenant_id, group_id)
        .await
        .map_err(|error| people_error_response(&error))?;
    if !named {
        return Ok(BTreeSet::new());
    }
    Ok(stores
        .iter()
        .filter(|row| row.record.status == EntityStatus::Active)
        .map(|row| row.record.store_id)
        .filter(|id| before.contains(id) != members.contains(id))
        .collect())
}
