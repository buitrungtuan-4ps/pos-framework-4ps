// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/sessions`: an admin's own sessions, listed and revoked (ADR-0067 slice 4).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::response::api_error_with_details;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;

use super::admin_service_unavailable;
use crate::auth::admin::{
    AdminStore, SessionSummary, authenticated_admin, current_session_token_hash,
};
use crate::http::{CloudApp, Json, hex_decode_32, hex_encode, not_found};

// --- Console self-service sessions ([ADR-0067] slice 4) -----------------------------------------

/// One of the acting admin's live sessions, as the console lists it. `id` is the opaque revocation
/// handle (hex of `SHA-256(token)` — never the token, and not reversible to it); `current` marks the
/// session making this very request, which the console protects from accidental self-revocation.
#[derive(Debug, Clone, serde::Serialize)]
struct AdminSessionView {
    /// The opaque handle the console revokes this session by.
    id: String,
    /// The client IP the session was minted for, if it was known.
    ip: Option<String>,
    /// The client user-agent the session was minted for, if it was known.
    user_agent: Option<String>,
    /// When the session was minted (Unix ms).
    created_at_ms: i64,
    /// When the session currently expires (Unix ms), after any sliding.
    expires_at_ms: i64,
    /// Whether this is the session making the current request.
    current: bool,
}

/// `GET /admin/sessions` — the acting admin's own live sessions, newest first, with the current one
/// flagged. Self-service: available to any authenticated admin regardless of role (an admin always
/// manages their own sessions), so it is gated by the plain session guard, not a
/// [`ConsolePermission`].
pub(in crate::http) async fn admin_list_sessions<S, R, K, C, A, T, W>(
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
    match app
        .admin
        .list_admin_sessions(&context.admin.id, app.clock.now())
        .await
    {
        Ok(sessions) => {
            let current = current_session_token_hash(&headers);
            let views: Vec<AdminSessionView> = sessions
                .into_iter()
                .map(|session: SessionSummary| AdminSessionView {
                    current: current == Some(session.token_hash),
                    id: hex_encode(&session.token_hash),
                    ip: session.ip,
                    user_agent: session.user_agent,
                    created_at_ms: session.created_at.as_milliseconds_since_epoch(),
                    expires_at_ms: session.expires_at.as_milliseconds_since_epoch(),
                })
                .collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(_) => admin_service_unavailable(),
    }
}

/// `DELETE /admin/sessions/{id}` — revoke one of the acting admin's own sessions by its handle.
/// Scoped to the caller, so an admin can only revoke a session that is theirs: an unknown or
/// other-owned handle is a `404`, never a cross-admin revocation.
pub(in crate::http) async fn admin_revoke_session<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
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
    let Some(token_hash) = hex_decode_32(&id) else {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the session id is not a valid handle",
            &[("id", "INVALID_FORMAT")],
        );
    };
    match app
        .admin
        .revoke_admin_session(&context.admin.id, token_hash)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("session"),
        Err(_) => admin_service_unavailable(),
    }
}

/// `POST /admin/sessions/revoke-others` — revoke every one of the acting admin's sessions except the
/// one making this request ("sign out everywhere else").
pub(in crate::http) async fn admin_revoke_other_sessions<S, R, K, C, A, T, W>(
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
    // The guard succeeded, so a live session cookie is present and this resolves to the current
    // session. If it somehow does not, a never-matching handle revokes every session — a full
    // sign-out, which fails safe (the admin simply signs in again) rather than leaving one behind.
    let except = current_session_token_hash(&headers).unwrap_or([0_u8; 32]);
    match app
        .admin
        .revoke_other_admin_sessions(&context.admin.id, except)
        .await
    {
        Ok(_revoked) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => admin_service_unavailable(),
    }
}
