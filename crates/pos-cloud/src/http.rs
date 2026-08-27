// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud HTTP surface.
//!
//! Five kinds of route:
//!
//!  * `/health` — liveness.
//!  * `/internal/*` — the ingest re-push target and the reconciliation diff the fleet uses
//!    (`docs/roadmap.md` P7): `/internal/ingest` (the primary production path is the NATS cursor
//!    feed) and `/internal/reconcile` ([ADR-0040](../../../docs/adr/0040-reconciliation.md), an edge
//!    asking which ids the cloud is missing). Not part of the external contract, and not
//!    authenticated — reachable only on the cloud's own private network. Built by [`reconcile_router`]
//!    (its own state) and merged into the main router.
//!  * `/v1/*` — the **public** API external integrators build against. Every data route requires a
//!    scoped per-tenant API key ([`crate::auth::bearer`]) and answers only for the key's own tenant.
//!    Every `/v1` handler carries a [`utoipa::path`] annotation and every response type derives
//!    `utoipa::ToSchema`, so `/v1/openapi.json` is generated from the code and can never drift from
//!    it ([ADR-0019](../../../docs/adr/0019-openapi-generation.md)).
//!  * `/sync/*` — the **store-facing** surface a first-party store operates its own state on:
//!    pulling configuration ([ADR-0039](../../../docs/adr/0039-config-delivery.md), `read_config`
//!    scope) and proposing/reading its devices ([ADR-0041](../../../docs/adr/0041-device-onboarding.md),
//!    `manage_devices` scope, via [`device_router`]). Bearer-authed, tenant-isolated, and — being
//!    store operation rather than an integrator API — absent from the public OpenAPI, like `/admin`
//!    and `/internal`.
//!  * `/admin/*` — the **interactive** super-admin surface ([`crate::auth::admin`],
//!    [ADR-0034](../../../docs/adr/0034-super-admin-auth.md)): a two-factor login that issues a
//!    host-only session cookie, the session guard the rest of the admin routes stand behind, and —
//!    behind that guard — provisioning of scoped per-tenant API keys ([ADR-0037](../../../docs/adr/0037-api-keys.md)):
//!    issue (returning the token once), list, and revoke; authoring the four-level configuration
//!    tree ([ADR-0033](../../../docs/adr/0033-config-tree.md)): publish a level of a store's tree
//!    (validated, versioned) and read its effective document; and registering webhook endpoints
//!    ([ADR-0032](../../../docs/adr/0032-webhooks.md)): register a destination (SSRF-vetted, returning
//!    the signing secret once), list, and delete; resolving device-onboarding proposals
//!    ([ADR-0041](../../../docs/adr/0041-device-onboarding.md), via [`device_router`]): list the
//!    pending queue and approve or reject; and authoring the translation grid
//!    ([ADR-0043](../../../docs/adr/0043-translation-grid.md), via [`translation_router`]): read and
//!    replace a tenant's localized strings, `en`-validated. Not part of the public contract, so —
//!    like `/internal` — it is absent from the OpenAPI document.
//!
//! The router is generic over its collaborators — the [`EventStore`], the [`RollupStore`], the
//! [`ApiKeyStore`], the [`AdminStore`], the [`ConfigTreeStore`], the [`WebhookEndpointStore`], and the
//! [`ClockSource`] — bundled in [`CloudApp`]. Tests drive it against `pos-fakes` and the binary serves
//! it over `store-postgres` with the identical handler code (ADR-0026).

use core::fmt;
use core::fmt::Write as _;
use std::collections::BTreeSet;

use argon2::password_hash::SaltString;
use axum::extract::{Path, Query, Request, State};
use axum::http::header::{
    CONTENT_SECURITY_POLICY, REFERRER_POLICY, RETRY_AFTER, SET_COOKIE, USER_AGENT,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use pos_ports::PortError;
use pos_ports::config_store::ConfigUpdate;
use pos_ports::event_store::EventStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{
    ConfigVersionId, DeviceId, DisplayCategoryId, DisplaySubcategoryId, EventId, MenuItemId,
    StoreId, TaxClassId, TenantId,
};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use pos_core::activation::{ActivationCode, Redemption, redeem};

use crate::activation::{ActivationCodeStore, hash_code, mint_device_credential};
use crate::auth::admin::{
    AdminContext, AdminRole, AdminStatus, AdminStore, IMPLICIT_OWNER_EMAIL, IMPLICIT_OWNER_ID,
    LoginRequest, NewAdminInvite, NewAdminUser, NewRecoveryCode, SessionDenied, SessionMint,
    SessionSummary, authenticate_session, authenticated_admin, current_session_token_hash,
    hash_recovery_code, hash_session_token, login, logout,
};
use crate::auth::apikey::{ApiKeyAdminStore, ApiKeyId, ApiKeyStore, Scope, issue};
use crate::auth::bearer::{authenticate, require_scope};
use crate::auth::console_rbac::{ConsolePermission, role_grants};
use crate::auth::enrol::{
    MIN_PASSWORD_LEN, SetupRequest, TOTP_SECRET_BYTES, build_enrolment, constant_time_eq,
};
use crate::auth::password::hash_password;
use crate::auth::rate_limit::LoginRateLimiter;
use crate::auth::session::{clear_cookie, set_cookie};
use crate::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, ChannelPrice, DisplayCategory,
    DisplaySubcategory, ItemCategory, ItemCategoryId, ItemSubcategory, ItemSubcategoryId,
    LayoutButton, Menu, MenuId, MenuPlacement, MenuSection, MenuSectionId, ModifierGroup,
    ModifierGroupId, TaxClass,
};
use crate::catalog_compiler::{compile_layout_book, compile_menu};
use crate::cloud::{Cloud, DailyRollup};
use crate::config_tree::{
    CapabilityValidator, ConfigError, ConfigLevel, ConfigTree, ConfigTreeStore, SyncOutcome,
};
use crate::dashboard::{RollupError, RollupStore, StoredRollups, dashboard};
use crate::devices::{
    DeviceKind, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore, DeviceProposalSummary,
    PersistedDeviceProposal,
};
use crate::openapi::ApiDoc;
use crate::reconcile::ReconcileStore;
use crate::registry::{
    BrandId, BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError,
    StoreRecord, TenantRecord,
};
use crate::translations::{TranslationGrid, TranslationStore};
use crate::webhook::{
    PersistedWebhook, SigningSecret, WebhookEndpointId, WebhookEndpointStore, WebhookSummary, vet,
};
use utoipa::OpenApi as _;

/// The super-admin session TTL a [`CloudApp`] uses when the binary does not override it. Eight hours,
/// matching [`crate::config`]'s default; `main.rs` threads the configured value in via
/// [`CloudApp::with_admin_session_ttl_secs`].
const DEFAULT_ADMIN_SESSION_TTL_SECS: u64 = 8 * 60 * 60;

/// How long a console-admin invitation stays acceptable when the binary does not override it — three
/// days, long enough to hand the copy-invite-link over out-of-band without leaving a stale
/// credential-granting token valid indefinitely ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
const DEFAULT_ADMIN_INVITE_TTL_SECS: u64 = 3 * 24 * 60 * 60;

/// How long a console-admin session may sit idle before it expires when the binary does not override
/// it — thirty minutes, the sliding idle window bounded by the absolute session cap
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4). `main.rs` threads the
/// configured value in via [`CloudApp::with_admin_session_idle_ttl_secs`].
const DEFAULT_ADMIN_SESSION_IDLE_TTL_SECS: u64 = 30 * 60;

/// How many `/admin/login` attempts one client may make within the window before a `429`, when the
/// binary does not override it ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 5).
const DEFAULT_ADMIN_LOGIN_MAX_ATTEMPTS: usize = 10;

/// The sliding `/admin/login` rate-limit window, in seconds, when the binary does not override it.
const DEFAULT_ADMIN_LOGIN_WINDOW_SECS: u64 = 5 * 60;

/// How many one-time recovery codes a generation issues at once
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6) — ten, the familiar
/// batch, enough to survive several lost-authenticator events before regenerating.
const RECOVERY_CODE_COUNT: usize = 10;

/// The `Content-Security-Policy` for the admin console
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 5). The built SPA loads one
/// external module script and one external stylesheet from its own origin (see
/// `dashboard/dist/index.html`), so scripts are locked to `'self'` with no inline allowance — the
/// strongest lever against injected script. `style-src` keeps `'unsafe-inline'` because the SolidJS
/// components set inline styles at runtime; an injected *style* is a far weaker foothold than an
/// injected *script*, so this is a deliberate, bounded relaxation. Images allow `data:`/`blob:` for
/// the embedded catalog thumbnails; `frame-ancestors 'none'` backs up `X-Frame-Options: DENY` against
/// clickjacking, and `base-uri`/`form-action`/`object-src` are pinned shut.
const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; \
     font-src 'self'; \
     connect-src 'self'; \
     object-src 'none'; \
     base-uri 'self'; \
     form-action 'self'; \
     frame-ancestors 'none'";

/// Everything a request handler needs, bundled so the router carries one state type: the event
/// store's application layer, the materialised rollup read model, the API-key store the `/v1` bearer
/// check consults, the super-admin store the `/admin` login and session guard use, the config-tree
/// store the `/admin` config routes author, the webhook-endpoint store the `/admin` webhook routes
/// register into, and the clock the checks verify time against.
///
/// Cloneable and cheap to clone — each collaborator is itself a shared handle (a pool, an `Arc`), so
/// a clone talks to the same backing store.
pub struct CloudApp<S, R, K, C, A, T, W> {
    cloud: Cloud<S>,
    rollups: R,
    keys: K,
    clock: C,
    admin: A,
    config_trees: T,
    webhooks: W,
    admin_session_ttl_secs: u64,
    admin_session_idle_ttl_secs: u64,
    admin_invite_ttl_secs: u64,
    admin_setup_token: Option<String>,
    login_rate_limiter: LoginRateLimiter,
}

impl<S, R, K, C, A, T, W> fmt::Debug for CloudApp<S, R, K, C, A, T, W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The collaborators are opaque handles — a pool, a key store, a clock — and some hold
        // secrets, so the fields are deliberately elided rather than rendered.
        formatter.debug_struct("CloudApp").finish_non_exhaustive()
    }
}

impl<S, R, K, C, A, T, W> Clone for CloudApp<S, R, K, C, A, T, W>
where
    S: Clone,
    R: Clone,
    K: Clone,
    C: Clone,
    A: Clone,
    T: Clone,
    W: Clone,
{
    fn clone(&self) -> Self {
        Self {
            cloud: self.cloud.clone(),
            rollups: self.rollups.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
            admin: self.admin.clone(),
            config_trees: self.config_trees.clone(),
            webhooks: self.webhooks.clone(),
            admin_session_ttl_secs: self.admin_session_ttl_secs,
            admin_session_idle_ttl_secs: self.admin_session_idle_ttl_secs,
            admin_invite_ttl_secs: self.admin_invite_ttl_secs,
            admin_setup_token: self.admin_setup_token.clone(),
            login_rate_limiter: self.login_rate_limiter.clone(),
        }
    }
}

impl<S, R, K, C, A, T, W> CloudApp<S, R, K, C, A, T, W> {
    /// Bundles the collaborators into one shareable application state, with the default super-admin
    /// session TTL ([`CloudApp::with_admin_session_ttl_secs`] overrides it) and a default login
    /// rate-limiter ([`CloudApp::with_login_rate_limit`] overrides it).
    pub fn new(
        cloud: Cloud<S>,
        rollups: R,
        keys: K,
        clock: C,
        admin: A,
        config_trees: T,
        webhooks: W,
    ) -> Self {
        Self {
            cloud,
            rollups,
            keys,
            clock,
            admin,
            config_trees,
            webhooks,
            admin_session_ttl_secs: DEFAULT_ADMIN_SESSION_TTL_SECS,
            admin_session_idle_ttl_secs: DEFAULT_ADMIN_SESSION_IDLE_TTL_SECS,
            admin_invite_ttl_secs: DEFAULT_ADMIN_INVITE_TTL_SECS,
            admin_setup_token: None,
            login_rate_limiter: LoginRateLimiter::new(
                DEFAULT_ADMIN_LOGIN_MAX_ATTEMPTS,
                DEFAULT_ADMIN_LOGIN_WINDOW_SECS,
            ),
        }
    }

    /// Sets how long an issued super-admin session stays valid, in seconds — the binary threads the
    /// configured value in ([`crate::config::CloudConfig::admin_session_ttl_secs`]).
    #[must_use]
    pub const fn with_admin_session_ttl_secs(mut self, secs: u64) -> Self {
        self.admin_session_ttl_secs = secs;
        self
    }

    /// Sets how long an admin session may sit idle before it expires, in seconds — the sliding idle
    /// window bounded by [`Self::with_admin_session_ttl_secs`]; the binary threads the configured
    /// value in ([`crate::config::CloudConfig::admin_session_idle_ttl_secs`]).
    #[must_use]
    pub const fn with_admin_session_idle_ttl_secs(mut self, secs: u64) -> Self {
        self.admin_session_idle_ttl_secs = secs;
        self
    }

    /// Sets the `/admin/login` rate limit — at most `max_attempts` sign-in attempts per client within
    /// a `window_secs` sliding window ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)
    /// slice 5); the binary threads the configured values in
    /// ([`crate::config::CloudConfig::admin_login_max_attempts`],
    /// [`crate::config::CloudConfig::admin_login_window_secs`]).
    #[must_use]
    pub fn with_login_rate_limit(mut self, max_attempts: usize, window_secs: u64) -> Self {
        self.login_rate_limiter = LoginRateLimiter::new(max_attempts, window_secs);
        self
    }

    /// Sets how long a console-admin invitation stays acceptable, in seconds — the binary threads the
    /// configured value in ([`crate::config::CloudConfig::admin_invite_ttl_secs`]).
    #[must_use]
    pub const fn with_admin_invite_ttl_secs(mut self, secs: u64) -> Self {
        self.admin_invite_ttl_secs = secs;
        self
    }

    /// Sets the one-time super-admin setup token that gates first-boot enrolment
    /// ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md)) — the binary threads in the
    /// configured value ([`crate::config::CloudConfig::admin_setup_token`]). `None` leaves
    /// `/admin/setup` disabled (a `404`), the posture once an admin is enrolled and the token removed.
    #[must_use]
    pub fn with_admin_setup_token(mut self, token: Option<String>) -> Self {
        self.admin_setup_token = token;
        self
    }
}

/// Builds the cloud router over `app`.
#[expect(
    clippy::too_many_lines,
    reason = "a flat registration of every cloud route in one place; splitting it would scatter the \
              route table across helpers and obscure the surface, which is the opposite of clear"
)]
pub fn router<S, R, K, C, A, T, W>(app: CloudApp<S, R, K, C, A, T, W>) -> Router
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/health", get(health))
        .route("/internal/ingest", post(ingest::<S, R, K, C, A, T, W>))
        .route(
            "/v1/stores/{store_id}/rollups/daily",
            get(daily_rollups::<S, R, K, C, A, T, W>),
        )
        .route("/v1/openapi.json", get(openapi))
        .route(
            "/sync/stores/{store_id}/config",
            get(edge_config_sync::<S, R, K, C, A, T, W>),
        )
        .route("/admin/login", post(admin_login::<S, R, K, C, A, T, W>))
        .route("/admin/logout", post(admin_logout::<S, R, K, C, A, T, W>))
        .route("/admin/session", get(admin_session::<S, R, K, C, A, T, W>))
        .route("/admin/whoami", get(admin_whoami::<S, R, K, C, A, T, W>))
        .route(
            "/admin/sessions",
            get(admin_list_sessions::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/sessions/revoke-others",
            post(admin_revoke_other_sessions::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/sessions/{id}",
            delete(admin_revoke_session::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/totp",
            post(admin_reenrol_totp::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/recovery-codes",
            post(admin_generate_recovery_codes::<S, R, K, C, A, T, W>)
                .get(admin_recovery_codes_status::<S, R, K, C, A, T, W>),
        )
        .route("/admin/setup", post(admin_setup::<S, R, K, C, A, T, W>))
        .route(
            "/admin/api-keys",
            post(admin_create_api_key::<S, R, K, C, A, T, W>)
                .get(admin_list_api_keys::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/api-keys/{id}",
            delete(admin_revoke_api_key::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/admins",
            get(admin_list_admins::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/admins/{id}/role",
            axum::routing::patch(admin_set_admin_role::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/admins/{id}/status",
            axum::routing::patch(admin_set_admin_status::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/invites",
            post(admin_invite_admin::<S, R, K, C, A, T, W>)
                .get(admin_list_invites::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/invites/accept",
            post(admin_accept_invite::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/invites/{id}",
            delete(admin_revoke_invite::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/config",
            get(admin_config_effective::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/config/{level}",
            axum::routing::put(admin_config_publish::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/rollups/reset",
            post(admin_rollups_reset::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/rollups/daily",
            get(admin_daily_rollups::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/webhooks",
            post(admin_register_webhook::<S, R, K, C, A, T, W>)
                .get(admin_list_webhooks::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/webhooks/{id}",
            delete(admin_delete_webhook::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/webhooks/{id}/enable",
            post(admin_enable_webhook::<S, R, K, C, A, T, W>),
        )
        .with_state(app)
        // The admin-console security headers ([ADR-0067] slice 5) on every response this router
        // serves. `main.rs` applies the same layer to the fully-composed service so the SPA fallback
        // and the other merged routers are covered too; re-inserting the same header values there is
        // idempotent.
        .layer(axum::middleware::from_fn(security_headers))
}

/// Builds the reconciliation sub-router, stated independently of [`CloudApp`].
///
/// `POST /internal/reconcile` is the cloud's half of reconciliation ([ADR-0040](../../../docs/adr/0040-reconciliation.md)):
/// an edge sends the ids it holds for a store, and the cloud answers with the subset it is missing —
/// the ids to re-push through `/internal/ingest`. It needs only the [`ReconcileStore`], so it carries
/// its own state and is merged into the main router in `main`, rather than threading an extra
/// collaborator through every `CloudApp` handler. Internal, private-network, and absent from the
/// public OpenAPI, exactly like `/internal/ingest`.
pub fn reconcile_router<Rec>(store: Rec) -> Router
where
    Rec: ReconcileStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/internal/reconcile", post(reconcile::<Rec>))
        .with_state(store)
}

/// Liveness: answers as soon as the process is serving.
async fn health() -> &'static str {
    "ok"
}

/// An edge's reconciliation manifest: the ids it holds for one store, for the cloud to diff.
#[derive(Debug, Clone, Deserialize)]
struct ReconcileRequest {
    /// The tenant the store belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store whose log is being reconciled (a 26-character ULID).
    store_id: String,
    /// The event ids the edge holds for this store over the window it is reconciling.
    event_ids: Vec<String>,
}

/// The ids the cloud is missing from the manifest — what the edge should re-push.
#[derive(Debug, Clone, serde::Serialize)]
struct ReconcileResponse {
    /// The subset of the manifest the cloud's event log does not contain (ULID strings).
    missing: Vec<String>,
}

/// Answers which of the edge's candidate ids the cloud is missing for a store
/// ([ADR-0040](../../../docs/adr/0040-reconciliation.md)). Internal (the reconciliation partner of
/// `/internal/ingest`), so it carries no authentication and is absent from the public OpenAPI.
async fn reconcile<Rec>(State(store): State<Rec>, Json(request): Json<ReconcileRequest>) -> Response
where
    Rec: ReconcileStore + Clone + Send + Sync + 'static,
{
    let (Ok(tenant_id), Ok(store_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    let mut candidates = Vec::with_capacity(request.event_ids.len());
    for raw in &request.event_ids {
        match raw.parse::<EventId>() {
            Ok(id) => candidates.push(id),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "an event id is not a ULID").into_response();
            }
        }
    }
    match store
        .absent_event_ids(tenant_id, store_id, &candidates)
        .await
    {
        Ok(missing) => (
            StatusCode::OK,
            Json(ReconcileResponse {
                missing: missing.iter().map(ToString::to_string).collect(),
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "a reconciliation diff failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the reconciliation service is unavailable",
            )
                .into_response()
        }
    }
}

// --- Device onboarding (`/sync/.../devices` + `/admin/devices/proposals`) ------------------------

/// The collaborators the device-onboarding routes need, stated independently of [`CloudApp`]: the
/// proposal store, plus the admin, API-key, and clock stores the two auth paths use.
#[derive(Clone)]
struct DeviceState<D, A, K, C> {
    devices: D,
    admin: A,
    keys: K,
    clock: C,
}

/// Builds the device-onboarding sub-router, stated independently of [`CloudApp`]
/// ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
///
/// A store proposes a discovered printer/KDS and reads back its approved devices on the store-facing
/// `/sync` surface (API key, `manage_devices` scope); a super-admin lists the pending queue and
/// approves or rejects on `/admin` (session guard). It needs the proposal store plus the existing
/// admin/api-key/clock collaborators, so — like [`reconcile_router`] — it carries its own state and is
/// merged into the main router rather than adding an eighth `CloudApp` generic.
pub fn device_router<D, A, K, C>(devices: D, admin: A, keys: K, clock: C) -> Router
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/devices",
            post(propose_device::<D, A, K, C>).get(list_store_devices::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals",
            get(list_pending_devices::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/approve",
            post(approve_device::<D, A, K, C>),
        )
        .route(
            "/admin/devices/proposals/{id}/reject",
            post(reject_device::<D, A, K, C>),
        )
        .with_state(DeviceState {
            devices,
            admin,
            keys,
            clock,
        })
}

/// A store's proposal of a discovered device.
#[derive(Debug, Clone, Deserialize)]
struct ProposeDeviceRequest {
    /// `printer` or `kds`.
    kind: String,
    /// A human-readable name for the device.
    name: String,
    /// The device's network address, as discovered (e.g. `192.168.1.50:9100`).
    address: String,
}

/// The id and status of a freshly-proposed device (always `pending`).
#[derive(Debug, Clone, serde::Serialize)]
struct ProposeDeviceResponse {
    /// The proposal's id (a ULID).
    id: String,
    /// The status — `pending` until an operator resolves it.
    status: String,
}

/// The tenant a device listing or resolution is scoped to (the super-admin is global).
#[derive(Debug, Clone, Deserialize)]
struct DeviceTenantQuery {
    /// The tenant whose proposals to act on (a 26-character ULID).
    tenant_id: String,
}

/// A store proposes a discovered device (`manage_devices` scope). Stored `pending` for an operator to
/// approve; the id is minted here and returned once.
async fn propose_device<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<ProposeDeviceRequest>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ManageDevices) {
        return forbidden.into_response();
    }
    let Ok(store_id) = store_id.parse::<Ulid>().map(StoreId::new) else {
        return (StatusCode::BAD_REQUEST, "the store id is not a ULID").into_response();
    };
    let Some(kind) = DeviceKind::from_wire(&request.kind) else {
        return (StatusCode::BAD_REQUEST, "kind must be one of printer, kds").into_response();
    };
    if request.name.trim().is_empty() || request.address.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name and address are required").into_response();
    }
    let Some(id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DeviceProposalId::new)
    else {
        tracing::error!("could not read OS entropy to mint a device-proposal id");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the device service is unavailable",
        )
            .into_response();
    };
    let proposal = PersistedDeviceProposal {
        id,
        tenant_id: grant.tenant(),
        store_id,
        kind,
        name: request.name,
        address: request.address,
    };
    match state.devices.propose(&proposal).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(ProposeDeviceResponse {
                id: id.to_string(),
                status: DeviceProposalStatus::Pending.as_wire().to_owned(),
            }),
        )
            .into_response(),
        Err(error) => device_error_response(&error),
    }
}

/// A store lists its **approved** devices (`manage_devices` scope) — what the edge acts on, never a
/// raw discovery.
async fn list_store_devices<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ManageDevices) {
        return forbidden.into_response();
    }
    let Ok(store_id) = store_id.parse::<Ulid>().map(StoreId::new) else {
        return (StatusCode::BAD_REQUEST, "the store id is not a ULID").into_response();
    };
    match state
        .devices
        .list(
            grant.tenant(),
            Some(store_id),
            DeviceProposalStatus::Approved,
        )
        .await
    {
        Ok(devices) => {
            (StatusCode::OK, Json::<Vec<DeviceProposalSummary>>(devices)).into_response()
        }
        Err(error) => device_error_response(&error),
    }
}

/// A super-admin lists a tenant's pending device proposals — the approval queue.
async fn list_pending_devices<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Query(query): Query<DeviceTenantQuery>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state
        .devices
        .list(tenant_id, None, DeviceProposalStatus::Pending)
        .await
    {
        Ok(devices) => {
            (StatusCode::OK, Json::<Vec<DeviceProposalSummary>>(devices)).into_response()
        }
        Err(error) => device_error_response(&error),
    }
}

/// A super-admin approves a pending proposal.
async fn approve_device<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DeviceTenantQuery>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    resolve_device(&state, &headers, &id, &query, true).await
}

/// A super-admin rejects a pending proposal.
async fn reject_device<D, A, K, C>(
    State(state): State<DeviceState<D, A, K, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DeviceTenantQuery>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    resolve_device(&state, &headers, &id, &query, false).await
}

/// Resolves a proposal to approved or rejected (super-admin only). `204` whether or not a pending row
/// was found — resolving is idempotent, tenant-scoped, and telling the caller which case it was is a
/// needless enumeration signal.
async fn resolve_device<D, A, K, C>(
    state: &DeviceState<D, A, K, C>,
    headers: &HeaderMap,
    id: &str,
    query: &DeviceTenantQuery,
    approved: bool,
) -> Response
where
    D: DeviceProposalStore,
    A: AdminStore,
    C: ClockSource,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        id.parse::<Ulid>().map(DeviceProposalId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the proposal id is not a ULID",
        )
            .into_response();
    };
    match state.devices.resolve(tenant_id, id, approved).await {
        Ok(_found) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => device_error_response(&error),
    }
}

/// Maps a device-proposal store failure to a retryable `503`, logging the detail rather than leaking it.
fn device_error_response(error: &crate::devices::DeviceProposalError) -> Response {
    tracing::error!(%error, "a device-proposal store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the device service is unavailable",
    )
        .into_response()
}

// --- Org registry (`/admin/tenants|brands|stores`, ADR-0065) ------------------------------------

/// The collaborators the registry routes need, stated independently of [`CloudApp`]: the registry
/// store, plus the admin and clock stores every route's session guard uses. Like [`device_router`],
/// it carries its own state and is merged into the main router rather than adding a `CloudApp` generic.
#[derive(Clone)]
struct RegistryState<Rg, A, C> {
    registry: Rg,
    admin: A,
    clock: C,
}

/// Builds the org-registry sub-router ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
///
/// Every route is behind the super-admin session guard, and names a tenant the admin-is-global way
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): a `?tenant_id=` query for the
/// listings, the request body for a create. Device routes nest under their store, so they never
/// collide with the `/admin/devices/proposals` onboarding queue ([`device_router`]).
pub fn registry_router<Rg, A, C>(registry: Rg, admin: A, clock: C) -> Router
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/tenants",
            get(admin_list_tenants::<Rg, A, C>).post(admin_create_tenant::<Rg, A, C>),
        )
        .route(
            "/admin/tenants/{tenant_id}",
            axum::routing::patch(admin_update_tenant::<Rg, A, C>),
        )
        .route(
            "/admin/brands",
            get(admin_list_brands::<Rg, A, C>).post(admin_create_brand::<Rg, A, C>),
        )
        .route(
            "/admin/brands/{brand_id}",
            axum::routing::patch(admin_update_brand::<Rg, A, C>),
        )
        .route(
            "/admin/stores",
            get(admin_list_stores::<Rg, A, C>).post(admin_create_store::<Rg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}",
            axum::routing::patch(admin_update_store::<Rg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}/devices",
            get(admin_list_devices::<Rg, A, C>).post(admin_create_device::<Rg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}/devices/{device_id}",
            axum::routing::patch(admin_update_device::<Rg, A, C>),
        )
        .with_state(RegistryState {
            registry,
            admin,
            clock,
        })
}

/// A `?tenant_id=` query for the tenant-scoped listings.
#[derive(Debug, Clone, Deserialize)]
struct RegistryTenantQuery {
    /// The tenant whose entities to list (a 26-character ULID).
    tenant_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateTenantRequest {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateEntityRequest {
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateBrandRequest {
    tenant_id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateBrandRequest {
    tenant_id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateStoreRequest {
    tenant_id: String,
    brand_id: Option<String>,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateStoreRequest {
    tenant_id: String,
    brand_id: Option<String>,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateDeviceRequest {
    tenant_id: String,
    name: String,
    kind: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateDeviceRequest {
    tenant_id: String,
    name: String,
    kind: String,
    status: String,
}

/// Maps a registry store failure to a retryable `503`, logging the detail rather than leaking it.
fn registry_error_response(error: &RegistryStoreError) -> Response {
    tracing::error!(%error, "a registry store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the registry service is unavailable",
    )
        .into_response()
}

/// The `503` returned when OS entropy is unavailable to mint an id.
fn registry_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a registry id");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the registry service is unavailable",
    )
        .into_response()
}

/// Parses a status word from a request body; `None` (a `400`) for anything but the two known values.
fn parse_entity_status(value: &str) -> Option<EntityStatus> {
    match value {
        "active" => Some(EntityStatus::Active),
        "archived" => Some(EntityStatus::Archived),
        _ => None,
    }
}

/// A super-admin lists every tenant.
async fn admin_list_tenants<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    match state.registry.list_tenants().await {
        Ok(tenants) => (StatusCode::OK, Json::<Vec<TenantRecord>>(tenants)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin creates a tenant; the id is minted here and returned once in the created record.
async fn admin_create_tenant<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateTenantRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        return denied;
    }
    let Some(tenant_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(TenantId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = TenantRecord {
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.registry.create_tenant(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin renames a tenant and/or sets its status.
async fn admin_update_tenant<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(tenant_id): Path<String>,
    Json(request): Json<UpdateEntityRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "the tenant id is not a ULID").into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = TenantRecord {
        tenant_id,
        name: request.name,
        status,
    };
    match state.registry.update_tenant(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such tenant").into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin lists a tenant's brands.
async fn admin_list_brands<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.registry.list_brands(tenant_id).await {
        Ok(brands) => (StatusCode::OK, Json::<Vec<BrandRecord>>(brands)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin creates a brand under a tenant.
async fn admin_create_brand<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateBrandRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Some(brand_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(BrandId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = BrandRecord {
        brand_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.registry.create_brand(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin renames a brand and/or sets its status.
async fn admin_update_brand<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(brand_id): Path<String>,
    Json(request): Json<UpdateBrandRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        return denied;
    }
    let (Ok(brand_id), Ok(tenant_id)) = (
        brand_id.parse::<Ulid>().map(BrandId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the brand id or tenant_id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = BrandRecord {
        brand_id,
        tenant_id,
        name: request.name,
        status,
    };
    match state.registry.update_brand(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such brand").into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin lists a tenant's stores.
async fn admin_list_stores<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.registry.list_stores(tenant_id).await {
        Ok(stores) => (StatusCode::OK, Json::<Vec<StoreRecord>>(stores)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// Parses an optional brand id from a request; `Err` marks a present-but-malformed value.
fn parse_optional_brand(brand_id: Option<&str>) -> Result<Option<BrandId>, ()> {
    match brand_id {
        Some(text) => text
            .parse::<Ulid>()
            .map(BrandId::new)
            .map(Some)
            .map_err(|_ignored| ()),
        None => Ok(None),
    }
}

/// A super-admin creates a store under a tenant, with an optional brand.
async fn admin_create_store<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateStoreRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Ok(brand_id) = parse_optional_brand(request.brand_id.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "brand_id is not a ULID").into_response();
    };
    let Some(store_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(StoreId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = StoreRecord {
        store_id,
        tenant_id,
        brand_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.registry.create_store(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin renames a store, (re)assigns or clears its brand, and/or sets its status.
async fn admin_update_store<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<UpdateStoreRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await
    {
        return denied;
    }
    let (Ok(store_id), Ok(tenant_id)) = (
        store_id.parse::<Ulid>().map(StoreId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the store id or tenant_id is not a ULID",
        )
            .into_response();
    };
    let Ok(brand_id) = parse_optional_brand(request.brand_id.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "brand_id is not a ULID").into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = StoreRecord {
        store_id,
        tenant_id,
        brand_id,
        name: request.name,
        status,
    };
    match state.registry.update_store(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such store").into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin lists a store's devices (tenant named on the query).
async fn admin_list_devices<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the store id is not a ULID",
        )
            .into_response();
    };
    match state.registry.list_devices(tenant_id, store_id).await {
        Ok(devices) => (StatusCode::OK, Json::<Vec<DeviceRecord>>(devices)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin creates a device under a store.
async fn admin_create_device<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<CreateDeviceRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the store id is not a ULID",
        )
            .into_response();
    };
    let Some(device_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DeviceId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = DeviceRecord {
        device_id,
        tenant_id,
        store_id,
        name: request.name,
        kind: request.kind,
        status: EntityStatus::Active,
    };
    match state.registry.create_device(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin renames a device, sets its kind, and/or sets its status.
async fn admin_update_device<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path((store_id, device_id)): Path<(String, String)>,
    Json(request): Json<UpdateDeviceRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id), Ok(device_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
        device_id.parse::<Ulid>().map(DeviceId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, the store id, or the device id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = DeviceRecord {
        device_id,
        tenant_id,
        store_id,
        name: request.name,
        kind: request.kind,
        status,
    };
    match state.registry.update_device(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such device").into_response(),
        Err(error) => registry_error_response(&error),
    }
}

// --- Catalog authoring (`/admin/catalog/...`, ADR-0066) -----------------------------------------

/// The collaborators the catalog routes need, stated independently of [`CloudApp`], like
/// [`RegistryState`]: the catalog authoring store, plus the admin and clock every session guard uses.
#[derive(Clone)]
struct CatalogState<Cat, A, C> {
    catalog: Cat,
    admin: A,
    clock: C,
}

/// Builds the catalog authoring sub-router ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
///
/// The write surface for the menu source of truth an operator edits — items, menus (with an
/// inheritance edge), and the per-channel placements a menu is compiled from. Every route is behind
/// the super-admin session guard and names a tenant the admin-is-global way
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): a `?tenant_id=` query for the
/// reads and deletes, the request body for a create or upsert. A placement is addressed by its
/// `(menu_id, menu_item_id)` pair: `PUT` upserts it, `DELETE` removes it. The routes live under
/// `/admin/catalog/`, a fresh prefix that never collides with the registry or device surfaces.
#[expect(
    clippy::too_many_lines,
    reason = "this is one flat route table for the whole catalog authoring surface (items, tax \
              classes, item/display taxonomy, layout buttons, modifier groups, menus, sections and \
              placements); splitting it would only scatter one router across helpers"
)]
pub fn catalog_router<Cat, A, C>(catalog: Cat, admin: A, clock: C) -> Router
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/items",
            get(admin_list_items::<Cat, A, C>).post(admin_create_item::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/items/{menu_item_id}",
            axum::routing::patch(admin_update_item::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/tax-classes",
            get(admin_list_tax_classes::<Cat, A, C>).post(admin_create_tax_class::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/tax-classes/{tax_class_id}",
            axum::routing::patch(admin_update_tax_class::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/item-categories",
            get(admin_list_item_categories::<Cat, A, C>)
                .post(admin_create_item_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/item-categories/{item_category_id}",
            axum::routing::patch(admin_update_item_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/item-subcategories",
            get(admin_list_item_subcategories::<Cat, A, C>)
                .post(admin_create_item_subcategory::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/item-subcategories/{item_subcategory_id}",
            axum::routing::patch(admin_update_item_subcategory::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-categories",
            get(admin_list_display_categories::<Cat, A, C>)
                .post(admin_create_display_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-categories/{display_category_id}",
            axum::routing::patch(admin_update_display_category::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-subcategories",
            get(admin_list_display_subcategories::<Cat, A, C>)
                .post(admin_create_display_subcategory::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/display-subcategories/{display_subcategory_id}",
            axum::routing::patch(admin_update_display_subcategory::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/layout-buttons",
            get(admin_list_layout_buttons::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/layout-buttons/{sales_channel}/{menu_item_id}",
            axum::routing::put(admin_set_layout_button::<Cat, A, C>)
                .delete(admin_remove_layout_button::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/modifier-groups",
            get(admin_list_modifier_groups::<Cat, A, C>)
                .post(admin_create_modifier_group::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/modifier-groups/{modifier_group_id}",
            axum::routing::patch(admin_update_modifier_group::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus",
            get(admin_list_menus::<Cat, A, C>).post(admin_create_menu::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}",
            axum::routing::patch(admin_update_menu::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/sections",
            get(admin_list_menu_sections::<Cat, A, C>).post(admin_create_menu_section::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/sections/{menu_section_id}",
            axum::routing::patch(admin_update_menu_section::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/placements",
            get(admin_list_placements::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/placements/{menu_item_id}",
            axum::routing::put(admin_set_placement::<Cat, A, C>)
                .delete(admin_remove_placement::<Cat, A, C>),
        )
        .with_state(CatalogState {
            catalog,
            admin,
            clock,
        })
}

#[derive(Debug, Clone, Deserialize)]
struct CreateItemRequest {
    tenant_id: String,
    name: String,
    tax_class_id: String,
    #[serde(default)]
    item_category_id: Option<String>,
    #[serde(default)]
    item_subcategory_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateItemRequest {
    tenant_id: String,
    name: String,
    tax_class_id: String,
    #[serde(default)]
    item_category_id: Option<String>,
    #[serde(default)]
    item_subcategory_id: Option<String>,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateTaxClassRequest {
    tenant_id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateTaxClassRequest {
    tenant_id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateItemCategoryRequest {
    tenant_id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateItemCategoryRequest {
    tenant_id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateItemSubcategoryRequest {
    tenant_id: String,
    item_category_id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateItemSubcategoryRequest {
    tenant_id: String,
    item_category_id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateDisplayCategoryRequest {
    tenant_id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateDisplayCategoryRequest {
    tenant_id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateDisplaySubcategoryRequest {
    tenant_id: String,
    display_category_id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateDisplaySubcategoryRequest {
    tenant_id: String,
    display_category_id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SetLayoutButtonRequest {
    tenant_id: String,
    display_category_id: String,
    #[serde(default)]
    display_subcategory_id: Option<String>,
    label: String,
    #[serde(default)]
    grid_column: Option<u16>,
    #[serde(default)]
    grid_row: Option<u16>,
    #[serde(default)]
    sort: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateModifierGroupRequest {
    tenant_id: String,
    name: String,
    #[serde(default)]
    min_select: u16,
    #[serde(default)]
    max_select: u16,
    #[serde(default)]
    member_item_ids: Vec<String>,
    #[serde(default)]
    attached_item_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateModifierGroupRequest {
    tenant_id: String,
    name: String,
    #[serde(default)]
    min_select: u16,
    #[serde(default)]
    max_select: u16,
    #[serde(default)]
    member_item_ids: Vec<String>,
    #[serde(default)]
    attached_item_ids: Vec<String>,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateMenuRequest {
    tenant_id: String,
    name: String,
    parent_menu_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateMenuRequest {
    tenant_id: String,
    name: String,
    parent_menu_id: Option<String>,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateMenuSectionRequest {
    tenant_id: String,
    name: String,
    #[serde(default)]
    sort: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateMenuSectionRequest {
    tenant_id: String,
    name: String,
    #[serde(default)]
    sort: i32,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SetPlacementRequest {
    tenant_id: String,
    #[serde(default)]
    menu_section_id: Option<String>,
    prices: Vec<ChannelPrice>,
    available: bool,
}

/// Maps a catalog store failure to a retryable `503`, logging the detail rather than leaking it.
fn catalog_error_response(error: &CatalogStoreError) -> Response {
    tracing::error!(%error, "a catalog store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the catalog service is unavailable",
    )
        .into_response()
}

/// The `503` returned when OS entropy is unavailable to mint a catalog id.
fn catalog_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a catalog id");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the catalog service is unavailable",
    )
        .into_response()
}

/// Parses an optional parent-menu id from a request body; `Err` for a present-but-malformed value.
fn parse_optional_menu(value: Option<&str>) -> Result<Option<MenuId>, ()> {
    match value {
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(MenuId::new(ulid)))
            .map_err(|_| ()),
        None => Ok(None),
    }
}

/// Parses an optional item-category id; an absent field or an empty string is "unclassified" (`None`),
/// a present-but-malformed value is `Err` (a `400`).
fn parse_optional_category(value: Option<&str>) -> Result<Option<ItemCategoryId>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(ItemCategoryId::new(ulid)))
            .map_err(|_| ()),
        None => Ok(None),
    }
}

/// Parses an optional item-sub-category id, with the same empty-is-`None` rule as
/// [`parse_optional_category`].
fn parse_optional_subcategory(value: Option<&str>) -> Result<Option<ItemSubcategoryId>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(ItemSubcategoryId::new(ulid)))
            .map_err(|_| ()),
        None => Ok(None),
    }
}

/// Parses an optional display-sub-category id, with the same empty-is-`None` rule as
/// [`parse_optional_category`] — a layout button may sit directly under a display category.
fn parse_optional_display_subcategory(
    value: Option<&str>,
) -> Result<Option<DisplaySubcategoryId>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(DisplaySubcategoryId::new(ulid)))
            .map_err(|_| ()),
        None => Ok(None),
    }
}

/// Parses an optional menu-section id, with the same empty-is-`None` rule as
/// [`parse_optional_category`] — a placement may sit in a menu without a section.
fn parse_optional_menu_section(value: Option<&str>) -> Result<Option<MenuSectionId>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(MenuSectionId::new(ulid)))
            .map_err(|_| ()),
        None => Ok(None),
    }
}

/// Parses a list of item ids from a request body; `Err` if any entry is not a ULID.
fn parse_item_id_list(values: &[String]) -> Result<Vec<MenuItemId>, ()> {
    values
        .iter()
        .map(|text| text.parse::<Ulid>().map(MenuItemId::new).map_err(|_| ()))
        .collect()
}

/// A super-admin lists a tenant's items.
async fn admin_list_items<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_items(tenant_id).await {
        Ok(items) => (StatusCode::OK, Json::<Vec<CatalogItem>>(items)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates an item; the id is minted here and returned once in the created record.
async fn admin_create_item<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateItemRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(tax_class_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.tax_class_id.parse::<Ulid>().map(TaxClassId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or tax_class_id is not a ULID",
        )
            .into_response();
    };
    let (Ok(item_category_id), Ok(item_subcategory_id)) = (
        parse_optional_category(request.item_category_id.as_deref()),
        parse_optional_subcategory(request.item_subcategory_id.as_deref()),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "item_category_id or item_subcategory_id is not a ULID",
        )
            .into_response();
    };
    let Some(menu_item_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MenuItemId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = CatalogItem {
        menu_item_id,
        tenant_id,
        name: request.name,
        tax_class_id,
        item_category_id,
        item_subcategory_id,
        status: EntityStatus::Active,
    };
    match state.catalog.create_item(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames an item, sets its tax class and/or status.
async fn admin_update_item<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_item_id): Path<String>,
    Json(request): Json<UpdateItemRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(menu_item_id), Ok(tenant_id), Ok(tax_class_id)) = (
        menu_item_id.parse::<Ulid>().map(MenuItemId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.tax_class_id.parse::<Ulid>().map(TaxClassId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the item id, tenant_id or tax_class_id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let (Ok(item_category_id), Ok(item_subcategory_id)) = (
        parse_optional_category(request.item_category_id.as_deref()),
        parse_optional_subcategory(request.item_subcategory_id.as_deref()),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "item_category_id or item_subcategory_id is not a ULID",
        )
            .into_response();
    };
    let record = CatalogItem {
        menu_item_id,
        tenant_id,
        name: request.name,
        tax_class_id,
        item_category_id,
        item_subcategory_id,
        status,
    };
    match state.catalog.update_item(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such item").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's tax classes.
async fn admin_list_tax_classes<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_tax_classes(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<TaxClass>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates a tax class; the id is minted here and returned once in the created record.
async fn admin_create_tax_class<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateTaxClassRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Some(tax_class_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(TaxClassId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = TaxClass {
        tax_class_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_tax_class(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames a tax class and/or sets its status.
async fn admin_update_tax_class<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(tax_class_id): Path<String>,
    Json(request): Json<UpdateTaxClassRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tax_class_id), Ok(tenant_id)) = (
        tax_class_id.parse::<Ulid>().map(TaxClassId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the tax class id or tenant_id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = TaxClass {
        tax_class_id,
        tenant_id,
        name: request.name,
        status,
    };
    match state.catalog.update_tax_class(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such tax class").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's item categories.
async fn admin_list_item_categories<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_item_categories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<ItemCategory>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates an item category; the id is minted here and returned once.
async fn admin_create_item_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateItemCategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Some(item_category_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(ItemCategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = ItemCategory {
        item_category_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_item_category(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames an item category and/or sets its status.
async fn admin_update_item_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(item_category_id): Path<String>,
    Json(request): Json<UpdateItemCategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(item_category_id), Ok(tenant_id)) = (
        item_category_id.parse::<Ulid>().map(ItemCategoryId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the category id or tenant_id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = ItemCategory {
        item_category_id,
        tenant_id,
        name: request.name,
        status,
    };
    match state.catalog.update_item_category(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such item category").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's item sub-categories.
async fn admin_list_item_subcategories<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_item_subcategories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<ItemSubcategory>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates an item sub-category under a parent category; the id is minted here.
async fn admin_create_item_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateItemSubcategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(item_category_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request
            .item_category_id
            .parse::<Ulid>()
            .map(ItemCategoryId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or item_category_id is not a ULID",
        )
            .into_response();
    };
    let Some(item_subcategory_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(ItemSubcategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = ItemSubcategory {
        item_subcategory_id,
        tenant_id,
        item_category_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_item_subcategory(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames an item sub-category, (re)parents it, and/or sets its status.
async fn admin_update_item_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(item_subcategory_id): Path<String>,
    Json(request): Json<UpdateItemSubcategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(item_subcategory_id), Ok(tenant_id), Ok(item_category_id)) = (
        item_subcategory_id
            .parse::<Ulid>()
            .map(ItemSubcategoryId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request
            .item_category_id
            .parse::<Ulid>()
            .map(ItemCategoryId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the sub-category id, tenant_id or item_category_id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = ItemSubcategory {
        item_subcategory_id,
        tenant_id,
        item_category_id,
        name: request.name,
        status,
    };
    match state.catalog.update_item_subcategory(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such item sub-category").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's display categories.
async fn admin_list_display_categories<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_display_categories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<DisplayCategory>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates a display category; the id is minted here and returned once.
async fn admin_create_display_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateDisplayCategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Some(display_category_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DisplayCategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = DisplayCategory {
        display_category_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_display_category(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames a display category and/or sets its status.
async fn admin_update_display_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(display_category_id): Path<String>,
    Json(request): Json<UpdateDisplayCategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(display_category_id), Ok(tenant_id)) = (
        display_category_id
            .parse::<Ulid>()
            .map(DisplayCategoryId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the display category id or tenant_id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = DisplayCategory {
        display_category_id,
        tenant_id,
        name: request.name,
        status,
    };
    match state.catalog.update_display_category(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such display category").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's display sub-categories.
async fn admin_list_display_subcategories<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_display_subcategories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<DisplaySubcategory>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates a display sub-category under a parent display category; the id is minted here.
async fn admin_create_display_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateDisplaySubcategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(display_category_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request
            .display_category_id
            .parse::<Ulid>()
            .map(DisplayCategoryId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or display_category_id is not a ULID",
        )
            .into_response();
    };
    let Some(display_subcategory_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DisplaySubcategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = DisplaySubcategory {
        display_subcategory_id,
        tenant_id,
        display_category_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_display_subcategory(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames a display sub-category, (re)parents it, and/or sets its status.
async fn admin_update_display_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(display_subcategory_id): Path<String>,
    Json(request): Json<UpdateDisplaySubcategoryRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(display_subcategory_id), Ok(tenant_id), Ok(display_category_id)) = (
        display_subcategory_id
            .parse::<Ulid>()
            .map(DisplaySubcategoryId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request
            .display_category_id
            .parse::<Ulid>()
            .map(DisplayCategoryId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the sub-category id, tenant_id or display_category_id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = DisplaySubcategory {
        display_subcategory_id,
        tenant_id,
        display_category_id,
        name: request.name,
        status,
    };
    match state.catalog.update_display_subcategory(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such display sub-category").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's layout buttons across all channels.
async fn admin_list_layout_buttons<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_layout_buttons(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<LayoutButton>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin upserts an item's button in a channel's layout. The channel (a wire token) and item
/// are named on the path; the display grouping, caption, grid slot and order are in the body.
async fn admin_set_layout_button<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((sales_channel, menu_item_id)): Path<(String, String)>,
    Json(request): Json<SetLayoutButtonRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_item_id), Ok(display_category_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_item_id.parse::<Ulid>().map(MenuItemId::new),
        request
            .display_category_id
            .parse::<Ulid>()
            .map(DisplayCategoryId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, the item id or display_category_id is not a ULID",
        )
            .into_response();
    };
    let Ok(display_subcategory_id) =
        parse_optional_display_subcategory(request.display_subcategory_id.as_deref())
    else {
        return (
            StatusCode::BAD_REQUEST,
            "display_subcategory_id is not a ULID",
        )
            .into_response();
    };
    // A grid slot exists only when both column and row are given; otherwise the button flows by order.
    let position = match (request.grid_column, request.grid_row) {
        (Some(column), Some(row)) => Some(GridPosition { column, row }),
        _ => None,
    };
    let record = LayoutButton {
        tenant_id,
        sales_channel: Open::<SalesChannel>::parse(&sales_channel),
        display_category_id,
        display_subcategory_id,
        menu_item_id,
        label: request.label,
        position,
        sort: request.sort,
    };
    match state.catalog.set_layout_button(&record).await {
        Ok(()) => (StatusCode::OK, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin removes an item's button from a channel's layout (tenant named on the query).
async fn admin_remove_layout_button<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((sales_channel, menu_item_id)): Path<(String, String)>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_item_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_item_id.parse::<Ulid>().map(MenuItemId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the item id is not a ULID",
        )
            .into_response();
    };
    match state
        .catalog
        .remove_layout_button(
            tenant_id,
            Open::<SalesChannel>::parse(&sales_channel),
            menu_item_id,
        )
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such layout button").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's modifier groups.
async fn admin_list_modifier_groups<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_modifier_groups(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<ModifierGroup>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates a modifier group; the id is minted here and returned once.
async fn admin_create_modifier_group<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateModifierGroupRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let (Ok(member_item_ids), Ok(attached_item_ids)) = (
        parse_item_id_list(&request.member_item_ids),
        parse_item_id_list(&request.attached_item_ids),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "a member or attached item id is not a ULID",
        )
            .into_response();
    };
    let Some(modifier_group_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(ModifierGroupId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = ModifierGroup {
        modifier_group_id,
        tenant_id,
        name: request.name,
        min_select: request.min_select,
        max_select: request.max_select,
        member_item_ids,
        attached_item_ids,
        status: EntityStatus::Active,
    };
    match state.catalog.create_modifier_group(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames a modifier group, sets its rule, members, attachments and/or status.
async fn admin_update_modifier_group<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(modifier_group_id): Path<String>,
    Json(request): Json<UpdateModifierGroupRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(modifier_group_id), Ok(tenant_id)) = (
        modifier_group_id.parse::<Ulid>().map(ModifierGroupId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the modifier group id or tenant_id is not a ULID",
        )
            .into_response();
    };
    let (Ok(member_item_ids), Ok(attached_item_ids)) = (
        parse_item_id_list(&request.member_item_ids),
        parse_item_id_list(&request.attached_item_ids),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "a member or attached item id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = ModifierGroup {
        modifier_group_id,
        tenant_id,
        name: request.name,
        min_select: request.min_select,
        max_select: request.max_select,
        member_item_ids,
        attached_item_ids,
        status,
    };
    match state.catalog.update_modifier_group(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such modifier group").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's menus.
async fn admin_list_menus<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.catalog.list_menus(tenant_id).await {
        Ok(menus) => (StatusCode::OK, Json::<Vec<Menu>>(menus)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates a menu, optionally under a parent it inherits from.
async fn admin_create_menu<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateMenuRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Ok(parent_menu_id) = parse_optional_menu(request.parent_menu_id.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "parent_menu_id is not a ULID").into_response();
    };
    let Some(menu_id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MenuId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = Menu {
        menu_id,
        tenant_id,
        name: request.name,
        parent_menu_id,
        status: EntityStatus::Active,
    };
    match state.catalog.create_menu(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames a menu, (re)sets its parent and/or status.
async fn admin_update_menu<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_id): Path<String>,
    Json(request): Json<UpdateMenuRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(menu_id), Ok(tenant_id)) = (
        menu_id.parse::<Ulid>().map(MenuId::new),
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the menu id or tenant_id is not a ULID",
        )
            .into_response();
    };
    let Ok(parent_menu_id) = parse_optional_menu(request.parent_menu_id.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "parent_menu_id is not a ULID").into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = Menu {
        menu_id,
        tenant_id,
        name: request.name,
        parent_menu_id,
        status,
    };
    match state.catalog.update_menu(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such menu").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a menu's sections (tenant named on the query).
async fn admin_list_menu_sections<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_id.parse::<Ulid>().map(MenuId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the menu id is not a ULID",
        )
            .into_response();
    };
    match state.catalog.list_menu_sections(tenant_id, menu_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<MenuSection>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates a section within a menu.
async fn admin_create_menu_section<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_id): Path<String>,
    Json(request): Json<CreateMenuSectionRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_id.parse::<Ulid>().map(MenuId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the menu id is not a ULID",
        )
            .into_response();
    };
    let Some(menu_section_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MenuSectionId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = MenuSection {
        menu_section_id,
        tenant_id,
        menu_id,
        name: request.name,
        sort: request.sort,
        status: EntityStatus::Active,
    };
    match state.catalog.create_menu_section(&record).await {
        Ok(()) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames a menu section, sets its sort and/or status.
async fn admin_update_menu_section<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((menu_id, menu_section_id)): Path<(String, String)>,
    Json(request): Json<UpdateMenuSectionRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_id), Ok(menu_section_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_id.parse::<Ulid>().map(MenuId::new),
        menu_section_id.parse::<Ulid>().map(MenuSectionId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, the menu id or the section id is not a ULID",
        )
            .into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let record = MenuSection {
        menu_section_id,
        tenant_id,
        menu_id,
        name: request.name,
        sort: request.sort,
        status,
    };
    match state.catalog.update_menu_section(&record).await {
        Ok(true) => (StatusCode::OK, Json(record)).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such menu section").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a menu's placements (tenant named on the query).
async fn admin_list_placements<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_id.parse::<Ulid>().map(MenuId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the menu id is not a ULID",
        )
            .into_response();
    };
    match state.catalog.list_placements(tenant_id, menu_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<MenuPlacement>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin upserts an item's placement in a menu — its per-channel prices and availability.
async fn admin_set_placement<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((menu_id, menu_item_id)): Path<(String, String)>,
    Json(request): Json<SetPlacementRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_id), Ok(menu_item_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_id.parse::<Ulid>().map(MenuId::new),
        menu_item_id.parse::<Ulid>().map(MenuItemId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, the menu id or the item id is not a ULID",
        )
            .into_response();
    };
    let Ok(menu_section_id) = parse_optional_menu_section(request.menu_section_id.as_deref())
    else {
        return (StatusCode::BAD_REQUEST, "menu_section_id is not a ULID").into_response();
    };
    let record = MenuPlacement {
        tenant_id,
        menu_id,
        menu_item_id,
        menu_section_id,
        prices: request.prices,
        available: request.available,
    };
    match state.catalog.set_placement(&record).await {
        Ok(()) => (StatusCode::OK, Json(record)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin removes an item from a menu (tenant named on the query).
async fn admin_remove_placement<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((menu_id, menu_item_id)): Path<(String, String)>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(menu_id), Ok(menu_item_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        menu_id.parse::<Ulid>().map(MenuId::new),
        menu_item_id.parse::<Ulid>().map(MenuItemId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, the menu id or the item id is not a ULID",
        )
            .into_response();
    };
    match state
        .catalog
        .remove_placement(tenant_id, menu_id, menu_item_id)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such placement").into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

// --- Catalog publish (`/admin/catalog/publish`, ADR-0066) ---------------------------------------

/// The collaborators the publish route needs: the catalog to compile from, the config-tree store to
/// publish into, plus the admin and clock the session guard and version-id minting use.
#[derive(Clone)]
struct CatalogPublishState<Cat, Cfg, A, C> {
    catalog: Cat,
    config_trees: Cfg,
    admin: A,
    clock: C,
}

/// Builds the catalog publish sub-router ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
///
/// The step that turns authored catalog into what a store actually pulls: compile a menu into a
/// per-channel [`pos_proto::MenuBook`] and write it onto the `menu` node of the store's **Store**
/// config layer, so it rides the config tree to the store like every other configuration change
/// ([ADR-0033](../../../docs/adr/0033-config-tree.md)) — no new channel. It is a separate sub-router
/// because it needs the config-tree store the CRUD routes do not.
pub fn catalog_publish_router<Cat, Cfg, A, C>(
    catalog: Cat,
    config_trees: Cfg,
    admin: A,
    clock: C,
) -> Router
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/publish",
            post(admin_publish_menu::<Cat, Cfg, A, C>),
        )
        .with_state(CatalogPublishState {
            catalog,
            config_trees,
            admin,
            clock,
        })
}

/// A super-admin selects the (tenant, store, menu) to compile and publish.
#[expect(
    clippy::struct_field_names,
    reason = "tenant_id/store_id/menu_id are the wire field names; the shared _id postfix is the ULID naming convention (docs/naming-and-api.md), not a smell"
)]
#[derive(Debug, Clone, Deserialize)]
struct PublishMenuRequest {
    /// The tenant that owns the catalog and the store (a 26-character ULID).
    tenant_id: String,
    /// The store whose `menu` config node receives the compiled book (a ULID).
    store_id: String,
    /// The menu to compile — its inheritance chain and placements (a ULID).
    menu_id: String,
}

/// A super-admin publishes a menu to a store: compile the price book and the presentation layout →
/// write the `MenuBook` and `LayoutBook` to the store's `menu` and `layout` config nodes → version it
/// through the config tree.
#[expect(
    clippy::too_many_lines,
    reason = "one publish is a single linear transaction — load items/menus/placements, compile the \
              price book, load the display taxonomy and layout buttons, compile the layout, set both \
              nodes on the Store layer and version it; splitting the load-compile-write flow would \
              scatter the config-tree state the final publish needs"
)]
async fn admin_publish_menu<Cat, Cfg, A, C>(
    State(state): State<CatalogPublishState<Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishMenuRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id), Ok(menu_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
        request.menu_id.parse::<Ulid>().map(MenuId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, store_id or menu_id is not a ULID",
        )
            .into_response();
    };

    // Load the tenant's authoring model. Placements are gathered across every menu; the compiler
    // filters to the requested menu's inheritance chain, so extra rows are harmless.
    let items = match state.catalog.list_items(tenant_id).await {
        Ok(items) => items,
        Err(error) => return catalog_error_response(&error),
    };
    let menus = match state.catalog.list_menus(tenant_id).await {
        Ok(menus) => menus,
        Err(error) => return catalog_error_response(&error),
    };
    let mut placements = Vec::new();
    for menu in &menus {
        match state.catalog.list_placements(tenant_id, menu.menu_id).await {
            Ok(rows) => placements.extend(rows),
            Err(error) => return catalog_error_response(&error),
        }
    }

    // Compile the price book. A refusal here is a configuration error the operator must fix, not a
    // store failure.
    let book = match compile_menu(&items, &menus, &placements, menu_id) {
        Ok(book) => book,
        Err(error) => return (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()).into_response(),
    };
    let Ok(book_value) = serde_json::to_value(&book) else {
        tracing::error!("could not serialise a compiled menu book");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the catalog service is unavailable",
        )
            .into_response();
    };

    // Compile the presentation layout alongside the price book (ADR-0066): the display taxonomy plus
    // the tenant's layout buttons resolve to a per-channel `LayoutBook`, delivered on a separate
    // `layout` node so a button moving reprices nothing. The layout compiler is forgiving (a stale
    // button is skipped), so this never fails a publish that the price compile accepted.
    let display_categories = match state.catalog.list_display_categories(tenant_id).await {
        Ok(rows) => rows,
        Err(error) => return catalog_error_response(&error),
    };
    let display_subcategories = match state.catalog.list_display_subcategories(tenant_id).await {
        Ok(rows) => rows,
        Err(error) => return catalog_error_response(&error),
    };
    let layout_buttons = match state.catalog.list_layout_buttons(tenant_id).await {
        Ok(rows) => rows,
        Err(error) => return catalog_error_response(&error),
    };
    let layout = compile_layout_book(&display_categories, &display_subcategories, &layout_buttons);
    let Ok(layout_value) = serde_json::to_value(&layout) else {
        tracing::error!("could not serialise a compiled layout book");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the catalog service is unavailable",
        )
            .into_response();
    };

    // Load the store's tree (or start one), set the `menu` and `layout` keys on its Store layer, and
    // re-publish that layer. The Store layer is index 2 in the Tenant→Brand→Store→Device order
    // (`ConfigLevel::ORDER`); writing the whole layer back preserves any other Store-level keys there.
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |s| s.layers[2].clone(),
    );
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert("menu".to_owned(), book_value);
        map.insert("layout".to_owned(), layout_value);
    } else {
        store_layer = serde_json::json!({ "menu": book_value, "layout": layout_value });
    }

    let mut tree = match state_before {
        Some(existing) => ConfigTree::from_state(store_id, CapabilityValidator, existing),
        None => ConfigTree::new(store_id, CapabilityValidator),
    };
    let Some(version_id) = mint_version_id(state.clock.now().as_milliseconds_since_epoch()) else {
        tracing::error!("could not read OS entropy to mint a config version id");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the configuration service is unavailable",
        )
            .into_response();
    };
    match tree.publish(ConfigLevel::Store, store_layer, version_id) {
        Ok(id) => {
            if let Err(error) = state
                .config_trees
                .save(tenant_id, store_id, &tree.state())
                .await
            {
                return config_store_error_response(&error);
            }
            (
                StatusCode::OK,
                Json(PublishedConfig {
                    config_version_id: id.to_string(),
                }),
            )
                .into_response()
        }
        Err(ConfigError::Invalid(violations)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ConfigViolations { violations }),
        )
            .into_response(),
    }
}

// --- Device activation (`/admin/activation-codes` + `/activate`) --------------------------------

/// The collaborators the activation routes need, stated independently of [`CloudApp`]: the activation
/// store, plus the admin and clock stores the issue/revoke routes' session guard uses.
#[derive(Clone)]
struct ActivationState<X, A, C> {
    activations: X,
    admin: A,
    clock: C,
}

/// Builds the activation sub-router ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
///
/// A super-admin issues a code bound to a device slot and can cancel a slot's pending code, both
/// behind the `/admin` session guard. A device presents its code on `/activate` and receives its
/// credential — that route is **not** authenticated, because the code itself is the bearer credential
/// (single-use, 55-bit); the exchange is where a box first earns a credential. Like [`device_router`],
/// it carries its own state and is merged into the main router rather than adding a `CloudApp` generic.
pub fn activation_router<X, A, C>(activations: X, admin: A, clock: C) -> Router
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/activation-codes",
            post(admin_issue_activation_code::<X, A, C>),
        )
        .route(
            "/admin/activation-codes/revoke",
            post(admin_revoke_activation_codes::<X, A, C>),
        )
        .route("/activate", post(exchange_activation_code::<X, A, C>))
        .with_state(ActivationState {
            activations,
            admin,
            clock,
        })
}

/// A super-admin issues an activation code for a device slot.
#[expect(
    clippy::struct_field_names,
    reason = "tenant_id/store_id/device_id are the wire field names; the shared _id postfix is the ULID naming convention (docs/naming-and-api.md), not a smell"
)]
#[derive(Debug, Clone, Deserialize)]
struct IssueActivationRequest {
    /// The tenant the device belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store the device belongs to (a ULID).
    store_id: String,
    /// The device slot to activate (a ULID).
    device_id: String,
}

/// The freshly-issued code, shown once for the operator to put on the setup sheet.
#[derive(Debug, Clone, serde::Serialize)]
struct IssueActivationResponse {
    /// The `XXXX-XXXX-XXXX` code — the only time it is visible; only its hash is stored.
    activation_code: String,
}

/// A super-admin cancels the pending activation for a device slot (a leaked setup sheet).
#[expect(
    clippy::struct_field_names,
    reason = "tenant_id/store_id/device_id are the wire field names; the shared _id postfix is the ULID naming convention (docs/naming-and-api.md), not a smell"
)]
#[derive(Debug, Clone, Deserialize)]
struct RevokeActivationRequest {
    /// The tenant the device belongs to (a ULID).
    tenant_id: String,
    /// The store the device belongs to (a ULID).
    store_id: String,
    /// The device slot whose issued codes to cancel (a ULID).
    device_id: String,
}

/// How many still-issued codes were cancelled.
#[derive(Debug, Clone, serde::Serialize)]
struct RevokeActivationResponse {
    /// The number of codes moved from `issued` to `revoked`.
    revoked: u64,
}

/// A device presents its activation code. Unauthenticated: the code is the credential.
#[derive(Debug, Clone, Deserialize)]
struct ExchangeRequest {
    /// The `XXXX-XXXX-XXXX` code the operator typed, in any casing or spacing.
    code: String,
}

/// The device's minted credential and the device id it was activated as — shown once.
#[derive(Debug, Clone, serde::Serialize)]
struct ExchangeResponse {
    /// The device id the credential authenticates as.
    device_id: String,
    /// The `posdev_<id>_<secret>` bearer token the device stores in its `KeyVault`.
    credential: String,
}

/// A super-admin issues an activation code bound to a device slot.
async fn admin_issue_activation_code<X, A, C>(
    State(state): State<ActivationState<X, A, C>>,
    headers: HeaderMap,
    Json(request): Json<IssueActivationRequest>,
) -> Response
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id), Ok(device_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
        request.device_id.parse::<Ulid>().map(DeviceId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, store_id, and device_id must be ULIDs",
        )
            .into_response();
    };
    let Some(code) = mint_activation_code() else {
        tracing::error!("could not read OS entropy to mint an activation code");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the activation service is unavailable",
        )
            .into_response();
    };
    match state
        .activations
        .issue(hash_code(&code), tenant_id, store_id, device_id)
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(IssueActivationResponse {
                activation_code: code.as_str().to_owned(),
            }),
        )
            .into_response(),
        Err(error) => activation_error_response(&error),
    }
}

/// A super-admin cancels a device slot's still-issued codes.
async fn admin_revoke_activation_codes<X, A, C>(
    State(state): State<ActivationState<X, A, C>>,
    headers: HeaderMap,
    Json(request): Json<RevokeActivationRequest>,
) -> Response
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id), Ok(device_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
        request.device_id.parse::<Ulid>().map(DeviceId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, store_id, and device_id must be ULIDs",
        )
            .into_response();
    };
    match state
        .activations
        .revoke_slot(tenant_id, store_id, device_id)
        .await
    {
        Ok(revoked) => (StatusCode::OK, Json(RevokeActivationResponse { revoked })).into_response(),
        Err(error) => activation_error_response(&error),
    }
}

/// A device exchanges its activation code for a credential (unauthenticated — the code is the
/// credential). Single-use and deny-by-default, per [`pos_core::activation::redeem`].
async fn exchange_activation_code<X, A, C>(
    State(state): State<ActivationState<X, A, C>>,
    Json(request): Json<ExchangeRequest>,
) -> Response
where
    X: ActivationCodeStore + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let Ok(code) = ActivationCode::parse(&request.code) else {
        // A malformed code never named a real one, so this is a plain client error, not an oracle.
        return (StatusCode::BAD_REQUEST, "the activation code is malformed").into_response();
    };
    let code_hash = hash_code(&code);
    let issued = match state.activations.lookup(code_hash).await {
        Ok(Some(issued)) => issued,
        // An unknown code collapses to the same refusal a spent one gets — no oracle.
        Ok(None) => return activation_refused(),
        Err(error) => return activation_error_response(&error),
    };
    match redeem(issued.status) {
        Redemption::Reject(reason) => {
            // The reason is for the server's log; the device sees one generic refusal.
            tracing::info!(?reason, "activation refused");
            activation_refused()
        }
        Redemption::Grant => {
            let now_ms = state.clock.now().as_milliseconds_since_epoch();
            let (Some(id), Some(secret)) = (mint_ulid(now_ms), random_hex_32()) else {
                tracing::error!("could not read OS entropy to mint a device credential");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the activation service is unavailable",
                )
                    .into_response();
            };
            let (credential, token) = mint_device_credential(id, &secret);
            match state
                .activations
                .consume_and_provision(code_hash, &credential)
                .await
            {
                Ok(true) => (
                    StatusCode::CREATED,
                    Json(ExchangeResponse {
                        device_id: issued.device_id.to_string(),
                        credential: token,
                    }),
                )
                    .into_response(),
                // The code was spent or revoked between the lookup and the consume — same refusal.
                Ok(false) => activation_refused(),
                Err(error) => activation_error_response(&error),
            }
        }
    }
}

/// The one generic activation refusal. A spent, revoked, unknown, or raced code all collapse to this,
/// so a prober cannot tell them apart ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
fn activation_refused() -> Response {
    (StatusCode::FORBIDDEN, "activation refused").into_response()
}

/// Maps an activation-store failure to a retryable `503`, logging the detail rather than leaking it.
fn activation_error_response(error: &crate::activation::ActivationStoreError) -> Response {
    tracing::error!(%error, "an activation store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the activation service is unavailable",
    )
        .into_response()
}

/// Mints a fresh activation code from OS entropy, or `None` if the entropy source is unavailable — in
/// which case the caller fails closed rather than issue a guessable code.
fn mint_activation_code() -> Option<ActivationCode> {
    let mut entropy = [0_u8; pos_core::activation::PAYLOAD_LEN];
    getrandom::fill(&mut entropy).ok()?;
    Some(ActivationCode::from_entropy(entropy))
}

// --- Translation grid (`/admin/translations`) ---------------------------------------------------

/// The collaborators the translation-grid routes need, stated independently of [`CloudApp`].
#[derive(Clone)]
struct TranslationState<Tr, A, C> {
    translations: Tr,
    admin: A,
    clock: C,
}

/// The tenant a translation request is scoped to (the super-admin is global).
#[derive(Debug, Clone, Deserialize)]
struct TranslationTenantQuery {
    /// The tenant whose grid to read or write (a 26-character ULID).
    tenant_id: String,
}

/// The keys a rejected grid failed the fallback rule on — every key must carry a non-empty `en`.
#[derive(Debug, Clone, serde::Serialize)]
struct GridViolations {
    /// The keys missing a non-empty `en` value ([ADR-0043](../../../docs/adr/0043-translation-grid.md)).
    missing_fallback: Vec<String>,
}

/// Builds the translation-grid sub-router, stated independently of [`CloudApp`]
/// ([ADR-0043](../../../docs/adr/0043-translation-grid.md)).
///
/// A super-admin reads and replaces a tenant's whole grid behind the session guard; a `PUT` is
/// validated so every key carries a non-empty `en` fallback before anything is stored. Like the other
/// merged sub-routers, it carries its own state rather than adding a `CloudApp` generic.
pub fn translation_router<Tr, A, C>(translations: Tr, admin: A, clock: C) -> Router
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/translations",
            get(get_translations::<Tr, A, C>).put(put_translations::<Tr, A, C>),
        )
        .with_state(TranslationState {
            translations,
            admin,
            clock,
        })
}

/// Returns a tenant's translation grid (super-admin only), or an empty grid if it has authored none.
async fn get_translations<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.translations.load(tenant_id).await {
        // A tenant with no grid yet is an empty grid to edit, not a 404.
        Ok(grid) => (StatusCode::OK, Json(grid.unwrap_or_default())).into_response(),
        Err(error) => translation_error_response(&error),
    }
}

/// Replaces a tenant's translation grid (super-admin only). A grid whose every key does not carry a
/// non-empty `en` is a `422` naming the offending keys, and nothing is stored.
async fn put_translations<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
    Json(grid): Json<TranslationGrid>,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let missing = grid.keys_missing_fallback();
    if !missing.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(GridViolations {
                missing_fallback: missing,
            }),
        )
            .into_response();
    }
    match state.translations.save(tenant_id, &grid).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => translation_error_response(&error),
    }
}

/// Maps a translation-store failure to a retryable `503`, logging the detail rather than leaking it.
fn translation_error_response(error: &crate::translations::TranslationStoreError) -> Response {
    tracing::error!(%error, "a translation store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the translation service is unavailable",
    )
        .into_response()
}

/// The generated OpenAPI document for the public `/v1` surface.
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Ingests a batch of event envelopes, idempotently. Internal (the reconciliation re-push target),
/// so it is deliberately absent from the public OpenAPI document and carries no authentication.
async fn ingest<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
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
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
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
pub(crate) async fn daily_rollups<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
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
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
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

// --- The store-facing configuration surface (`/sync`) -------------------------------------------

/// The version a store reports it currently holds, if any.
#[derive(Debug, Clone, Deserialize)]
struct ConfigSyncQuery {
    /// The config version the store holds (a ULID), or absent if it holds none yet.
    #[serde(default)]
    held_version: Option<String>,
}

/// What a store should do to reach the current configuration — the wire form of a
/// [`SyncOutcome`](crate::config_tree::SyncOutcome).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ConfigSyncResponse {
    /// The store already holds the current version; it applies nothing.
    UpToDate,
    /// The store should apply this snapshot or delta.
    Update {
        /// The snapshot or collapsed delta to apply ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
        update: ConfigUpdate,
    },
}

/// Serves a store its own configuration update: nothing (up to date), a delta, or a full snapshot
/// past *K* versions behind ([ADR-0039](../../../docs/adr/0039-config-delivery.md)).
///
/// Requires a valid API key with the `read_config` scope, and answers **only** for that key's tenant:
/// the tenant comes from the verified grant, never the path, so a store can pull a `store_id` only
/// within its own tenant. A store outside the tenant, or one with nothing published, reads `404`.
async fn edge_config_sync<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigSyncQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&app.keys, &app.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadConfig) {
        return forbidden.into_response();
    }
    let Ok(store_id) = store_id.parse::<Ulid>().map(StoreId::new) else {
        return (StatusCode::BAD_REQUEST, "the store id is not a ULID").into_response();
    };
    let held = match query.held_version {
        None => None,
        Some(ref raw) => match raw.parse::<Ulid>().map(ConfigVersionId::new) {
            Ok(version) => Some(version),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "held_version is not a ULID").into_response();
            }
        },
    };
    // The tenant is the grant's, not the path's — a store reaches only its own tenant's trees.
    match app.config_trees.load(grant.tenant(), store_id).await {
        Ok(Some(state)) => {
            let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
            let response = match tree.update_for(held) {
                SyncOutcome::UpToDate => ConfigSyncResponse::UpToDate,
                SyncOutcome::Deliver(update) => ConfigSyncResponse::Update { update },
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            "the store has no published configuration",
        )
            .into_response(),
        Err(error) => config_store_error_response(&error),
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
async fn admin_login<S, R, K, C, A, T, W>(
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
    let ip = client_ip(&headers);
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
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the sign-in service is unavailable",
        )
            .into_response();
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

/// The client IP for an admin request, read from the reverse proxy's forwarding headers. Prefers the
/// first hop of `X-Forwarded-For` (the original client, before the proxy chain), then `X-Real-IP`;
/// `None` if neither is present. Behind the P8 Caddy proxy these are set by the proxy, so they name
/// the real client rather than the proxy's own address; the value is only ever shown back to the
/// admin whose session it is, never trusted for authorization.
fn client_ip(headers: &HeaderMap) -> Option<&str> {
    header_str(headers, "x-forwarded-for")
        .map(|value| value.split(',').next().unwrap_or(value).trim())
        .filter(|ip| !ip.is_empty())
        .or_else(|| header_str(headers, "x-real-ip"))
}

/// A header's value as a trimmed `&str`, or `None` when it is absent or not valid UTF-8.
fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// A `429 Too Many Requests` for a rate-limited sign-in, with a `Retry-After` in seconds
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 5). Generic — it says
/// nothing about whether the credential was right, and the throttle runs before the credential check,
/// so it cannot become an oracle.
fn too_many_login_attempts(retry_after_secs: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        "too many sign-in attempts; try again later",
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

/// Adds the admin-console security headers to every response
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 5). Applied as a router layer
/// so it covers the console SPA, its assets, and the `/admin` API alike: `Content-Security-Policy`
/// (see [`CONTENT_SECURITY_POLICY_VALUE`]), `X-Content-Type-Options: nosniff` (no MIME sniffing),
/// `X-Frame-Options: DENY` + the CSP's `frame-ancestors 'none'` (clickjacking), and
/// `Referrer-Policy: no-referrer` (a console URL never leaks off-site). Harmless on the JSON `/v1`
/// responses it also covers — those are never framed or rendered as documents.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY_VALUE),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

/// `POST /admin/setup` — first-boot super-admin enrolment ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md)).
///
/// Token-gated and self-disabling: `404` when no setup token is configured, `401` on a token mismatch
/// (compared in constant time), `422` if the chosen password is shorter than [`MIN_PASSWORD_LEN`],
/// `409` once an administrator is already enrolled, and on success `201` with the one-time TOTP
/// enrolment. The password is hashed with Argon2id under a fresh CSPRNG salt and never stored in the
/// clear; the TOTP secret is generated here and returned exactly once.
async fn admin_setup<S, R, K, C, A, T, W>(
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
        // No token configured: setup is off. Reveal nothing more than "no such route".
        return (StatusCode::NOT_FOUND, "setup is not enabled").into_response();
    };
    if !constant_time_eq(&request.setup_token, expected) {
        return (StatusCode::UNAUTHORIZED, "setup failed").into_response();
    }
    if request.password.len() < MIN_PASSWORD_LEN {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "the password is too short",
        )
            .into_response();
    }
    let Some((secret, phc)) = mint_credential(&request.password) else {
        tracing::error!("could not mint a super-admin credential (entropy or hashing failed)");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the setup service is unavailable",
        )
            .into_response();
    };
    match app
        .admin
        .provision_credential(phc.clone(), secret.to_vec())
        .await
    {
        Ok(true) => {
            // Mirror the freshly-enrolled super-admin into `admin_users` as the first `owner`
            // ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)) — the same identity the
            // migration seeds on an upgraded install. Best-effort: the session guard falls back to an
            // implicit owner when this row is absent, so a store blip here never locks the operator
            // out; it only means the owner is missing from the admins list until reconciled.
            let owner = NewAdminUser {
                id: IMPLICIT_OWNER_ID.to_owned(),
                email: IMPLICIT_OWNER_EMAIL.to_owned(),
                name: "Owner".to_owned(),
                role: AdminRole::Owner,
                password_phc: phc,
                totp_secret: secret.to_vec(),
            };
            if let Err(error) = app.admin.create_admin_user(owner).await {
                tracing::warn!(
                    %error,
                    "super-admin enrolled but mirroring it into admin_users failed; the guard's \
                     implicit-owner fallback keeps sign-in working"
                );
            }
            (StatusCode::CREATED, Json(build_enrolment(&secret))).into_response()
        }
        Ok(false) => (StatusCode::CONFLICT, "an administrator is already enrolled").into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the setup service is unavailable",
        )
            .into_response(),
    }
}

/// Signs a super-admin out: revokes the session server-side and clears the client cookie.
///
/// Idempotent — a request with no session, or one the store cannot reach, still clears the client
/// cookie, so the browser is always logged out even if the server-side row lingers to its TTL.
async fn admin_logout<S, R, K, C, A, T, W>(
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

/// Confirms the caller holds a live super-admin session — the guard every other `/admin` route will
/// stand behind, exposed here as a `204`/`401` "am I signed in?" check for the admin UI.
async fn admin_session<S, R, K, C, A, T, W>(
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

/// `GET /admin/whoami` — the acting admin's own identity (id, email, name, role, status), so the
/// console can label the signed-in operator and show only the areas their role grants
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 7). Self-service: available
/// to any authenticated admin regardless of role, so it is gated by the plain session guard rather
/// than a [`ConsolePermission`]. It returns the same credential-free [`AdminUser`] shape the roster
/// lists — never a password hash or a TOTP secret — and role gating in the console is only a UX
/// convenience; the server re-checks every route's required permission regardless.
async fn admin_whoami<S, R, K, C, A, T, W>(
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

/// The role-aware `/admin` guard ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)):
/// authenticates the session, resolves the acting admin, and checks that their role grants
/// `permission`. Every permission-gated `/admin` route calls this in place of the bare session check.
///
/// On success the acting [`AdminContext`] is returned (routes that only gate can ignore it). On
/// failure it returns the response to send: `401`/`503` from the session check (absent/invalid
/// session, or the store down), or a `403` when the session is valid but the role lacks the
/// permission — a distinct verdict, since the caller is authenticated and only under-privileged.
async fn require_permission<A, C>(
    admin_store: &A,
    clock: &C,
    headers: &HeaderMap,
    permission: ConsolePermission,
) -> Result<AdminContext, Response>
where
    A: AdminStore,
    C: ClockSource,
{
    let context = authenticated_admin(admin_store, clock, headers)
        .await
        .map_err(|denied: SessionDenied| denied.into_response())?;
    if role_grants(context.admin.role, permission) {
        Ok(context)
    } else {
        Err((StatusCode::FORBIDDEN, "insufficient permissions").into_response())
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
async fn admin_create_api_key<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<CreateApiKeyRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageApiKeys,
    )
    .await
    {
        return denied;
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
async fn admin_list_api_keys<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Query(query): Query<ListApiKeysQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await
    {
        return denied;
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
async fn admin_revoke_api_key<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageApiKeys,
    )
    .await
    {
        return denied;
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
async fn admin_list_sessions<S, R, K, C, A, T, W>(
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
async fn admin_revoke_session<S, R, K, C, A, T, W>(
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
        return (
            StatusCode::BAD_REQUEST,
            "the session id is not a valid handle",
        )
            .into_response();
    };
    match app
        .admin
        .revoke_admin_session(&context.admin.id, token_hash)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such session").into_response(),
        Err(_) => admin_service_unavailable(),
    }
}

/// `POST /admin/sessions/revoke-others` — revoke every one of the acting admin's sessions except the
/// one making this request ("sign out everywhere else").
async fn admin_revoke_other_sessions<S, R, K, C, A, T, W>(
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

/// Lower-case hex of a 32-byte hash — the opaque session handle the console sees. Not reversible to
/// the token, and revocation is admin-scoped, so exposing it grants no capability.
fn hex_encode(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Parses a 64-character lower/upper-case hex handle back into the 32-byte hash, or `None` if it is
/// not exactly 64 hex digits.
fn hex_decode_32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0_u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        *slot = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(out)
}

/// One hex digit's value, or `None` if the byte is not a hex digit.
fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

// --- Console self-service security: TOTP re-enrol + recovery codes ([ADR-0067] slice 6) ---------

/// A request to re-enrol TOTP: the current password, re-confirming the knowledge factor before the
/// possession factor is rotated. [`fmt::Debug`] redacts it.
#[derive(Clone, Deserialize)]
struct ReenrolTotpRequest {
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

/// `POST /admin/totp` — a signed-in admin re-enrols their authenticator
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6). Re-confirms the current
/// password (the knowledge factor) before rotating the TOTP secret (the possession factor), so a
/// session-only attacker — one holding the cookie but not the password — cannot lock the owner out by
/// re-enrolling. On success the new one-time enrolment (QR + base32 secret) is returned once; existing
/// sessions stay valid, and the next sign-in uses the new authenticator.
async fn admin_reenrol_totp<S, R, K, C, A, T, W>(
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
    if let Err(denied) = authenticated_admin(&app.admin, &app.clock, &headers).await {
        return denied.into_response();
    }
    let credential = match app.admin.load_credential().await {
        Ok(Some(credential)) => credential,
        Ok(None) => return (StatusCode::CONFLICT, "no administrator is enrolled").into_response(),
        Err(error) => {
            tracing::error!(%error, "loading the credential for TOTP re-enrolment failed");
            return admin_service_unavailable();
        }
    };
    if !credential.credential.password_matches(&request.password) {
        // A distinct 403: the caller is signed in but has not re-proved the knowledge factor.
        return (StatusCode::FORBIDDEN, "the password is incorrect").into_response();
    }
    let Some(secret) = mint_totp_secret() else {
        tracing::error!("could not read OS entropy to mint a TOTP secret");
        return admin_service_unavailable();
    };
    match app.admin.rotate_totp_secret(secret.to_vec()).await {
        Ok(()) => (StatusCode::OK, Json(build_enrolment(&secret))).into_response(),
        Err(error) => {
            tracing::error!(%error, "rotating the TOTP secret failed");
            admin_service_unavailable()
        }
    }
}

/// `POST /admin/recovery-codes` — (re)generate the acting admin's one-time recovery codes
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6). Self-service, so it is
/// gated by the session guard, not a role permission. Mints [`RECOVERY_CODE_COUNT`] codes at the
/// edge, stores only their hashes (replacing any previous set), and returns the plaintext once.
async fn admin_generate_recovery_codes<S, R, K, C, A, T, W>(
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

/// `GET /admin/recovery-codes` — how many unused recovery codes the acting admin has left (never the
/// codes themselves), so the console can prompt a regeneration when the supply runs low.
async fn admin_recovery_codes_status<S, R, K, C, A, T, W>(
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

// --- Console admin management and invitations ([ADR-0067]) --------------------------------------

/// A `503` when the admin store is unreachable — the transient, retryable failure for the admin
/// surface, with the detail kept to the server's log rather than the client.
fn admin_service_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the admin service is unavailable",
    )
        .into_response()
}

/// Refuses a change that would remove the **last active owner** — a demotion or a suspension of the
/// sole remaining `owner` ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)). `removes_owner`
/// is true when the pending change would stop the target being an active owner; otherwise the guard
/// is a no-op. An absent target passes through, so the caller's own update returns the `404`.
async fn guard_last_owner_change<A>(
    admin: &A,
    id: &str,
    removes_owner: bool,
) -> Result<(), Response>
where
    A: AdminStore,
{
    if !removes_owner {
        return Ok(());
    }
    let target = match admin.get_admin_user(id).await {
        Ok(Some(target)) => target,
        Ok(None) => return Ok(()),
        Err(error) => {
            tracing::error!(%error, "reading an admin failed");
            return Err(admin_service_unavailable());
        }
    };
    if target.role == AdminRole::Owner && target.status == AdminStatus::Active {
        let owners = admin.count_active_owners().await.map_err(|error| {
            tracing::error!(%error, "counting owners failed");
            admin_service_unavailable()
        })?;
        if owners <= 1 {
            return Err(
                (StatusCode::CONFLICT, "cannot remove the last active owner").into_response(),
            );
        }
    }
    Ok(())
}

/// A request to invite a new console admin.
#[derive(Debug, Clone, Deserialize)]
struct InviteAdminRequest {
    /// The invitee's email — the address they will sign in with (normalised server-side).
    email: String,
    /// The invitee's display name.
    name: String,
    /// The role to grant on acceptance: `owner`/`admin`/`ops`/`viewer`.
    role: String,
}

/// The one-time response to an invitation: the id, the single-use token (shown once) the inviter
/// copies into the invite link, and the expiry. Only the token's hash is stored.
#[derive(Debug, Clone, serde::Serialize)]
struct InviteAdminResponse {
    /// The invite's id (a ULID).
    invite_id: String,
    /// The single-use invite token. **Shown once** — only its hash is stored, so it cannot be
    /// recovered later; the inviter hands the link carrying it to the invitee out-of-band.
    token: String,
    /// When the invite stops being acceptable (Unix milliseconds).
    expires_at_ms: i64,
}

/// Invites a new console admin ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)). Needs
/// `console.admins.invite` (owner or admin); only an owner may invite another owner, so an admin
/// cannot escalate. Refuses an address that is already an admin. Returns the single-use token once.
async fn admin_invite_admin<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<InviteAdminRequest>,
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::InviteAdmins,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Some(role) = AdminRole::from_token(&request.role) else {
        return (
            StatusCode::BAD_REQUEST,
            "role must be owner, admin, ops, or viewer",
        )
            .into_response();
    };
    // No privilege escalation: only an owner may mint another owner.
    if role == AdminRole::Owner && context.admin.role != AdminRole::Owner {
        return (StatusCode::FORBIDDEN, "only an owner may invite an owner").into_response();
    }
    let email = request.email.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') {
        return (StatusCode::BAD_REQUEST, "a valid email is required").into_response();
    }
    match app.admin.find_admin_user_by_email(&email).await {
        Ok(Some(_)) => {
            return (
                StatusCode::CONFLICT,
                "an admin with that email already exists",
            )
                .into_response();
        }
        Ok(None) => {}
        Err(error) => {
            tracing::error!(%error, "checking for an existing admin failed");
            return admin_service_unavailable();
        }
    }
    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(token)) = (mint_ulid(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint an invite");
        return admin_service_unavailable();
    };
    let ttl_ms = i64::try_from(app.admin_invite_ttl_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
    let Ok(expires_at) =
        pos_proto::time::Timestamp::from_milliseconds_since_epoch(now_ms.saturating_add(ttl_ms))
    else {
        return admin_service_unavailable();
    };
    let invite = NewAdminInvite {
        id: id.to_string(),
        email,
        name: request.name,
        role,
        token_hash: hash_session_token(&token),
        invited_by: context.admin.id,
        expires_at,
    };
    match app.admin.create_invite(invite).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(InviteAdminResponse {
                invite_id: id.to_string(),
                token,
                expires_at_ms: expires_at.as_milliseconds_since_epoch(),
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "creating an invite failed");
            admin_service_unavailable()
        }
    }
}

/// Lists the pending (not accepted, not expired) invitations. Needs `console.admins.invite`.
async fn admin_list_invites<S, R, K, C, A, T, W>(
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
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::InviteAdmins,
    )
    .await
    {
        return denied;
    }
    match app.admin.list_pending_invites(app.clock.now()).await {
        Ok(invites) => (StatusCode::OK, Json(invites)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing invites failed");
            admin_service_unavailable()
        }
    }
}

/// Revokes a pending invitation by id. `204` whether or not one was pending — revoking is idempotent.
/// Needs `console.admins.invite`.
async fn admin_revoke_invite<S, R, K, C, A, T, W>(
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
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::InviteAdmins,
    )
    .await
    {
        return denied;
    }
    match app.admin.revoke_invite(&id).await {
        Ok(_removed) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "revoking an invite failed");
            admin_service_unavailable()
        }
    }
}

/// A self-enrolment request: the single-use invite token and the password the invitee chooses.
///
/// [`fmt::Debug`] redacts both, so a logged request cannot leak the token or the password.
#[derive(Clone, Deserialize)]
struct AcceptInviteRequest {
    /// The single-use invite token from the invite link.
    token: String,
    /// The password the invitee is choosing.
    password: String,
}

impl fmt::Debug for AcceptInviteRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptInviteRequest")
            .field("token", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Accepts an invitation and self-enrols the new admin — **pre-auth**: the invite token is the
/// authorisation, so there is no session guard ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
/// Mints the admin's own Argon2id password hash and TOTP secret exactly as first-boot enrolment does,
/// claims the invite single-use, creates the admin, and returns the one-time TOTP enrolment. A bad or
/// expired token is a generic `401`.
async fn admin_accept_invite<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    Json(request): Json<AcceptInviteRequest>,
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
    if request.password.len() < MIN_PASSWORD_LEN {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "the password is too short",
        )
            .into_response();
    }
    let now = app.clock.now();
    let invite = match app
        .admin
        .find_pending_invite_by_token(hash_session_token(&request.token), now)
        .await
    {
        Ok(Some(invite)) => invite,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                "the invite is invalid or has expired",
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!(%error, "looking up an invite failed");
            return admin_service_unavailable();
        }
    };
    let Some((secret, phc)) = mint_credential(&request.password) else {
        tracing::error!("could not mint a credential for invite acceptance");
        return admin_service_unavailable();
    };
    // Claim the invite before creating the admin, so a replayed acceptance cannot enrol twice.
    match app.admin.mark_invite_accepted(&invite.id, now).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::UNAUTHORIZED,
                "the invite is invalid or has expired",
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!(%error, "claiming an invite failed");
            return admin_service_unavailable();
        }
    }
    let Some(admin_id) = mint_ulid(now.as_milliseconds_since_epoch()) else {
        tracing::error!("could not read OS entropy to mint an admin id");
        return admin_service_unavailable();
    };
    let user = NewAdminUser {
        id: admin_id.to_string(),
        email: invite.email,
        name: invite.name,
        role: invite.role,
        password_phc: phc,
        totp_secret: secret.to_vec(),
    };
    match app.admin.create_admin_user(user).await {
        Ok(true) => (StatusCode::CREATED, Json(build_enrolment(&secret))).into_response(),
        Ok(false) => (
            StatusCode::CONFLICT,
            "an admin with that email already exists",
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "creating the admin failed");
            admin_service_unavailable()
        }
    }
}

/// Lists the console admins — identity and role only, never a credential. Needs
/// `console.admins.invite` (owner or admin may view the roster).
async fn admin_list_admins<S, R, K, C, A, T, W>(
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
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::InviteAdmins,
    )
    .await
    {
        return denied;
    }
    match app.admin.list_admin_users().await {
        Ok(admins) => (StatusCode::OK, Json(admins)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing admins failed");
            admin_service_unavailable()
        }
    }
}

/// A request to change an admin's role.
#[derive(Debug, Clone, Deserialize)]
struct UpdateAdminRoleRequest {
    /// The new role: `owner`/`admin`/`ops`/`viewer`.
    role: String,
}

/// Changes an admin's role. Needs `console.admins.manage` (owner). Refuses demoting the last active
/// owner. `404` if there is no admin with that id.
async fn admin_set_admin_role<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateAdminRoleRequest>,
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
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageAdmins,
    )
    .await
    {
        return denied;
    }
    let Some(role) = AdminRole::from_token(&request.role) else {
        return (
            StatusCode::BAD_REQUEST,
            "role must be owner, admin, ops, or viewer",
        )
            .into_response();
    };
    if let Err(response) = guard_last_owner_change(&app.admin, &id, role != AdminRole::Owner).await
    {
        return response;
    }
    match app.admin.set_admin_user_role(&id, role).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such admin").into_response(),
        Err(error) => {
            tracing::error!(%error, "setting an admin role failed");
            admin_service_unavailable()
        }
    }
}

/// A request to change an admin's status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateAdminStatusRequest {
    /// The new status: `active` or `suspended`.
    status: String,
}

/// Suspends or reactivates an admin. Needs `console.admins.manage` (owner). Refuses suspending the
/// last active owner. `404` if there is no admin with that id.
async fn admin_set_admin_status<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateAdminStatusRequest>,
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
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageAdmins,
    )
    .await
    {
        return denied;
    }
    let Some(status) = AdminStatus::from_token(&request.status) else {
        return (
            StatusCode::BAD_REQUEST,
            "status must be active or suspended",
        )
            .into_response();
    };
    if let Err(response) =
        guard_last_owner_change(&app.admin, &id, status == AdminStatus::Suspended).await
    {
        return response;
    }
    match app.admin.set_admin_user_status(&id, status).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such admin").into_response(),
        Err(error) => {
            tracing::error!(%error, "setting an admin status failed");
            admin_service_unavailable()
        }
    }
}

/// The tenant a config request is scoped to. The super-admin is global, so it names the tenant on
/// the query string, exactly as API-key provisioning does.
#[derive(Debug, Clone, Deserialize)]
struct ConfigTenantQuery {
    /// The tenant whose store's config tree to author or read (a 26-character ULID).
    tenant_id: String,
}

/// The version id a successful publish produced.
#[derive(Debug, Clone, serde::Serialize)]
struct PublishedConfig {
    /// The new config version id (a ULID).
    config_version_id: String,
}

/// The violations a rejected publish reported — the composed document failed validation, so nothing
/// changed and the last good version stays current.
#[derive(Debug, Clone, serde::Serialize)]
struct ConfigViolations {
    /// One human-readable message per violation.
    violations: Vec<String>,
}

/// Authors one level of a store's config tree and publishes the composed version (super-admin only).
///
/// Loads the store's tree for the query's tenant, replaces the named `level`'s document with the
/// request body, and publishes — composing, validating, and (only if valid) appending a new version,
/// which is then persisted. A rejected version is a `422` carrying the violations; nothing is stored
/// and the last good version stays current ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
async fn admin_config_publish<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path((store_id, level)): Path<(String, String)>,
    Query(query): Query<ConfigTenantQuery>,
    Json(document): Json<serde_json::Value>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    let Some(level) = parse_config_level(&level) else {
        return (
            StatusCode::BAD_REQUEST,
            "level must be one of tenant, brand, store, device",
        )
            .into_response();
    };

    // Rehydrate the store's tree (or start a fresh one), authoring against the §10-aware validator.
    let mut tree = match app.config_trees.load(tenant_id, store_id).await {
        Ok(Some(state)) => ConfigTree::from_state(store_id, CapabilityValidator, state),
        Ok(None) => ConfigTree::new(store_id, CapabilityValidator),
        Err(error) => return config_store_error_response(&error),
    };
    let Some(version_id) = mint_version_id(app.clock.now().as_milliseconds_since_epoch()) else {
        tracing::error!("could not read OS entropy to mint a config version id");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the configuration service is unavailable",
        )
            .into_response();
    };
    match tree.publish(level, document, version_id) {
        Ok(id) => {
            if let Err(error) = app
                .config_trees
                .save(tenant_id, store_id, &tree.state())
                .await
            {
                return config_store_error_response(&error);
            }
            (
                StatusCode::OK,
                Json(PublishedConfig {
                    config_version_id: id.to_string(),
                }),
            )
                .into_response()
        }
        Err(ConfigError::Invalid(violations)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ConfigViolations { violations }),
        )
            .into_response(),
    }
}

/// The current effective (composed, validated) config document for a store (super-admin only), or
/// `404` if nothing has been published to it yet.
async fn admin_config_effective<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match app.config_trees.load(tenant_id, store_id).await {
        Ok(Some(state)) => {
            let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
            match tree.current_effective() {
                Some(effective) => (StatusCode::OK, Json(effective.clone())).into_response(),
                None => (
                    StatusCode::NOT_FOUND,
                    "the store has no published configuration",
                )
                    .into_response(),
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            "the store has no published configuration",
        )
            .into_response(),
        Err(error) => config_store_error_response(&error),
    }
}

/// The daily rollup for a store, read under the super-admin session (ADR-0060). The `/v1` rollup
/// read is bearer-authed and tenant-scoped by the key; the dashboard carries the admin session, not
/// a tenant key, so this reads the same rollup while naming the tenant with `?tenant_id=`. The
/// super-admin is global ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)) — it already reads
/// any tenant's configuration — so this is the same trust boundary, and it is a read: nothing is
/// mutated.
async fn admin_daily_rollups<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    // The tenant is named explicitly here (the admin is global), unlike the `/v1` read where it is
    // the API key's. It is a read of the materialised rollup — event counts only, no PII.
    match dashboard(&app.rollups, tenant_id, store_id).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// Resets a store's materialised rollup so the projector rebuilds it from the event log
/// (super-admin only). Saving the empty default clears the per-store cursor, and the next projector
/// pass re-folds every event from the start — the "reset-cursor-and-replay" that rebuilds the cloud's
/// read model from the durable log (`docs/roadmap.md` P7, [ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
/// `204` regardless — a store with no rollup yet is simply reset to the same empty state.
async fn admin_rollups_reset<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match app
        .rollups
        .save(tenant_id, store_id, &StoredRollups::default())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

// --- Webhook endpoint administration (`/admin/webhooks`) ----------------------------------------

/// A super-admin request to register a webhook endpoint for a tenant.
#[derive(Debug, Clone, Deserialize)]
struct RegisterWebhookRequest {
    /// The tenant the endpoint belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store whose event log the endpoint follows (a 26-character ULID).
    store_id: String,
    /// The destination URL. Must be `https`, must carry no credentials, and must resolve only to
    /// public-unicast addresses — vetted here before the endpoint is stored ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
    url: String,
}

/// The one-time response to a registration: the id, the normalized URL, and the signing secret shown
/// **exactly once** — the tenant needs it to verify the HMAC signature on every delivery, and the
/// cloud keeps only its own copy thereafter.
#[derive(Debug, Clone, serde::Serialize)]
struct RegisterWebhookResponse {
    /// The endpoint's public id (a ULID).
    id: String,
    /// The normalized destination URL that was stored.
    url: String,
    /// The HMAC signing secret. Shown once; it is the tenant's copy of what the cloud signs with.
    signing_secret: String,
}

/// The tenant a webhook listing or deletion is scoped to. The super-admin is global, so it names the
/// tenant on the query string, exactly as API-key provisioning and config authoring do.
#[derive(Debug, Clone, Deserialize)]
struct WebhookTenantQuery {
    /// The tenant whose endpoints to list or delete within (a 26-character ULID).
    tenant_id: String,
}

/// Registers a webhook endpoint and returns the one-time signing secret (super-admin only).
///
/// Behind the session guard. The destination is SSRF-vetted before anything is stored: `https` only,
/// no credentials, and every resolved address must be public unicast — so a webhook URL can never
/// become a probe into the cloud's own network ([ADR-0032](../../../docs/adr/0032-webhooks.md)). The
/// id and signing secret are minted at the edge; the secret in the `201` body is the only time it is
/// visible, because the cloud stores it to *sign* with rather than to verify a hash of.
async fn admin_register_webhook<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<RegisterWebhookRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageWebhooks,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(store_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };

    // Vet the destination before it is ever stored. `vet` resolves the host with a real
    // getaddrinfo-backed resolver, which blocks, so it runs on the blocking pool. The vetted
    // addresses are not kept — the delivery slice re-vets before each connect (ADR-0032) — but a
    // structurally unsafe or inward-pointing URL is refused up front rather than at first delivery.
    let raw_url = request.url.clone();
    let vetted = match tokio::task::spawn_blocking(move || {
        vet(&raw_url, crate::webhook::ssrf::resolve_host)
    })
    .await
    {
        Ok(Ok(vetted)) => vetted,
        Ok(Err(rejection)) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("the webhook URL was rejected: {rejection}"),
            )
                .into_response();
        }
        Err(join_error) => {
            tracing::error!(%join_error, "the SSRF vetting task failed to join");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "the webhook service is unavailable",
            )
                .into_response();
        }
    };

    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(secret)) = (mint_webhook_id(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint a webhook endpoint");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the webhook service is unavailable",
        )
            .into_response();
    };
    let endpoint = PersistedWebhook {
        id,
        tenant_id,
        store_id,
        url: vetted.url.clone(),
        secret: SigningSecret::new(secret.clone()),
        cursor: None,
        disabled: false,
    };
    if let Err(error) = app.webhooks.insert(&endpoint).await {
        tracing::error!(%error, "persisting a new webhook endpoint failed");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the webhook service is unavailable",
        )
            .into_response();
    }
    (
        StatusCode::CREATED,
        Json(RegisterWebhookResponse {
            id: id.to_string(),
            url: vetted.url,
            signing_secret: secret,
        }),
    )
        .into_response()
}

/// Lists a tenant's webhook endpoints as metadata only — never a secret (super-admin only).
async fn admin_list_webhooks<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Query(query): Query<WebhookTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match app.webhooks.list_for_tenant(tenant_id).await {
        Ok(summaries) => (StatusCode::OK, Json::<Vec<WebhookSummary>>(summaries)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing webhook endpoints failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the webhook service is unavailable",
            )
                .into_response()
        }
    }
}

/// Deletes a webhook endpoint by id within a tenant (super-admin only). `204` whether or not a live
/// endpoint was found — deletion is idempotent, the tenant scope stops one tenant removing another's
/// endpoint, and reporting which case it was is a needless enumeration signal.
async fn admin_delete_webhook<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WebhookTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageWebhooks,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        id.parse::<Ulid>().map(WebhookEndpointId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the endpoint id is not a ULID",
        )
            .into_response();
    };
    match app.webhooks.delete(tenant_id, id).await {
        Ok(_removed) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "deleting a webhook endpoint failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the webhook service is unavailable",
            )
                .into_response()
        }
    }
}

/// Re-enables a webhook endpoint that a day of continuous failure had auto-disabled (super-admin
/// only). Delivery resumes from the endpoint's stored cursor, so nothing that queued while it was
/// down is skipped ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
///
/// Tenant-scoped like deletion, but enforced differently: [`WebhookEndpointStore::set_disabled`]
/// addresses an endpoint by id alone — the delivery task, which also calls it, holds no tenant — so
/// the scope is checked here by confirming the id appears in this tenant's own listing before the
/// flag is cleared. An id that is not this tenant's is a `404`, never a cross-tenant write. Idempotent:
/// re-enabling an already-active endpoint is a no-op `204`.
async fn admin_enable_webhook<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WebhookTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageWebhooks,
    )
    .await
    {
        return denied;
    }
    let (Ok(tenant_id), Ok(endpoint_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        id.parse::<Ulid>().map(WebhookEndpointId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or the endpoint id is not a ULID",
        )
            .into_response();
    };
    // Confirm the endpoint is this tenant's before clearing its flag: `set_disabled` is not itself
    // tenant-scoped, so the scope is enforced here against the tenant's own listing.
    match app.webhooks.list_for_tenant(tenant_id).await {
        Ok(summaries) => {
            if !summaries
                .iter()
                .any(|summary| summary.id == endpoint_id.to_string())
            {
                return (
                    StatusCode::NOT_FOUND,
                    "no such webhook endpoint for this tenant",
                )
                    .into_response();
            }
        }
        Err(error) => {
            tracing::error!(%error, "listing webhook endpoints failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "the webhook service is unavailable",
            )
                .into_response();
        }
    }
    match app.webhooks.set_disabled(endpoint_id, false).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "re-enabling a webhook endpoint failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the webhook service is unavailable",
            )
                .into_response()
        }
    }
}

/// Mints a webhook endpoint id from `now_ms`, or `None` if the OS entropy source is unavailable.
fn mint_webhook_id(now_ms: i64) -> Option<WebhookEndpointId> {
    mint_ulid(now_ms).map(WebhookEndpointId::new)
}

/// Maps a config-tree store failure to a retryable `503`, logging the detail rather than leaking it.
fn config_store_error_response(error: &crate::config_tree::ConfigStoreError) -> Response {
    tracing::error!(%error, "a config-tree store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the configuration service is unavailable",
    )
        .into_response()
}

/// Parses a config-tree level from its path segment, or `None` for an unknown one.
fn parse_config_level(level: &str) -> Option<ConfigLevel> {
    match level {
        "tenant" => Some(ConfigLevel::Tenant),
        "brand" => Some(ConfigLevel::Brand),
        "store" => Some(ConfigLevel::Store),
        "device" => Some(ConfigLevel::Device),
        _ => None,
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

/// Mints an API-key id from `now_ms`, or `None` if the OS entropy source is unavailable.
fn mint_api_key_id(now_ms: i64) -> Option<ApiKeyId> {
    mint_ulid(now_ms).map(ApiKeyId::new)
}

/// Mints a config version id from `now_ms`, or `None` if the OS entropy source is unavailable.
fn mint_version_id(now_ms: i64) -> Option<ConfigVersionId> {
    mint_ulid(now_ms).map(ConfigVersionId::new)
}

/// A fresh ULID: `now_ms` as the timestamp and 80 CSPRNG bits as the randomness, or `None` if the OS
/// entropy source is unavailable (the caller then fails closed rather than mint a guessable id).
fn mint_ulid(now_ms: i64) -> Option<Ulid> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    let ms = u64::try_from(now_ms.max(0)).unwrap_or(0);
    // `Ulid::from_parts` masks the randomness to the low 80 bits the format defines.
    Some(Ulid::from_parts(ms, u128::from_le_bytes(bytes)))
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

/// Mints a fresh super-admin credential from OS entropy: a [`TOTP_SECRET_BYTES`]-byte TOTP secret and
/// the Argon2id PHC hash of `password` under a fresh 16-byte CSPRNG salt. `None` if the OS entropy
/// source is unavailable or hashing fails — the caller then fails closed rather than provision a
/// credential built on weak randomness.
fn mint_credential(password: &str) -> Option<([u8; TOTP_SECRET_BYTES], String)> {
    let mut secret = [0_u8; TOTP_SECRET_BYTES];
    getrandom::fill(&mut secret).ok()?;
    let mut salt_bytes = [0_u8; 16];
    getrandom::fill(&mut salt_bytes).ok()?;
    let salt = SaltString::encode_b64(&salt_bytes).ok()?;
    let phc = hash_password(password, &salt).ok()?;
    Some((secret, phc))
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
