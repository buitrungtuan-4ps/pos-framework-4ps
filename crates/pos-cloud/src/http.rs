// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud HTTP surface.
//!
//! Four kinds of route:
//!
//!  * `/health` — liveness.
//!  * `/internal/*` — the ingest re-push target the reconciliation loop uses (`docs/roadmap.md`
//!    P7); the primary production path is the NATS cursor feed. Not part of the external contract,
//!    and not authenticated — it is reachable only on the cloud's own private network.
//!  * `/v1/*` — the **public** API external integrators build against. Every data route requires a
//!    scoped per-tenant API key ([`crate::auth::bearer`]) and answers only for the key's own tenant.
//!    Every `/v1` handler carries a [`utoipa::path`] annotation and every response type derives
//!    `utoipa::ToSchema`, so `/v1/openapi.json` is generated from the code and can never drift from
//!    it ([ADR-0019](../../../docs/adr/0019-openapi-generation.md)).
//!  * `/admin/*` — the **interactive** super-admin surface ([`crate::auth::admin`],
//!    [ADR-0034](../../../docs/adr/0034-super-admin-auth.md)): a two-factor login that issues a
//!    host-only session cookie, the session guard the rest of the admin routes stand behind, and —
//!    behind that guard — provisioning of scoped per-tenant API keys ([ADR-0037](../../../docs/adr/0037-api-keys.md)):
//!    issue (returning the token once), list, and revoke. Not part of the public contract, so — like
//!    `/internal` — it is absent from the OpenAPI document.
//!
//! The router is generic over its collaborators — the [`EventStore`], the [`RollupStore`], the
//! [`ApiKeyStore`], the [`AdminStore`], and the [`ClockSource`] — bundled in [`CloudApp`]. Tests
//! drive it against `pos-fakes` and the binary serves it over `store-postgres` with the identical
//! handler code (ADR-0026).

use core::fmt;
use core::fmt::Write as _;
use std::collections::BTreeSet;

use axum::extract::{Path, Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;

use crate::auth::admin::{AdminStore, LoginRequest, authenticate_session, login, logout};
use crate::auth::apikey::{ApiKeyAdminStore, ApiKeyId, ApiKeyStore, Scope, issue};
use crate::auth::bearer::{authenticate, require_scope};
use crate::auth::session::{clear_cookie, set_cookie};
use crate::cloud::{Cloud, DailyRollup};
use crate::dashboard::{RollupError, RollupStore, dashboard};
use crate::openapi::ApiDoc;
use utoipa::OpenApi as _;

/// The super-admin session TTL a [`CloudApp`] uses when the binary does not override it. Eight hours,
/// matching [`crate::config`]'s default; `main.rs` threads the configured value in via
/// [`CloudApp::with_admin_session_ttl_secs`].
const DEFAULT_ADMIN_SESSION_TTL_SECS: u64 = 8 * 60 * 60;

/// Everything a request handler needs, bundled so the router carries one state type: the event
/// store's application layer, the materialised rollup read model, the API-key store the `/v1` bearer
/// check consults, the super-admin store the `/admin` login and session guard use, and the clock both
/// checks verify time against.
///
/// Cloneable and cheap to clone — each collaborator is itself a shared handle (a pool, an `Arc`), so
/// a clone talks to the same backing store.
pub struct CloudApp<S, R, K, C, A> {
    cloud: Cloud<S>,
    rollups: R,
    keys: K,
    clock: C,
    admin: A,
    admin_session_ttl_secs: u64,
}

impl<S, R, K, C, A> fmt::Debug for CloudApp<S, R, K, C, A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The collaborators are opaque handles — a pool, a key store, a clock — and some hold
        // secrets, so the fields are deliberately elided rather than rendered.
        formatter.debug_struct("CloudApp").finish_non_exhaustive()
    }
}

impl<S, R, K, C, A> Clone for CloudApp<S, R, K, C, A>
where
    S: Clone,
    R: Clone,
    K: Clone,
    C: Clone,
    A: Clone,
{
    fn clone(&self) -> Self {
        Self {
            cloud: self.cloud.clone(),
            rollups: self.rollups.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
            admin: self.admin.clone(),
            admin_session_ttl_secs: self.admin_session_ttl_secs,
        }
    }
}

impl<S, R, K, C, A> CloudApp<S, R, K, C, A> {
    /// Bundles the collaborators into one shareable application state, with the default super-admin
    /// session TTL ([`CloudApp::with_admin_session_ttl_secs`] overrides it).
    pub const fn new(cloud: Cloud<S>, rollups: R, keys: K, clock: C, admin: A) -> Self {
        Self {
            cloud,
            rollups,
            keys,
            clock,
            admin,
            admin_session_ttl_secs: DEFAULT_ADMIN_SESSION_TTL_SECS,
        }
    }

    /// Sets how long an issued super-admin session stays valid, in seconds — the binary threads the
    /// configured value in ([`crate::config::CloudConfig::admin_session_ttl_secs`]).
    #[must_use]
    pub const fn with_admin_session_ttl_secs(mut self, secs: u64) -> Self {
        self.admin_session_ttl_secs = secs;
        self
    }
}

/// Builds the cloud router over `app`.
pub fn router<S, R, K, C, A>(app: CloudApp<S, R, K, C, A>) -> Router
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/health", get(health))
        .route("/internal/ingest", post(ingest::<S, R, K, C, A>))
        .route(
            "/v1/stores/{store_id}/rollups/daily",
            get(daily_rollups::<S, R, K, C, A>),
        )
        .route("/v1/openapi.json", get(openapi))
        .route("/admin/login", post(admin_login::<S, R, K, C, A>))
        .route("/admin/logout", post(admin_logout::<S, R, K, C, A>))
        .route("/admin/session", get(admin_session::<S, R, K, C, A>))
        .route(
            "/admin/api-keys",
            post(admin_create_api_key::<S, R, K, C, A>).get(admin_list_api_keys::<S, R, K, C, A>),
        )
        .route(
            "/admin/api-keys/{id}",
            delete(admin_revoke_api_key::<S, R, K, C, A>),
        )
        .with_state(app)
}

/// Liveness: answers as soon as the process is serving.
async fn health() -> &'static str {
    "ok"
}

/// The generated OpenAPI document for the public `/v1` surface.
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Ingests a batch of event envelopes, idempotently. Internal (the reconciliation re-push target),
/// so it is deliberately absent from the public OpenAPI document and carries no authentication.
async fn ingest<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    Json(events): Json<Vec<EventEnvelope<RawPayload>>>,
) -> Response
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
    // R, K, C, A are unused here, but the shared `CloudApp` state must be `Clone + Send + Sync` for
    // the `State` extractor, and that decomposes to every field being so.
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
{
    match app.cloud.ingest(&events).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(error) => error_response(&error),
    }
}

/// A store's per-trading-day activity rollups, oldest day first — answered from the materialised
/// rollup, never a log scan.
///
/// Requires a valid API key with the `read_rollups` scope, and answers **only** for that key's
/// tenant: the tenant comes from the verified grant, never the request, so a caller can read a
/// store's rollups only if the store is within its own tenant. A `store_id` outside the tenant is
/// not an error — it simply has no rollup and reads back as an empty list.
#[utoipa::path(
    get,
    path = "/v1/stores/{store_id}/rollups/daily",
    params(("store_id" = String, Path, description = "The store's 26-character ULID")),
    security(("api_key" = [])),
    responses(
        (status = 200, description = "Daily activity rollups, oldest day first", body = Vec<DailyRollup>),
        (status = 400, description = "The store id is not a ULID"),
        (status = 401, description = "The API key is missing, malformed, or invalid"),
        (status = 403, description = "The API key lacks the read_rollups scope"),
        (status = 503, description = "The rollup store is unreachable"),
    ),
    tag = "rollups",
)]
pub(crate) async fn daily_rollups<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    // S and A are unused here but are part of the shared `CloudApp` state, which the `State`
    // extractor needs whole as `Clone + Send + Sync`.
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
{
    // Identity first: who is calling, and are they who they claim? Then authorisation: may this key
    // read rollups at all? Only then does the request touch a resource.
    let grant = match authenticate(&app.keys, &app.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadRollups) {
        return forbidden.into_response();
    }
    let store_id = match store_id.parse::<Ulid>() {
        Ok(ulid) => StoreId::new(ulid),
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "the store id is not a ULID").into_response();
        }
    };
    // The tenant is the grant's, not the request's — this is the isolation boundary.
    match dashboard(&app.rollups, grant.tenant(), store_id).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

// --- The interactive super-admin surface (`/admin`) ---------------------------------------------

/// Signs a super-admin in: a two-factor login that, on success, sets a host-only session cookie.
///
/// The session token is minted here — a 256-bit CSPRNG value, at the binary edge — and passed to
/// [`login`], which stores only its hash ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)); the
/// browser gets the token in a `__Host-` cookie. Every credential failure is one generic `401`; a
/// store outage is a `503`. Not in the OpenAPI document — this is the admin surface, not the public
/// `/v1` API.
async fn admin_login<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    Json(request): Json<LoginRequest>,
) -> Response
where
    // Only A and C are used, but the whole shared state must be `Clone + Send + Sync` for `State`.
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
{
    let Some(token) = mint_session_token() else {
        // The OS entropy source is unavailable: never mint a token that is not fully random, so fail
        // closed with a retryable status rather than issue a guessable session.
        tracing::error!("could not read OS entropy to mint a session token");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the sign-in service is unavailable",
        )
            .into_response();
    };
    match login(
        &app.admin,
        &app.clock,
        &request,
        &token,
        app.admin_session_ttl_secs,
    )
    .await
    {
        Ok(()) => set_cookie_response(
            StatusCode::NO_CONTENT,
            &set_cookie(&token, app.admin_session_ttl_secs),
        ),
        Err(denied) => denied.into_response(),
    }
}

/// Signs a super-admin out: revokes the session server-side and clears the client cookie.
///
/// Idempotent — a request with no session, or one the store cannot reach, still clears the client
/// cookie, so the browser is always logged out even if the server-side row lingers to its TTL.
async fn admin_logout<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    headers: HeaderMap,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
{
    if let Err(error) = logout(&app.admin, &headers).await {
        // The server-side revoke failed, but clearing the client cookie still logs the browser out;
        // the lingering row expires at its TTL. Log and carry on rather than leave the user unable to
        // sign out.
        tracing::warn!(%error, "revoking an admin session failed; clearing the client cookie anyway");
    }
    set_cookie_response(StatusCode::NO_CONTENT, &clear_cookie())
}

/// Confirms the caller holds a live super-admin session — the guard every other `/admin` route will
/// stand behind, exposed here as a `204`/`401` "am I signed in?" check for the admin UI.
async fn admin_session<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    headers: HeaderMap,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
{
    match authenticate_session(&app.admin, &app.clock, &headers).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(denied) => denied.into_response(),
    }
}

/// A super-admin request to provision a new API key for a tenant.
#[derive(Debug, Clone, Deserialize)]
struct CreateApiKeyRequest {
    /// The tenant the key will act for (a 26-character ULID).
    tenant_id: String,
    /// The scopes to grant, as their wire names (`read_rollups`, …). Deny-by-default: only these.
    scopes: Vec<String>,
    /// An optional expiry, in milliseconds since the Unix epoch. Omit for a key that never expires.
    #[serde(default)]
    expires_at_ms: Option<i64>,
}

/// The one-time response to a provisioning request: the id, and the token shown exactly once.
#[derive(Debug, Clone, serde::Serialize)]
struct CreateApiKeyResponse {
    /// The key's public id (the ULID half of the token).
    id: String,
    /// The full `pos_<id>_<secret>` token. **Shown once** — only its hash is stored, so it cannot be
    /// recovered later.
    token: String,
}

/// The tenant a listing is scoped to.
#[derive(Debug, Clone, Deserialize)]
struct ListApiKeysQuery {
    /// The tenant whose keys to list (a 26-character ULID).
    tenant_id: String,
}

/// Provisions a new scoped per-tenant API key and returns the one-time token (super-admin only).
///
/// Behind the session guard. Mints a CSPRNG id and secret at the edge, [`issue`]s the key, and
/// persists it (storing only the secret's hash). The token in the `201` body is the only time the
/// secret is visible.
async fn admin_create_api_key<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    headers: HeaderMap,
    Json(request): Json<CreateApiKeyRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = authenticate_session(&app.admin, &app.clock, &headers).await {
        return denied.into_response();
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    // Strict: an unknown scope name is a `400`, not a silent drop — the admin is granting explicitly,
    // so a typo must not quietly issue a key that authorises nothing.
    let scopes = match parse_scopes(&request.scopes) {
        Ok(scopes) => scopes,
        Err(unknown) => {
            return (StatusCode::BAD_REQUEST, format!("unknown scope: {unknown}")).into_response();
        }
    };
    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(secret)) = (mint_api_key_id(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint an API key");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the provisioning service is unavailable",
        )
            .into_response();
    };
    let expires_at = match request.expires_at_ms {
        Some(ms) => match pos_proto::time::Timestamp::from_milliseconds_since_epoch(ms) {
            Ok(timestamp) => Some(timestamp),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "expires_at_ms is out of range").into_response();
            }
        },
        None => None,
    };
    let (stored, token) = issue(id, tenant_id, scopes, &secret, expires_at);
    if let Err(error) = app.keys.insert(&stored).await {
        tracing::error!(%error, "persisting a new API key failed");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the provisioning service is unavailable",
        )
            .into_response();
    }
    (
        StatusCode::CREATED,
        Json(CreateApiKeyResponse {
            id: id.to_string(),
            token,
        }),
    )
        .into_response()
}

/// Lists a tenant's API keys as metadata only — never a secret (super-admin only).
async fn admin_list_api_keys<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    headers: HeaderMap,
    Query(query): Query<ListApiKeysQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = authenticate_session(&app.admin, &app.clock, &headers).await {
        return denied.into_response();
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match app.keys.list_for_tenant(tenant_id).await {
        Ok(summaries) => (StatusCode::OK, Json(summaries)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing API keys failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the provisioning service is unavailable",
            )
                .into_response()
        }
    }
}

/// Revokes an API key by id (super-admin only). `204` whether or not a live key was found — revoking
/// is idempotent, and telling the caller which it was is a needless enumeration signal.
async fn admin_revoke_api_key<S, R, K, C, A>(
    State(app): State<CloudApp<S, R, K, C, A>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = authenticate_session(&app.admin, &app.clock, &headers).await {
        return denied.into_response();
    }
    let Ok(id) = id.parse::<Ulid>().map(ApiKeyId::new) else {
        return (StatusCode::BAD_REQUEST, "the key id is not a ULID").into_response();
    };
    match app.keys.revoke(id).await {
        Ok(_found) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "revoking an API key failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the provisioning service is unavailable",
            )
                .into_response()
        }
    }
}

/// Parses wire scope names strictly, returning the first unknown name rather than dropping it — the
/// deny-by-default read tolerance is wrong for a write, where a typo would silently grant nothing.
fn parse_scopes(names: &[String]) -> Result<BTreeSet<Scope>, String> {
    let mut scopes = BTreeSet::new();
    for name in names {
        let scope = Scope::from_wire(name).ok_or_else(|| name.clone())?;
        scopes.insert(scope);
    }
    Ok(scopes)
}

/// Mints an API-key id: a ULID from `now_ms` and 80 CSPRNG bits, or `None` if the OS entropy source
/// is unavailable (the caller then fails closed rather than mint a guessable id).
fn mint_api_key_id(now_ms: i64) -> Option<ApiKeyId> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    let ms = u64::try_from(now_ms.max(0)).unwrap_or(0);
    // `Ulid::from_parts` masks the randomness to the low 80 bits the format defines.
    Some(ApiKeyId::new(Ulid::from_parts(
        ms,
        u128::from_le_bytes(bytes),
    )))
}

/// A response with `status` and one `Set-Cookie` header. A cookie this code built is always a valid
/// header value, so a failure to parse it is impossible; the `unwrap_or_else` keeps a fabricated
/// bad value from taking the process down rather than papering over a real one.
fn set_cookie_response(status: StatusCode, cookie: &str) -> Response {
    let mut response = status.into_response();
    let value =
        HeaderValue::from_str(cookie).unwrap_or_else(|_| HeaderValue::from_static("invalid"));
    response.headers_mut().insert(SET_COOKIE, value);
    response
}

/// Mints a session token: 256 CSPRNG bits as lowercase hex, or `None` if the OS entropy source is
/// unavailable — in which case the caller must fail closed rather than issue a guessable token.
fn mint_session_token() -> Option<String> {
    random_hex_32()
}

/// 32 CSPRNG bytes (256 bits) as a 64-character lowercase hex string, or `None` if the OS entropy
/// source is unavailable. Used for both the session token and an API-key secret — both need a long,
/// unguessable, ASCII-safe value.
fn random_hex_32() -> Option<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String is infallible; the result is ignored deliberately.
        let _ = write!(hex, "{byte:02x}");
    }
    Some(hex)
}

/// Maps a [`PortError`] to an HTTP response, translating the AIP-193 status to a status code so a
/// caller retries the retryable ones (`503`, `429`) and not the terminal ones.
fn error_response(error: &PortError) -> Response {
    let status = match error.status() {
        ErrorStatus::InvalidArgument => StatusCode::BAD_REQUEST,
        ErrorStatus::FailedPrecondition => StatusCode::CONFLICT,
        ErrorStatus::ResourceExhausted => StatusCode::TOO_MANY_REQUESTS,
        ErrorStatus::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string()).into_response()
}

/// Maps a rollup-read failure to a `503`, logging the detail rather than returning it — a dashboard
/// read only fails when the store itself is unreachable, which is transient and the caller's cue to
/// retry, and the internal reason is not the client's business.
fn rollup_error_response(error: &RollupError) -> Response {
    tracing::error!(%error, "a dashboard rollup read failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the dashboard is temporarily unavailable",
    )
        .into_response()
}
