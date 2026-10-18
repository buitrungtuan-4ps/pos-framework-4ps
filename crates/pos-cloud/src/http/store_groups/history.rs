// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A group's batch history: the batches a group has received, and one batch's report.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use serde::Deserialize;

use super::publish::batch_report;
use super::{StoreGroupState, store_group_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{RegistryTenantQuery, not_found};
use crate::registry::RegistryStore;
use crate::store_groups::{ConfigBatchId, StoreGroupId, StoreGroupStore};

/// How many of a group's batches a list returns when the caller names no limit.
const DEFAULT_BATCH_HISTORY: u32 = 20;

/// The most a caller may ask for in one page of batch history.
const MAX_BATCH_HISTORY: u32 = 200;

/// A `GET /admin/store-groups/{group_id}/batches` query.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct BatchHistoryQuery {
    tenant_id: String,
    #[serde(default)]
    limit: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/admin/store-groups/{group_id}/batches",
    params(
        ("group_id" = String, Path, description = "The cohort whose history to read (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The owning tenant (a 26-character ULID)"),
        ("limit" = Option<u32>, Query, description = "How many batches, newest first (default 20, max 200)"),
    ),
    responses(
        (status = 200, description = "The group's batches, newest first, each with its per-store \
                                      outcomes. A member whose last batch was not `applied` is a \
                                      store that is not running what its group runs"),
        (status = 400, description = "An id is not a ULID, or limit is out of range", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The store-group store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "store groups",
)]
/// A super-admin reads a group's batch history, each batch with every member's outcome.
pub(super) async fn admin_list_store_group_batches<Grp, Reg, Cfg, A, C>(
    State(state): State<StoreGroupState<Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    Query(query): Query<BatchHistoryQuery>,
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
    let (tenant_id, group_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("group_id", &group_id)]) {
            Ok([tenant_id, group_id]) => (TenantId::new(tenant_id), StoreGroupId::new(group_id)),
            Err(refusal) => return refusal,
        };
    let limit = query.limit.unwrap_or(DEFAULT_BATCH_HISTORY);
    if limit == 0 || limit > MAX_BATCH_HISTORY {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("limit must be between 1 and {MAX_BATCH_HISTORY}"),
            &[("limit", "OUT_OF_RANGE")],
        );
    }
    let batches = match state.groups.list_batches(tenant_id, group_id, limit).await {
        Ok(batches) => batches,
        Err(error) => return store_group_error_response(&error),
    };
    let mut reports = Vec::with_capacity(batches.len());
    for batch in batches {
        let results = match state.groups.batch_results(tenant_id, batch.batch_id).await {
            Ok(results) => results,
            Err(error) => return store_group_error_response(&error),
        };
        let finished = batch.finished_at_ms;
        reports.push(batch_report(&batch, finished, &results));
    }
    (StatusCode::OK, Json(reports)).into_response()
}

#[utoipa::path(
    get,
    path = "/admin/store-groups/{group_id}/batches/{batch_id}",
    params(
        ("group_id" = String, Path, description = "The cohort the batch ran against (a 26-character ULID)"),
        ("batch_id" = String, Path, description = "The batch (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The owning tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The batch and an outcome for every member it reached"),
        (status = 400, description = "An id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such batch on this group in this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The store-group store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "store groups",
)]
/// A super-admin reads one batch and what it did at every member.
pub(super) async fn admin_read_store_group_batch<Grp, Reg, Cfg, A, C>(
    State(state): State<StoreGroupState<Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path((group_id, batch_id)): Path<(String, String)>,
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
    let (tenant_id, group_id, batch_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("group_id", &group_id),
        ("batch_id", &batch_id),
    ]) {
        Ok([tenant_id, group_id, batch_id]) => (
            TenantId::new(tenant_id),
            StoreGroupId::new(group_id),
            ConfigBatchId::new(batch_id),
        ),
        Err(refusal) => return refusal,
    };
    // Read the group's history rather than the batch by id alone: it is what confines the lookup
    // to the group in the path, so a batch id from a sibling cohort answers `404` and not somebody
    // else's report.
    let batches = match state
        .groups
        .list_batches(tenant_id, group_id, MAX_BATCH_HISTORY)
        .await
    {
        Ok(batches) => batches,
        Err(error) => return store_group_error_response(&error),
    };
    let Some(batch) = batches.into_iter().find(|batch| batch.batch_id == batch_id) else {
        return not_found("config batch");
    };
    let results = match state.groups.batch_results(tenant_id, batch_id).await {
        Ok(results) => results,
        Err(error) => return store_group_error_response(&error),
    };
    let finished = batch.finished_at_ms;
    (
        StatusCode::OK,
        Json(batch_report(&batch, finished, &results)),
    )
        .into_response()
}
