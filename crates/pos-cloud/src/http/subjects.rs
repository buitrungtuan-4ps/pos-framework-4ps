// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Subject-request tooling (`/admin/subjects`, ADR-0076): look up, export and erase a data
//! subject.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, parse_ulid_fields};
use pos_cloud_config::publish::publish_config_nodes_with;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{SubjectId, TenantId};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::erasures::{SubjectHolders, append_erasure, requested};
use crate::http::{Json, RegistryTenantQuery, not_found};
use crate::retention::{RetentionError, SubjectStore};

// --- Subject-request tooling (PDPD/GDPR, ADR-0076, Track M5) ---------------------------------------

/// State for the subject-request routes: the subject store, the stores holding a subject that the
/// cloud does not, their config trees, the admin store for the permission gate, the clock (for the
/// erase mask stamp and audit time), and the audit recorder.
#[derive(Clone)]
struct SubjectState<Su, H, Cfg, A, C> {
    subjects: Su,
    /// The stores whose events name a subject (ADR-0206 decision 4).
    holders: H,
    /// The config trees an erasure is published to, on each holding store's `erasures` node.
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A subject lookup — existence and status, without the personal fields. What a lookup returns so the
/// operator can confirm the right subject before exporting or erasing; the values only leave the server
/// on an explicit export.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SubjectMetaView {
    subject_id: String,
    collected_at_ms: i64,
    /// Whether the personal data has already been masked (erased) — a masked row holds no PII.
    masked: bool,
    /// How many personal fields the record has, so the operator sees there is data without seeing it.
    field_count: usize,
}

/// A subject export — the portability/access payload: the record with its field values. This is the one
/// route that returns the personal data, and only to an operator holding `console.subjects.manage`; the
/// audit trail records that an export happened and how many fields, never the values.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SubjectExportView {
    subject_id: String,
    collected_at_ms: i64,
    masked: bool,
    fields: std::collections::BTreeMap<String, String>,
}

/// Builds the subject-request sub-router ([ADR-0076](../../../docs/adr/0076-subject-request-tooling.md),
/// Track M5) — the Data Protection contact's instrument for a PDPD/GDPR access, portability, or erasure
/// request. Three routes, each **per-subject** and tenant-scoped (`?tenant_id=`), behind the owner-only
/// [`ConsolePermission::ManageSubjects`] and audited: `GET /admin/subjects/{id}` looks one up (status,
/// not values); `GET .../export` returns the record for a portability/access request; `POST .../erase`
/// masks it (irreversible, idempotent) for a right-to-erasure request. There is deliberately no
/// list-all or bulk route — a bulk T1 export remains an escalation, not a feature.
///
/// A subject whose personal values a store holds (ADR-0206) is erased at that store: the erase
/// route adds its id to the store's `erasures` node, and `GET .../stores` shows which stores hold
/// it and where the erasure is still pending.
pub fn subjects_router<Su, H, Cfg, A, C>(
    subjects: Su,
    (holders, config_trees): (H, Cfg),
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
    H: SubjectHolders + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/subjects/{subject_id}",
            get(admin_lookup_subject::<Su, H, Cfg, A, C>),
        )
        .route(
            "/admin/subjects/{subject_id}/export",
            get(admin_export_subject::<Su, H, Cfg, A, C>),
        )
        .route(
            "/admin/subjects/{subject_id}/erase",
            post(admin_erase_subject::<Su, H, Cfg, A, C>),
        )
        .route(
            "/admin/subjects/{subject_id}/stores",
            get(admin_subject_stores::<Su, H, Cfg, A, C>),
        )
        .with_state(SubjectState {
            subjects,
            holders,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Parses `?tenant_id=` and the `{subject_id}` path into their ids, or the refusal naming whichever
/// one is not a ULID. Shared by the three subject routes.
///
/// It used to return `Option`, which threw away *which* of the two failed and left each caller to
/// write `"tenant_id or subject_id is not a ULID"` — a refusal that named both because by then the
/// information was gone. Handing back the response keeps it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these three routes return"
)]
fn parse_subject_target(
    tenant_id: &str,
    subject_id: &str,
) -> Result<(TenantId, SubjectId), Response> {
    let ids = parse_ulid_fields([("tenant_id", tenant_id), ("subject_id", subject_id)])?;
    let [tenant, subject] = ids;
    Ok((TenantId::new(tenant), SubjectId::new(subject)))
}

/// What an erasure answers: whether it is done, and whether the record was already masked.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SubjectErased {
    /// `true` once the subject is masked everywhere it is held: always, for a record the cloud
    /// holds. `false` while a store in `pending_store_ids` has still to mask it.
    erased: bool,
    /// Whether it was masked before this request: the cloud's record, or every store's copy.
    already_masked: bool,
    /// The stores asked to mask the subject that have not yet confirmed it (ADR-0206 decision 4).
    /// Absent when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_store_ids: Vec<String>,
}

/// One store whose events name a subject.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SubjectStoreView {
    store_id: String,
    /// Whether the store has confirmed masking the subject (`data.subject.masked`).
    masked: bool,
    /// Whether the store's `erasures` node asks it to, and it has not confirmed yet.
    erasure_pending: bool,
}

/// The stores holding a subject's personal values: ids and standing, never a value.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SubjectStoresView {
    subject_id: String,
    stores: Vec<SubjectStoreView>,
}

#[utoipa::path(
    get,
    path = "/admin/subjects/{subject_id}",
    extensions(("x-permission" = json!("console.subjects.manage"))),
    security(("session_cookie" = [])),
    params(
        ("subject_id" = String, Path, description = "The subject's 26-character ULID — the id an \
                                                     event or an invoice carries in place of the \
                                                     person's data"),
        ("tenant_id" = String, Query, description = "The tenant the subject belongs to (a \
                                                   26-character ULID). Another tenant's subject is \
                                                   not found"),
    ),
    responses(
        (status = 200, body = SubjectMetaView, description = "The subject exists: `subject_id`, `collected_at_ms`, `masked` \
                                      (whether it has been erased) and `field_count`, how many \
                                      personal fields the record holds. No field value is returned. \
                                      This is the step that confirms the right person before an \
                                      export or an erasure. Audited"),
        (status = 400, description = "tenant_id or subject_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.subjects.manage, which only an owner \
                                      holds", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no subject with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The subject store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "subject requests",
)]
/// Looks a subject up — its existence and whether it is masked — without returning the personal fields.
async fn admin_lookup_subject<Su, H, Cfg, A, C>(
    State(state): State<SubjectState<Su, H, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    Path(subject_id): Path<String>,
) -> Response
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
    H: SubjectHolders + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageSubjects,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant, subject) = match parse_subject_target(&query.tenant_id, &subject_id) {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    match state.subjects.fetch(tenant, subject).await {
        Ok(Some(record)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant),
                "subject.lookup",
                "subject",
                &subject.to_string(),
                None,
                Some(serde_json::json!({ "masked": record.masked_at.is_some() })),
            )
            .await;
            (
                StatusCode::OK,
                Json(SubjectMetaView {
                    subject_id: subject.to_string(),
                    collected_at_ms: record.collected_at.as_milliseconds_since_epoch(),
                    masked: record.masked_at.is_some(),
                    field_count: record.fields.len(),
                }),
            )
                .into_response()
        }
        Ok(None) => not_found("subject for this tenant"),
        Err(error) => subject_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/subjects/{subject_id}/export",
    extensions(("x-permission" = json!("console.subjects.manage"))),
    security(("session_cookie" = [])),
    params(
        ("subject_id" = String, Path, description = "The subject's 26-character ULID — the id an \
                                                     event or an invoice carries in place of the \
                                                     person's data"),
        ("tenant_id" = String, Query, description = "The tenant the subject belongs to (a \
                                                   26-character ULID). Another tenant's subject is \
                                                   not found"),
    ),
    responses(
        (status = 200, body = SubjectExportView, description = "The record for an access or portability request: \
                                      `subject_id`, `collected_at_ms`, `masked` and `fields`, the \
                                      personal field values by name. The only route on this surface \
                                      that returns them. An erased record returns its redacted \
                                      values. Audited with the field count, never the values. \
                                      Moving the payload across a border is the operator's to clear \
                                      first"),
        (status = 400, description = "tenant_id or subject_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.subjects.manage, which only an owner \
                                      holds", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no subject with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The subject store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "subject requests",
)]
/// Exports a subject's record — the portability/access payload, including the field values. Audited as
/// an export (field count only). A masked record exports its redacted values (nothing personal remains).
async fn admin_export_subject<Su, H, Cfg, A, C>(
    State(state): State<SubjectState<Su, H, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    Path(subject_id): Path<String>,
) -> Response
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
    H: SubjectHolders + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageSubjects,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant, subject) = match parse_subject_target(&query.tenant_id, &subject_id) {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    match state.subjects.fetch(tenant, subject).await {
        Ok(Some(record)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant),
                "subject.export",
                "subject",
                &subject.to_string(),
                None,
                // Metadata only — the audit trail records that an export happened and its size, never
                // the personal field values it returned.
                Some(serde_json::json!({ "field_count": record.fields.len() })),
            )
            .await;
            (
                StatusCode::OK,
                Json(SubjectExportView {
                    subject_id: subject.to_string(),
                    collected_at_ms: record.collected_at.as_milliseconds_since_epoch(),
                    masked: record.masked_at.is_some(),
                    fields: record.fields,
                }),
            )
                .into_response()
        }
        Ok(None) => not_found("subject for this tenant"),
        Err(error) => subject_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/subjects/{subject_id}/erase",
    extensions(("x-permission" = json!("console.subjects.manage"))),
    security(("session_cookie" = [])),
    params(
        ("subject_id" = String, Path, description = "The subject's 26-character ULID — the id an \
                                                     event or an invoice carries in place of the \
                                                     person's data"),
        ("tenant_id" = String, Query, description = "The tenant the subject belongs to (a \
                                                   26-character ULID). Another tenant's subject is \
                                                   not found"),
    ),
    responses(
        (status = 200, body = SubjectErased, description = "Erased: every personal field value is masked in place and the \
                                      erasure time stamped, while the id and `collected_at` remain so \
                                      the books still reconcile (ADR-0035). Irreversible. Idempotent: \
                                      erasing a subject already masked changes nothing and still \
                                      succeeds. The body is `erased` and `already_masked`. A subject \
                                      the cloud does not hold but a store's events name (ADR-0206) \
                                      is added to each such store's `erasures` node: the body is then \
                                      `erased: false` with `pending_store_ids` until every store has \
                                      confirmed. Audited"),
        (status = 400, description = "A holding store's erasures node already names 1,000 \
                                      subjects, its cap", body = crate::openapi_admin::ErrorResponse),
        (status = 400, description = "tenant_id or subject_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.subjects.manage, which only an owner \
                                      holds", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no subject with that id, and no store's events \
                                      name it", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "Concurrent config publishes kept winning the race", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The subject store or the config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "subject requests",
)]
/// Erases a subject — masks every personal field value in place ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)),
/// keeping the id and `collected_at` so the books still reconcile. Irreversible and idempotent: erasing
/// an already-masked subject is a no-op that still returns `200`. Audited as an erasure.
async fn admin_erase_subject<Su, H, Cfg, A, C>(
    State(state): State<SubjectState<Su, H, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    Path(subject_id): Path<String>,
) -> Response
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
    H: SubjectHolders + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageSubjects,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant, subject) = match parse_subject_target(&query.tenant_id, &subject_id) {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    let record = match state.subjects.fetch(tenant, subject).await {
        Ok(Some(record)) => record,
        Ok(None) => return erase_at_stores(&state, &context, tenant, subject).await,
        Err(error) => return subject_error_response(&error),
    };
    let already_masked = record.masked_at.is_some();
    if !already_masked {
        let masked = record.masked(state.clock.now());
        if let Err(error) = state.subjects.save_masked(&[masked]).await {
            return subject_error_response(&error);
        }
    }
    // A record the cloud kept for a store, a delivery order's recipient (ADR-0215), has a copy at
    // that store: the erasure reaches it on the store's `erasures` node, as a custom value's does.
    let holders = match state.holders.holders(tenant, subject).await {
        Ok(holders) => holders,
        Err(error) => return holders_unavailable(&error),
    };
    let pending = match publish_erasures(&state, tenant, subject, &holders).await {
        Ok(pending) => pending,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant),
        "subject.erase",
        "subject",
        &subject.to_string(),
        None,
        Some(serde_json::json!({
            "already_masked": already_masked,
            "stores": holders.len(),
            "pending": pending.len(),
        })),
    )
    .await;
    (
        StatusCode::OK,
        Json(SubjectErased {
            erased: pending.is_empty(),
            already_masked,
            pending_store_ids: pending,
        }),
    )
        .into_response()
}

/// Puts `subject` on the `erasures` node of each of `holders` that has not confirmed masking it,
/// and returns those stores' ids.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design, as every node publish's is"
)]
async fn publish_erasures<Su, H, Cfg, A, C>(
    state: &SubjectState<Su, H, Cfg, A, C>,
    tenant: TenantId,
    subject: SubjectId,
    holders: &[crate::erasures::SubjectHolder],
) -> Result<Vec<String>, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let mut pending = Vec::new();
    for holder in holders.iter().filter(|holder| holder.masked_time.is_none()) {
        publish_config_nodes_with(
            &state.config_trees,
            &state.clock,
            tenant,
            holder.store_id,
            ConfigLevel::Store,
            |before| append_erasure(before, subject),
        )
        .await?;
        pending.push(holder.store_id.to_string());
    }
    Ok(pending)
}

/// Erases a subject the cloud does not hold at every store whose events name it (ADR-0206 decision
/// 4): its id goes on each store that has not confirmed masking it, on that store's `erasures`
/// node. A subject no store names is not found.
async fn erase_at_stores<Su, H, Cfg, A, C>(
    state: &SubjectState<Su, H, Cfg, A, C>,
    context: &crate::auth::admin::AdminContext,
    tenant: TenantId,
    subject: SubjectId,
) -> Response
where
    H: SubjectHolders,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let holders = match state.holders.holders(tenant, subject).await {
        Ok(holders) if holders.is_empty() => return not_found("subject for this tenant"),
        Ok(holders) => holders,
        Err(error) => return holders_unavailable(&error),
    };
    let pending = match publish_erasures(state, tenant, subject, &holders).await {
        Ok(pending) => pending,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(tenant),
        "subject.erase",
        "subject",
        &subject.to_string(),
        None,
        Some(serde_json::json!({ "stores": holders.len(), "pending": pending.len() })),
    )
    .await;
    (
        StatusCode::OK,
        Json(SubjectErased {
            erased: pending.is_empty(),
            already_masked: pending.is_empty(),
            pending_store_ids: pending,
        }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/admin/subjects/{subject_id}/stores",
    extensions(("x-permission" = json!("console.subjects.manage"))),
    security(("session_cookie" = [])),
    params(
        ("subject_id" = String, Path, description = "The subject's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant the subject belongs to (a \
                                                   26-character ULID)"),
    ),
    responses(
        (status = 200, body = SubjectStoresView, description = "The stores whose events name the subject, whose personal \
                                      custom values stay on the store (ADR-0206): each `store_id`, \
                                      whether it has confirmed masking the subject (`masked`), and \
                                      whether an erasure is waiting for it (`erasure_pending`), at \
                                      most 64. Empty when no store names it. No value is returned. \
                                      Audited"),
        (status = 400, description = "tenant_id or subject_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.subjects.manage, which only an owner \
                                      holds", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The projection or the config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "subject requests",
)]
/// The stores holding a subject, and where an erasure of it is still pending (ADR-0206 decision 4).
async fn admin_subject_stores<Su, H, Cfg, A, C>(
    State(state): State<SubjectState<Su, H, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    Path(subject_id): Path<String>,
) -> Response
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
    H: SubjectHolders + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageSubjects,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant, subject) = match parse_subject_target(&query.tenant_id, &subject_id) {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    let holders = match state.holders.holders(tenant, subject).await {
        Ok(holders) => holders,
        Err(error) => return holders_unavailable(&error),
    };
    let mut stores = Vec::with_capacity(holders.len());
    for holder in holders {
        let masked = holder.masked_time.is_some();
        let requested = if masked {
            false
        } else {
            match state.config_trees.load(tenant, holder.store_id).await {
                Ok(tree) => {
                    requested(tree.as_ref().map(|versioned| &versioned.record)).contains(&subject)
                }
                Err(error) => {
                    tracing::error!(%error, "reading a store's config tree failed");
                    return api_error(ErrorStatus::Unavailable, "the config tree is unavailable");
                }
            }
        };
        stores.push(SubjectStoreView {
            store_id: holder.store_id.to_string(),
            masked,
            erasure_pending: requested,
        });
    }
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant),
        "subject.stores",
        "subject",
        &subject.to_string(),
        None,
        Some(serde_json::json!({ "stores": stores.len() })),
    )
    .await;
    (
        StatusCode::OK,
        Json(SubjectStoresView {
            subject_id: subject.to_string(),
            stores,
        }),
    )
        .into_response()
}

/// Maps a failure to read which stores hold a subject to a retryable `503`.
fn holders_unavailable(error: &pos_ports::PortError) -> Response {
    tracing::error!(%error, "reading which stores hold a subject failed");
    api_error(
        ErrorStatus::Unavailable,
        "the subject service is unavailable",
    )
}

/// Maps a subject-store failure to a retryable `503`, logging the detail rather than leaking it.
fn subject_error_response(error: &RetentionError) -> Response {
    tracing::error!(%error, "a subject-store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the subject service is unavailable",
    )
}

#[cfg(test)]
mod answer_tests {
    //! Each answer serialises to the bytes the `json!` it replaced did.

    use super::SubjectErased;

    #[test]
    fn an_erasure_answers_that_it_is_done() {
        let answer = SubjectErased {
            erased: true,
            already_masked: false,
            pending_store_ids: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(answer).expect("json"),
            serde_json::json!({ "erased": true, "already_masked": false })
        );
    }
}
