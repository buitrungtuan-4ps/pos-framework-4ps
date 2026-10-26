// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/config/campaigns/schedule` and `/admin/config/scheduled`: campaigns published at a set time.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::{campaign_entropy_unavailable, campaign_error_response};
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::campaigns::{CampaignStore, to_node as campaigns_to_node};
use crate::http::{RegistryTenantQuery, not_found};
use crate::scheduling::{
    NewScheduledPublish, ScheduledPublishError, ScheduledPublishStatus, ScheduledPublishStore,
};
use crate::version::records;

// --- Scheduled publishes (ADR-0077, Track M3): the effective-dated / Tết-menu mechanism -----------

/// The state the scheduled-publish routes share: the scheduled-publish store, the campaign store (to
/// snapshot the campaigns node at schedule time), and the admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct ScheduledPublishState<Sch, Camp, A, C> {
    scheduled: Sch,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `POST /admin/config/campaigns/schedule` body: the `(tenant, store)` and the future instant the
/// snapshot should publish, Unix milliseconds.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct ScheduleCampaignsRequest {
    tenant_id: String,
    store_id: String,
    effective_at_ms: i64,
}

/// A scheduled publish as listed — metadata only; the snapshotted node value is not returned (it can
/// be large, and the console shows what and when, not the payload).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct ScheduledPublishView {
    id: String,
    node_key: String,
    effective_at_ms: i64,
    status: ScheduledPublishStatus,
    created_at_ms: i64,
}

/// A `GET /admin/config/scheduled` query: the `(tenant, store)` whose pending publishes to list.
#[derive(Debug, Clone, Deserialize)]
struct ScheduledListQuery {
    tenant_id: String,
    store_id: String,
}

/// Builds the scheduled-publish sub-router ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// `POST /admin/config/campaigns/schedule` snapshots the tenant's campaigns and schedules them to
/// publish to a store at a future instant (the Tết-menu case) — behind [`ConsolePermission::PublishConfig`],
/// the permission an immediate publish uses. `GET /admin/config/scheduled` lists a store's pending
/// publishes ([`ConsolePermission::Read`]); `DELETE /admin/config/scheduled/{id}` cancels one
/// (`PublishConfig`). A background activator applies them at their time.
pub fn scheduled_publish_router<Sch, Camp, A, C>(
    scheduled: Sch,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/campaigns/schedule",
            post(admin_schedule_campaigns::<Sch, Camp, A, C>),
        )
        .route(
            "/admin/config/scheduled",
            get(admin_list_scheduled::<Sch, Camp, A, C>),
        )
        .route(
            "/admin/config/scheduled/{id}",
            delete(admin_cancel_scheduled::<Sch, Camp, A, C>),
        )
        .with_state(ScheduledPublishState {
            scheduled,
            campaigns,
            admin,
            clock,
            audit,
        })
}

#[utoipa::path(
    post,
    path = "/admin/config/campaigns/schedule",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        content = ScheduleCampaignsRequest, description = "`tenant_id` and `store_id`, and `effective_at_ms`, the instant to publish, \
                       in Unix milliseconds, which must be in the future. The campaigns are taken \
                       as authored now: later edits do not reach this publish",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Scheduled: the compiled `campaigns` node is held until its \
                                      time, when the scheduler publishes it. The body carries `id`, \
                                      what a cancel names, and `effective_at_ms`. Audited with the \
                                      number of campaigns and the instant"),
        (status = 400, description = "tenant_id or store_id is not a ULID, or effective_at_ms is not \
                                      in the future", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The campaign store or the scheduler's store is unreachable, or \
                                      no id could be minted", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "campaigns",
)]
/// A super-admin schedules the tenant's campaigns to publish to a store at a future instant. The node
/// is snapshotted now (what is authored and reviewed), so later edits do not leak into the publish.
async fn admin_schedule_campaigns<Sch, Camp, A, C>(
    State(state): State<ScheduledPublishState<Sch, Camp, A, C>>,
    headers: HeaderMap,
    Json(request): Json<ScheduleCampaignsRequest>,
) -> Response
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    if request.effective_at_ms <= state.clock.now().as_milliseconds_since_epoch() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "effective_at_ms must be in the future",
            &[("effective_at_ms", "OUT_OF_RANGE")],
        );
    }
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => records(campaigns),
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(node_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node to schedule");
        return service_unavailable("campaign");
    };
    let Some(id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()) else {
        return campaign_entropy_unavailable();
    };
    let publish = NewScheduledPublish {
        id: id.to_string(),
        tenant_id,
        store_id,
        node_key: "campaigns".to_owned(),
        node_value,
        effective_at_ms: request.effective_at_ms,
        created_by: context.admin.id.clone(),
        // Authored one node at a time from the Campaigns screen, so it belongs to no release
        // ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).
        release_id: None,
        // This route takes an instant directly, so no wall-clock time was resolved against any
        // store's clock and there is none to name.
        resolved_timezone: None,
    };
    match state.scheduled.schedule(&publish).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.campaigns.schedule",
                "store",
                &store_id.to_string(),
                None,
                serde_json::to_value(serde_json::json!({
                    "campaigns": campaigns.len(),
                    "effective_at_ms": request.effective_at_ms,
                }))
                .ok(),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": publish.id,
                    "effective_at_ms": request.effective_at_ms,
                })),
            )
                .into_response()
        }
        Err(error) => scheduled_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/config/scheduled",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = [ScheduledPublishView], description = "The store's publishes still waiting for their time, soonest \
                                      first: for each, `id`, `node_key`, the node it will publish, \
                                      `effective_at_ms`, `status` and `created_at_ms`. The node's \
                                      value is not returned"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The scheduler's store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "scheduled publishes",
)]
/// A super-admin lists a store's pending scheduled publishes.
async fn admin_list_scheduled<Sch, Camp, A, C>(
    State(state): State<ScheduledPublishState<Sch, Camp, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ScheduledListQuery>,
) -> Response
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    match state.scheduled.list_for_store(tenant_id, store_id).await {
        Ok(rows) => {
            let view: Vec<ScheduledPublishView> = rows
                .into_iter()
                .map(|row| ScheduledPublishView {
                    id: row.id,
                    node_key: row.node_key,
                    effective_at_ms: row.effective_at_ms,
                    status: row.status,
                    created_at_ms: row.created_at_ms,
                })
                .collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => scheduled_error_response(&error),
    }
}

#[utoipa::path(
    delete,
    path = "/admin/config/scheduled/{id}",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    params(
        ("id" = String, Path, description = "The scheduled publish, as listed"),
        ("tenant_id" = String, Query, description = "The tenant it belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Cancelled: it will never be applied. Audited"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no pending scheduled publish with this id: \
                                      it never existed, or was already applied or cancelled", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The scheduler's store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "scheduled publishes",
)]
/// A super-admin cancels a pending scheduled publish.
async fn admin_cancel_scheduled<Sch, Camp, A, C>(
    State(state): State<ScheduledPublishState<Sch, Camp, A, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.scheduled.cancel(tenant_id, &id).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.schedule.cancel",
                "scheduled_publish",
                &id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("pending scheduled publish"),
        Err(error) => scheduled_error_response(&error),
    }
}

/// Maps a scheduled-publish store failure to a retryable `503`, logging the detail rather than leaking it.
pub(in crate::http) fn scheduled_error_response(error: &ScheduledPublishError) -> Response {
    tracing::error!(%error, "a scheduled-publish store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the scheduling service is unavailable",
    )
}
