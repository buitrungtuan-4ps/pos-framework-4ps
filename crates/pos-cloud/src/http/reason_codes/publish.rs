// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/reason-codes`: a tenant's reason codes written to a store's `reason_codes` node.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::reason_codes::{PublishedReasonCodes, ReasonAction, ReasonCode};
use pos_proto::wire_enum::Open;
use pos_proto::{ErrorStatus, WireEnum};
use serde::Deserialize;

use super::reason_code_error_response;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{Json, publish_config_nodes};
use crate::reason_codes::{ReasonCodeStore, to_node as reason_codes_to_node};
use crate::version::records;

// --- Reason-code publish (`/admin/config/reason-codes`, ADR-0115, roadmap B2.2) ----------------

/// The collaborators the reason-code publish needs: the store the entries are read from, the
/// config-tree store the `reason_codes` node is written onto, plus the admin/clock/audit every
/// write carries.
#[derive(Clone)]
struct ConfigReasonCodeState<R, Cfg, A, C> {
    reason_codes: R,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `PUT /admin/config/reason-codes` body: the `(tenant, store)` to push the tenant's authored list
/// to. Like every other node publish, the entries come from what is authored, not from the body.
#[derive(Debug, Clone, Deserialize)]
struct PublishReasonCodesRequest {
    tenant_id: String,
    store_id: String,
}

/// What a reason-code publish put on a store, in the terms that decide whether the control holds.
///
/// The counts, and — the part that matters — **which actions the published list leaves with no
/// reason to cite**. An action with an empty picker is blocked as surely as one the till refuses,
/// so this is the difference between a store that can record a cash paid-in and one that quietly
/// cannot. It is answered rather than refused: an operator may legitimately decide their staff do
/// not comp, and the way to say so is to author no comp reason. What must not happen is that they
/// do it without being told.
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::http) struct ReasonCodePublishSummary {
    /// Every entry that went into the node, retired ones included — they are there so a historic
    /// event still resolves.
    published: usize,
    /// How many of those staff may actually pick.
    active: usize,
    /// The actions no active entry covers, and which that blocks, as wire tokens. Empty is the
    /// healthy answer.
    uncovered_actions: Vec<String>,
}

impl ReasonCodePublishSummary {
    fn of(node: &PublishedReasonCodes) -> Self {
        Self {
            published: node.codes().len(),
            active: node.codes().iter().filter(|code| code.active).count(),
            uncovered_actions: uncovered_actions(node),
        }
    }
}

/// The actions this node offers no active reason for, and which that blocks, in
/// `ReasonAction::ALL` order.
///
/// Two are skipped ([`ReasonAction::blocks_when_uncovered`]). `Unspecified` is the wire enum's
/// mandatory zero value, not an act anyone performs. And a drawer closed over or short still closes
/// where the list offers no reason for it
/// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 12), so the
/// notice must not say staff can no longer record it.
fn uncovered_actions(node: &PublishedReasonCodes) -> Vec<String> {
    ReasonAction::ALL
        .iter()
        .filter(|action| action.blocks_when_uncovered())
        .filter(|action| node.for_action(**action).next().is_none())
        .map(|action| action.as_wire().to_owned())
        .collect()
}

/// Builds the reason-code publish sub-router
/// ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md), B2.2 slice 4).
///
/// One route: assemble the tenant's authored entries into a [`PublishedReasonCodes`], write it as
/// the store's `reason_codes` config node, and version it through the config tree — the same
/// node-merge every other publish uses, so the other Store-level keys survive. Behind
/// [`ConsolePermission::PublishConfig`]: composing what is already authored is the ops act, and
/// authoring the list is the separate one ([`ConsolePermission::ManageReasonCodes`]).
///
/// The edge **replaces** its framework default with this node wholesale rather than merging, which
/// is what makes an empty publish dangerous and why one is refused — see [`admin_publish_reason_codes`].
pub fn config_reason_codes_router<R, Cfg, A, C>(
    reason_codes: R,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/reason-codes",
            axum::routing::put(admin_publish_reason_codes::<R, Cfg, A, C>),
        )
        .with_state(ConfigReasonCodeState {
            reason_codes,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// The `400` an empty reason-code publish earns.
///
/// A published node replaces the framework default **wholesale** (ADR-0115), so publishing an empty
/// list is not "publish nothing" — it is taking every reason away from a trading store, which leaves
/// staff unable to void a mis-keyed line at all. ADR-0115's "absence is not a brick" reasoning runs
/// in this direction too: absence is safe precisely because the framework set is still there, and an
/// empty *publish* is the one way to remove it.
fn empty_reason_code_publish() -> Response {
    api_error_with_details(
        ErrorStatus::FailedPrecondition,
        "a store cannot be published an empty reason list: a published list replaces the \
         framework default, so this would leave staff unable to cite a reason for anything. \
         Author at least one reason first, or leave the store on the framework set by not \
         publishing",
        &[("reason_codes", "EMPTY_NOT_ALLOWED")],
    )
}

/// The `400` a publish earns when an authored entry is valid for no action.
///
/// ADR-0115 puts this guarantee here: an entry a till can offer for nothing is a data-entry mistake,
/// and letting it through would put an unusable row in a picker. The console refuses it at authoring
/// too; this is the check the edge actually relies on, and it also catches a row written before that
/// rule existed or through another path.
fn unusable_reason_code(code: &ReasonCode) -> Response {
    api_error_with_details(
        ErrorStatus::FailedPrecondition,
        format!(
            "the reason {code} is valid for no action, so it would sit on no picker: give it at \
             least one action or retire it before publishing"
        ),
        &[("applies_to", "REQUIRED")],
    )
}

/// Compiles a tenant's authored reason codes into the store's `reason_codes` node, with a summary.
///
/// A publish wants the records, not the versions the read saw: the node is assembled from what is
/// authored now, and a conditional write against any one row would say nothing about the set. Both
/// refusals here are the operator's to fix — an empty list, and an entry no action recognises —
/// and they refuse the batch as a whole rather than one store, because neither depends on which
/// store is being published to
/// ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §3).
pub(in crate::http) async fn reason_code_nodes<R>(
    reason_codes: &R,
    tenant_id: TenantId,
) -> Result<(Vec<(String, serde_json::Value)>, ReasonCodePublishSummary), Response>
where
    R: ReasonCodeStore,
{
    let authored = match reason_codes.list(tenant_id).await {
        Ok(rows) => records(rows),
        Err(error) => return Err(reason_code_error_response(&error)),
    };
    if authored.is_empty() {
        return Err(empty_reason_code_publish());
    }
    if let Some(unusable) = authored.iter().find(|code| {
        code.applies_to.is_empty() || code.applies_to.iter().all(Open::is_unrecognised)
    }) {
        return Err(unusable_reason_code(&unusable.code));
    }
    let node = reason_codes_to_node(authored);
    let summary = ReasonCodePublishSummary::of(&node);
    let Ok(node_value) = serde_json::to_value(node) else {
        tracing::error!("could not serialise a reason code node");
        return Err(service_unavailable("reason codes"));
    };
    Ok((vec![("reason_codes".to_owned(), node_value)], summary))
}

#[utoipa::path(
    put,
    path = "/admin/config/reason-codes",
    request_body(
        description = "The tenant and the store to publish to. The entries come from what the \
                       tenant has authored, never from this body",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published. The body carries the new config version, how many \
                                      entries went on (retired ones included, so a historic event \
                                      still resolves), how many staff may pick, and \
                                      `uncovered_actions` — the acts this list leaves with no \
                                      reason to cite, which a store cannot then record"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The tenant has authored no reasons (a published list \
                                      replaces the framework set, so an empty one would leave the \
                                      store unable to cite anything), or an authored entry is \
                                      valid for no action", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The store's config tree changed under a contended publish", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The reason code store or the config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reason codes",
)]
/// A super-admin publishes the tenant's authored reason codes onto one store's config tree.
async fn admin_publish_reason_codes<R, Cfg, A, C>(
    State(state): State<ConfigReasonCodeState<R, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishReasonCodesRequest>,
) -> Response
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    // Set the `reason_codes` key on the store's Store layer and re-publish it, preserving the other
    // Store-level keys (`menu`, `tax`, `campaigns`, `inventory`, `permissions`, `floor`, …).
    let (nodes, summary) = match reason_code_nodes(&state.reason_codes, tenant_id).await {
        Ok(compiled) => compiled,
        Err(refusal) => return refusal,
    };
    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.reason_codes.publish",
        "store",
        &store_id.to_string(),
        None,
        serde_json::to_value(&summary).ok(),
    )
    .await;
    // The uncovered actions ride back on the response as well as into the trail, so the console can
    // say "published, and nobody can now record a cash paid-in" in the same breath as the success.
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "config_version_id": id.to_string(),
            "published": summary.published,
            "active": summary.active,
            "uncovered_actions": summary.uncovered_actions,
        })),
    )
        .into_response()
}
