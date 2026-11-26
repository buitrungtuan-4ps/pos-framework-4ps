// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/einvoices/correction-requests` and `POST .../{request_id}/approve` or `/decline`:
//! HQ's queue of the correction requests store managers sent from the till
//! ([ADR-0250](../../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)).
//!
//! A decision is written onto the store's own `einvoice` node, on its Store layer, as the symbol is
//! (ADR-0240), and published: the store learns it at its next config pull. The node's other fields
//! are kept, a decision for the same bill replaces the earlier one, and the newest
//! [`CORRECTION_DECISIONS_MAX`] are kept. Nothing is issued here: the store's void, once approved,
//! is what the cloud corrects (ADR-0245). Requester and approver are audited by id.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::einvoice::{CORRECTION_DECISIONS_MAX, PublishedEInvoice};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use serde::{Deserialize, Serialize};
use store_postgres::CorrectionRequestRow;

use super::EInvoicesState;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::einvoice::requests::{STATUS_APPROVED, STATUS_DECLINED, STATUS_PENDING};
use crate::http::Json;

/// `GET /admin/einvoices/correction-requests` query.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
pub(crate) struct RequestsQuery {
    tenant_id: String,
    /// `PENDING`, `APPROVED` or `DECLINED`; every request when absent.
    status: Option<String>,
}

/// A decision's body: the tenant the request belongs to.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(crate) struct DecideRequest {
    tenant_id: String,
}

/// One correction request. People are named by id only.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct CorrectionRequestView {
    request_id: String,
    store_id: String,
    bill_id: String,
    amount_minor: i64,
    currency_code: String,
    /// `ADJUSTMENT` or `REPLACEMENT`.
    kind: String,
    #[schema(required = true)]
    reason_code_id: Option<String>,
    /// The requesting employee's id.
    #[schema(required = true)]
    requested_by: Option<String>,
    requested_time_ms: i64,
    /// `PENDING`, `APPROVED` or `DECLINED`.
    status: String,
    #[schema(required = true)]
    decided_by_user_id: Option<String>,
    #[schema(required = true)]
    decided_time_ms: Option<i64>,
    /// The config version that published the decision to the store, on a decision's answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    config_version_id: Option<String>,
}

impl From<CorrectionRequestRow> for CorrectionRequestView {
    fn from(row: CorrectionRequestRow) -> Self {
        Self {
            request_id: row.request_id,
            store_id: row.store_id,
            bill_id: row.bill_id,
            amount_minor: row.amount_minor,
            currency_code: row.currency_code,
            kind: row.kind,
            reason_code_id: row.reason_code_id,
            requested_by: row.requested_by,
            requested_time_ms: row.requested_time_ms,
            status: row.status,
            decided_by_user_id: row.decided_by_user_id,
            decided_time_ms: row.decided_time_ms,
            config_version_id: None,
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/einvoices/correction-requests",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(RequestsQuery),
    responses(
        (status = 200, body = [CorrectionRequestView], description = "The tenant's correction requests, oldest first, at most 1,000, \
                                      optionally of one status. People are named by id only"),
        (status = 400, description = "tenant_id is not a ULID, or status is not PENDING, APPROVED or DECLINED \
                                      (`INVALID_FORMAT` on `status`)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The queue is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "e-invoices",
)]
/// HQ's queue of correction requests (ADR-0250 decision 3).
pub(crate) async fn admin_list_correction_requests<Cfg, A, C>(
    State(state): State<EInvoicesState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RequestsQuery>,
) -> Result<Response, Response>
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await?;
    let [tenant] = parse_ulid_fields([("tenant_id", &query.tenant_id)])?;
    let status = match query.status.as_deref().filter(|status| !status.is_empty()) {
        None => None,
        Some(STATUS_PENDING) => Some(STATUS_PENDING),
        Some(STATUS_APPROVED) => Some(STATUS_APPROVED),
        Some(STATUS_DECLINED) => Some(STATUS_DECLINED),
        Some(_) => {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "status is PENDING, APPROVED or DECLINED",
                &[("status", "INVALID_FORMAT")],
            ));
        }
    };
    let rows = state
        .requests
        .list(TenantId::new(tenant), status)
        .await
        .map_err(|_| service_unavailable("correction requests"))?;
    let views: Vec<CorrectionRequestView> =
        rows.into_iter().map(CorrectionRequestView::from).collect();
    Ok((StatusCode::OK, Json(views)).into_response())
}

#[utoipa::path(
    post,
    path = "/admin/einvoices/correction-requests/{request_id}/approve",
    extensions(("x-permission" = json!("console.einvoices.correct"))),
    security(("session_cookie" = [])),
    params(("request_id" = String, Path, description = "The request (a ULID)")),
    request_body(content = DecideRequest, content_type = "application/json", description = "The tenant"),
    responses(
        (status = 200, body = CorrectionRequestView, description = "Approved, audited with the requester and the approver, and published \
                                      on the store's `einvoice` node: the store's manager may then void the bill once, and the cloud \
                                      corrects its invoice. Approving an approved request publishes it again"),
        (status = 400, description = "An id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.einvoices.correct", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such request in the tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The request was declined (`CORRECTION_ALREADY_DECIDED` on `status`)", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The queue or the configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "e-invoices",
)]
/// HQ approves a correction request (ADR-0250 decision 3).
pub(crate) async fn admin_approve_correction_request<Cfg, A, C>(
    State(state): State<EInvoicesState<Cfg, A, C>>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    Json(body): Json<DecideRequest>,
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    decide(&state, &headers, &request_id, &body, STATUS_APPROVED).await
}

#[utoipa::path(
    post,
    path = "/admin/einvoices/correction-requests/{request_id}/decline",
    extensions(("x-permission" = json!("console.einvoices.correct"))),
    security(("session_cookie" = [])),
    params(("request_id" = String, Path, description = "The request (a ULID)")),
    request_body(content = DecideRequest, content_type = "application/json", description = "The tenant"),
    responses(
        (status = 200, body = CorrectionRequestView, description = "Declined, audited with the requester and the decider, and published \
                                      on the store's `einvoice` node: the void stays refused at the till. Declining a declined \
                                      request publishes it again"),
        (status = 400, description = "An id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.einvoices.correct", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such request in the tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The request was approved (`CORRECTION_ALREADY_DECIDED` on `status`)", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The queue or the configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "e-invoices",
)]
/// HQ declines a correction request (ADR-0250 decision 3).
pub(crate) async fn admin_decline_correction_request<Cfg, A, C>(
    State(state): State<EInvoicesState<Cfg, A, C>>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    Json(body): Json<DecideRequest>,
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    decide(&state, &headers, &request_id, &body, STATUS_DECLINED).await
}

/// Records `status` on a pending request (or repeats a decision already taken, to publish it again),
/// writes it onto the store's node, publishes and audits.
async fn decide<Cfg, A, C>(
    state: &EInvoicesState<Cfg, A, C>,
    headers: &HeaderMap,
    request_id: &str,
    body: &DecideRequest,
    status: &'static str,
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = require_permission(
        &state.admin,
        &state.clock,
        headers,
        ConsolePermission::CorrectInvoices,
    )
    .await?;
    let [tenant, request, decider] = parse_ulid_fields([
        ("tenant_id", &body.tenant_id),
        ("request_id", request_id),
        ("decided_by_user_id", &context.admin.id),
    ])?;
    let tenant = TenantId::new(tenant);
    let unreachable = |_: String| service_unavailable("correction requests");
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let found = state.requests.get(tenant, request.to_string()).await;
    let row = found
        .map_err(unreachable)?
        .ok_or_else(|| api_error(ErrorStatus::NotFound, "no such correction request"))?;
    if row.status == STATUS_PENDING {
        let decided = state.requests.decide(
            tenant,
            request.to_string(),
            status,
            decider.to_string(),
            now_ms,
        );
        decided.await.map_err(unreachable)?;
    }
    let row = state
        .requests
        .get(tenant, request.to_string())
        .await
        .map_err(unreachable)?
        .ok_or_else(|| api_error(ErrorStatus::NotFound, "no such correction request"))?;
    if row.status != status {
        return Err(api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "the request was already decided the other way",
            &[("status", "CORRECTION_ALREADY_DECIDED")],
        ));
    }
    let (store, entry) =
        decision_entry(&row).ok_or_else(|| service_unavailable("correction requests"))?;
    let version = pos_cloud_config::publish::publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant,
        store,
        ConfigLevel::Store,
        |tree| Ok(vec![decision_node(tree, &entry)]),
    )
    .await?;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant),
        &format!(
            "einvoice.correction_request.{}",
            status.to_ascii_lowercase()
        ),
        "correction_request",
        &row.request_id,
        None,
        Some(serde_json::json!({
            "store_id": row.store_id,
            "bill_id": row.bill_id,
            "amount_minor": row.amount_minor,
            "kind": row.kind,
            "requested_by": row.requested_by,
            "decided_by_user_id": row.decided_by_user_id,
        })),
    )
    .await;
    let mut view = CorrectionRequestView::from(row);
    view.config_version_id = Some(version.to_string());
    Ok((StatusCode::OK, Json(view)).into_response())
}

/// The store and the node entry a decided row publishes, or `None` for a row that cannot be read
/// back.
fn decision_entry(row: &CorrectionRequestRow) -> Option<(StoreId, serde_json::Value)> {
    let store = row.store_id.parse::<Ulid>().ok().map(StoreId::new)?;
    let time =
        pos_proto::time::Timestamp::from_milliseconds_since_epoch(row.decided_time_ms?).ok()?;
    let entry = serde_json::json!({
        "bill_id": row.bill_id,
        "decision": format!("CORRECTION_DECISION_{}", row.status),
        "decided_by_user_id": row.decided_by_user_id.as_deref()?,
        "decided_time": serde_json::to_value(time).ok()?,
    });
    Some((store, entry))
}

/// The store's `einvoice` node with `entry` in its `correction_decisions`: the node's other fields
/// kept, an earlier entry for the same bill replaced, and the newest
/// [`CORRECTION_DECISIONS_MAX`] kept.
pub(crate) fn decision_node(
    tree: Option<&ConfigTreeState>,
    entry: &serde_json::Value,
) -> (String, serde_json::Value) {
    let mut node = tree
        .and_then(|tree| tree.layer(ConfigLevel::Store).get(PublishedEInvoice::NODE))
        .filter(|node| node.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut decisions: Vec<serde_json::Value> = node
        .get("correction_decisions")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    decisions.retain(|held| held.get("bill_id") != entry.get("bill_id"));
    decisions.push(entry.clone());
    let excess = decisions.len().saturating_sub(CORRECTION_DECISIONS_MAX);
    decisions.drain(..excess);
    node["correction_decisions"] = serde_json::Value::Array(decisions);
    (PublishedEInvoice::NODE.to_owned(), node)
}

#[cfg(test)]
mod tests {
    use super::decision_node;

    #[test]
    fn a_decision_replaces_the_bills_entry_keeps_the_node_and_the_newest_two_hundred() {
        let mut node = serde_json::json!({ "symbol": "1C26MAA", "correction_decisions": [] });
        for bill in 0..205 {
            let entry = serde_json::json!({ "bill_id": format!("B{bill}"), "decision": "CORRECTION_DECISION_APPROVED" });
            node["correction_decisions"]
                .as_array_mut()
                .expect("a list")
                .push(entry);
        }
        let tree = crate::config_tree::ConfigTreeState {
            layers: [
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({ "einvoice": node }),
                serde_json::json!({}),
            ],
            history: Vec::new(),
            k: 3,
        };
        let entry =
            serde_json::json!({ "bill_id": "B204", "decision": "CORRECTION_DECISION_DECLINED" });
        let (key, written) = decision_node(Some(&tree), &entry);
        assert_eq!(key, "einvoice");
        assert_eq!(written["symbol"], "1C26MAA");
        let list = written["correction_decisions"].as_array().expect("a list");
        assert_eq!(list.len(), 200);
        assert_eq!(list[0]["bill_id"], "B5");
        assert_eq!(list[199], entry);
        assert_eq!(
            list.iter().filter(|held| held["bill_id"] == "B204").count(),
            1
        );
    }
}
