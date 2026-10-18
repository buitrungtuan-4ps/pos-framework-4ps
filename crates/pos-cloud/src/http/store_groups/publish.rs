// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/store-groups/{group_id}/publish`: one node fanned out over a group's stores
//! (ADR-0122 §4–§7).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::batch::{
    BatchReport, BatchResultView, PublishToStoreGroupRequest, publish_batch_member,
};
use super::batch_node::{BatchNode, BatchNodePlan};
use super::{StoreGroupState, store_group_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{not_found, registry_error_response};
use crate::registry::{EntityStatus, RegistryStore};
use crate::store_groups::{
    BatchOutcome, BatchResult, ConfigBatch, ConfigBatchId, StoreGroup, StoreGroupId,
    StoreGroupStore,
};

#[utoipa::path(
    post,
    path = "/admin/store-groups/{group_id}/publish",
    params(("group_id" = String, Path, description = "The cohort to publish to (a 26-character ULID)")),
    responses(
        (status = 200, description = "The batch ran and every member has an outcome — `applied`, \
                                      `skipped` or `failed`. A `200` does **not** mean every store \
                                      succeeded: a batch is deliberately not atomic (ADR-0122 §5), \
                                      and the per-store report is the answer"),
        (status = 400, description = "An id is not a ULID, the node is not one of the thirteen \
                                      batchable kinds, or its arguments are the wrong shape", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such group in this tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The group is archived or empty, or the instruction itself is \
                                      unpublishable (a menu that will not compile, an empty \
                                      reason-code list) — nothing was written to any store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The store-group, registry or config store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "store groups",
)]
/// A super-admin publishes one node to every member of a store group.
///
/// The fan-out, in order: settle the instruction (which node, and can it compile at all), record
/// the batch, walk the membership recording an outcome for each, close the batch, answer with the
/// whole report. Nothing is written to any store until the instruction has compiled once, so an
/// unpublishable request costs no partial write; after that the batch does not stop, because
/// stopping is what turns one bad shop into fifty un-updated ones (ADR-0122 §5).
pub(super) async fn admin_publish_to_store_group<Grp, Reg, Cfg, A, C>(
    State(state): State<StoreGroupState<Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    Json(request): Json<PublishToStoreGroupRequest>,
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
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, group_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("group_id", &group_id)]) {
            Ok([tenant_id, group_id]) => (TenantId::new(tenant_id), StoreGroupId::new(group_id)),
            Err(refusal) => return refusal,
        };
    let Some(node) = state
        .nodes
        .iter()
        .find(|candidate| candidate.key() == request.node)
    else {
        return unknown_batch_node(&state.nodes, &request.node);
    };

    let members = match publishable_membership(&state.groups, tenant_id, group_id).await {
        Ok(members) => members,
        Err(refusal) => return refusal,
    };

    // Compile the instruction once, before anything is written. A refusal here is the request's
    // own — an unknown flag, a menu that will not compile — and would be the answer at every
    // member, so it refuses the batch rather than being recorded 200 times.
    let plan = match node.plan(tenant_id, &request.arguments).await {
        Ok(plan) => plan,
        Err(refusal) => return refusal,
    };

    // One registry read for the whole batch. The alternative — a read per member — turns the
    // protective check into the batch's dominant cost at the 200-store cap.
    let statuses = match state.registry.list_stores(tenant_id).await {
        Ok(rows) => rows
            .into_iter()
            .map(|row| (row.record.store_id, row.record.status))
            .collect::<std::collections::BTreeMap<_, _>>(),
        Err(error) => return registry_error_response(&error),
    };

    let started_at_ms = state.clock.now().as_milliseconds_since_epoch();
    let Some(batch_id) = mint_ulid(started_at_ms).map(ConfigBatchId::new) else {
        return service_unavailable("store groups");
    };
    let batch = ConfigBatch {
        batch_id,
        tenant_id,
        group_id,
        node: node.key().to_owned(),
        arguments: request.arguments.clone(),
        actor_email: context.admin.email.clone(),
        started_at_ms,
        finished_at_ms: None,
    };
    if let Err(error) = state.groups.start_batch(&batch).await {
        return store_group_error_response(&error);
    }

    let results =
        match fan_out_batch(&state, plan.as_ref(), node, &batch, &members, &statuses).await {
            Ok(results) => results,
            Err(refusal) => return refusal,
        };

    let finished_at_ms = state.clock.now().as_milliseconds_since_epoch();
    if let Err(error) = state
        .groups
        .finish_batch(tenant_id, batch_id, finished_at_ms)
        .await
    {
        return store_group_error_response(&error);
    }

    let report = batch_report(&batch, Some(finished_at_ms), &results);
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "store_group.publish",
        "store_group",
        &group_id.to_string(),
        None,
        Some(serde_json::json!({
            "batch_id": batch_id.to_string(),
            "node": node.key(),
            "audit_action": node.audit_action(),
            "summary": plan.summary(),
            "applied": report.applied,
            "skipped": report.skipped,
            "failed": report.failed,
        })),
    )
    .await;
    (StatusCode::OK, Json(report)).into_response()
}

/// Walks a cohort's membership, publishing to each member and recording what happened.
///
/// The loop is deliberately sequential rather than a `join_all`: ADR-0122 §8 sizes the batch to
/// finish inside one request at 200 members, and firing 200 concurrent config-tree writes at one
/// connection pool trades a bounded few seconds for a saturated database and a fresh set of lost
/// conditional-write races between a batch and itself.
///
/// The `Err` here is not a store failing — that is a recorded `failed`. It is the *report* failing
/// to record, which ADR-0122 §5 makes the one thing that must not be partial, so the batch stops.
async fn fan_out_batch<Grp, Reg, Cfg, A, C>(
    state: &StoreGroupState<Grp, Reg, Cfg, A, C>,
    plan: &dyn BatchNodePlan,
    node: &Arc<dyn BatchNode>,
    batch: &ConfigBatch,
    members: &[StoreId],
    statuses: &std::collections::BTreeMap<StoreId, EntityStatus>,
) -> Result<Vec<BatchResult>, Response>
where
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let mut results = Vec::with_capacity(members.len());
    for &store_id in members {
        let member = publish_batch_member(
            &state.config_trees,
            &state.clock,
            plan,
            node.prerequisites(),
            batch.tenant_id,
            store_id,
            statuses.get(&store_id).copied(),
        )
        .await;
        let result = BatchResult {
            store_id,
            outcome: member.outcome,
            detail: member.detail,
            version_id: member.version_id,
            at_ms: state.clock.now().as_milliseconds_since_epoch(),
        };
        state
            .groups
            .record_result(batch.tenant_id, batch.batch_id, &result)
            .await
            .map_err(|error| store_group_error_response(&error))?;
        results.push(result);
    }
    Ok(results)
}

/// The `400` for a node kind that is not one of the thirteen, naming the ones that are.
///
/// Listing them is the difference between an operator fixing a typo and an operator reading the
/// ADR: three of the sixteen publishes are excluded *by construction* (ADR-0122 §4), and someone
/// who asks for `locale` needs to be told it is per-store, not that it does not exist.
pub(in crate::http) fn unknown_batch_node(nodes: &[Arc<dyn BatchNode>], asked: &str) -> Response {
    let known = nodes
        .iter()
        .map(|node| node.key())
        .collect::<Vec<_>>()
        .join(", ");
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        format!("`{asked}` is not a node a batch can publish; the batchable nodes are {known}"),
        &[("node", "INVALID_ENUM_VALUE")],
    )
}

/// The membership a batch may publish to, or the refusal that says why there is none.
///
/// Both refusals are `422` rather than a batch that runs and reports nothing: an archived cohort
/// and an empty one are states an operator has to *change*, and a `200` carrying zero results
/// reads as "it worked" for a publish that reached no shop at all.
pub(in crate::http) async fn publishable_membership<Grp>(
    groups: &Grp,
    tenant_id: TenantId,
    group_id: StoreGroupId,
) -> Result<Vec<StoreId>, Response>
where
    Grp: StoreGroupStore,
{
    let group = find_store_group(groups, tenant_id, group_id).await?;
    if group.status == EntityStatus::Archived {
        return Err(api_error(
            ErrorStatus::Unprocessable,
            "this store group is archived; restore it before publishing to it",
        ));
    }
    let members = groups
        .list_members(tenant_id, group_id)
        .await
        .map_err(|error| store_group_error_response(&error))?;
    if members.is_empty() {
        return Err(api_error(
            ErrorStatus::Unprocessable,
            "this store group has no members, so a publish to it would reach nobody",
        ));
    }
    Ok(members)
}

/// Finds one group within its tenant, or the `404`/`503` that stands in for it.
async fn find_store_group<Grp>(
    groups: &Grp,
    tenant_id: TenantId,
    group_id: StoreGroupId,
) -> Result<StoreGroup, Response>
where
    Grp: StoreGroupStore,
{
    match groups.list_groups(tenant_id).await {
        Ok(rows) => rows
            .into_iter()
            .map(|row| row.record)
            .find(|group| group.group_id == group_id)
            .ok_or_else(|| not_found("store group")),
        Err(error) => Err(store_group_error_response(&error)),
    }
}

/// Assembles a batch and its per-store results into the report both the publish and the reads return.
pub(super) fn batch_report(
    batch: &ConfigBatch,
    finished_at_ms: Option<i64>,
    results: &[BatchResult],
) -> BatchReport {
    let count = |wanted: BatchOutcome| results.iter().filter(|r| r.outcome == wanted).count();
    BatchReport {
        batch_id: batch.batch_id.to_string(),
        group_id: batch.group_id.to_string(),
        node: batch.node.clone(),
        arguments: batch.arguments.clone(),
        actor_email: batch.actor_email.clone(),
        started_at_ms: batch.started_at_ms,
        finished_at_ms,
        applied: count(BatchOutcome::Applied),
        skipped: count(BatchOutcome::Skipped),
        failed: count(BatchOutcome::Failed),
        results: results
            .iter()
            .map(|result| BatchResultView {
                store_id: result.store_id.to_string(),
                outcome: result.outcome.as_wire().to_owned(),
                detail: result.detail.clone(),
                config_version_id: result.version_id.clone(),
                at_ms: result.at_ms,
            })
            .collect(),
    }
}
