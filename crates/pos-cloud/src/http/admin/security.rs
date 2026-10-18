// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An admin's own security: TOTP re-enrolment and recovery codes (ADR-0067 slice 6).

use core::fmt;
use std::fmt::Write;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::mint_ulid;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use serde::Deserialize;

use super::admin_service_unavailable;
use crate::auth::admin::{AdminStore, NewRecoveryCode, authenticated_admin, hash_recovery_code};
use crate::auth::enrol::{TOTP_SECRET_BYTES, build_enrolment};
use crate::http::{CloudApp, Json, RECOVERY_CODE_COUNT};

// --- Console self-service security: TOTP re-enrol + recovery codes ([ADR-0067] slice 6) ---------

/// A request to re-enrol TOTP: the current password, re-confirming the knowledge factor before the
/// possession factor is rotated. [`fmt::Debug`] redacts it.
#[derive(Clone, Deserialize)]
pub(crate) struct ReenrolTotpRequest {
    /// The current super-admin password.
    password: String,
}

impl fmt::Debug for ReenrolTotpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReenrolTotpRequest")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The freshly-generated recovery codes, returned exactly once. [`fmt::Debug`] redacts the codes so
/// they cannot reach a log — they exist in the response body and nowhere else.
#[derive(Clone, serde::Serialize)]
struct RecoveryCodesResponse {
    /// The plaintext codes, shown this once; only their hashes are stored.
    codes: Vec<String>,
    /// How many codes are now available (the count just generated).
    remaining: usize,
}

impl fmt::Debug for RecoveryCodesResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryCodesResponse")
            .field("codes", &"<redacted>")
            .field("remaining", &self.remaining)
            .finish()
    }
}

/// How many unused recovery codes the acting admin has left — the codes themselves are never listed.
#[derive(Debug, Clone, serde::Serialize)]
struct RecoveryCodesStatus {
    /// The number of unused recovery codes.
    remaining: u64,
}

/// A signed-in admin re-enrols their authenticator.
///
/// [ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6. Re-confirms the current
/// password (the knowledge factor) before rotating the TOTP secret (the possession factor), so a
/// session-only attacker — one holding the cookie but not the password — cannot lock the owner out by
/// re-enrolling. On success the new one-time enrolment (QR + base32 secret) is returned once; existing
/// sessions stay valid, and the next sign-in uses the new authenticator.
#[utoipa::path(
    post,
    path = "/admin/totp",
    security(("session_cookie" = [])),
    request_body(
        description = "The acting administrator's current password — re-asked so a session-only \
                       attacker cannot rotate the second factor and lock the owner out",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Re-enrolled. The new one-time enrolment (QR payload and \
                                      base32 secret) is returned once; existing sessions stay \
                                      valid and the next sign-in uses the new authenticator"),
        (status = 401, description = "No live session, or the password is wrong", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The admin store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_reenrol_totp<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<ReenrolTotpRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match authenticated_admin(&app.admin, &app.clock, &headers).await {
        Ok(context) => context,
        Err(denied) => return denied.into_response(),
    };
    // The acting admin's *own* credential, re-read by their own address — before ADR-0119 this read
    // the single `super_admin` row, so an invited admin re-proving their password was checked
    // against the owner's hash and then rotated the owner's secret.
    let credential = match app
        .admin
        .find_admin_login_by_email(&context.admin.email)
        .await
    {
        Ok(Some(login)) => login.credential,
        Ok(None) => {
            return api_error(
                ErrorStatus::FailedPrecondition,
                "no administrator is enrolled",
            );
        }
        Err(error) => {
            tracing::error!(%error, "loading the credential for TOTP re-enrolment failed");
            return admin_service_unavailable();
        }
    };
    if !credential.credential.password_matches(&request.password) {
        // A distinct 403: the caller is signed in but has not re-proved the knowledge factor.
        return api_error(ErrorStatus::PermissionDenied, "the password is incorrect");
    }
    let Some(secret) = mint_totp_secret() else {
        tracing::error!("could not read OS entropy to mint a TOTP secret");
        return admin_service_unavailable();
    };
    match app
        .admin
        .rotate_admin_totp_secret(&context.admin.id, secret.to_vec())
        .await
    {
        Ok(()) => (StatusCode::OK, Json(build_enrolment(&secret))).into_response(),
        Err(error) => {
            tracing::error!(%error, "rotating the TOTP secret failed");
            admin_service_unavailable()
        }
    }
}

/// (Re)generates the acting admin's one-time recovery codes.
///
/// [ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6. Self-service, so it is
/// gated by the session guard, not a role permission. Mints [`RECOVERY_CODE_COUNT`] codes at the
/// edge, stores only their hashes (replacing any previous set), and returns the plaintext once.
#[utoipa::path(
    post,
    path = "/admin/recovery-codes",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "A fresh set of one-time recovery codes, returned exactly \
                                      once. Only their hashes are stored, and any previous set is \
                                      replaced"),
        (status = 401, description = "No live session", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The admin store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_generate_recovery_codes<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match authenticated_admin(&app.admin, &app.clock, &headers).await {
        Ok(context) => context,
        Err(denied) => return denied.into_response(),
    };
    let now = app.clock.now();
    let mut plaintext = Vec::with_capacity(RECOVERY_CODE_COUNT);
    let mut to_store = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _ in 0..RECOVERY_CODE_COUNT {
        let (Some(code), Some(id)) = (
            mint_recovery_code(),
            mint_ulid(now.as_milliseconds_since_epoch()),
        ) else {
            tracing::error!("could not read OS entropy to mint a recovery code");
            return admin_service_unavailable();
        };
        to_store.push(NewRecoveryCode {
            id: id.to_string(),
            code_hash: hash_recovery_code(&code),
        });
        plaintext.push(code);
    }
    match app
        .admin
        .store_recovery_codes(&context.admin.id, to_store)
        .await
    {
        Ok(()) => {
            let remaining = plaintext.len();
            (
                StatusCode::OK,
                Json(RecoveryCodesResponse {
                    codes: plaintext,
                    remaining,
                }),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "storing recovery codes failed");
            admin_service_unavailable()
        }
    }
}

/// How many unused recovery codes the acting admin has left.
///
/// Never the codes themselves — so a console can prompt a regeneration when the supply runs low.
#[utoipa::path(
    get,
    path = "/admin/recovery-codes",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "How many unused recovery codes remain — never the codes \
                                      themselves — so a console can prompt a regeneration before \
                                      the supply runs out"),
        (status = 401, description = "No live session", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The admin store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_recovery_codes_status<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match authenticated_admin(&app.admin, &app.clock, &headers).await {
        Ok(context) => context,
        Err(denied) => return denied.into_response(),
    };
    match app.admin.count_recovery_codes(&context.admin.id).await {
        Ok(remaining) => (StatusCode::OK, Json(RecoveryCodesStatus { remaining })).into_response(),
        Err(error) => {
            tracing::error!(%error, "counting recovery codes failed");
            admin_service_unavailable()
        }
    }
}

/// Mints a fresh TOTP shared secret from OS entropy, or `None` if the entropy source is unavailable —
/// the caller then fails closed rather than install a weak secret.
fn mint_totp_secret() -> Option<[u8; TOTP_SECRET_BYTES]> {
    let mut secret = [0_u8; TOTP_SECRET_BYTES];
    getrandom::fill(&mut secret).ok()?;
    Some(secret)
}

/// Mints a human-typeable one-time recovery code: 64 CSPRNG bits as lowercase hex in four
/// dash-separated groups (`"1a2b-3c4d-5e6f-7a8b"`), or `None` if the entropy source is unavailable.
/// The dashes and case are cosmetic — the code is normalised before hashing, so the admin may type it
/// either way.
fn mint_recovery_code() -> Option<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).ok()?;
    let mut code = String::with_capacity(19);
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 && index.is_multiple_of(2) {
            code.push('-');
        }
        // Writing to a String is infallible; the result is ignored deliberately.
        let _ = write!(code, "{byte:02x}");
    }
    Some(code)
}
