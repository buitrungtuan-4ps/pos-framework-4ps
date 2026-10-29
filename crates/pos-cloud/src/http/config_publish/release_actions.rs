// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Scheduling, cancelling, listing and reading a config release, and its report.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::config_releases::{
    ReleasePairView, ReleaseReport, ReleaseState, ReleaseTenantQuery, ReleaseTenantRequest,
    ReleaseView, ScheduleReleaseRequest, release_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::campaigns::scheduled_error_response;
use crate::http::{Json, not_found, release_schedule};
use crate::registry::RegistryStore;
use crate::releases::{ReleaseStatus, ReleaseStore};
use crate::scheduling::{ScheduledPublishStatus, ScheduledPublishStore};
use crate::store_groups::StoreGroupStore;

/// What the release list answers: the tenant's releases, newest first.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct ReleaseList {
    /// Each release, as its own read reports it.
    releases: Vec<ReleaseView>,
}

#[utoipa::path(
    post,
    path = "/admin/config-releases/{release_id}/schedule",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    params(("release_id" = String, Path, description = "The draft to schedule")),
    request_body(content = ScheduleReleaseRequest, content_type = "application/json"),
    responses(
        (status = 200, body = ReleaseReport, description = "The release is scheduled, and the body is its node × store \
                                      report. Every pair is `PENDING` until its instant arrives"),
        (status = 400, description = "An id is not a ULID, the node is not one of the batchable \
                                      kinds, or a pair would make its store run a value that \
                                      needs a recorded confirmation, which it does not run now \
                                      (the setting key, CONFIRMATION_REQUIRED; confirm it on \
                                      Settings). Nothing is written", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such release in this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The release is not a draft, has no moment, carries no nodes, \
                                      names no reachable store, cannot be timed at one or more \
                                      stores (each named), is missing a prerequisite node at one or \
                                      more stores, or carries an instruction that will not compile. \
                                      **Nothing is written** — a half-written release is the \
                                      half-published Tết this record exists to prevent", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The release, schedule, group, registry or config store is \
                                      unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config releases",
)]
/// A super-admin schedules a draft: the snapshot, the store list and the per-store instants are all
/// taken here, and the pairs are written.
///
/// Nothing is written until everything has resolved. A store that cannot be timed, a node that will
/// not compile, a prerequisite a store is missing — each refuses the whole release, because a
/// half-written release is exactly the half-published Tết this record exists to prevent.
pub(super) async fn admin_schedule_release<Rel, Sch, Grp, Reg, Cfg, A, C>(
    State(state): State<ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path(release_id): Path<String>,
    Json(request): Json<ScheduleReleaseRequest>,
) -> Result<Response, Response>
where
    Rel: ReleaseStore + Clone + Send + Sync + 'static,
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    // The gauntlet, in the record's order (ADR-0125): settle the instruction, expand the target,
    // resolve every instant, check every prerequisite, compile every node — and only then write.
    // Each step refuses with the response to send, so nothing after a refusal runs.
    let context = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await?;
    let [tenant_id] = parse_ulid_fields([("tenant_id", &request.tenant_id)])?;
    let tenant_id = TenantId::new(tenant_id);
    let (release, moment) =
        release_schedule::draft_to_schedule(&state.releases, tenant_id, &release_id, &request)
            .await?;
    let carried = release_schedule::settle_nodes(&state.nodes, &request.nodes)?;
    let (stores, targets) = release_schedule::target_stores(
        (&state.groups, &state.registry, &state.config_trees),
        tenant_id,
        &release,
        &request,
    )
    .await?;
    let resolved = release_schedule::resolve_instants(&moment, &targets)?;
    let carried_keys: Vec<&str> = carried.iter().map(|(node, _)| node.key()).collect();
    release_schedule::check_prerequisites(
        &state.config_trees,
        tenant_id,
        &stores,
        &carried,
        &carried_keys,
    )
    .await?;
    let pairs = release_schedule::compile_pairs(tenant_id, &stores, &carried).await?;
    release_schedule::check_confirmations(&state.config_trees, tenant_id, &pairs).await?;

    // Only now, with every check passed, is anything written.
    let written = release_schedule::schedule_pairs(
        &state.scheduled,
        (tenant_id, &release_id),
        &context.admin.id,
        state.clock.now().as_milliseconds_since_epoch(),
        &resolved,
        pairs,
    )
    .await?;
    let summary = serde_json::json!({
        "nodes": carried_keys,
        "stores": stores.len(),
        "pairs": written,
    });
    Ok(release_schedule::mark_scheduled(&state, &context, (tenant_id, &release_id), summary).await)
}

#[utoipa::path(
    post,
    path = "/admin/config-releases/{release_id}/cancel",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    params(("release_id" = String, Path, description = "The release to withdraw")),
    request_body(content = ReleaseTenantRequest, content_type = "application/json"),
    responses(
        (status = 200, body = ReleaseReport, description = "The still-pending pairs are cancelled and the body is the \
                                      report. Applied pairs are left alone — they are config \
                                      versions now"),
        (status = 400, description = "An id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such release in this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The release has already started applying; roll back the \
                                      versions it published instead (ADR-0095)", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The release or schedule store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config releases",
)]
/// A super-admin withdraws a release that has not started applying.
///
/// Only before `applying`, because once a pair has published there is nothing to cancel — an applied
/// write is a config version, and the way back is ADR-0095's version history, not a release-shaped
/// undo (ADR-0125 §4). Cancelling withdraws the still-pending pairs and leaves any applied ones
/// alone, which is the shape the roll-up reads as `partial`.
pub(super) async fn admin_cancel_release<Rel, Sch, Grp, Reg, Cfg, A, C>(
    State(state): State<ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path(release_id): Path<String>,
    Json(request): Json<ReleaseTenantRequest>,
) -> Response
where
    Rel: ReleaseStore + Clone + Send + Sync + 'static,
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
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
        ConsolePermission::PublishConfig,
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
    let release = match state.releases.fetch_one(tenant_id, &release_id).await {
        Ok(Some(release)) => release,
        Ok(None) => return not_found("release"),
        Err(error) => return release_error_response(&error),
    };
    if matches!(
        release.status,
        ReleaseStatus::Applying | ReleaseStatus::Applied | ReleaseStatus::Partial
    ) {
        return api_error(
            ErrorStatus::Unprocessable,
            "this release has already started applying; roll back the versions it published instead",
        );
    }
    let pairs = match state
        .scheduled
        .list_for_release(tenant_id, &release_id)
        .await
    {
        Ok(pairs) => pairs,
        Err(error) => return scheduled_error_response(&error),
    };
    let mut cancelled = 0_usize;
    for pair in &pairs {
        if pair.status != ScheduledPublishStatus::Pending {
            continue;
        }
        match state.scheduled.cancel(tenant_id, &pair.id).await {
            Ok(true) => cancelled += 1,
            Ok(false) => {}
            Err(error) => return scheduled_error_response(&error),
        }
    }
    let settled = if cancelled == 0 && pairs.is_empty() {
        // Never scheduled. It goes back to nothing rather than to a terminal state that would claim
        // pairs it does not have.
        ReleaseStatus::Draft
    } else {
        ReleaseStatus::Partial
    };
    if let Err(error) = state
        .releases
        .set_status(tenant_id, &release_id, settled)
        .await
    {
        return release_error_response(&error);
    }
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "release.cancelled",
        "release",
        &release_id,
        None,
        Some(serde_json::json!({ "cancelled_pairs": cancelled })),
    )
    .await;
    release_report_response(&state, tenant_id, &release_id).await
}

#[utoipa::path(
    get,
    path = "/admin/config-releases",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)")),
    responses(
        (status = 200, body = ReleaseList, description = "The tenant's releases, newest first, each with its stored \
                                      roll-up state"),
        (status = 400, description = "tenant_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The release store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config releases",
)]
/// The tenant's releases, newest first.
pub(super) async fn admin_list_releases<Rel, Sch, Grp, Reg, Cfg, A, C>(
    State(state): State<ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ReleaseTenantQuery>,
) -> Response
where
    Rel: ReleaseStore + Clone + Send + Sync + 'static,
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
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
    match state.releases.list_for_tenant(tenant_id).await {
        Ok(rows) => {
            let views: Vec<ReleaseView> = rows.iter().map(ReleaseView::of).collect();
            Json(ReleaseList { releases: views }).into_response()
        }
        Err(error) => release_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/config-releases/{release_id}",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("release_id" = String, Path, description = "The release to report on"),
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = ReleaseReport, description = "The release and one row per (node, store) pair, each with its \
                                      status, its instant, the version it published as, and why it \
                                      last failed. Snapshotted node values are deliberately not \
                                      returned"),
        (status = 400, description = "An id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such release in this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The release or schedule store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config releases",
)]
/// One release and the node × store grid of its pairs — finding F9's answer.
pub(super) async fn admin_read_release<Rel, Sch, Grp, Reg, Cfg, A, C>(
    State(state): State<ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path(release_id): Path<String>,
    Query(query): Query<ReleaseTenantQuery>,
) -> Response
where
    Rel: ReleaseStore + Clone + Send + Sync + 'static,
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
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
    release_report_response(&state, tenant_id, &release_id).await
}

/// Assembles the release-and-pairs answer the three routes that return a release share.
///
/// The pairs carry no `node_value`: a release's report is read by a screen showing forty rows, and a
/// snapshotted menu per row is both enormous and, for a priced node, T2 material this read has no
/// reason to hand out.
pub(in crate::http) async fn release_report_response<Rel, Sch, Grp, Reg, Cfg, A, C>(
    state: &ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C>,
    tenant_id: TenantId,
    release_id: &str,
) -> Response
where
    Rel: ReleaseStore,
    Sch: ScheduledPublishStore,
{
    let release = match state.releases.fetch_one(tenant_id, release_id).await {
        Ok(Some(release)) => release,
        Ok(None) => return not_found("release"),
        Err(error) => return release_error_response(&error),
    };
    let pairs = match state
        .scheduled
        .list_for_release(tenant_id, release_id)
        .await
    {
        Ok(pairs) => pairs,
        Err(error) => return scheduled_error_response(&error),
    };
    Json(ReleaseReport {
        release: ReleaseView::of(&release),
        pairs: pairs
            .into_iter()
            .map(|pair| ReleasePairView {
                pair_id: pair.id,
                store_id: pair.store_id.to_string(),
                node: pair.node_key,
                status: pair.status,
                effective_at_ms: pair.effective_at_ms,
                resolved_timezone: pair.resolved_timezone,
                applied_version_id: pair.applied_version_id,
                failure: pair.failure,
            })
            .collect(),
    })
    .into_response()
}

#[cfg(test)]
mod answer_tests {
    //! Each answer serialises to the bytes the `json!` it replaced did.

    use super::ReleaseList;

    #[test]
    fn the_release_list_is_its_releases() {
        let answer = serde_json::to_value(ReleaseList {
            releases: Vec::new(),
        });
        assert_eq!(answer.expect("json"), serde_json::json!({ "releases": [] }));
    }
}
