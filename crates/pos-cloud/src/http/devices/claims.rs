// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/claim`, `/claim/{claim_id}/collect` and `/admin/claims/bind`: a box installed with no store
//! shows a code, and the console claims it (ADR-0148).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{DeviceId, StoreId, TenantId};
use serde::{Deserialize, Serialize};

use crate::activation::mint_device_credential;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::claim::{
    BindOutcome, CLAIM_TTL_MS, ClaimStore, CollectOutcome, USER_CODE_ENTROPY, UserCode,
    hash_claim_secret, hash_user_code,
};
use crate::http::{CLAIM_PATH, random_hex_32};

// --- Device claims (`/claim`, `/claim/{claim_id}/collect`, `/admin/claims/bind`) ------------------

/// How often a box waiting to be claimed should ask whether it has been, in seconds.
const CLAIM_POLL_INTERVAL_SECS: u64 = 5;

/// The collaborators the claim routes need, stated independently of [`CloudApp`].
#[derive(Clone)]
struct ClaimState<Cl, A, C> {
    claims: Cl,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the claim sub-router
/// ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)).
///
/// A box installed with no store opens a claim on `/claim` and polls `/claim/{claim_id}/collect`.
/// Neither is authenticated, as `/activate` is not: the box has no credential yet. `/claim` shares
/// `/activate`'s small budget and collection has one sized for polling ([`throttle_sync`](crate::http::throttle_sync)). A console
/// user with `console.devices.manage` binds a code to a device slot on `/admin/claims/bind`, which is
/// audited.
pub fn claim_router<Cl, A, C>(
    claims: Cl,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cl: ClaimStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(CLAIM_PATH, post(open_claim::<Cl, A, C>))
        .route("/claim/{claim_id}/collect", post(collect_claim::<Cl, A, C>))
        .route("/admin/claims/bind", post(admin_bind_claim::<Cl, A, C>))
        .with_state(ClaimState {
            claims,
            admin,
            clock,
            audit,
        })
}

/// What `POST /claim` answers. The box shows `user_code` and keeps `secret` to itself.
#[derive(Debug, Serialize)]
struct OpenedClaim {
    claim_id: String,
    /// The code to show, as `XXXX-XXXX`.
    user_code: String,
    /// 256 bits, as hex. Shown nowhere; presented only to collect.
    secret: String,
    expires_in_secs: i64,
    poll_interval_secs: u64,
}

/// A box opens a claim. Unauthenticated: the box has nothing to authenticate with yet.
async fn open_claim<Cl, A, C>(State(state): State<ClaimState<Cl, A, C>>) -> Response
where
    Cl: ClaimStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let (Some(claim_id), Some(secret), Some(code), Ok(expires_at)) = (
        mint_ulid(now_ms),
        random_hex_32(),
        mint_user_code(),
        pos_proto::time::Timestamp::from_milliseconds_since_epoch(
            now_ms.saturating_add(CLAIM_TTL_MS),
        ),
    ) else {
        tracing::error!("could not read OS entropy to open a claim");
        return service_unavailable("claim");
    };
    match state
        .claims
        .open(
            claim_id,
            hash_user_code(&code),
            hash_claim_secret(&secret),
            expires_at,
        )
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(OpenedClaim {
                claim_id: claim_id.to_string(),
                user_code: code.display(),
                secret,
                expires_in_secs: CLAIM_TTL_MS / 1000,
                poll_interval_secs: CLAIM_POLL_INTERVAL_SECS,
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "a claim could not be opened");
            service_unavailable("claim")
        }
    }
}

/// A box presents its secret to collect.
#[derive(Debug, Deserialize)]
struct CollectClaimRequest {
    secret: String,
}

/// What a collection answers once: the box's slot and its device credential.
#[derive(Debug, Serialize)]
struct CollectedClaim {
    tenant_id: String,
    store_id: String,
    device_id: String,
    /// The `posdev_<id>_<secret>` token, shown this once; only its hash is kept.
    credential: String,
}

/// A box polls for its credential. `202` while nobody has bound the code, the credential once it
/// has, `409` with reason `EXPIRED` when the hour is over, and one `403` for everything else.
async fn collect_claim<Cl, A, C>(
    State(state): State<ClaimState<Cl, A, C>>,
    Path(claim_id): Path<String>,
    Json(request): Json<CollectClaimRequest>,
) -> Response
where
    Cl: ClaimStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let claim_id = match parse_ulid_fields([("claim_id", &claim_id)]) {
        Ok([claim_id]) => claim_id,
        Err(refusal) => return refusal,
    };
    let now = state.clock.now();
    let (Some(credential_id), Some(credential_secret)) = (
        mint_ulid(now.as_milliseconds_since_epoch()),
        random_hex_32(),
    ) else {
        tracing::error!("could not read OS entropy to mint a device credential");
        return service_unavailable("claim");
    };
    let (credential, token) = mint_device_credential(credential_id, &credential_secret);
    match state
        .claims
        .collect(
            claim_id,
            hash_claim_secret(&request.secret),
            &credential,
            now,
        )
        .await
    {
        Ok(CollectOutcome::Collected {
            tenant_id,
            store_id,
            device_id,
        }) => (
            StatusCode::OK,
            Json(CollectedClaim {
                tenant_id: tenant_id.to_string(),
                store_id: store_id.to_string(),
                device_id: device_id.to_string(),
                credential: token,
            }),
        )
            .into_response(),
        Ok(CollectOutcome::Pending) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "status": "PENDING" })),
        )
            .into_response(),
        Ok(CollectOutcome::Expired) => api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "this claim has expired; open a new one",
            &[("claim_id", "EXPIRED")],
        ),
        Ok(CollectOutcome::Refused) => api_error(
            ErrorStatus::PermissionDenied,
            "this claim cannot be collected",
        ),
        Err(error) => {
            tracing::error!(%error, "a claim could not be collected");
            service_unavailable("claim")
        }
    }
}

/// A console user binds the code a box shows to a device slot.
#[derive(Debug, Deserialize)]
struct BindClaimRequest {
    user_code: String,
    tenant_id: String,
    store_id: String,
    device_id: String,
}

/// Binds a claim. Needs `console.devices.manage`, and is audited as `device.claim`.
async fn admin_bind_claim<Cl, A, C>(
    State(state): State<ClaimState<Cl, A, C>>,
    headers: HeaderMap,
    Json(request): Json<BindClaimRequest>,
) -> Response
where
    Cl: ClaimStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id, device_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("device_id", &request.device_id),
    ]) {
        Ok([tenant_id, store_id, device_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            DeviceId::new(device_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(code) = UserCode::parse(&request.user_code) else {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a claim code is eight letters and digits, as the box shows it",
            &[("user_code", "INVALID_VALUE")],
        );
    };
    let outcome = match state
        .claims
        .bind(
            hash_user_code(&code),
            tenant_id,
            store_id,
            device_id,
            state.clock.now(),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(%error, "a claim could not be bound");
            return service_unavailable("claim");
        }
    };
    match outcome {
        BindOutcome::Bound => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "device.claim",
                "device",
                &device_id.to_string(),
                None,
                Some(serde_json::json!({
                    "store_id": store_id.to_string(),
                    "device_id": device_id.to_string(),
                })),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        BindOutcome::Unknown => api_error_with_details(
            ErrorStatus::NotFound,
            "no box is waiting with that code",
            &[("user_code", "NOT_FOUND")],
        ),
        BindOutcome::Expired => api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "that code has expired; the box shows a new one",
            &[("user_code", "EXPIRED")],
        ),
        BindOutcome::AlreadyBound => api_error_with_details(
            ErrorStatus::AlreadyExists,
            "that code has already been used",
            &[("user_code", "ALREADY_BOUND")],
        ),
    }
}

/// Draws a user code from OS entropy, or `None` if the entropy source is unavailable.
fn mint_user_code() -> Option<UserCode> {
    let mut entropy = [0_u8; USER_CODE_ENTROPY];
    getrandom::fill(&mut entropy).ok()?;
    Some(UserCode::from_entropy(entropy))
}
