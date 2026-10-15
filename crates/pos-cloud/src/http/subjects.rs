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
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{SubjectId, TenantId};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{Json, RegistryTenantQuery, not_found};
use crate::retention::{RetentionError, SubjectStore};

// --- Subject-request tooling (PDPD/GDPR, ADR-0076, Track M5) ---------------------------------------

/// State for the subject-request routes: the subject store, the admin store for the permission gate,
/// the clock (for the erase mask stamp and audit time), and the audit recorder.
#[derive(Clone)]
struct SubjectState<Su, A, C> {
    subjects: Su,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A subject lookup — existence and status, without the personal fields. What a lookup returns so the
/// operator can confirm the right subject before exporting or erasing; the values only leave the server
/// on an explicit export.
#[derive(Debug, Clone, serde::Serialize)]
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
#[derive(Debug, Clone, serde::Serialize)]
struct SubjectExportView {
    subject_id: String,
    collected_at_ms: i64,
    masked: bool,
    fields: std::collections::BTreeMap<String, String>,
}

/// Builds the subject-request sub-router ([ADR-0076](../../../../docs/adr/0076-subject-request-tooling.md),
/// Track M5) — the Data Protection contact's instrument for a PDPD/GDPR access, portability, or erasure
/// request. Three routes, each **per-subject** and tenant-scoped (`?tenant_id=`), behind the owner-only
/// [`ConsolePermission::ManageSubjects`] and audited: `GET /admin/subjects/{id}` looks one up (status,
/// not values); `GET .../export` returns the record for a portability/access request; `POST .../erase`
/// masks it (irreversible, idempotent) for a right-to-erasure request. There is deliberately no
/// list-all or bulk route — a bulk T1 export remains an escalation, not a feature.
pub fn subjects_router<Su, A, C>(
    subjects: Su,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/subjects/{subject_id}",
            get(admin_lookup_subject::<Su, A, C>),
        )
        .route(
            "/admin/subjects/{subject_id}/export",
            get(admin_export_subject::<Su, A, C>),
        )
        .route(
            "/admin/subjects/{subject_id}/erase",
            post(admin_erase_subject::<Su, A, C>),
        )
        .with_state(SubjectState {
            subjects,
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

/// Looks a subject up — its existence and whether it is masked — without returning the personal fields.
async fn admin_lookup_subject<Su, A, C>(
    State(state): State<SubjectState<Su, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    Path(subject_id): Path<String>,
) -> Response
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
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

/// Exports a subject's record — the portability/access payload, including the field values. Audited as
/// an export (field count only). A masked record exports its redacted values (nothing personal remains).
async fn admin_export_subject<Su, A, C>(
    State(state): State<SubjectState<Su, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    Path(subject_id): Path<String>,
) -> Response
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
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

/// Erases a subject — masks every personal field value in place ([ADR-0035](../../../../docs/adr/0035-retention-and-pii-masking.md)),
/// keeping the id and `collected_at` so the books still reconcile. Irreversible and idempotent: erasing
/// an already-masked subject is a no-op that still returns `200`. Audited as an erasure.
async fn admin_erase_subject<Su, A, C>(
    State(state): State<SubjectState<Su, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    Path(subject_id): Path<String>,
) -> Response
where
    Su: SubjectStore + Clone + Send + Sync + 'static,
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
        Ok(None) => {
            return not_found("subject for this tenant");
        }
        Err(error) => return subject_error_response(&error),
    };
    let already_masked = record.masked_at.is_some();
    if !already_masked {
        let masked = record.masked(state.clock.now());
        if let Err(error) = state.subjects.save_masked(&[masked]).await {
            return subject_error_response(&error);
        }
    }
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant),
        "subject.erase",
        "subject",
        &subject.to_string(),
        None,
        Some(serde_json::json!({ "already_masked": already_masked })),
    )
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({ "erased": true, "already_masked": already_masked })),
    )
        .into_response()
}

/// Maps a subject-store failure to a retryable `503`, logging the detail rather than leaking it.
fn subject_error_response(error: &RetentionError) -> Response {
    tracing::error!(%error, "a subject-store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the subject service is unavailable",
    )
}
