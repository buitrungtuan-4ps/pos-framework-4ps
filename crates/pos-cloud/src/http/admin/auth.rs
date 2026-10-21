// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Signing in (`/admin/login`), first-boot setup, signing out, the session and `whoami`.

use axum::extract::State;
use axum::http::header::USER_AGENT;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::response::{api_error, api_error_with_details, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;

use crate::auth::admin::{
    AdminRole, AdminStore, IMPLICIT_OWNER_EMAIL, IMPLICIT_OWNER_ID, LoginRequest, NewAdminUser,
    SessionMint, authenticate_session, authenticated_admin, login, logout,
};
use crate::auth::enrol::{MIN_PASSWORD_LEN, SetupRequest, build_enrolment, constant_time_eq};
use crate::auth::session::{clear_cookie, set_cookie};
use crate::http::{
    CloudApp, Json, client_ip, header_str, mint_credential, mint_session_token,
    set_cookie_response, too_many_login_attempts,
};

// --- The interactive super-admin surface (`/admin`) ---------------------------------------------

/// Signs a super-admin in: a two-factor login that, on success, sets a host-only session cookie.
///
/// The session token is minted here — a 256-bit CSPRNG value, at the binary edge — and passed to
/// [`login`], which stores only its hash ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)); the
/// browser gets the token in a `__Host-` cookie. Every credential failure is one generic `401`; a
/// store outage is a `503`. Described in the console document (`/admin/openapi.json`), not the
/// integrator one.
#[utoipa::path(
    post,
    path = "/admin/login",
    security(),
    extensions(("x-public-reason" = json!("an email, password and TOTP code (ADR-0034)"))),
    request_body(
        description = "Email, password, and the six-digit TOTP code",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Signed in. The session cookie is set; the body is empty"),
        (status = 401, description = "One generic refusal for every credential failure — wrong \
                                      email, wrong password, wrong or reused TOTP code — so a \
                                      caller cannot tell which", body = crate::openapi_admin::ErrorResponse),
        (status = 429, description = "Too many attempts; `Retry-After` names the wait", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The admin store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_login<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Response
where
    // Only A and C are used, but the whole shared state must be `Clone + Send + Sync` for `State`.
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    // Throttle sign-in attempts before any expensive work ([ADR-0067] slice 5): an online guesser
    // meets a cheap `429` rather than an Argon2id hashing storm, and a refused attempt is not even
    // recorded, so a legitimate admin's next try is not pushed further out. Keyed by client IP today;
    // the per-email key lights up when email login lands.
    let ip = client_ip(&headers, app.trusted_proxy_hops);
    let rate_keys = [format!("ip:{}", ip.unwrap_or("unknown"))];
    if let Err(retry_after_secs) = app
        .login_rate_limiter
        .check_and_record(&rate_keys, app.clock.now())
    {
        return too_many_login_attempts(retry_after_secs);
    }
    let Some(token) = mint_session_token() else {
        // The OS entropy source is unavailable: never mint a token that is not fully random, so fail
        // closed with a retryable status rather than issue a guessable session.
        tracing::error!("could not read OS entropy to mint a session token");
        return service_unavailable("sign-in");
    };
    // Capture the client IP and user-agent so the admin can recognise this session in their own
    // session list ([ADR-0067] slice 4). Behind the P8 reverse proxy the real client IP arrives in
    // `X-Forwarded-For`, not the socket peer (which is the proxy); the user-agent is a plain header.
    let mint = SessionMint {
        token: &token,
        idle_ttl_secs: app.admin_session_idle_ttl_secs,
        absolute_ttl_secs: app.admin_session_ttl_secs,
        ip,
        user_agent: header_str(&headers, USER_AGENT.as_str()),
    };
    match login(&app.admin, &app.clock, &request, &mint).await {
        // The cookie's `Max-Age` is the absolute cap: the browser keeps presenting the token for the
        // whole possible session life, while the server enforces the sliding idle timeout by expiring
        // the row.
        Ok(()) => set_cookie_response(
            StatusCode::NO_CONTENT,
            &set_cookie(&token, app.admin_session_ttl_secs),
        ),
        Err(denied) => denied.into_response(),
    }
}

/// First-boot super-admin enrolment.
///
/// [ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md).
///
/// Token-gated and self-disabling: `404` when no setup token is configured, `401` on a token mismatch
/// (compared in constant time), `400` if the chosen password is shorter than [`MIN_PASSWORD_LEN`],
/// `409` once an administrator is already enrolled, and on success `201` with the one-time TOTP
/// enrolment. The password is hashed with Argon2id under a fresh CSPRNG salt and never stored in the
/// clear; the TOTP secret is generated here and returned exactly once.
#[utoipa::path(
    post,
    path = "/admin/setup",
    security(),
    extensions(("x-public-reason" = json!("the enrolment token from the server's environment, used once"))),
    request_body(
        description = "The enrolment token from the server's environment, and the first \
                       administrator's chosen email and password",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Enrolled. The body carries the one-time TOTP enrolment (QR \
                                      payload and base32 secret) — returned exactly once, never \
                                      readable again"),
        (status = 400, description = "The password is shorter than the minimum", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "The enrolment token is wrong", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "An administrator is already enrolled", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The admin store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_setup<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    Json(request): Json<SetupRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let Some(expected) = app.admin_setup_token.as_deref() else {
        // No token configured: setup is off. Reveal nothing more than "no such route" — so this one
        // keeps its own wording rather than joining `not_found`, whose "no such {entity}" would
        // imply a setup resource that might exist under another name, and it carries no `details`.
        return api_error(ErrorStatus::NotFound, "setup is not enabled");
    };
    if !constant_time_eq(&request.setup_token, expected) {
        return api_error(ErrorStatus::Unauthenticated, "setup failed");
    }
    if request.password.len() < MIN_PASSWORD_LEN {
        // One field, out of range — a `400`, not the `422` this answered before ADR-0096. The
        // difference is what it tells the reader to do: there *is* a field to go and fix, which is
        // exactly what separates `INVALID_ARGUMENT` from `UNPROCESSABLE`.
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("the password must be at least {MIN_PASSWORD_LEN} characters"),
            &[("password", "OUT_OF_RANGE")],
        );
    }
    let Some((secret, phc)) = mint_credential(&request.password) else {
        tracing::error!("could not mint a super-admin credential (entropy or hashing failed)");
        return service_unavailable("setup");
    };
    // `admin_users` first, because since
    // [ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md) that row *is* the
    // credential sign-in reads. It was written second and best-effort while `super_admin` was the
    // authority; leaving that order would have made a store blip here enrol an operator who could
    // then never sign in. `create_admin_user` is first-writer-wins on the `lower(email)` unique
    // index, so it is also the "already enrolled" gate `provision_credential` used to be — and it
    // answers the same way on an upgraded install, whose owner the migration already backfilled.
    let owner = NewAdminUser {
        id: IMPLICIT_OWNER_ID.to_owned(),
        email: IMPLICIT_OWNER_EMAIL.to_owned(),
        name: "Owner".to_owned(),
        role: AdminRole::Owner,
        password_phc: phc.clone(),
        totp_secret: secret.to_vec(),
    };
    match app.admin.create_admin_user(owner).await {
        Ok(true) => {
            // The legacy `super_admin` row, now written second and best-effort: nothing reads it
            // (ADR-0119 §8 keeps it in the schema and stops reading it), so a failure here costs
            // only the ability to downgrade to a build that did read it.
            if let Err(error) = app.admin.provision_credential(phc, secret.to_vec()).await {
                tracing::warn!(
                    %error,
                    "the first admin is enrolled and can sign in, but mirroring the credential into \
                     the legacy super_admin row failed"
                );
            }
            (StatusCode::CREATED, Json(build_enrolment(&secret))).into_response()
        }
        Ok(false) => api_error(
            ErrorStatus::AlreadyExists,
            "an administrator is already enrolled",
        ),
        Err(_) => service_unavailable("setup"),
    }
}

/// Signs a super-admin out: revokes the session server-side and clears the client cookie.
///
/// Idempotent — a request with no session, or one the store cannot reach, still clears the client
/// cookie, so the browser is always logged out even if the server-side row lingers to its TTL.
#[utoipa::path(
    post,
    path = "/admin/logout",
    security(("session_cookie" = [])),
    responses(
        (status = 204, description = "Signed out, and idempotent: a request with no session, or \
                                      one whose store is unreachable, still clears the client \
                                      cookie"),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_logout<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(error) = logout(&app.admin, &headers).await {
        // The server-side revoke failed, but clearing the client cookie still logs the browser out;
        // the lingering row expires at its TTL. Log and carry on rather than leave the user unable to
        // sign out.
        tracing::warn!(%error, "revoking an admin session failed; clearing the client cookie anyway");
    }
    set_cookie_response(StatusCode::NO_CONTENT, &clear_cookie())
}

/// Confirms the caller holds a live super-admin session.
///
/// The guard every other `/admin` route stands behind, exposed here as a `204`/`401` "am I signed
/// in?" check for the admin UI.
#[utoipa::path(
    get,
    path = "/admin/session",
    security(("session_cookie" = [])),
    responses(
        (status = 204, description = "The session is live. The guard every other /admin route \
                                      stands behind, exposed as an \"am I signed in?\" check"),
        (status = 401, description = "No session cookie, or it names no live session", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The session store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_session<S, R, K, C, A, T, W>(
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
    match authenticate_session(&app.admin, &app.clock, &headers).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(denied) => denied.into_response(),
    }
}

/// The acting admin's own identity — id, email, name, role, status.
///
/// So a console can label the signed-in operator and show only the areas their role grants
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 7). Self-service: available
/// to any authenticated admin regardless of role, so it is gated by the plain session guard rather
/// than a [`ConsolePermission`]. It returns the same credential-free [`AdminUser`] shape the roster
/// lists — never a password hash or a TOTP secret — and role gating in the console is only a UX
/// convenience; the server re-checks every route's required permission regardless.
#[utoipa::path(
    get,
    path = "/admin/whoami",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The acting administrator — id, email, role, status — never a \
                                      password hash or a TOTP secret. Role gating in a console is \
                                      a convenience only: the server re-checks every route's \
                                      required permission regardless"),
        (status = 401, description = "No live session", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The admin store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "auth",
)]
pub(crate) async fn admin_whoami<S, R, K, C, A, T, W>(
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
    match authenticated_admin(&app.admin, &app.clock, &headers).await {
        Ok(context) => (StatusCode::OK, Json(context.admin)).into_response(),
        Err(denied) => denied.into_response(),
    }
}
