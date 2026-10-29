// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/sync/stores/{store_id}/report` and `/internal/ota/report`: a store's OTA report.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::{OtaReportRequest, OtaReportState, StoreReportRequest};
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::fleet::OtaReportStore;
use crate::http::internal_guard;

#[utoipa::path(
    post,
    path = "/sync/stores/{store_id}/report",
    security(("device_credential" = ["read_config"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
    ),
    request_body(
        content = StoreReportRequest, description = "`installed`, the release the store now runs, and `self_test_passed` where it has run \
                       its post-install self-test (ADR-0078)",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Recorded"),
        (status = 400, description = "store_id is not a ULID, or installed is blank", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `read_config` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store, or the fleet store, is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "updates",
)]
/// Records one store's OTA report ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)).
/// Internal (the reporting partner of `/internal/ingest`), so it is absent from the public OpenAPI
/// and requires the `X-Pos-Internal-Key` shared secret
/// ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)) on top of the proxy denies.
/// A malformed id or an empty version is a `400`; a store failure is a retryable `503`; success is
/// `204`. The key does not make the report *attributable* — the store id rides in the body — which is
/// A **store** reports the version it is running, on the route that can say who it is.
///
/// The tenant comes from the scoped key and the store from the path, so a report is
/// tenant-attributable — which the `/internal` shape could not be at any strength of secret, because
/// it read both out of the body ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)).
///
/// **What this does not close.** A [`Grant`](crate::auth::apikey::Grant) pins a tenant, not a store,
/// so a key issued to one store can still name a sibling store of the *same tenant* in the path.
/// That is a real residual and it is stated rather than glossed: the move closes cross-tenant
/// forgery, not intra-tenant. It is bounded by what a report *is* — pure telemetry that never changes
/// what any box runs (ADR-0078) — so the worst outcome is a distorted rollout picture for one
/// operator's own fleet. Closing it needs a store-scoped grant, which is a key-issuance change and
/// its own slice.
///
/// Gated on `read_config`, for the reason [`serve_ota_artifact`] is: every provisioned box already
/// carries it, so the OTA path works with no re-provisioning, and a report is not a secret.
pub(super) async fn receive_store_report<R, C, K>(
    State(state): State<OtaReportState<R, C, K>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<StoreReportRequest>,
) -> Response
where
    R: OtaReportStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadConfig) {
        return forbidden.into_response();
    }
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    record_ota_report(
        &state,
        grant.tenant(),
        store_id,
        &request.installed,
        request.self_test_passed,
    )
    .await
}

/// The half both report routes share: check the version is not blank, then write the row.
///
/// Extracted so the two routes cannot drift on validation or on the status they answer — the whole
/// difference between them is meant to be *where identity comes from*, and a shared body makes that
/// true rather than merely intended.
async fn record_ota_report<R, C, K>(
    state: &OtaReportState<R, C, K>,
    tenant_id: TenantId,
    store_id: StoreId,
    installed: &str,
    self_test_passed: Option<bool>,
) -> Response
where
    R: OtaReportStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    let installed = installed.trim();
    if installed.is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "an installed version is required",
            &[("installed", "REQUIRED")],
        );
    }
    match state
        .reports
        .record_report(
            tenant_id,
            store_id,
            installed,
            self_test_passed,
            state.clock.now(),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "recording an OTA report failed");
            service_unavailable("fleet")
        }
    }
}

/// why ADR-0097 records this route as owing a move to `/sync` when it gains a real caller.
pub(super) async fn ingest_ota_report<R, C, K>(
    State(state): State<OtaReportState<R, C, K>>,
    headers: HeaderMap,
    Json(request): Json<OtaReportRequest>,
) -> Response
where
    R: OtaReportStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    if let Err(refusal) = internal_guard(state.internal_shared_secret.as_ref(), &headers) {
        return refusal;
    }
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    record_ota_report(
        &state,
        tenant_id,
        store_id,
        &request.installed,
        request.self_test_passed,
    )
    .await
}
