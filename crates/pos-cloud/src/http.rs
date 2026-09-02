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
use std::sync::Arc;

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
use pos_proto::campaign::{
    PublishedAction, PublishedCampaign, PublishedCampaignKind, PublishedConditions,
};
use pos_proto::channels::{
    PublishedChannels, PublishedTender, PublishedVendorPolicies, PublishedVendorPolicy,
};
use pos_proto::determinism::ClockSource;
use pos_proto::display::GridPosition;
use pos_proto::enums::{PaymentMethod, SalesChannel, UnitOfMeasure};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{
    AreaId, CampaignId, ConfigVersionId, CourseId, DeviceId, DisplayCategoryId,
    DisplaySubcategoryId, EventId, IngredientId, MenuItemId, StationId, StoreId, SubjectId,
    SupplierId, TableId, TaxClassId, TenantId, VoucherId,
};
use pos_proto::inventory::{
    PublishedIngredient, PublishedRecipe, PublishedRecipeLine, PublishedSupplier,
};
use pos_proto::locale::TaxRate;
use pos_proto::money::CurrencyCode;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::{Open, WireEnum};

use pos_country::CountryRegistry;

use pos_core::activation::{ActivationCode, Redemption, redeem};
use pos_core::business_date::{CutoffHour, StoreTimeZone};

use crate::activation::{ActivationCodeStore, hash_code, mint_device_credential};
use crate::alerts::{AlertRecord, AlertStore, AlertStoreError};
use crate::audit::{AuditActor, AuditEntry, AuditId, AuditRecorder, AuditStore, NoopAuditRecorder};
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
use crate::campaigns::{CampaignStore, CampaignStoreError, to_node as campaigns_to_node};
use crate::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, ChannelPrice, DisplayCategory,
    DisplaySubcategory, ItemCategory, ItemCategoryId, ItemSubcategory, ItemSubcategoryId,
    LayoutButton, Menu, MenuId, MenuPlacement, MenuSection, MenuSectionId, ModifierGroup,
    ModifierGroupId, TaxClass,
};
use crate::catalog_compiler::{compile_layout_book, compile_menu};
use crate::cloud::{Cloud, DailyRollup};
use crate::config_tree::{
    CapabilityValidator, ConfigError, ConfigLevel, ConfigTree, ConfigTreeState, ConfigTreeStore,
    ConfigValidator, SyncOutcome,
    merge::{diff, merge_layers},
};
use crate::dashboard::{
    RollupError, RollupStore, RollupWindow, StoredRollups, dashboard, revenue, xz_report,
};
use crate::devices::{
    DeviceKind, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore, DeviceProposalSummary,
    PersistedDeviceProposal,
};
use crate::export;
use crate::fleet::{FleetRow, FleetStore, FleetStoreError, OtaReportStore};
use crate::floor_compiler::{compile_floor, compile_stations};
use crate::floorplan::{
    Area, AreaStore, AreaUpdate, NewArea, NewRoutingRule, NewStation, NewTable, RoutingRule,
    RoutingRuleId, RoutingRuleStore, Station, StationStore, StationUpdate, Table, TableStore,
    TableUpdate,
};
use crate::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use crate::images::{self, ImagePipelineError};
use crate::import;
use crate::inventory::{InventoryStore, InventoryStoreError, to_node as inventory_to_node};
use crate::media::{MediaId, MediaStore, MediaStoreError, NewMediaAsset, Rendition};
use crate::openapi::ApiDoc;
use crate::people::{
    Assignment, AssignmentId, AssignmentStore, Employee, EmployeeId, EmployeeStore, EmployeeUpdate,
    NewAssignment, NewEmployee, NewRoleTemplate, PermissionInfo, RoleTemplate, RoleTemplateId,
    RoleTemplateStore, RoleTemplateUpdate, is_known_permission, permission_catalogue,
};
use crate::people_compiler::compile_permissions;
use crate::qr::{TableTokenSecret, mint_table_token};
use crate::reconcile::{ReconcileRun, ReconcileRunStore, ReconcileStore};
use crate::registry::{
    BrandId, BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError,
    StoreRecord, TenantRecord,
};
use crate::retention::{RetentionError, SubjectStore};
use crate::scheduling::{
    NewScheduledPublish, ScheduledPublishError, ScheduledPublishStatus, ScheduledPublishStore,
};
use crate::tax::{TaxRateEntry, TaxRateStore, TaxRateStoreError, to_table};
use crate::translations::{TranslationGrid, TranslationStore};
use crate::vouchers::{NewVoucher, VoucherStore, VoucherStoreError, generate_code};
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
    /// How many proxies in front of this process are trusted to have appended to `X-Forwarded-For`
    /// ([ADR-0090](../../../docs/adr/0090-tls-postures.md)). It is the rate limit's client key, so
    /// it belongs to the deployment's TLS posture rather than to this code.
    trusted_proxy_hops: usize,
    /// The console audit recorder ([ADR-0069](../../../docs/adr/0069-audit-trail.md)). Defaults to a
    /// no-op, so a route can always record unconditionally; the binary wires the real store in.
    audit: Arc<dyn AuditRecorder>,
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
            trusted_proxy_hops: self.trusted_proxy_hops,
            audit: Arc::clone(&self.audit),
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
            trusted_proxy_hops: DEFAULT_TRUSTED_PROXY_HOPS,
            audit: Arc::new(NoopAuditRecorder),
        }
    }

    /// Sets the console audit recorder ([ADR-0069](../../../docs/adr/0069-audit-trail.md)); the binary
    /// wraps its `store-postgres` audit store and threads it in. Left unset, mutations are not audited
    /// (a no-op recorder), the posture the fakes-backed tests use unless they assert on audit.
    #[must_use]
    pub fn with_audit(mut self, audit: Arc<dyn AuditRecorder>) -> Self {
        self.audit = audit;
        self
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

    /// Sets how many proxies in front of this process are trusted to have appended to
    /// `X-Forwarded-For` — the binary threads the configured value in
    /// ([`crate::config::CloudConfig::trusted_proxy_hops`],
    /// [ADR-0090](../../../docs/adr/0090-tls-postures.md)).
    ///
    /// This is the `/admin/login` rate limit's client key. One is correct behind the bundled Caddy
    /// alone; a deployment where TLS terminates further upstream has two, and `bootstrap.sh` derives
    /// the value from `TLS_MODE` so nobody has to remember it. `0` is refused at config load.
    #[must_use]
    pub const fn with_trusted_proxy_hops(mut self, hops: usize) -> Self {
        self.trusted_proxy_hops = hops;
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
        .route(
            "/sync/stores/{store_id}/heartbeat",
            post(edge_heartbeat::<S, R, K, C, A, T, W>),
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
            "/admin/stores/{store_id}/config/versions",
            get(admin_config_versions::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/config/versions/{version_id}",
            get(admin_config_version_effective::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/config/rollback",
            post(admin_config_rollback::<S, R, K, C, A, T, W>),
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
            "/admin/stores/{store_id}/revenue/daily",
            get(admin_revenue_daily::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/reports/xz",
            get(admin_xz_report::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/rollups/export",
            get(admin_export_rollups::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/revenue/export",
            get(admin_export_revenue::<S, R, K, C, A, T, W>),
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

/// The collaborators the reconciliation routes share: the diff-and-history store, the admin store the
/// admin read authenticates against, and the clock that stamps each recorded run and drives the
/// session guard.
#[derive(Clone)]
struct ReconcileState<Rec, A, C> {
    store: Rec,
    admin: A,
    clock: C,
}

/// Builds the reconciliation sub-router, stated independently of [`CloudApp`].
///
/// `POST /internal/reconcile` is the cloud's half of reconciliation ([ADR-0040](../../../docs/adr/0040-reconciliation.md)):
/// an edge sends the ids it holds for a store, and the cloud answers with the subset it is missing —
/// the ids to re-push through `/internal/ingest`. Every diff now also records a run into the history
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)), so `GET /admin/reconcile` can show the
/// console that reconciliation happened and what it caught. The internal route is private-network and
/// unauthenticated (absent from the public OpenAPI, exactly like `/internal/ingest`); the admin read is
/// behind [`ConsolePermission::Read`]. Stated independently and merged in `main`, rather than threading
/// an extra collaborator through every `CloudApp` handler.
pub fn reconcile_router<Rec, A, C>(store: Rec, admin: A, clock: C) -> Router
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/internal/reconcile", post(reconcile::<Rec, A, C>))
        .route("/admin/reconcile", get(admin_reconcile_runs::<Rec, A, C>))
        .with_state(ReconcileState {
            store,
            admin,
            clock,
        })
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
/// ([ADR-0040](../../../docs/adr/0040-reconciliation.md)), and records the run into the history
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)). Internal (the reconciliation partner of
/// `/internal/ingest`), so it carries no authentication and is absent from the public OpenAPI.
async fn reconcile<Rec, A, C>(
    State(state): State<ReconcileState<Rec, A, C>>,
    Json(request): Json<ReconcileRequest>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
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
    match state
        .store
        .absent_event_ids(tenant_id, store_id, &candidates)
        .await
    {
        Ok(missing) => {
            // Record the run into the history — best-effort, so a recording failure never denies the
            // edge the diff it is waiting on (the diff is the primary product; the history is a trail).
            let now = state.clock.now();
            if let Some(run_id) = mint_ulid(now.as_milliseconds_since_epoch()) {
                let run = ReconcileRun {
                    run_id: run_id.to_string(),
                    store: store_id,
                    candidates_offered: u32::try_from(candidates.len()).unwrap_or(u32::MAX),
                    missing_found: u32::try_from(missing.len()).unwrap_or(u32::MAX),
                    ran_at: now,
                };
                if let Err(error) = state.store.record_run(tenant_id, &run).await {
                    tracing::warn!(%error, "recording a reconciliation run failed; the diff still answered");
                }
            }
            (
                StatusCode::OK,
                Json(ReconcileResponse {
                    missing: missing.iter().map(ToString::to_string).collect(),
                }),
            )
                .into_response()
        }
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

/// The default and maximum page size for the reconciliation history read.
const RECONCILE_HISTORY_DEFAULT_LIMIT: u32 = 100;
const RECONCILE_HISTORY_MAX_LIMIT: u32 = 500;

/// A `GET /admin/reconcile` query: the tenant whose runs to list, an optional store filter, and an
/// optional page size.
#[derive(Debug, Clone, Deserialize)]
struct ReconcileHistoryQuery {
    /// The tenant whose reconciliation history to read (a 26-character ULID).
    tenant_id: String,
    /// Narrow to one store (a ULID); absent reads across the tenant's stores.
    #[serde(default)]
    store_id: Option<String>,
    /// Cap the number of runs returned; defaults to [`RECONCILE_HISTORY_DEFAULT_LIMIT`], clamped to
    /// [`RECONCILE_HISTORY_MAX_LIMIT`].
    #[serde(default)]
    limit: Option<u32>,
}

/// One reconciliation run on the wire ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)):
/// counts and a timestamp, never event contents or a customer identifier.
#[derive(Debug, Clone, serde::Serialize)]
struct ReconcileRunView {
    /// The run's id (a ULID string; chronological).
    run_id: String,
    /// The store the diff was for (a ULID string).
    store_id: String,
    /// How many ids the edge offered in its manifest.
    candidates_offered: u32,
    /// How many of them the cloud was missing (asked the edge to re-push); zero means fully in sync.
    missing_found: u32,
    /// Unix ms of the diff.
    ran_at_ms: i64,
}

impl ReconcileRunView {
    fn from_run(run: ReconcileRun) -> Self {
        Self {
            run_id: run.run_id,
            store_id: run.store.to_string(),
            candidates_offered: run.candidates_offered,
            missing_found: run.missing_found,
            ran_at_ms: run.ran_at.as_milliseconds_since_epoch(),
        }
    }
}

/// Lists a tenant's most recent reconciliation runs, newest first
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)) — so the console shows that
/// reconciliation ran and what it caught. Tenant-scoped (a store's runs are its tenant's data), behind
/// [`ConsolePermission::Read`].
async fn admin_reconcile_runs<Rec, A, C>(
    State(state): State<ReconcileState<Rec, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ReconcileHistoryQuery>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
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
    let store = match query.store_id.as_deref() {
        Some(raw) => match raw.parse::<Ulid>().map(StoreId::new) {
            Ok(store) => Some(store),
            Err(_) => return (StatusCode::BAD_REQUEST, "store_id is not a ULID").into_response(),
        },
        None => None,
    };
    let limit = query
        .limit
        .unwrap_or(RECONCILE_HISTORY_DEFAULT_LIMIT)
        .min(RECONCILE_HISTORY_MAX_LIMIT);
    match state.store.list_runs(tenant_id, store, limit).await {
        Ok(runs) => {
            let view: Vec<ReconcileRunView> =
                runs.into_iter().map(ReconcileRunView::from_run).collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "reading the reconciliation history failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the reconciliation service is unavailable",
            )
                .into_response()
        }
    }
}

/// The state the OTA-report ingest carries: the write seam and the server clock that stamps the
/// report's arrival instant.
#[derive(Clone)]
struct OtaReportState<R, C> {
    reports: R,
    clock: C,
}

/// An edge's OTA report body ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)): which store
/// reported, the version it is now running, and whether the post-install self-test passed. Identity is
/// in the body because `/internal` is trusted-network and unauthenticated, exactly like `/internal/reconcile`.
#[derive(Debug, Clone, Deserialize)]
struct OtaReportRequest {
    /// The tenant the store belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store reporting (a 26-character ULID).
    store_id: String,
    /// The release the store is now running (a version string, e.g. `v1.2.3`).
    installed: String,
    /// Whether the post-install self-test passed.
    self_test_passed: bool,
}

/// Builds the OTA-report ingest sub-router ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// stated independently of [`CloudApp`] like [`reconcile_router`].
///
/// `POST /internal/ota/report` records the version a store is running and its last self-test outcome
/// onto the fleet-liveness read model, so the cloud can see rollout-ring progress. Internal,
/// private-network, and absent from the public OpenAPI, exactly like `/internal/ingest` and
/// `/internal/reconcile`; the server stamps the arrival instant from its own clock.
pub fn ota_report_router<R, C>(reports: R, clock: C) -> Router
where
    R: OtaReportStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/internal/ota/report", post(ingest_ota_report::<R, C>))
        .with_state(OtaReportState { reports, clock })
}

/// Records one store's OTA report ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)).
/// Internal (the reporting partner of `/internal/ingest`), so it carries no authentication and is
/// absent from the public OpenAPI. A malformed id or an empty version is a `400`; a store failure is
/// a retryable `503`; success is `204`.
async fn ingest_ota_report<R, C>(
    State(state): State<OtaReportState<R, C>>,
    Json(request): Json<OtaReportRequest>,
) -> Response
where
    R: OtaReportStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
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
    let installed = request.installed.trim();
    if installed.is_empty() {
        return (StatusCode::BAD_REQUEST, "an installed version is required").into_response();
    }
    match state
        .reports
        .record_report(
            tenant_id,
            store_id,
            installed,
            request.self_test_passed,
            state.clock.now(),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "recording an OTA report failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the fleet service is unavailable",
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
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the device-onboarding sub-router, stated independently of [`CloudApp`]
/// ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
///
/// A store proposes a discovered printer/KDS and reads back its approved devices on the store-facing
/// `/sync` surface (API key, `manage_devices` scope); a super-admin lists the pending queue and
/// approves or rejects on `/admin` (session guard). It needs the proposal store plus the existing
/// admin/api-key/clock collaborators, so — like [`reconcile_router`] — it carries its own state and is
/// merged into the main router rather than adding an eighth `CloudApp` generic.
pub fn device_router<D, A, K, C>(
    devices: D,
    admin: A,
    keys: K,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
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
            audit,
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(found) => {
            // Only a resolve that actually acted on a pending proposal is worth an entry; resolving
            // an already-resolved id is an idempotent `204` and records nothing. `resolved_by` is the
            // acting admin, snapshotted onto the entry by `audit_action`.
            if found {
                audit_action(
                    &state.audit,
                    &state.clock,
                    &context,
                    Some(tenant_id),
                    if approved {
                        "device_proposal.approve"
                    } else {
                        "device_proposal.reject"
                    },
                    "device_proposal",
                    &id.to_string(),
                    None,
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
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
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the org-registry sub-router ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
///
/// Every route is behind the super-admin session guard, and names a tenant the admin-is-global way
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): a `?tenant_id=` query for the
/// listings, the request body for a create. Device routes nest under their store, so they never
/// collide with the `/admin/devices/proposals` onboarding queue ([`device_router`]).
pub fn registry_router<Rg, A, C>(
    registry: Rg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
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
            audit,
        })
}

// --- People & access (`/admin/employees|roles|assignments`, ADR-0070) ---------------------------

/// The collaborators the people routes need, stated independently of [`CloudApp`]: the people store
/// (one type that is the employee, role-template, and assignment seam at once — the binary's
/// `PostgresPeople` implements all three), plus the admin and clock every session guard uses, and the
/// audit recorder every write emits to. Like [`RegistryState`], it carries its own state and is merged
/// into the main router.
#[derive(Clone)]
struct PeopleState<P, A, C> {
    people: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// The tenant a people read/write is scoped to (the super-admin is global, ADR-0060).
#[derive(Debug, Clone, Deserialize)]
struct PeopleTenantQuery {
    /// The tenant to act within (a 26-character ULID).
    tenant_id: String,
}

/// The tenant plus which side to list assignments from: exactly one of `store_id` (everyone at a
/// store) or `employee_id` (every store a person works at).
#[derive(Debug, Clone, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "the field names are the query-string wire contract"
)]
struct AssignmentListQuery {
    tenant_id: String,
    #[serde(default)]
    store_id: Option<String>,
    #[serde(default)]
    employee_id: Option<String>,
}

/// Create an employee — identity only; the PIN is set separately.
#[derive(Debug, Clone, Deserialize)]
struct CreateEmployeeRequest {
    tenant_id: String,
    code: String,
    name: String,
}

/// Rename an employee and/or set their status (`active`/`archived`).
#[derive(Debug, Clone, Deserialize)]
struct UpdateEmployeeRequest {
    tenant_id: String,
    name: String,
    status: String,
}

/// Set or reset an employee's sign-in PIN. The digits are hashed here and never stored or logged in
/// the clear (ADR-0070); the request body is the only place they appear, and it is never audited.
#[derive(Debug, Clone, Deserialize)]
struct SetPinRequest {
    tenant_id: String,
    pin: String,
}

/// Create a role template — a named subset of the `pos-core` permission catalogue (§9).
#[derive(Debug, Clone, Deserialize)]
struct CreateRoleRequest {
    tenant_id: String,
    name: String,
    permissions: Vec<String>,
}

/// Update a role template's name, permission set, and status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateRoleRequest {
    tenant_id: String,
    name: String,
    permissions: Vec<String>,
    status: String,
}

/// Assign an employee to a store with a role.
#[derive(Debug, Clone, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "the field names are the JSON wire contract"
)]
struct CreateAssignmentRequest {
    tenant_id: String,
    employee_id: String,
    store_id: String,
    role_template_id: String,
}

/// A PIN must be 4–8 digits. Its defence is the Argon2id cost plus the edge's attempt rate-limit, not
/// length (ADR-0030/0070); this only rejects the obviously-wrong (non-digits, too short/long).
fn pin_is_well_formed(pin: &str) -> bool {
    (4..=8).contains(&pin.len()) && pin.bytes().all(|byte| byte.is_ascii_digit())
}

/// Hashes a PIN with Argon2id under a fresh 16-byte CSPRNG salt — the same primitive and shape as
/// [`mint_credential`]. `None` if OS entropy is unavailable or hashing fails, so the caller fails
/// closed rather than store a weak or empty hash.
fn hash_pin(pin: &str) -> Option<String> {
    let mut salt_bytes = [0_u8; 16];
    getrandom::fill(&mut salt_bytes).ok()?;
    let salt = SaltString::encode_b64(&salt_bytes).ok()?;
    hash_password(pin, &salt).ok()
}

/// Maps any people-store failure to a retryable `503`, logging the detail rather than leaking it.
fn people_error_response(error: &impl std::fmt::Display) -> Response {
    tracing::error!(%error, "a people & access store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the people service is unavailable",
    )
        .into_response()
}

/// `503` when OS entropy is unavailable to mint an id or hash a PIN — the request cannot proceed
/// safely, and it is transient.
fn people_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy for a people & access write");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the people service is unavailable",
    )
        .into_response()
}

/// Builds the people & access sub-router ([ADR-0070](../../../docs/adr/0070-people-and-access.md)).
///
/// Reads are behind [`ConsolePermission::Read`] (every console role); every write is behind the new
/// [`ConsolePermission::ManagePeople`] (Owner/Admin) and emits an audit entry that records the
/// employee **id, code, status, and role — never the name, and never the PIN or its hash** (ADR-0070).
/// The tenant is named the admin-is-global way: a `?tenant_id=` query on reads, the request body on
/// writes.
pub fn people_router<P, A, C>(
    people: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/people/permissions",
            get(admin_list_permissions::<P, A, C>),
        )
        .route(
            "/admin/employees",
            get(admin_list_employees::<P, A, C>).post(admin_create_employee::<P, A, C>),
        )
        .route(
            "/admin/employees/{employee_id}",
            get(admin_get_employee::<P, A, C>).patch(admin_update_employee::<P, A, C>),
        )
        .route(
            "/admin/employees/{employee_id}/pin",
            axum::routing::put(admin_set_employee_pin::<P, A, C>),
        )
        .route(
            "/admin/roles",
            get(admin_list_roles::<P, A, C>).post(admin_create_role::<P, A, C>),
        )
        .route(
            "/admin/roles/{role_id}",
            get(admin_get_role::<P, A, C>).patch(admin_update_role::<P, A, C>),
        )
        .route(
            "/admin/assignments",
            get(admin_list_assignments::<P, A, C>).post(admin_create_assignment::<P, A, C>),
        )
        .route(
            "/admin/assignments/{assignment_id}",
            delete(admin_remove_assignment::<P, A, C>),
        )
        .with_state(PeopleState {
            people,
            admin,
            clock,
            audit,
        })
}

/// The `pos-core` permission catalogue (§9), for the console's role editor to offer. Behind `Read` and
/// tenant-independent — the catalogue is the same for every tenant.
async fn admin_list_permissions<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
) -> Response
where
    P: Clone + Send + Sync + 'static,
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
    (
        StatusCode::OK,
        Json::<Vec<PermissionInfo>>(permission_catalogue()),
    )
        .into_response()
}

/// A super-admin lists a tenant's employees.
async fn admin_list_employees<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
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
    match state.people.list(tenant_id).await {
        Ok(employees) => (StatusCode::OK, Json::<Vec<Employee>>(employees)).into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin reads one employee within its tenant.
async fn admin_get_employee<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Path(employee_id): Path<String>,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
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
    let (Ok(tenant_id), Ok(employee_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        employee_id.parse::<Ulid>().map(EmployeeId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the employee id or tenant_id is not a ULID",
        )
            .into_response();
    };
    match state.people.get(tenant_id, employee_id).await {
        Ok(Some(employee)) => (StatusCode::OK, Json(employee)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such employee").into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin creates an employee (no PIN yet). Audited `employee.create` with id/code/status —
/// never the name.
async fn admin_create_employee<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateEmployeeRequest>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    if request.code.trim().is_empty() || request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "code and name are required").into_response();
    }
    let Some(employee_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(EmployeeId::new)
    else {
        return people_entropy_unavailable();
    };
    let new_employee = NewEmployee {
        employee_id,
        tenant_id,
        code: request.code.clone(),
        name: request.name,
    };
    match state.people.create(&new_employee).await {
        Ok(()) => {
            // The trail records id/code/status — never the name (ADR-0070).
            let after = serde_json::json!({
                "id": employee_id.to_string(),
                "code": request.code,
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "employee.create",
                "employee",
                &employee_id.to_string(),
                None,
                Some(after),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": employee_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin renames an employee and/or sets their status. Audited `employee.update` with
/// before/after id/code/status — never the name.
async fn admin_update_employee<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Path(employee_id): Path<String>,
    Json(request): Json<UpdateEmployeeRequest>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(employee_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        employee_id.parse::<Ulid>().map(EmployeeId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the employee id or tenant_id is not a ULID",
        )
            .into_response();
    };
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    // Read the current row for the audit `before` (id/code/status, never the name) and to answer 404.
    let existing = match state.people.get(tenant_id, employee_id).await {
        Ok(Some(employee)) => employee,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such employee").into_response(),
        Err(error) => return people_error_response(&error),
    };
    let update = EmployeeUpdate {
        employee_id,
        tenant_id,
        name: request.name,
        status,
    };
    match state.people.update(&update).await {
        Ok(true) => {
            let before = serde_json::json!({
                "id": employee_id.to_string(),
                "code": existing.code,
                "status": existing.status.as_str(),
            });
            let after = serde_json::json!({
                "id": employee_id.to_string(),
                "code": existing.code,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "employee.update",
                "employee",
                &employee_id.to_string(),
                Some(before),
                Some(after),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such employee").into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin sets or resets an employee's PIN. The digits are hashed with Argon2id here and never
/// returned, stored raw, or **audited** — the entry records only that a PIN was set, by whom
/// (ADR-0070).
async fn admin_set_employee_pin<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Path(employee_id): Path<String>,
    Json(request): Json<SetPinRequest>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(employee_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        employee_id.parse::<Ulid>().map(EmployeeId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the employee id or tenant_id is not a ULID",
        )
            .into_response();
    };
    if !pin_is_well_formed(&request.pin) {
        return (StatusCode::BAD_REQUEST, "the PIN must be 4 to 8 digits").into_response();
    }
    let Some(pin_phc) = hash_pin(&request.pin) else {
        return people_entropy_unavailable();
    };
    match state.people.set_pin(tenant_id, employee_id, &pin_phc).await {
        Ok(true) => {
            // No before/after: the trail records that a PIN was set for this employee, by whom — never
            // the PIN or its hash (ADR-0070).
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "employee.set_pin",
                "employee",
                &employee_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such employee").into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin lists a tenant's role templates.
async fn admin_list_roles<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: RoleTemplateStore + Clone + Send + Sync + 'static,
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
    match state.people.list(tenant_id).await {
        Ok(roles) => (StatusCode::OK, Json::<Vec<RoleTemplate>>(roles)).into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin reads one role template within its tenant.
async fn admin_get_role<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Path(role_id): Path<String>,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: RoleTemplateStore + Clone + Send + Sync + 'static,
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
    let (Ok(tenant_id), Ok(role_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        role_id.parse::<Ulid>().map(RoleTemplateId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the role id or tenant_id is not a ULID",
        )
            .into_response();
    };
    match state.people.get(tenant_id, role_id).await {
        Ok(Some(role)) => (StatusCode::OK, Json(role)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such role").into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// Validates that every permission id is in the `pos-core` catalogue (§9); returns the first unknown.
fn first_unknown_permission(permissions: &[String]) -> Option<&str> {
    permissions
        .iter()
        .map(String::as_str)
        .find(|id| !is_known_permission(id))
}

/// A super-admin creates a role template. The permission set is validated against the catalogue.
/// Audited `role.create` with id/name/permissions (a role name is not PII).
async fn admin_create_role<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateRoleRequest>,
) -> Response
where
    P: RoleTemplateStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    if let Some(unknown) = first_unknown_permission(&request.permissions) {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown permission id: {unknown}"),
        )
            .into_response();
    }
    let Some(role_template_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(RoleTemplateId::new)
    else {
        return people_entropy_unavailable();
    };
    let new_role = NewRoleTemplate {
        role_template_id,
        tenant_id,
        name: request.name.clone(),
        permissions: request.permissions.clone(),
    };
    match state.people.create(&new_role).await {
        Ok(()) => {
            let after = serde_json::json!({
                "id": role_template_id.to_string(),
                "name": request.name,
                "permissions": request.permissions,
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "role.create",
                "role",
                &role_template_id.to_string(),
                None,
                Some(after),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": role_template_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin updates a role template's name, permissions, and status. Audited `role.update`.
async fn admin_update_role<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Path(role_id): Path<String>,
    Json(request): Json<UpdateRoleRequest>,
) -> Response
where
    P: RoleTemplateStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(role_template_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        role_id.parse::<Ulid>().map(RoleTemplateId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the role id or tenant_id is not a ULID",
        )
            .into_response();
    };
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    if let Some(unknown) = first_unknown_permission(&request.permissions) {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown permission id: {unknown}"),
        )
            .into_response();
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let update = RoleTemplateUpdate {
        role_template_id,
        tenant_id,
        name: request.name.clone(),
        permissions: request.permissions.clone(),
        status,
    };
    match state.people.update(&update).await {
        Ok(true) => {
            let after = serde_json::json!({
                "id": role_template_id.to_string(),
                "name": request.name,
                "permissions": request.permissions,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "role.update",
                "role",
                &role_template_id.to_string(),
                None,
                Some(after),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such role").into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin lists assignments — everyone at a store (`?store_id=`) or every store a person works
/// at (`?employee_id=`), exactly one.
async fn admin_list_assignments<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Query(query): Query<AssignmentListQuery>,
) -> Response
where
    P: AssignmentStore + Clone + Send + Sync + 'static,
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
    let result = match (query.store_id.as_deref(), query.employee_id.as_deref()) {
        (Some(store), None) => {
            let Ok(store_id) = store.parse::<Ulid>().map(StoreId::new) else {
                return (StatusCode::BAD_REQUEST, "store_id is not a ULID").into_response();
            };
            state.people.list_for_store(tenant_id, store_id).await
        }
        (None, Some(employee)) => {
            let Ok(employee_id) = employee.parse::<Ulid>().map(EmployeeId::new) else {
                return (StatusCode::BAD_REQUEST, "employee_id is not a ULID").into_response();
            };
            state.people.list_for_employee(tenant_id, employee_id).await
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "name exactly one of store_id or employee_id",
            )
                .into_response();
        }
    };
    match result {
        Ok(assignments) => (StatusCode::OK, Json::<Vec<Assignment>>(assignments)).into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin assigns an employee to a store with a role. Audited `assignment.create` with the
/// three ids.
async fn admin_create_assignment<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateAssignmentRequest>,
) -> Response
where
    P: AssignmentStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(employee_id), Ok(store_id), Ok(role_template_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.employee_id.parse::<Ulid>().map(EmployeeId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
        request
            .role_template_id
            .parse::<Ulid>()
            .map(RoleTemplateId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, employee_id, store_id, or role_template_id is not a ULID",
        )
            .into_response();
    };
    let Some(assignment_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(AssignmentId::new)
    else {
        return people_entropy_unavailable();
    };
    let new_assignment = NewAssignment {
        assignment_id,
        tenant_id,
        employee_id,
        store_id,
        role_template_id,
    };
    match state.people.assign(&new_assignment).await {
        Ok(()) => {
            let after = serde_json::json!({
                "id": assignment_id.to_string(),
                "employee_id": employee_id.to_string(),
                "store_id": store_id.to_string(),
                "role_template_id": role_template_id.to_string(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "assignment.create",
                "assignment",
                &assignment_id.to_string(),
                None,
                Some(after),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": assignment_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin removes an assignment — offboarding a person from a store. Audited `assignment.remove`
/// when a row was actually removed; a no-op removal records nothing.
async fn admin_remove_assignment<P, A, C>(
    State(state): State<PeopleState<P, A, C>>,
    headers: HeaderMap,
    Path(assignment_id): Path<String>,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: AssignmentStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(assignment_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        assignment_id.parse::<Ulid>().map(AssignmentId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the assignment id or tenant_id is not a ULID",
        )
            .into_response();
    };
    match state.people.remove(tenant_id, assignment_id).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "assignment.remove",
                "assignment",
                &assignment_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such assignment").into_response(),
        Err(error) => people_error_response(&error),
    }
}

// --- Capability catalogue (`/admin/capabilities`, ADR-0071) -------------------------------------

/// One capability flag as the console's form editor reads it: its config key, default, and the
/// one-line description (§10 catalogue). Mirrors `pos-core`'s `CapabilityMeta` so the console renders
/// toggles from the framework's own source of truth rather than a hand-kept list.
#[derive(Debug, Clone, serde::Serialize)]
struct CapabilityFlagView {
    key: &'static str,
    default_on: bool,
    description: &'static str,
}

/// One capability preset (§10) — a named starting profile the console offers as a button, given as the
/// set of flag keys it turns on.
#[derive(Debug, Clone, serde::Serialize)]
struct CapabilityPresetView {
    id: &'static str,
    keys: Vec<&'static str>,
}

/// One inter-flag rule (§10) the console previews before publish, so a conflict shows the moment it is
/// created rather than as a `422` on publish.
#[derive(Debug, Clone, serde::Serialize)]
struct CapabilityRuleView {
    id: &'static str,
    description: &'static str,
}

/// The whole capability catalogue the form editor needs: the flags, the presets, and the inter-flag
/// rules — all §10 data, tenant-independent.
#[derive(Debug, Clone, serde::Serialize)]
struct CapabilityCatalogueView {
    flags: Vec<CapabilityFlagView>,
    presets: Vec<CapabilityPresetView>,
    rules: Vec<CapabilityRuleView>,
}

/// The collaborators the capability-catalogue read needs: just the admin and clock its session guard
/// uses. The catalogue itself is static `pos-core` data, so there is no store to carry.
#[derive(Clone)]
struct CapabilitiesState<A, C> {
    admin: A,
    clock: C,
}

/// The keys a preset turns on, in catalogue order.
fn preset_keys(context: pos_core::capability::CapabilityContext) -> Vec<&'static str> {
    pos_core::capability::Capability::ALL
        .iter()
        .copied()
        .filter(|capability| context.enabled(*capability))
        .map(|capability| capability.meta().key)
        .collect()
}

/// Builds the capability-catalogue sub-router ([ADR-0071](../../../docs/adr/0071-config-without-json.md)).
///
/// One read, behind [`ConsolePermission::Read`] (every console role): the §10 capability flags, the
/// presets, and the inter-flag rules, so the Config screen's form editor renders toggles and previews
/// conflicts from the framework's own catalogue. Tenant-independent — the catalogue is the same for
/// every store.
pub fn capabilities_router<A, C>(admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/capabilities", get(admin_list_capabilities::<A, C>))
        .with_state(CapabilitiesState { admin, clock })
}

/// Serves the §10 capability catalogue for the console's form editor.
async fn admin_list_capabilities<A, C>(
    State(state): State<CapabilitiesState<A, C>>,
    headers: HeaderMap,
) -> Response
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    use pos_core::capability::{Capability, CapabilityContext, RULES};

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
    let flags = Capability::ALL
        .iter()
        .copied()
        .map(|capability| {
            let meta = capability.meta();
            CapabilityFlagView {
                key: meta.key,
                default_on: meta.default_on,
                description: meta.description,
            }
        })
        .collect();
    let presets = vec![
        CapabilityPresetView {
            id: "full_service",
            keys: preset_keys(CapabilityContext::full_service()),
        },
        CapabilityPresetView {
            id: "counter",
            keys: preset_keys(CapabilityContext::counter()),
        },
        CapabilityPresetView {
            id: "retail",
            keys: preset_keys(CapabilityContext::retail()),
        },
    ];
    let rules = RULES
        .iter()
        .map(|rule| CapabilityRuleView {
            id: rule.id,
            description: rule.description,
        })
        .collect();
    (
        StatusCode::OK,
        Json(CapabilityCatalogueView {
            flags,
            presets,
            rules,
        }),
    )
        .into_response()
}

// --- Capability publish (`/admin/config/capabilities`, ADR-0071) --------------------------------

/// The collaborators the capability-publish route needs: the config-tree store the flags are merged
/// onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigCapabilitiesState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin sets a store's capability flags from the form editor: the (tenant, store) and the flag
/// values to write. Only the flags named are changed; the rest of the store's config is untouched.
#[derive(Debug, Clone, Deserialize)]
struct PublishCapabilitiesRequest {
    tenant_id: String,
    store_id: String,
    /// The capability flag values to set, keyed by each capability's config key (`tables_enabled`, …).
    flags: std::collections::BTreeMap<String, bool>,
}

/// Builds the capability-publish sub-router ([ADR-0071](../../../docs/adr/0071-config-without-json.md)).
///
/// One route: merge a store's capability flag booleans into its Store config layer and version it
/// through the config tree — the node-merge the catalog/people publishes use, so the other Store-level
/// keys (`menu`, `layout`, `permissions`) survive. Behind [`ConsolePermission::PublishConfig`], and the
/// config tree runs the §10 inter-flag rules, so an invalid combination is a `422`, never a stored
/// state.
pub fn config_capabilities_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/capabilities",
            axum::routing::put(admin_publish_capabilities::<Cfg, A, C>),
        )
        .with_state(ConfigCapabilitiesState {
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Merges a store's capability flags into its Store config layer and versions it — the same
/// load→merge→publish→version shape as the catalog publish, onto the top-level flag keys instead of a
/// node. Rejects an unknown flag key (the form only sends catalogue keys). The §10 inter-flag rules run
/// in the config tree, so an invalid combination returns `422` with the violated rules.
async fn admin_publish_capabilities<Cfg, A, C>(
    State(state): State<ConfigCapabilitiesState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishCapabilitiesRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    // Every key must be a known §10 capability flag — the form only sends catalogue keys, and this
    // keeps a typo from writing a stray boolean into the config document.
    for key in request.flags.keys() {
        if !pos_core::capability::Capability::ALL
            .iter()
            .any(|capability| capability.meta().key == key)
        {
            return (
                StatusCode::BAD_REQUEST,
                format!("unknown capability flag: {key}"),
            )
                .into_response();
        }
    }

    // Load the store's tree, set the flag keys on its Store layer (index 2), and re-publish that layer,
    // preserving the other Store-level keys (`menu`, `layout`, `permissions`).
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut store_layer {
        for (key, value) in &request.flags {
            map.insert(key.clone(), serde_json::Value::Bool(*value));
        }
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
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.capabilities.publish",
                "store",
                &store_id.to_string(),
                None,
                serde_json::to_value(&request.flags).ok(),
            )
            .await;
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

// --- Tax publish (`/admin/config/tax`, ADR-0074, Track M4) --------------------------------------

/// The collaborators the tax-publish route needs: the tax-rate store the table is read from, the
/// config-tree store the `tax` node is written onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigTaxState<Tax, Cfg, A, C> {
    tax_rates: Tax,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin publishes a tenant's authored tax rates to one of its stores: the `(tenant, store)`.
/// The rates come from the authored table, not the body — publishing is "push what is authored".
#[derive(Debug, Clone, Deserialize)]
struct PublishTaxRequest {
    tenant_id: String,
    store_id: String,
}

/// Builds the tax-publish sub-router ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// One route: assemble the tenant's authored `(tax class × channel)` rates into a `TaxRateTable`, write
/// it as the store's `tax` config node, and version it through the config tree — the node-merge the
/// catalog/floor/people publishes use, so the other Store-level keys survive. Behind
/// [`ConsolePermission::PublishConfig`]. The edge applies the node to `EdgeSession::tax_rates`.
pub fn config_tax_router<Tax, Cfg, A, C>(
    tax_rates: Tax,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/tax",
            axum::routing::put(admin_publish_tax::<Tax, Cfg, A, C>),
        )
        .with_state(ConfigTaxState {
            tax_rates,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Assembles a tenant's authored rates into a `TaxRateTable`, writes it as the store's `tax` node, and
/// versions it — the same load→merge→publish→version shape as the other node publishes.
async fn admin_publish_tax<Tax, Cfg, A, C>(
    State(state): State<ConfigTaxState<Tax, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishTaxRequest>,
) -> Response
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let entries = match state.tax_rates.list_tax_rates(tenant_id).await {
        Ok(entries) => entries,
        Err(error) => return tax_rate_error_response(&error),
    };
    let Ok(tax_value) = serde_json::to_value(to_table(&entries)) else {
        tracing::error!("could not serialise a tax rate table");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the tax-rate service is unavailable",
        )
            .into_response();
    };

    // Set the `tax` key on the store's Store layer (index 2) and re-publish it, preserving the other
    // Store-level keys (`menu`, `layout`, `permissions`, `floor`, capability flags).
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert("tax".to_owned(), tax_value);
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
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.tax.publish",
                "store",
                &store_id.to_string(),
                None,
                serde_json::to_value(entries.len()).ok(),
            )
            .await;
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

// --- Locale publish (`/admin/config/locale`, ADR-0074, Track M4) --------------------------------

/// The collaborators the locale-publish route needs: the config-tree store the `locale` node is
/// written onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigLocaleState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin sets a store's locale settings: the `(tenant, store)` and the currency, IANA
/// timezone, and business-date cutoff hour. These drive money display and business-date derivation on
/// the edge (ADR-0014); until M4 they were hardcoded to VND/UTC/04:00 in the edge bootstrap.
#[derive(Debug, Clone, Deserialize)]
struct PublishLocaleRequest {
    tenant_id: String,
    store_id: String,
    currency_code: String,
    timezone: String,
    cutoff_hour: u8,
    /// The store's display language (a locale code, e.g. `"vi"`), which selects a compiled item's
    /// per-locale name at the edge (ADR-0074). Optional; when absent or blank the store shows each
    /// item's default name, exactly as before.
    #[serde(default)]
    display_language: Option<String>,
}

/// Builds the locale-publish sub-router ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// One route: validate a store's currency / IANA timezone / cutoff hour with the domain constructors,
/// write them as the store's `locale` config node, and version it through the config tree — the
/// node-merge the other publishes use, so sibling nodes survive. Behind
/// [`ConsolePermission::PublishConfig`]. The edge applies the node to its session's currency, timezone,
/// and cutoff.
pub fn config_locale_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/locale",
            axum::routing::put(admin_publish_locale::<Cfg, A, C>),
        )
        .with_state(ConfigLocaleState {
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Validates a store's locale settings and writes them as its `locale` node, versioned — the same
/// load→merge→publish→version shape as the other node publishes. Each field is checked with its domain
/// constructor before anything is written (a real IANA timezone against the tz database, a 3-letter
/// currency, an hour in `0..=23`), so a bad value is a `400` naming it rather than a stored error.
#[expect(
    clippy::too_many_lines,
    reason = "one publish scenario: validate three fields with their domain constructors, then the \
              load→merge→version→save→audit shape shared with the other node publishes"
)]
async fn admin_publish_locale<Cfg, A, C>(
    State(state): State<ConfigLocaleState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishLocaleRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    if CurrencyCode::parse(&request.currency_code).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "currency_code is not a 3-letter code",
        )
            .into_response();
    }
    if StoreTimeZone::from_iana_name(&request.timezone).is_err() {
        return (StatusCode::BAD_REQUEST, "timezone is not a valid IANA name").into_response();
    }
    if CutoffHour::new(request.cutoff_hour).is_err() {
        return (StatusCode::BAD_REQUEST, "cutoff_hour must be in 0..=23").into_response();
    }
    let mut locale_value = serde_json::json!({
        "currency_code": request.currency_code,
        "timezone": request.timezone,
        "cutoff_hour": request.cutoff_hour,
    });
    // The display language is optional: include it only when a non-blank code was given, so a store
    // that never sets one keeps a clean node and shows each item's default name (ADR-0074).
    if let Some(language) = request
        .display_language
        .as_deref()
        .map(str::trim)
        .filter(|language| !language.is_empty())
        && let serde_json::Value::Object(map) = &mut locale_value
    {
        map.insert(
            "display_language".to_owned(),
            serde_json::Value::String(language.to_owned()),
        );
    }

    // Set the `locale` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys.
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert("locale".to_owned(), locale_value.clone());
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
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.locale.publish",
                "store",
                &store_id.to_string(),
                None,
                Some(locale_value),
            )
            .await;
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

// --- Channels & tender settings (`/admin/config/channels`, `/admin/config/tender`, ADR-0080, M7) --

/// The collaborators the channels/tender settings routes need: the config-tree store the node is
/// written onto and read back from, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigChannelsState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `(tenant, store)` query for reading a store's current settings node.
#[derive(Debug, Clone, Deserialize)]
struct ConfigNodeQuery {
    tenant_id: String,
    store_id: String,
}

/// A `PUT /admin/config/channels` body: the `(tenant, store)` and the enabled sales-channel tokens.
#[derive(Debug, Clone, Deserialize)]
struct PublishChannelsRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    enabled: Vec<String>,
}

/// A `PUT /admin/config/tender` body: the `(tenant, store)` and the accepted payment-method tokens.
#[derive(Debug, Clone, Deserialize)]
struct PublishTenderRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    accepted: Vec<String>,
}

/// Parses each `UPPER_SNAKE_CASE` token to a known wire-enum value, refusing an unrecognised or
/// unspecified one — authoring rejects a typo up front rather than storing a token the edge would drop.
fn parse_known_tokens<E: WireEnum>(tokens: &[String]) -> Result<Vec<Open<E>>, String> {
    let mut out = Vec::with_capacity(tokens.len());
    for token in tokens {
        match E::from_wire(token) {
            Some(known) if known != E::UNSPECIFIED => out.push(Open::from_known(known)),
            _ => return Err(format!("{token} is not a recognised value")),
        }
    }
    Ok(out)
}

/// Reads a store's current Store-layer node value by key, or `null` when it has none.
async fn read_store_node<Cfg>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
    node_key: &str,
) -> Result<serde_json::Value, Response>
where
    Cfg: ConfigTreeStore,
{
    match config_trees.load(tenant_id, store_id).await {
        Ok(Some(state)) => Ok(state
            .layers
            .get(2)
            .and_then(|layer| layer.get(node_key))
            .cloned()
            .unwrap_or(serde_json::Value::Null)),
        Ok(None) => Ok(serde_json::Value::Null),
        Err(error) => Err(config_store_error_response(&error)),
    }
}

/// Publishes `node_value` as the store's `node_key`, versioned — the shared load→merge→publish→save→
/// audit tail every settings-node publish uses.
async fn publish_store_settings_node<Cfg, A, C>(
    state: &ConfigChannelsState<Cfg, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    store_id: StoreId,
    node_key: &str,
    action: &str,
    node_value: serde_json::Value,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let store_layer = store_layer_with(state_before.as_ref(), node_key, node_value.clone());
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
            audit_action(
                &state.audit,
                &state.clock,
                context,
                Some(tenant_id),
                action,
                "store",
                &store_id.to_string(),
                None,
                Some(node_value),
            )
            .await;
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

/// Builds the channels/tender settings sub-router ([ADR-0080](../../../docs/adr/0080-channels-and-payments.md), M7).
///
/// `GET`/`PUT /admin/config/channels` and `.../tender`: read a store's current enabled channels /
/// accepted tender, and publish new ones. Reads are behind [`ConsolePermission::Read`]; publishes are
/// behind [`ConsolePermission::PublishConfig`] and audited — settings, not a CRUD master-data domain,
/// so there is no dedicated authoring permission. The edge applies each node as a policy gate.
pub fn config_channels_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/channels",
            get(admin_read_channels::<Cfg, A, C>).put(admin_publish_channels::<Cfg, A, C>),
        )
        .route(
            "/admin/config/tender",
            get(admin_read_tender::<Cfg, A, C>).put(admin_publish_tender::<Cfg, A, C>),
        )
        .route(
            "/admin/config/qr",
            get(admin_read_qr::<Cfg, A, C>).put(admin_publish_qr::<Cfg, A, C>),
        )
        .route(
            "/admin/config/vendors",
            get(admin_read_vendors::<Cfg, A, C>).put(admin_publish_vendors::<Cfg, A, C>),
        )
        .with_state(ConfigChannelsState {
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// A super-admin reads a store's current `channels` node (the enabled sales channels), or `null`.
async fn admin_read_channels<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ConfigNodeQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
        query.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match read_store_node(&state.config_trees, tenant_id, store_id, "channels").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

/// A super-admin publishes a store's enabled sales channels as its `channels` node.
async fn admin_publish_channels<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishChannelsRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let enabled = match parse_known_tokens::<SalesChannel>(&request.enabled) {
        Ok(tokens) => tokens,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let Ok(value) = serde_json::to_value(PublishedChannels::new(enabled)) else {
        return channels_serialize_unavailable();
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "channels",
        "config.channels.publish",
        value,
    )
    .await
}

/// A super-admin reads a store's current `tender` node (the accepted payment methods), or `null`.
async fn admin_read_tender<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ConfigNodeQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
        query.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match read_store_node(&state.config_trees, tenant_id, store_id, "tender").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

/// A super-admin publishes a store's accepted payment methods as its `tender` node.
async fn admin_publish_tender<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishTenderRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let accepted = match parse_known_tokens::<PaymentMethod>(&request.accepted) {
        Ok(tokens) => tokens,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let Ok(value) = serde_json::to_value(PublishedTender::new(accepted)) else {
        return channels_serialize_unavailable();
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "tender",
        "config.tender.publish",
        value,
    )
    .await
}

/// The `503` for the impossible case that a channels/tender node fails to serialise.
fn channels_serialize_unavailable() -> Response {
    tracing::error!("could not serialise a channels/tender node");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the configuration service is unavailable",
    )
        .into_response()
}

/// A `qr.business_hours` window in a `PUT /admin/config/qr` body.
#[derive(Debug, Clone, Deserialize)]
struct QrBusinessHoursRequest {
    open_hour: u8,
    close_hour: u8,
    #[serde(default)]
    tz_offset_minutes: i64,
}

/// A `PUT /admin/config/qr` body: the store and the QR guardrail settings (P11b/ADR-0057). Composed
/// into the `qr` node the cloud's QR intake reads (`qr_http::qr_config_for`) and the edge reads for its
/// staff-confirmation decision.
#[derive(Debug, Clone, Deserialize)]
struct PublishQrRequest {
    tenant_id: String,
    store_id: String,
    enabled: bool,
    staff_confirmation_required: bool,
    per_table_limit: u32,
    rate_window_secs: u64,
    #[serde(default)]
    business_hours: Option<QrBusinessHoursRequest>,
}

/// A super-admin reads a store's current `qr` guardrail node, or `null`.
async fn admin_read_qr<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ConfigNodeQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
        query.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match read_store_node(&state.config_trees, tenant_id, store_id, "qr").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

/// A super-admin publishes a store's QR guardrail settings as its `qr` node.
async fn admin_publish_qr<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishQrRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let mut node = serde_json::json!({
        "enabled": request.enabled,
        "staff_confirmation_required": request.staff_confirmation_required,
        "per_table_limit": request.per_table_limit,
        "rate_window_secs": request.rate_window_secs,
    });
    if let Some(hours) = request.business_hours {
        if hours.open_hour > 23 || hours.close_hour > 23 {
            return (
                StatusCode::BAD_REQUEST,
                "open_hour and close_hour must be in 0..=23",
            )
                .into_response();
        }
        if let serde_json::Value::Object(map) = &mut node {
            map.insert(
                "business_hours".to_owned(),
                serde_json::json!({
                    "open_hour": hours.open_hour,
                    "close_hour": hours.close_hour,
                    "tz_offset_minutes": hours.tz_offset_minutes,
                }),
            );
        }
    }
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "qr",
        "config.qr.publish",
        node,
    )
    .await
}

/// A `PUT /admin/config/vendors` body: the store and its per-marketplace policies.
#[derive(Debug, Clone, Deserialize)]
struct PublishVendorsRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    policies: Vec<PublishedVendorPolicy>,
}

/// A super-admin reads a store's current `vendors` policy node, or `null`.
async fn admin_read_vendors<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ConfigNodeQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
        query.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match read_store_node(&state.config_trees, tenant_id, store_id, "vendors").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

/// A super-admin publishes a store's per-marketplace policies as its `vendors` node. Each policy's
/// availability must be a recognised value; the live loop that pushes it to a marketplace is deferred.
async fn admin_publish_vendors<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishVendorsRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    if request
        .policies
        .iter()
        .any(|policy| policy.availability.is_unspecified() || policy.availability.is_unrecognised())
    {
        return (
            StatusCode::BAD_REQUEST,
            "a vendor policy names an unknown availability (open/busy/closed)",
        )
            .into_response();
    }
    let Ok(value) = serde_json::to_value(PublishedVendorPolicies::new(request.policies)) else {
        return channels_serialize_unavailable();
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "vendors",
        "config.vendors.publish",
        value,
    )
    .await
}

// --- Floor & kitchen master data (`/admin/floor`, `/admin/kitchen`, ADR-0072) -------------------

/// The collaborators the floor/kitchen CRUD routes need: the master-data store, plus the
/// admin/clock/audit every write carries.
#[derive(Clone)]
struct FloorState<F, A, C> {
    floor: F,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// The (tenant, store) a floor/kitchen list is scoped to — a floor is per-store, so both are required.
#[derive(Debug, Clone, Deserialize)]
struct FloorListQuery {
    tenant_id: String,
    store_id: String,
}

/// The tenant a single-record floor/kitchen read is scoped to (the id is globally unique).
#[derive(Debug, Clone, Deserialize)]
struct FloorTenantQuery {
    tenant_id: String,
}

/// Create a floor area.
#[derive(Debug, Clone, Deserialize)]
struct CreateAreaRequest {
    tenant_id: String,
    store_id: String,
    name: String,
}

/// Rename an area and/or set its status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateAreaRequest {
    tenant_id: String,
    name: String,
    status: String,
}

/// Create a floor table.
#[derive(Debug, Clone, Deserialize)]
struct CreateTableRequest {
    tenant_id: String,
    store_id: String,
    area_id: String,
    name: String,
    #[serde(default)]
    seats: u16,
    #[serde(default)]
    grid_column: Option<u16>,
    #[serde(default)]
    grid_row: Option<u16>,
}

/// Update a table's area, label, seats, position, and status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateTableRequest {
    tenant_id: String,
    area_id: String,
    name: String,
    #[serde(default)]
    seats: u16,
    #[serde(default)]
    grid_column: Option<u16>,
    #[serde(default)]
    grid_row: Option<u16>,
    status: String,
}

/// Create a kitchen station.
#[derive(Debug, Clone, Deserialize)]
struct CreateStationRequest {
    tenant_id: String,
    store_id: String,
    name: String,
    #[serde(default)]
    backup_station_id: Option<String>,
    #[serde(default)]
    is_default: bool,
}

/// Update a station's name, backup, default flag, and status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateStationRequest {
    tenant_id: String,
    name: String,
    #[serde(default)]
    backup_station_id: Option<String>,
    #[serde(default)]
    is_default: bool,
    status: String,
}

/// Create an item→station routing rule.
#[derive(Debug, Clone, Deserialize)]
struct CreateRoutingRuleRequest {
    tenant_id: String,
    store_id: String,
    station_id: String,
    #[serde(default)]
    menu_item_id: Option<String>,
    #[serde(default)]
    course_id: Option<String>,
    #[serde(default)]
    sort: u16,
}

/// Folds two optional grid coordinates into a [`GridPosition`] — a table is placed only when both are
/// given; either alone is treated as unplaced.
fn grid_position(column: Option<u16>, row: Option<u16>) -> Option<GridPosition> {
    match (column, row) {
        (Some(column), Some(row)) => Some(GridPosition { column, row }),
        _ => None,
    }
}

/// Maps any floor-store failure to a retryable `503`, logging the detail rather than leaking it.
fn floor_error_response(error: &impl std::fmt::Display) -> Response {
    tracing::error!(%error, "a floor & kitchen store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the floor service is unavailable",
    )
        .into_response()
}

/// `503` when OS entropy is unavailable to mint a floor/kitchen id.
fn floor_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy for a floor & kitchen write");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the floor service is unavailable",
    )
        .into_response()
}

/// Parses an optional id field: `None`/empty → `Ok(None)`; a present value must be a ULID.
fn parse_optional_ulid<T>(value: Option<&str>, wrap: impl Fn(Ulid) -> T) -> Result<Option<T>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(wrap(ulid)))
            .map_err(|_ignored| ()),
    }
}

/// Builds the floor & kitchen master-data sub-router ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
///
/// Reads are behind [`ConsolePermission::Read`]; every write is behind [`ConsolePermission::ManageFloor`]
/// (Owner/Admin) and is audited. The tenant is named the admin-is-global way (a `?tenant_id=` /
/// `?store_id=` query on reads, the request body on writes). None of this data is PII.
pub fn floor_router<F, A, C>(floor: F, admin: A, clock: C, audit: Arc<dyn AuditRecorder>) -> Router
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/floor/areas",
            get(admin_list_areas::<F, A, C>).post(admin_create_area::<F, A, C>),
        )
        .route(
            "/admin/floor/areas/{area_id}",
            get(admin_get_area::<F, A, C>).patch(admin_update_area::<F, A, C>),
        )
        .route(
            "/admin/floor/tables",
            get(admin_list_tables::<F, A, C>).post(admin_create_table::<F, A, C>),
        )
        .route(
            "/admin/floor/tables/{table_id}",
            get(admin_get_table::<F, A, C>).patch(admin_update_table::<F, A, C>),
        )
        .route(
            "/admin/kitchen/stations",
            get(admin_list_stations::<F, A, C>).post(admin_create_station::<F, A, C>),
        )
        .route(
            "/admin/kitchen/stations/{station_id}",
            get(admin_get_station::<F, A, C>).patch(admin_update_station::<F, A, C>),
        )
        .route(
            "/admin/kitchen/routing",
            get(admin_list_routing::<F, A, C>).post(admin_create_routing::<F, A, C>),
        )
        .route(
            "/admin/kitchen/routing/{rule_id}",
            delete(admin_remove_routing::<F, A, C>),
        )
        .with_state(FloorState {
            floor,
            admin,
            clock,
            audit,
        })
}

/// Reads a `?tenant_id=` query into a `TenantId`, or returns the `400`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn floor_tenant(tenant_id: &str) -> Result<TenantId, Response> {
    tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|_ignored| (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response())
}

/// Reads a (tenant, store) list query, or returns the `400`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn floor_tenant_store(query: &FloorListQuery) -> Result<(TenantId, StoreId), Response> {
    let (Ok(tenant_id), Ok(store_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        query.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response());
    };
    Ok((tenant_id, store_id))
}

async fn admin_list_areas<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match AreaStore::list(&state.floor, tenant_id, store_id).await {
        Ok(areas) => (StatusCode::OK, Json::<Vec<Area>>(areas)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_get_area<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Path(area_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match floor_tenant(&query.tenant_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(area_id) = area_id.parse::<Ulid>().map(AreaId::new) else {
        return (StatusCode::BAD_REQUEST, "the area id is not a ULID").into_response();
    };
    match AreaStore::get(&state.floor, tenant_id, area_id).await {
        Ok(Some(area)) => (StatusCode::OK, Json(area)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such area").into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_create_area<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateAreaRequest>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let Some(area_id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(AreaId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_area = NewArea {
        area_id,
        tenant_id,
        store_id,
        name: request.name.clone(),
    };
    match AreaStore::create(&state.floor, &new_area).await {
        Ok(()) => {
            let after = serde_json::json!({
                "id": area_id.to_string(),
                "store_id": store_id.to_string(),
                "name": request.name,
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.area.create",
                "floor_area",
                &area_id.to_string(),
                None,
                Some(after),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": area_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_update_area<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Path(area_id): Path<String>,
    Json(request): Json<UpdateAreaRequest>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(area_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        area_id.parse::<Ulid>().map(AreaId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the area id or tenant_id is not a ULID",
        )
            .into_response();
    };
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let update = AreaUpdate {
        area_id,
        tenant_id,
        name: request.name.clone(),
        status,
    };
    match AreaStore::update(&state.floor, &update).await {
        Ok(true) => {
            let after = serde_json::json!({
                "id": area_id.to_string(),
                "name": request.name,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.area.update",
                "floor_area",
                &area_id.to_string(),
                None,
                Some(after),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such area").into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_list_tables<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match TableStore::list(&state.floor, tenant_id, store_id).await {
        Ok(tables) => (StatusCode::OK, Json::<Vec<Table>>(tables)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_get_table<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Path(table_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match floor_tenant(&query.tenant_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(table_id) = table_id.parse::<Ulid>().map(TableId::new) else {
        return (StatusCode::BAD_REQUEST, "the table id is not a ULID").into_response();
    };
    match TableStore::get(&state.floor, tenant_id, table_id).await {
        Ok(Some(table)) => (StatusCode::OK, Json(table)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such table").into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_create_table<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateTableRequest>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(store_id), Ok(area_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
        request.area_id.parse::<Ulid>().map(AreaId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, store_id, or area_id is not a ULID",
        )
            .into_response();
    };
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let Some(table_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(TableId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_table = NewTable {
        table_id,
        tenant_id,
        store_id,
        area_id,
        label: request.name.clone(),
        seats: request.seats,
        position: grid_position(request.grid_column, request.grid_row),
    };
    match TableStore::create(&state.floor, &new_table).await {
        Ok(()) => {
            let after = serde_json::json!({
                "id": table_id.to_string(),
                "area_id": area_id.to_string(),
                "label": request.name,
                "seats": request.seats,
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.table.create",
                "floor_table",
                &table_id.to_string(),
                None,
                Some(after),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": table_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_update_table<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Path(table_id): Path<String>,
    Json(request): Json<UpdateTableRequest>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(table_id), Ok(area_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        table_id.parse::<Ulid>().map(TableId::new),
        request.area_id.parse::<Ulid>().map(AreaId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the table id, tenant_id, or area_id is not a ULID",
        )
            .into_response();
    };
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let update = TableUpdate {
        table_id,
        tenant_id,
        area_id,
        label: request.name.clone(),
        seats: request.seats,
        position: grid_position(request.grid_column, request.grid_row),
        status,
    };
    match TableStore::update(&state.floor, &update).await {
        Ok(true) => {
            let after = serde_json::json!({
                "id": table_id.to_string(),
                "area_id": area_id.to_string(),
                "label": request.name,
                "seats": request.seats,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.table.update",
                "floor_table",
                &table_id.to_string(),
                None,
                Some(after),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such table").into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_list_stations<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match StationStore::list(&state.floor, tenant_id, store_id).await {
        Ok(stations) => (StatusCode::OK, Json::<Vec<Station>>(stations)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_get_station<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Path(station_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match floor_tenant(&query.tenant_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let Ok(station_id) = station_id.parse::<Ulid>().map(StationId::new) else {
        return (StatusCode::BAD_REQUEST, "the station id is not a ULID").into_response();
    };
    match StationStore::get(&state.floor, tenant_id, station_id).await {
        Ok(Some(station)) => (StatusCode::OK, Json(station)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such station").into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_create_station<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateStationRequest>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let Ok(backup_station_id) =
        parse_optional_ulid(request.backup_station_id.as_deref(), StationId::new)
    else {
        return (StatusCode::BAD_REQUEST, "backup_station_id is not a ULID").into_response();
    };
    let Some(station_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(StationId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_station = NewStation {
        station_id,
        tenant_id,
        store_id,
        name: request.name.clone(),
        backup_station_id,
        is_default: request.is_default,
    };
    match StationStore::create(&state.floor, &new_station).await {
        Ok(()) => {
            let after = serde_json::json!({
                "id": station_id.to_string(),
                "name": request.name,
                "is_default": request.is_default,
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "kitchen.station.create",
                "kitchen_station",
                &station_id.to_string(),
                None,
                Some(after),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": station_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_update_station<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Path(station_id): Path<String>,
    Json(request): Json<UpdateStationRequest>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(station_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        station_id.parse::<Ulid>().map(StationId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the station id or tenant_id is not a ULID",
        )
            .into_response();
    };
    if request.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }
    let Ok(backup_station_id) =
        parse_optional_ulid(request.backup_station_id.as_deref(), StationId::new)
    else {
        return (StatusCode::BAD_REQUEST, "backup_station_id is not a ULID").into_response();
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return (StatusCode::BAD_REQUEST, "status must be active or archived").into_response();
    };
    let update = StationUpdate {
        station_id,
        tenant_id,
        name: request.name.clone(),
        backup_station_id,
        is_default: request.is_default,
        status,
    };
    match StationStore::update(&state.floor, &update).await {
        Ok(true) => {
            let after = serde_json::json!({
                "id": station_id.to_string(),
                "name": request.name,
                "is_default": request.is_default,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "kitchen.station.update",
                "kitchen_station",
                &station_id.to_string(),
                None,
                Some(after),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such station").into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_list_routing<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: RoutingRuleStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match RoutingRuleStore::list(&state.floor, tenant_id, store_id).await {
        Ok(rules) => (StatusCode::OK, Json::<Vec<RoutingRule>>(rules)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_create_routing<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateRoutingRuleRequest>,
) -> Response
where
    F: RoutingRuleStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(store_id), Ok(station_id)) = (
        request.tenant_id.parse::<Ulid>().map(TenantId::new),
        request.store_id.parse::<Ulid>().map(StoreId::new),
        request.station_id.parse::<Ulid>().map(StationId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, store_id, or station_id is not a ULID",
        )
            .into_response();
    };
    let Ok(menu_item_id) = parse_optional_ulid(request.menu_item_id.as_deref(), MenuItemId::new)
    else {
        return (StatusCode::BAD_REQUEST, "menu_item_id is not a ULID").into_response();
    };
    let Ok(course_id) = parse_optional_ulid(request.course_id.as_deref(), CourseId::new) else {
        return (StatusCode::BAD_REQUEST, "course_id is not a ULID").into_response();
    };
    // A rule must match exactly one of an item or a course — the same rule the §10 validator enforces
    // at publish, surfaced here so the console cannot store a rule that matches nothing or both.
    if menu_item_id.is_some() == course_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "a routing rule must match exactly one of menu_item_id or course_id",
        )
            .into_response();
    }
    let Some(rule_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(RoutingRuleId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_rule = NewRoutingRule {
        rule_id,
        tenant_id,
        store_id,
        station_id,
        menu_item_id,
        course_id,
        sort: request.sort,
    };
    match RoutingRuleStore::create(&state.floor, &new_rule).await {
        Ok(()) => {
            let after = serde_json::json!({
                "id": rule_id.to_string(),
                "station_id": station_id.to_string(),
                "menu_item_id": menu_item_id.map(|id| id.to_string()),
                "course_id": course_id.map(|id| id.to_string()),
                "sort": request.sort,
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "kitchen.routing.create",
                "station_routing_rule",
                &rule_id.to_string(),
                None,
                Some(after),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": rule_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => floor_error_response(&error),
    }
}

async fn admin_remove_routing<F, A, C>(
    State(state): State<FloorState<F, A, C>>,
    headers: HeaderMap,
    Path(rule_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: RoutingRuleStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(rule_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        rule_id.parse::<Ulid>().map(RoutingRuleId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the rule id or tenant_id is not a ULID",
        )
            .into_response();
    };
    match RoutingRuleStore::remove(&state.floor, tenant_id, rule_id).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "kitchen.routing.remove",
                "station_routing_rule",
                &rule_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such routing rule").into_response(),
        Err(error) => floor_error_response(&error),
    }
}

// --- Floor & kitchen publish (`/admin/floor/publish`, ADR-0072) ---------------------------------

/// The collaborators the floor/kitchen publish route needs: the master-data store to compile from, the
/// config-tree store to write onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct FloorPublishState<F, Cfg, A, C> {
    floor: F,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin selects the (tenant, store) whose `floor`/`stations` nodes to compile and publish.
#[derive(Debug, Clone, Deserialize)]
struct PublishFloorRequest {
    tenant_id: String,
    store_id: String,
}

/// Builds the floor & kitchen publish sub-router ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
///
/// One route: compile a store's areas/tables into a `FloorPlan` and its stations/routing into a
/// `StationPlan`, run the §10 referential validation (`pos_core::floor`), and — only if valid — merge
/// both onto the store's `floor` and `stations` config nodes and version them through the config tree.
/// Behind [`ConsolePermission::PublishConfig`], the same gate as catalog/people/config publish.
pub fn floor_publish_router<F, Cfg, A, C>(
    floor: F,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/floor/publish",
            post(admin_publish_floor::<F, Cfg, A, C>),
        )
        .with_state(FloorPublishState {
            floor,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Compiles a store's floor & kitchen master data into its `floor`/`stations` config nodes and versions
/// them — the same load→compile→validate→write→version shape as [`admin_publish_menu`], onto the
/// `floor`/`stations` keys. The §10 referential rules run here (a rule to an unknown station, a stale
/// backup): an invalid plan is a `422` with the violated rules, never a stored state.
#[expect(
    clippy::too_many_lines,
    reason = "one publish is a single linear transaction — load areas/tables/stations/rules, compile \
              both nodes, validate, merge onto the Store layer and version it; splitting the \
              load-compile-write flow would scatter the config-tree state the final publish needs"
)]
async fn admin_publish_floor<F, Cfg, A, C>(
    State(state): State<FloorPublishState<F, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishFloorRequest>,
) -> Response
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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

    // Load the authoring rows. `list` is on all four seams, so each call is fully-qualified.
    let areas = match AreaStore::list(&state.floor, tenant_id, store_id).await {
        Ok(areas) => areas,
        Err(error) => return floor_error_response(&error),
    };
    let tables = match TableStore::list(&state.floor, tenant_id, store_id).await {
        Ok(tables) => tables,
        Err(error) => return floor_error_response(&error),
    };
    let stations = match StationStore::list(&state.floor, tenant_id, store_id).await {
        Ok(stations) => stations,
        Err(error) => return floor_error_response(&error),
    };
    let rules = match RoutingRuleStore::list(&state.floor, tenant_id, store_id).await {
        Ok(rules) => rules,
        Err(error) => return floor_error_response(&error),
    };

    let floor_plan = compile_floor(&areas, &tables);
    let station_plan = compile_stations(&stations, &rules);

    // The §10 referential validation, before anything is written — the cloud never publishes a plan
    // that names a station, backup, or area that does not exist.
    let mut violations = pos_core::floor::floor_violations(&floor_plan);
    violations.extend(pos_core::floor::station_violations(&station_plan));
    if !violations.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ConfigViolations { violations }),
        )
            .into_response();
    }

    let (Ok(floor_value), Ok(stations_value)) = (
        serde_json::to_value(&floor_plan),
        serde_json::to_value(&station_plan),
    ) else {
        tracing::error!("could not serialise a compiled floor or station plan");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the floor service is unavailable",
        )
            .into_response();
    };

    // Set the `floor` and `stations` keys on the store's Store layer (index 2) and re-publish it,
    // preserving the other Store-level keys (`menu`, `layout`, `permissions`, capability flags).
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert("floor".to_owned(), floor_value);
        map.insert("stations".to_owned(), stations_value);
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
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.publish",
                "store",
                &store_id.to_string(),
                None,
                Some(serde_json::json!({
                    "config_version_id": id.to_string(),
                    "area_count": floor_plan.areas().len(),
                    "table_count": floor_plan.tables().count(),
                    "station_count": station_plan.stations().len(),
                })),
            )
            .await;
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

// --- Table QR tokens (`/admin/floor/qr`, ADR-0072 + ADR-0057) -----------------------------------

/// The collaborators the table-QR route needs: the floor store to read a store's tables, the
/// admin/clock its session guard uses, and the signing secret the token is minted with.
#[derive(Clone)]
struct TableQrState<F, A, C> {
    floor: F,
    admin: A,
    clock: C,
    secret: TableTokenSecret,
}

/// One table's printable QR: its id, the label a host reads, and the signed token the guest's QR
/// carries. The token binds `(tenant, store, table)` — no personal data — and is the same value
/// `verify_table_token` checks on a guest order (ADR-0057).
#[derive(Debug, Clone, serde::Serialize)]
struct TableQrEntry {
    table_id: String,
    label: String,
    token: String,
}

/// The store's table QR tokens, for the console's printable sheet.
#[derive(Debug, Clone, serde::Serialize)]
struct TableQrView {
    store_id: String,
    tokens: Vec<TableQrEntry>,
}

/// Builds the table-QR sub-router ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md),
/// [ADR-0057](../../../docs/adr/0057-qr-ordering.md)).
///
/// One read, `GET /admin/floor/qr`, behind [`ConsolePermission::Read`]: mints the signed QR token for
/// each of a store's active tables so the console can print a QR sheet. Wired only when a table-token
/// secret is configured (the same gate the guest QR endpoint uses — a token no verifier would accept
/// is not worth minting). The token is not PII; it is the public value printed on the code.
pub fn table_qr_router<F, A, C>(floor: F, admin: A, clock: C, secret: TableTokenSecret) -> Router
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/floor/qr", get(admin_list_table_qr::<F, A, C>))
        .with_state(TableQrState {
            floor,
            admin,
            clock,
            secret,
        })
}

/// Mints a signed QR token for each of a store's active tables (ADR-0072).
async fn admin_list_table_qr<F, A, C>(
    State(state): State<TableQrState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let tables = match TableStore::list(&state.floor, tenant_id, store_id).await {
        Ok(tables) => tables,
        Err(error) => return floor_error_response(&error),
    };
    let tokens = tables
        .into_iter()
        .filter(|table| table.status == EntityStatus::Active)
        .map(|table| TableQrEntry {
            token: mint_table_token(&state.secret, tenant_id, store_id, table.table_id),
            table_id: table.table_id.to_string(),
            label: table.label,
        })
        .collect();
    (
        StatusCode::OK,
        Json(TableQrView {
            store_id: store_id.to_string(),
            tokens,
        }),
    )
        .into_response()
}

// --- Fleet liveness (`/admin/fleet`, ADR-0068) --------------------------------------------------

/// A store is **online** if its most recent contact is within this window of now. Liveness is captured
/// on every config pull ([ADR-0033](../../../docs/adr/0033-config-tree.md)) and on the edge's
/// heartbeat ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 2), so this threshold is a
/// few of those cycles of slack: one dropped pull or heartbeat must not flap a healthy store to
/// offline. Owned here, at the read, because online/offline is derived — never stored (ADR-0068).
const FLEET_ONLINE_THRESHOLD_MS: i64 = 180_000;

/// The collaborators the fleet routes need, stated independently of [`CloudApp`]: the fleet read
/// model, plus the admin and clock every session guard (and the online-at-read derivation) uses. Like
/// [`RegistryState`], it carries its own state and is merged into the main router.
#[derive(Clone)]
struct FleetState<F, A, C> {
    fleet: F,
    admin: A,
    clock: C,
}

/// One store as the fleet console sees it: the seam's raw facts plus the two verdicts derived at read
/// time — `online` (from `last_seen_at` against [`FLEET_ONLINE_THRESHOLD_MS`]) and `config_current`
/// (the held version equals the published one). Timestamps are Unix milliseconds, the shape the
/// dashboard reads; the store id is a string so the console shows a name and hides the ULID.
#[derive(Debug, Clone, serde::Serialize)]
struct FleetStoreView {
    store_id: String,
    name: String,
    status: EntityStatus,
    online: bool,
    last_seen_at_ms: Option<i64>,
    last_config_pull_at_ms: Option<i64>,
    config_version_held: Option<String>,
    config_version_published: Option<String>,
    config_current: bool,
    relay_backlog: u64,
    relay_oldest_pending_at_ms: Option<i64>,
    /// The binary version the store last reported running (ADR-0078), or `null` if it has never
    /// reported.
    installed_version: Option<String>,
    /// Whether the store's last post-install self-test passed, or `null`.
    self_test_ok: Option<bool>,
    /// Unix ms of the store's most recent OTA report, or `null`.
    reported_at_ms: Option<i64>,
}

impl FleetStoreView {
    /// Builds the view from a seam row, deriving `online` against `now_ms` and `config_current` from
    /// the held-vs-published comparison (a store that has published nothing, or holds nothing, is not
    /// "current" — there is a gap to close either way).
    fn from_row(row: FleetRow, now_ms: i64) -> Self {
        let last_seen_at_ms = row
            .last_seen_at
            .map(pos_proto::Timestamp::as_milliseconds_since_epoch);
        let online = last_seen_at_ms
            .is_some_and(|seen| now_ms.saturating_sub(seen) <= FLEET_ONLINE_THRESHOLD_MS);
        let config_current = match (&row.config_version_held, &row.config_version_published) {
            (Some(held), Some(published)) => held == published,
            _ => false,
        };
        Self {
            store_id: row.store_id.to_string(),
            name: row.name,
            status: row.status,
            online,
            last_seen_at_ms,
            last_config_pull_at_ms: row
                .last_config_pull_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            config_version_held: row.config_version_held,
            config_version_published: row.config_version_published,
            config_current,
            relay_backlog: row.relay_backlog,
            relay_oldest_pending_at_ms: row
                .relay_oldest_pending_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            installed_version: row.installed_version,
            self_test_ok: row.self_test_ok,
            reported_at_ms: row
                .reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
        }
    }
}

/// Builds the fleet-liveness sub-router ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 3).
///
/// Two reads, both behind [`ConsolePermission::Read`] (every console role, so Ops and Viewer see the
/// fleet) and both naming their tenant the admin-is-global way — a `?tenant_id=` query
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): the whole fleet, and one
/// store's detail. Online/offline is derived here at read time, so the answer is always current
/// without any background sweep.
pub fn fleet_router<F, A, C>(fleet: F, admin: A, clock: C) -> Router
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/fleet", get(admin_list_fleet::<F, A, C>))
        .route("/admin/fleet/{store_id}", get(admin_fleet_store::<F, A, C>))
        .with_state(FleetState {
            fleet,
            admin,
            clock,
        })
}

/// Maps a fleet read failure to a retryable `503`, logging the detail rather than leaking it.
fn fleet_error_response(error: &FleetStoreError) -> Response {
    tracing::error!(%error, "a fleet read failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the fleet service is unavailable",
    )
        .into_response()
}

/// A super-admin (any console role) lists a tenant's whole fleet, online/offline derived at read.
async fn admin_list_fleet<F, A, C>(
    State(state): State<FleetState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
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
    let Ok(tenant) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    match state.fleet.list_fleet(tenant).await {
        Ok(rows) => {
            let views: Vec<FleetStoreView> = rows
                .into_iter()
                .map(|row| FleetStoreView::from_row(row, now_ms))
                .collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(error) => fleet_error_response(&error),
    }
}

/// A super-admin (any console role) reads one store's fleet detail; `404` if the tenant has no such
/// store.
async fn admin_fleet_store<F, A, C>(
    State(state): State<FleetState<F, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
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
    let (Ok(tenant), Ok(store)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "the tenant_id or store id is not a ULID",
        )
            .into_response();
    };
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    match state.fleet.store_detail(tenant, store).await {
        Ok(Some(row)) => {
            (StatusCode::OK, Json(FleetStoreView::from_row(row, now_ms))).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "no such store").into_response(),
        Err(error) => fleet_error_response(&error),
    }
}

// --- Operational alerts (`/admin/alerts`, ADR-0073, Track O2) -----------------------------------

#[derive(Clone)]
struct AlertState<Al, A, C> {
    alerts: Al,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// The console's view of a stored alert: the record with the kind/severity as their wire tokens and
/// the lifecycle instants as Unix ms.
#[derive(Debug, serde::Serialize)]
struct AlertView {
    id: String,
    tenant_id: Option<String>,
    kind: &'static str,
    dedup_key: String,
    severity: &'static str,
    summary: String,
    detail: serde_json::Value,
    first_seen_at_ms: i64,
    last_seen_at_ms: i64,
    resolved_at_ms: Option<i64>,
    acknowledged_at_ms: Option<i64>,
}

impl From<AlertRecord> for AlertView {
    fn from(record: AlertRecord) -> Self {
        Self {
            id: record.id,
            tenant_id: record.tenant_id.map(|tenant| tenant.to_string()),
            kind: record.kind.as_str(),
            dedup_key: record.dedup_key,
            severity: record.severity.as_str(),
            summary: record.summary,
            detail: record.detail,
            first_seen_at_ms: record.first_seen_at.as_milliseconds_since_epoch(),
            last_seen_at_ms: record.last_seen_at.as_milliseconds_since_epoch(),
            resolved_at_ms: record
                .resolved_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            acknowledged_at_ms: record
                .acknowledged_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
        }
    }
}

/// Whether the list returns recent history (active *and* resolved) rather than only the active alerts,
/// and how many rows at most.
#[derive(Debug, Clone, Deserialize)]
struct AlertListQuery {
    #[serde(default)]
    recent: bool,
    limit: Option<u32>,
}

/// The fleet-wide alert list, acknowledge, and resolve routes ([ADR-0073](../../../docs/adr/0073-alerting.md)).
/// Reads are behind [`ConsolePermission::Read`] (every console role); acknowledge and resolve are behind
/// [`ConsolePermission::ManageAlerts`] (Owner/Admin/Ops) and are audited.
pub fn alerts_router<Al, A, C>(
    alerts: Al,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Al: AlertStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/alerts", get(admin_list_alerts::<Al, A, C>))
        .route("/admin/alerts/{id}/ack", post(admin_ack_alert::<Al, A, C>))
        .route(
            "/admin/alerts/{id}/resolve",
            post(admin_resolve_alert::<Al, A, C>),
        )
        .with_state(AlertState {
            alerts,
            admin,
            clock,
            audit,
        })
}

/// Maps an alert-store failure to a retryable `503`, logging the detail rather than leaking it.
fn alert_error_response(error: &AlertStoreError) -> Response {
    tracing::error!(%error, "an alert store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the alert service is unavailable",
    )
        .into_response()
}

/// A super-admin (any console role) lists the fleet's alerts: the active set by default, or recent
/// history (active and resolved, newest-seen first, capped) with `?recent=true`.
async fn admin_list_alerts<Al, A, C>(
    State(state): State<AlertState<Al, A, C>>,
    headers: HeaderMap,
    Query(query): Query<AlertListQuery>,
) -> Response
where
    Al: AlertStore + Clone + Send + Sync + 'static,
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
    let result = if query.recent {
        state.alerts.list_recent(query.limit.unwrap_or(200)).await
    } else {
        state.alerts.list_active().await
    };
    match result {
        Ok(rows) => {
            let views: Vec<AlertView> = rows.into_iter().map(AlertView::from).collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(error) => alert_error_response(&error),
    }
}

/// A super-admin holding `console.alerts.manage` acknowledges an alert (idempotent).
async fn admin_ack_alert<Al, A, C>(
    State(state): State<AlertState<Al, A, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response
where
    Al: AlertStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageAlerts,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    match state.alerts.acknowledge(&id, state.clock.now()).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                None,
                "alert.acknowledge",
                "alert",
                &id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => alert_error_response(&error),
    }
}

/// A super-admin holding `console.alerts.manage` resolves an alert by hand (idempotent — a condition
/// still firing reopens on the next evaluator tick).
async fn admin_resolve_alert<Al, A, C>(
    State(state): State<AlertState<Al, A, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response
where
    Al: AlertStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageAlerts,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    match state.alerts.resolve(&id, state.clock.now()).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                None,
                "alert.resolve",
                "alert",
                &id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => alert_error_response(&error),
    }
}

// --- Background-task health (`/admin/health/tasks`, ADR-0068 slice 4) ---------------------------

/// A task is judged **stale** if its most recent tick is older than its configured interval times
/// this slack — a few missed ticks, not one — so an interval-boundary jitter never flaps a healthy
/// loop to unhealthy. The interval comes from the tick's own recorded `interval_secs`.
const TASK_STALENESS_SLACK: i64 = 3;

/// The interval assumed for a tick whose detail omits `interval_secs` (a defensive default; every
/// loop this cloud ships records its interval).
const DEFAULT_TASK_INTERVAL_SECS: i64 = 300;

/// The collaborators the health route needs: the task-health store, the admin and clock the session
/// guard uses, and the set of task names this deployment *expects* to be running (so a loop that has
/// never ticked — dead since boot — is reported as unhealthy rather than silently absent). Its own
/// state, merged into the main router like [`FleetState`].
#[derive(Clone)]
struct HealthState<H, A, C> {
    health: H,
    admin: A,
    clock: C,
    expected: Vec<String>,
}

/// One background loop as the console sees it: identity, whether the deployment expects it to run, a
/// health verdict derived at read time, when it last ticked, and its self-describing detail.
#[derive(Debug, Clone, serde::Serialize)]
struct TaskHealthView {
    task: String,
    expected: bool,
    healthy: bool,
    last_tick_at_ms: Option<i64>,
    seconds_since: Option<i64>,
    detail: serde_json::Value,
}

impl TaskHealthView {
    /// The view for a task that has ticked at least once: fresh (within its interval × slack) and its
    /// last tick's work succeeded (`ok` — absent reads as alive).
    fn from_recorded(task: &str, expected: bool, recorded: &TaskHealth, now_ms: i64) -> Self {
        let last_tick_ms = recorded.last_tick_at.as_milliseconds_since_epoch();
        let interval_secs = recorded
            .detail
            .get("interval_secs")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(DEFAULT_TASK_INTERVAL_SECS);
        let ok = recorded
            .detail
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let age_secs = now_ms.saturating_sub(last_tick_ms) / 1000;
        let fresh = now_ms.saturating_sub(last_tick_ms)
            <= interval_secs
                .saturating_mul(1000)
                .saturating_mul(TASK_STALENESS_SLACK);
        Self {
            task: task.to_owned(),
            expected,
            healthy: fresh && ok,
            last_tick_at_ms: Some(last_tick_ms),
            seconds_since: Some(age_secs),
            detail: recorded.detail.clone(),
        }
    }

    /// The view for an expected task that has never ticked — dead since boot, so never healthy.
    fn never_ticked(task: &str) -> Self {
        Self {
            task: task.to_owned(),
            expected: true,
            healthy: false,
            last_tick_at_ms: None,
            seconds_since: None,
            detail: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// The whole-fleet-of-loops report: an overall verdict (every *expected* loop is healthy) plus the
/// per-loop views. Extra loops that ticked but the deployment does not expect are reported too
/// (`expected: false`), so nothing is hidden, but they do not sway the overall verdict.
#[derive(Debug, Clone, serde::Serialize)]
struct TaskHealthReport {
    healthy: bool,
    tasks: Vec<TaskHealthView>,
}

/// Builds the background-task health sub-router ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)
/// slice 4). One read, `GET /admin/health/tasks`, behind [`ConsolePermission::Read`] (every console
/// role). `expected` names the loops this deployment turned on, so a loop that never ticked is
/// surfaced as unhealthy rather than missing.
pub fn health_router<H, A, C>(health: H, admin: A, clock: C, expected: Vec<String>) -> Router
where
    H: TaskHealthStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/health/tasks", get(admin_task_health::<H, A, C>))
        .with_state(HealthState {
            health,
            admin,
            clock,
            expected,
        })
}

/// Maps a task-health read failure to a retryable `503`, logging the detail rather than leaking it.
fn health_error_response(error: &TaskHealthError) -> Response {
    tracing::error!(%error, "a task-health read failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the health service is unavailable",
    )
        .into_response()
}

/// Reports every background loop's health: each expected loop (whether or not it has ticked) plus any
/// extra loop that has. `503` if the store is unreachable.
async fn admin_task_health<H, A, C>(
    State(state): State<HealthState<H, A, C>>,
    headers: HeaderMap,
) -> Response
where
    H: TaskHealthStore + Clone + Send + Sync + 'static,
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
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let recorded = match state.health.list_health().await {
        Ok(rows) => rows,
        Err(error) => return health_error_response(&error),
    };

    let mut tasks = Vec::new();
    // Every expected loop first, in the deployment's order: its recorded row if it has ticked, else a
    // never-ticked (unhealthy) placeholder.
    for task in &state.expected {
        match recorded.iter().find(|row| &row.task == task) {
            Some(row) => tasks.push(TaskHealthView::from_recorded(task, true, row, now_ms)),
            None => tasks.push(TaskHealthView::never_ticked(task)),
        }
    }
    // Then any loop that ticked but the deployment does not expect — surfaced, but not counted toward
    // the overall verdict.
    for row in &recorded {
        if !state.expected.iter().any(|task| task == &row.task) {
            tasks.push(TaskHealthView::from_recorded(&row.task, false, row, now_ms));
        }
    }

    let healthy = tasks
        .iter()
        .filter(|view| view.expected)
        .all(|view| view.healthy);
    (StatusCode::OK, Json(TaskHealthReport { healthy, tasks })).into_response()
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "tenant.create",
                "tenant",
                &tenant_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "tenant.update",
                "tenant",
                &tenant_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "brand.create",
                "brand",
                &brand_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "brand.update",
                "brand",
                &brand_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "store.create",
                "store",
                &store_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "store.update",
                "store",
                &store_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
        Ok(()) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "device.create",
                "device",
                &device_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
        Ok(true) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "device.update",
                "device",
                &device_id.to_string(),
                None,
                after,
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    audit: Arc<dyn AuditRecorder>,
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
pub fn catalog_router<Cat, A, C>(
    catalog: Cat,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
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
            "/admin/catalog/export/items",
            get(admin_export_items::<Cat, A, C>),
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
            audit,
        })
}

#[derive(Debug, Clone, Deserialize)]
struct CreateItemRequest {
    tenant_id: String,
    name: String,
    /// Per-locale names keyed by locale code (ADR-0074). Optional and additive; `name` is the
    /// always-present fallback.
    #[serde(default)]
    name_translations: std::collections::BTreeMap<String, String>,
    tax_class_id: String,
    #[serde(default)]
    item_category_id: Option<String>,
    #[serde(default)]
    item_subcategory_id: Option<String>,
    /// The item's photo — a media id (ADR-0075), or absent/empty for none.
    #[serde(default)]
    image_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateItemRequest {
    tenant_id: String,
    name: String,
    /// Per-locale names keyed by locale code (ADR-0074). Optional and additive; `name` is the
    /// always-present fallback.
    #[serde(default)]
    name_translations: std::collections::BTreeMap<String, String>,
    tax_class_id: String,
    #[serde(default)]
    item_category_id: Option<String>,
    #[serde(default)]
    item_subcategory_id: Option<String>,
    /// The item's photo — a media id (ADR-0075), or absent/empty for none.
    #[serde(default)]
    image_ref: Option<String>,
    status: String,
}

/// Drops empty-key or empty-value entries from an authored per-locale name map and trims both sides,
/// so a blank row a form left behind never becomes a `""` translation the edge would show in place of
/// the default name (ADR-0074).
fn clean_name_translations(
    raw: std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    raw.into_iter()
        .filter_map(|(locale, name)| {
            let locale = locale.trim().to_owned();
            let name = name.trim().to_owned();
            (!locale.is_empty() && !name.is_empty()).then_some((locale, name))
        })
        .collect()
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

// --- Tax rates (ADR-0074, Track M4): the per-(tax class × channel) rate the edge applies ----------

/// The highest rate the table accepts, in basis points (100%). Matches the migration's CHECK.
const MAX_TAX_RATE_BPS: u32 = 10_000;

/// The state the tax-rate routes share: the rate store they write, the catalog store they validate a
/// class against, and the admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct TaxRateState<Tax, Cat, A, C> {
    tax_rates: Tax,
    catalog: Cat,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// One authored tax-rate row on the wire: the class id, the channel's wire token, and the rate in
/// basis points (10% is `1000`).
#[derive(Debug, Clone, serde::Serialize)]
struct TaxRateView {
    tax_class_id: String,
    sales_channel: String,
    rate_bps: u32,
}

/// A `PUT /admin/catalog/tax-rates` body: the tenant and its whole rate table (a wholesale replace).
#[derive(Debug, Clone, Deserialize)]
struct SetTaxRatesRequest {
    tenant_id: String,
    rates: Vec<TaxRateRowRequest>,
}

/// One row of a [`SetTaxRatesRequest`].
#[derive(Debug, Clone, Deserialize)]
struct TaxRateRowRequest {
    tax_class_id: String,
    sales_channel: String,
    rate_bps: u32,
}

/// Builds the tax-rate sub-router ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// One resource: the tenant's per-(tax class × channel) rate table. `GET` lists it (behind
/// [`ConsolePermission::Read`]); `PUT` replaces it wholesale (behind
/// [`ConsolePermission::ManageCatalog`], the permission tax *classes* already use), validating each
/// row's class, channel, and rate before the write, and auditing `tax_rate.set`.
pub fn tax_rate_router<Tax, Cat, A, C>(
    tax_rates: Tax,
    catalog: Cat,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/tax-rates",
            get(admin_list_tax_rates::<Tax, Cat, A, C>).put(admin_set_tax_rates::<Tax, Cat, A, C>),
        )
        .with_state(TaxRateState {
            tax_rates,
            catalog,
            admin,
            clock,
            audit,
        })
}

/// One authored entry as the wire view.
fn tax_rate_view(entry: &TaxRateEntry) -> TaxRateView {
    TaxRateView {
        tax_class_id: entry.tax_class_id.to_string(),
        sales_channel: entry.sales_channel.as_wire().to_owned(),
        rate_bps: entry.rate.basis_points(),
    }
}

/// A super-admin lists a tenant's authored tax rates.
async fn admin_list_tax_rates<Tax, Cat, A, C>(
    State(state): State<TaxRateState<Tax, Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
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
    match state.tax_rates.list_tax_rates(tenant_id).await {
        Ok(rows) => {
            let view: Vec<TaxRateView> = rows.iter().map(tax_rate_view).collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => tax_rate_error_response(&error),
    }
}

/// A super-admin replaces a tenant's whole tax-rate table.
///
/// Every row is validated before the write: its class must be one the tenant has authored, its channel
/// a token this build knows, its rate no more than 100%, and no `(class, channel)` pair repeated —
/// each a `400` naming the fault, so a bad grid never reaches the store as a `500`.
async fn admin_set_tax_rates<Tax, Cat, A, C>(
    State(state): State<TaxRateState<Tax, Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<SetTaxRatesRequest>,
) -> Response
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let known: BTreeSet<TaxClassId> = match state.catalog.list_tax_classes(tenant_id).await {
        Ok(classes) => classes.iter().map(|class| class.tax_class_id).collect(),
        Err(error) => return catalog_error_response(&error),
    };
    let mut entries = Vec::with_capacity(request.rates.len());
    let mut seen: BTreeSet<(TaxClassId, SalesChannel)> = BTreeSet::new();
    for row in &request.rates {
        let Ok(tax_class_id) = row.tax_class_id.parse::<Ulid>().map(TaxClassId::new) else {
            return (StatusCode::BAD_REQUEST, "a tax_class_id is not a ULID").into_response();
        };
        if !known.contains(&tax_class_id) {
            return (
                StatusCode::BAD_REQUEST,
                "a tax rate names an unknown tax class",
            )
                .into_response();
        }
        let Some(sales_channel) = SalesChannel::from_wire(&row.sales_channel) else {
            return (
                StatusCode::BAD_REQUEST,
                "a tax rate names an unknown sales channel",
            )
                .into_response();
        };
        if row.rate_bps > MAX_TAX_RATE_BPS {
            return (StatusCode::BAD_REQUEST, "a tax rate exceeds 100%").into_response();
        }
        if !seen.insert((tax_class_id, sales_channel)) {
            return (
                StatusCode::BAD_REQUEST,
                "a (tax class, channel) pair is repeated",
            )
                .into_response();
        }
        entries.push(TaxRateEntry {
            tax_class_id,
            sales_channel,
            rate: TaxRate::from_basis_points(row.rate_bps),
        });
    }
    match state.tax_rates.set_tax_rates(tenant_id, &entries).await {
        Ok(()) => {
            let view: Vec<TaxRateView> = entries.iter().map(tax_rate_view).collect();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "tax_rate.set",
                "tax_rate",
                &tenant_id.to_string(),
                None,
                serde_json::to_value(&view).ok(),
            )
            .await;
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => tax_rate_error_response(&error),
    }
}

/// Maps a tax-rate store failure to a retryable `503`, logging the detail rather than leaking it.
fn tax_rate_error_response(error: &TaxRateStoreError) -> Response {
    tracing::error!(%error, "a tax-rate store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the tax-rate service is unavailable",
    )
        .into_response()
}

// --- Campaigns (ADR-0077, Track M3): author the promotions the edge's pricing engine evaluates ----

/// The largest minute-of-day a schedule window accepts (exclusive) — a day has 1440 minutes.
const MINUTES_PER_DAY: u16 = 24 * 60;

/// The state the campaign routes share: the campaign store they read and write, and the
/// admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct CampaignState<Camp, A, C> {
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A create/update body: the tenant and the campaign's authoring fields. The id is **server-owned** —
/// minted on create, the path id on update — so a client never supplies or forges it. Everything else
/// is the wire campaign's own shape (kind, action, conditions), reused rather than re-declared.
#[derive(Debug, Clone, Deserialize)]
struct CampaignRequest {
    tenant_id: String,
    name: String,
    kind: PublishedCampaignKind,
    priority: i32,
    #[serde(default)]
    exclusion_group: Option<u16>,
    action: PublishedAction,
    #[serde(default)]
    conditions: PublishedConditions,
    #[serde(default)]
    quota_remaining: Option<u32>,
}

/// A compact audit summary of a campaign: enough to see *what* changed, without reproducing the exact
/// discount terms (T2 configuration) in the queryable audit trail — the terms live in the campaign
/// store, the trail records that they moved.
#[derive(Debug, Clone, serde::Serialize)]
struct CampaignAuditSummary {
    campaign_id: String,
    name: String,
    kind: PublishedCampaignKind,
    priority: i32,
}

impl CampaignAuditSummary {
    fn of(campaign: &PublishedCampaign) -> Self {
        Self {
            campaign_id: campaign.id.to_string(),
            name: campaign.name.as_str().to_owned(),
            kind: campaign.kind,
            priority: campaign.priority,
        }
    }
}

/// Builds the campaigns sub-router ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// Per-campaign CRUD over a tenant's promotions. `GET` (list and by-id) is behind
/// [`ConsolePermission::Read`]; `POST`/`PUT`/`DELETE` are behind [`ConsolePermission::ManageCampaigns`]
/// and audited (`campaign.create`/`update`/`delete`) with a summary, never the discount terms. The id
/// is server-owned. Publishing these campaigns to a store is a separate route that reuses
/// `PublishConfig` (a later slice).
pub fn campaign_router<Camp, A, C>(
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/campaigns",
            get(admin_list_campaigns::<Camp, A, C>).post(admin_create_campaign::<Camp, A, C>),
        )
        .route(
            "/admin/campaigns/{campaign_id}",
            get(admin_get_campaign::<Camp, A, C>)
                .put(admin_update_campaign::<Camp, A, C>)
                .delete(admin_delete_campaign::<Camp, A, C>),
        )
        .with_state(CampaignState {
            campaigns,
            admin,
            clock,
            audit,
        })
}

/// Validates a campaign request and builds the campaign with the given (server-owned) id. Returns the
/// fault message for a `400` rather than the whole response, so the error type stays small (a
/// `Result<_, Response>` trips `clippy::result_large_err`).
fn build_campaign(
    request: &CampaignRequest,
    id: CampaignId,
) -> Result<PublishedCampaign, &'static str> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err("a campaign name is required");
    }
    if let Some(schedule) = &request.conditions.schedule
        && (schedule.start_minute >= MINUTES_PER_DAY || schedule.end_minute >= MINUTES_PER_DAY)
    {
        return Err("a schedule minute is out of range (0..1440)");
    }
    if let Some(channels) = &request.conditions.channels
        && channels
            .iter()
            .any(|channel| channel.is_unrecognised() || channel.is_unspecified())
    {
        return Err("a campaign names an unknown sales channel");
    }
    match request.action {
        PublishedAction::Percentage { rate } if rate.numerator() < 0 => {
            return Err("a percentage rate cannot be negative");
        }
        PublishedAction::AmountOff { amount } if amount.is_negative() => {
            return Err("an amount off cannot be negative");
        }
        _ => {}
    }
    Ok(PublishedCampaign {
        id,
        name: DisplayName::new(name),
        kind: request.kind,
        priority: request.priority,
        exclusion_group: request.exclusion_group,
        action: request.action,
        conditions: request.conditions.clone(),
        quota_remaining: request.quota_remaining,
    })
}

/// A super-admin lists a tenant's authored campaigns.
async fn admin_list_campaigns<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => (StatusCode::OK, Json(campaigns)).into_response(),
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin reads one campaign by id.
async fn admin_get_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    let (Some(tenant_id), Some(campaign_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        campaign_id.parse::<Ulid>().ok().map(CampaignId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or campaign_id is not a ULID",
        )
            .into_response();
    };
    match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(campaign)) => (StatusCode::OK, Json(campaign)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such campaign").into_response(),
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin creates a campaign; the server mints its id.
async fn admin_create_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CampaignRequest>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCampaigns,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Some(campaign_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(CampaignId::new)
    else {
        return campaign_entropy_unavailable();
    };
    let campaign = match build_campaign(&request, campaign_id) {
        Ok(campaign) => campaign,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state.campaigns.upsert_campaign(tenant_id, &campaign).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "campaign.create",
                "campaign",
                &campaign_id.to_string(),
                None,
                serde_json::to_value(CampaignAuditSummary::of(&campaign)).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(campaign)).into_response()
        }
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin updates a campaign in place, by the path id.
async fn admin_update_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Json(request): Json<CampaignRequest>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCampaigns,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(campaign_id)) = (
        request.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        campaign_id.parse::<Ulid>().ok().map(CampaignId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or campaign_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such campaign").into_response(),
        Err(error) => return campaign_error_response(&error),
    };
    let campaign = match build_campaign(&request, campaign_id) {
        Ok(campaign) => campaign,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state.campaigns.upsert_campaign(tenant_id, &campaign).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "campaign.update",
                "campaign",
                &campaign_id.to_string(),
                serde_json::to_value(CampaignAuditSummary::of(&before)).ok(),
                serde_json::to_value(CampaignAuditSummary::of(&campaign)).ok(),
            )
            .await;
            (StatusCode::OK, Json(campaign)).into_response()
        }
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin deletes a campaign by id.
async fn admin_delete_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCampaigns,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(campaign_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        campaign_id.parse::<Ulid>().ok().map(CampaignId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or campaign_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such campaign").into_response(),
        Err(error) => return campaign_error_response(&error),
    };
    match state
        .campaigns
        .delete_campaign(tenant_id, campaign_id)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "campaign.delete",
                "campaign",
                &campaign_id.to_string(),
                serde_json::to_value(CampaignAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => campaign_error_response(&error),
    }
}

/// `503` when OS entropy is unavailable to mint a campaign id — the request cannot proceed and a retry
/// may succeed.
fn campaign_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a campaign id");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the campaign service is unavailable",
    )
        .into_response()
}

/// Maps a campaign store failure to a retryable `503`, logging the detail rather than leaking it.
fn campaign_error_response(error: &CampaignStoreError) -> Response {
    tracing::error!(%error, "a campaign store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the campaign service is unavailable",
    )
        .into_response()
}

// --- Inventory authoring (`/admin/inventory/*`, ADR-0079, Track M6) ----------------------------

/// The state the inventory routes share: the inventory store they read and write, and the
/// admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct InventoryState<Inv, A, C> {
    inventory: Inv,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A create/update body for an ingredient. The id is **server-owned** — minted on create, the path id
/// on update — so a client never supplies or forges it. `name` and `unit` are the wire ingredient's own
/// fields, reused rather than re-declared.
#[derive(Debug, Clone, Deserialize)]
struct IngredientRequest {
    tenant_id: String,
    name: String,
    unit: Open<UnitOfMeasure>,
}

/// A create-or-replace body for a recipe. The item it makes is the URL key — a recipe references an
/// existing menu item or modifier, so its id is client-owned, unlike an ingredient's server-minted id.
/// `lines` (the bill of materials) and `auto_86_threshold` are the wire recipe's own fields.
#[derive(Debug, Clone, Deserialize)]
struct RecipeRequest {
    tenant_id: String,
    #[serde(default)]
    lines: Vec<PublishedRecipeLine>,
    #[serde(default)]
    auto_86_threshold: i64,
}

/// A create/update body for a supplier. The id is **server-owned**, exactly like an ingredient's.
#[derive(Debug, Clone, Deserialize)]
struct SupplierRequest {
    tenant_id: String,
    name: String,
}

/// A compact audit summary of an ingredient — reference data (id, name, unit), never a T1 field.
#[derive(Debug, Clone, serde::Serialize)]
struct IngredientAuditSummary {
    ingredient_id: String,
    name: String,
    unit: String,
}

impl IngredientAuditSummary {
    fn of(ingredient: &PublishedIngredient) -> Self {
        Self {
            ingredient_id: ingredient.id.to_string(),
            name: ingredient.name.as_str().to_owned(),
            unit: ingredient.unit.as_wire().to_owned(),
        }
    }
}

/// A compact audit summary of a recipe: the item, how many BOM lines it has, and its threshold — enough
/// to see *that* a recipe changed, without reproducing the per-ingredient amounts (the recipe itself is
/// proprietary process, T2) in the queryable audit trail. The BOM lives in the inventory store; the
/// trail records that it moved.
#[derive(Debug, Clone, serde::Serialize)]
struct RecipeAuditSummary {
    item: String,
    line_count: usize,
    auto_86_threshold: i64,
}

impl RecipeAuditSummary {
    fn of(recipe: &PublishedRecipe) -> Self {
        Self {
            item: recipe.item.to_string(),
            line_count: recipe.lines.len(),
            auto_86_threshold: recipe.auto_86_threshold,
        }
    }
}

/// A compact audit summary of a supplier — reference data (id, name).
#[derive(Debug, Clone, serde::Serialize)]
struct SupplierAuditSummary {
    supplier_id: String,
    name: String,
}

impl SupplierAuditSummary {
    fn of(supplier: &PublishedSupplier) -> Self {
        Self {
            supplier_id: supplier.id.to_string(),
            name: supplier.name.as_str().to_owned(),
        }
    }
}

/// Builds the inventory sub-router ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md), M6).
///
/// Per-record CRUD over a tenant's ingredients, recipes, and supplier references. `GET` (list and
/// by-id) is behind [`ConsolePermission::Read`]; the writes are behind
/// [`ConsolePermission::ManageInventory`] and audited by summary — the ingredient/supplier reference
/// data in full, a recipe by its item, line count, and threshold only (never the BOM amounts, which are
/// proprietary process). An ingredient's and a supplier's id is server-minted; a recipe's key is the
/// menu item it makes, so a recipe is a `PUT` upsert keyed by the path id. Publishing the composed
/// `inventory` node to a store is a separate route that reuses `PublishConfig` (a later slice).
pub fn inventory_router<Inv, A, C>(
    inventory: Inv,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/inventory/ingredients",
            get(admin_list_ingredients::<Inv, A, C>).post(admin_create_ingredient::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/ingredients/{ingredient_id}",
            get(admin_get_ingredient::<Inv, A, C>)
                .put(admin_update_ingredient::<Inv, A, C>)
                .delete(admin_delete_ingredient::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/recipes",
            get(admin_list_recipes::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/recipes/{item_id}",
            get(admin_get_recipe::<Inv, A, C>)
                .put(admin_upsert_recipe::<Inv, A, C>)
                .delete(admin_delete_recipe::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/suppliers",
            get(admin_list_suppliers::<Inv, A, C>).post(admin_create_supplier::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/suppliers/{supplier_id}",
            get(admin_get_supplier::<Inv, A, C>)
                .put(admin_update_supplier::<Inv, A, C>)
                .delete(admin_delete_supplier::<Inv, A, C>),
        )
        .with_state(InventoryState {
            inventory,
            admin,
            clock,
            audit,
        })
}

/// Validates an ingredient request and builds the ingredient with the given (server-owned) id. Returns
/// the fault message for a `400` rather than the whole response, so the error type stays small.
fn build_ingredient(
    request: &IngredientRequest,
    id: IngredientId,
) -> Result<PublishedIngredient, &'static str> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err("an ingredient name is required");
    }
    if request.unit.is_unspecified() || request.unit.is_unrecognised() {
        return Err("an ingredient names an unknown unit of measure");
    }
    Ok(PublishedIngredient {
        id,
        name: DisplayName::new(name),
        unit: request.unit.clone(),
    })
}

/// Validates a recipe request and builds the recipe for the given (client-owned, path) item.
fn build_recipe(
    request: &RecipeRequest,
    item: MenuItemId,
) -> Result<PublishedRecipe, &'static str> {
    if request.auto_86_threshold < 0 {
        return Err("an auto-86 threshold cannot be negative");
    }
    if request
        .lines
        .iter()
        .any(|line| line.per_unit.as_milli() <= 0)
    {
        return Err("a recipe line must consume a positive amount");
    }
    Ok(PublishedRecipe {
        item,
        lines: request.lines.clone(),
        auto_86_threshold: request.auto_86_threshold,
    })
}

/// Validates a supplier request and builds the supplier with the given (server-owned) id.
fn build_supplier(
    request: &SupplierRequest,
    id: SupplierId,
) -> Result<PublishedSupplier, &'static str> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err("a supplier name is required");
    }
    Ok(PublishedSupplier {
        id,
        name: DisplayName::new(name),
    })
}

/// A super-admin lists a tenant's authored ingredients.
async fn admin_list_ingredients<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    match state.inventory.list_ingredients(tenant_id).await {
        Ok(ingredients) => (StatusCode::OK, Json(ingredients)).into_response(),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin reads one ingredient by id.
async fn admin_get_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(ingredient_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    let (Some(tenant_id), Some(ingredient_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        ingredient_id.parse::<Ulid>().ok().map(IngredientId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or ingredient_id is not a ULID",
        )
            .into_response();
    };
    match state.inventory.list_ingredients(tenant_id).await {
        Ok(ingredients) => match ingredients.into_iter().find(|i| i.id == ingredient_id) {
            Some(ingredient) => (StatusCode::OK, Json(ingredient)).into_response(),
            None => (StatusCode::NOT_FOUND, "no such ingredient").into_response(),
        },
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin creates an ingredient; the server mints its id.
async fn admin_create_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Json(request): Json<IngredientRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Some(ingredient_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(IngredientId::new)
    else {
        return inventory_entropy_unavailable();
    };
    let ingredient = match build_ingredient(&request, ingredient_id) {
        Ok(ingredient) => ingredient,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state
        .inventory
        .upsert_ingredient(tenant_id, &ingredient)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.ingredient.create",
                "ingredient",
                &ingredient_id.to_string(),
                None,
                serde_json::to_value(IngredientAuditSummary::of(&ingredient)).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(ingredient)).into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin updates an ingredient in place, by the path id.
async fn admin_update_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(ingredient_id): Path<String>,
    Json(request): Json<IngredientRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(ingredient_id)) = (
        request.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        ingredient_id.parse::<Ulid>().ok().map(IngredientId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or ingredient_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.inventory.list_ingredients(tenant_id).await {
        Ok(ingredients) => ingredients.into_iter().find(|i| i.id == ingredient_id),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return (StatusCode::NOT_FOUND, "no such ingredient").into_response();
    };
    let ingredient = match build_ingredient(&request, ingredient_id) {
        Ok(ingredient) => ingredient,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state
        .inventory
        .upsert_ingredient(tenant_id, &ingredient)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.ingredient.update",
                "ingredient",
                &ingredient_id.to_string(),
                serde_json::to_value(IngredientAuditSummary::of(&before)).ok(),
                serde_json::to_value(IngredientAuditSummary::of(&ingredient)).ok(),
            )
            .await;
            (StatusCode::OK, Json(ingredient)).into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin deletes an ingredient by id.
async fn admin_delete_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(ingredient_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(ingredient_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        ingredient_id.parse::<Ulid>().ok().map(IngredientId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or ingredient_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.inventory.list_ingredients(tenant_id).await {
        Ok(ingredients) => ingredients.into_iter().find(|i| i.id == ingredient_id),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return (StatusCode::NOT_FOUND, "no such ingredient").into_response();
    };
    match state
        .inventory
        .delete_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.ingredient.delete",
                "ingredient",
                &ingredient_id.to_string(),
                serde_json::to_value(IngredientAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin lists a tenant's authored recipes.
async fn admin_list_recipes<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    match state.inventory.list_recipes(tenant_id).await {
        Ok(recipes) => (StatusCode::OK, Json(recipes)).into_response(),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin reads one recipe by the item it makes.
async fn admin_get_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    let (Some(tenant_id), Some(item)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        item_id.parse::<Ulid>().ok().map(MenuItemId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or item_id is not a ULID",
        )
            .into_response();
    };
    match state.inventory.list_recipes(tenant_id).await {
        Ok(recipes) => match recipes.into_iter().find(|r| r.item == item) {
            Some(recipe) => (StatusCode::OK, Json(recipe)).into_response(),
            None => (StatusCode::NOT_FOUND, "no such recipe").into_response(),
        },
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin creates or replaces the recipe for the path item (an upsert, since the item is the
/// recipe's client-owned key). `201` when the item had no recipe before, `200` when it replaced one.
async fn admin_upsert_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Json(request): Json<RecipeRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(item)) = (
        request.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        item_id.parse::<Ulid>().ok().map(MenuItemId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or item_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.inventory.list_recipes(tenant_id).await {
        Ok(recipes) => recipes.into_iter().find(|r| r.item == item),
        Err(error) => return inventory_error_response(&error),
    };
    let recipe = match build_recipe(&request, item) {
        Ok(recipe) => recipe,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state.inventory.upsert_recipe(tenant_id, &recipe).await {
        Ok(()) => {
            let (action, status) = match &before {
                Some(_) => ("inventory.recipe.update", StatusCode::OK),
                None => ("inventory.recipe.create", StatusCode::CREATED),
            };
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                action,
                "recipe",
                &item.to_string(),
                before
                    .as_ref()
                    .and_then(|r| serde_json::to_value(RecipeAuditSummary::of(r)).ok()),
                serde_json::to_value(RecipeAuditSummary::of(&recipe)).ok(),
            )
            .await;
            (status, Json(recipe)).into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin deletes a recipe by the item it makes.
async fn admin_delete_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(item)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        item_id.parse::<Ulid>().ok().map(MenuItemId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or item_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.inventory.list_recipes(tenant_id).await {
        Ok(recipes) => recipes.into_iter().find(|r| r.item == item),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return (StatusCode::NOT_FOUND, "no such recipe").into_response();
    };
    match state.inventory.delete_recipe(tenant_id, item).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.recipe.delete",
                "recipe",
                &item.to_string(),
                serde_json::to_value(RecipeAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin lists a tenant's authored suppliers.
async fn admin_list_suppliers<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    match state.inventory.list_suppliers(tenant_id).await {
        Ok(suppliers) => (StatusCode::OK, Json(suppliers)).into_response(),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin reads one supplier by id.
async fn admin_get_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(supplier_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    let (Some(tenant_id), Some(supplier_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        supplier_id.parse::<Ulid>().ok().map(SupplierId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or supplier_id is not a ULID",
        )
            .into_response();
    };
    match state.inventory.list_suppliers(tenant_id).await {
        Ok(suppliers) => match suppliers.into_iter().find(|s| s.id == supplier_id) {
            Some(supplier) => (StatusCode::OK, Json(supplier)).into_response(),
            None => (StatusCode::NOT_FOUND, "no such supplier").into_response(),
        },
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin creates a supplier; the server mints its id.
async fn admin_create_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Json(request): Json<SupplierRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = request.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Some(supplier_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(SupplierId::new)
    else {
        return inventory_entropy_unavailable();
    };
    let supplier = match build_supplier(&request, supplier_id) {
        Ok(supplier) => supplier,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state.inventory.upsert_supplier(tenant_id, &supplier).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.supplier.create",
                "supplier",
                &supplier_id.to_string(),
                None,
                serde_json::to_value(SupplierAuditSummary::of(&supplier)).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(supplier)).into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin updates a supplier in place, by the path id.
async fn admin_update_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(supplier_id): Path<String>,
    Json(request): Json<SupplierRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(supplier_id)) = (
        request.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        supplier_id.parse::<Ulid>().ok().map(SupplierId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or supplier_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.inventory.list_suppliers(tenant_id).await {
        Ok(suppliers) => suppliers.into_iter().find(|s| s.id == supplier_id),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return (StatusCode::NOT_FOUND, "no such supplier").into_response();
    };
    let supplier = match build_supplier(&request, supplier_id) {
        Ok(supplier) => supplier,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state.inventory.upsert_supplier(tenant_id, &supplier).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.supplier.update",
                "supplier",
                &supplier_id.to_string(),
                serde_json::to_value(SupplierAuditSummary::of(&before)).ok(),
                serde_json::to_value(SupplierAuditSummary::of(&supplier)).ok(),
            )
            .await;
            (StatusCode::OK, Json(supplier)).into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin deletes a supplier by id.
async fn admin_delete_supplier<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(supplier_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(supplier_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        supplier_id.parse::<Ulid>().ok().map(SupplierId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or supplier_id is not a ULID",
        )
            .into_response();
    };
    let before = match state.inventory.list_suppliers(tenant_id).await {
        Ok(suppliers) => suppliers.into_iter().find(|s| s.id == supplier_id),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return (StatusCode::NOT_FOUND, "no such supplier").into_response();
    };
    match state
        .inventory
        .delete_supplier(tenant_id, supplier_id)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.supplier.delete",
                "supplier",
                &supplier_id.to_string(),
                serde_json::to_value(SupplierAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}

/// The `503` for when the OS entropy needed to mint an inventory id is unavailable.
fn inventory_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint an inventory id");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the inventory service is unavailable",
    )
        .into_response()
}

/// Maps an inventory store failure to a retryable `503`, logging the detail rather than leaking it.
fn inventory_error_response(error: &InventoryStoreError) -> Response {
    tracing::error!(%error, "an inventory store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the inventory service is unavailable",
    )
        .into_response()
}

// --- Inventory publish (`/admin/config/inventory`, ADR-0079, Track M6) -------------------------

/// The collaborators the inventory-publish route needs: the inventory store the ingredients, recipes,
/// and suppliers are read from, the config-tree store the `inventory` node is written onto, plus the
/// admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigInventoryState<Inv, Cfg, A, C> {
    inventory: Inv,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `PUT /admin/config/inventory` body: the `(tenant, store)` to push the tenant's authored inventory
/// to. Like every other node publish, the ingredients/recipes/suppliers come from what is authored, not
/// the body.
#[derive(Debug, Clone, Deserialize)]
struct PublishInventoryRequest {
    tenant_id: String,
    store_id: String,
}

/// A compact audit summary of an inventory publish — how many of each kind went into the node, never
/// the recipe amounts (T2 proprietary process live in the inventory store).
#[derive(Debug, Clone, serde::Serialize)]
struct InventoryPublishSummary {
    ingredients: usize,
    recipes: usize,
    suppliers: usize,
}

/// Reads a tenant's three authored inventory lists in one place, short-circuiting on the first store
/// failure. Split out of [`admin_publish_inventory`] to keep that handler within its line budget.
async fn list_inventory_parts<Inv>(
    inventory: &Inv,
    tenant_id: TenantId,
) -> Result<
    (
        Vec<PublishedIngredient>,
        Vec<PublishedRecipe>,
        Vec<PublishedSupplier>,
    ),
    InventoryStoreError,
>
where
    Inv: InventoryStore,
{
    Ok((
        inventory.list_ingredients(tenant_id).await?,
        inventory.list_recipes(tenant_id).await?,
        inventory.list_suppliers(tenant_id).await?,
    ))
}

/// Builds the inventory-publish sub-router ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md), M6).
///
/// One route: assemble the tenant's authored ingredients, recipes, and suppliers into a
/// `PublishedInventory`, write it as the store's `inventory` config node, and version it through the
/// config tree — the same node-merge the campaigns/tax/floor/menu publishes use, so the other
/// Store-level keys survive. Behind [`ConsolePermission::PublishConfig`]. The edge applies the node to
/// build its `RecipeBook` and per-item auto-86 thresholds (§8).
pub fn config_inventory_router<Inv, Cfg, A, C>(
    inventory: Inv,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/inventory",
            axum::routing::put(admin_publish_inventory::<Inv, Cfg, A, C>),
        )
        .with_state(ConfigInventoryState {
            inventory,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Sets one node `key` on a store's Store-layer object (index 2), preserving its other keys, and
/// returns the layer to re-publish. A missing or non-object prior layer starts from an empty object,
/// so the first publish and a corrupt layer both compose cleanly.
fn store_layer_with(
    state_before: Option<&ConfigTreeState>,
    key: &str,
    value: serde_json::Value,
) -> serde_json::Value {
    let mut layer = state_before.map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !layer.is_object() {
        layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut layer {
        map.insert(key.to_owned(), value);
    }
    layer
}

/// Assembles a tenant's authored inventory into a `PublishedInventory`, writes it as the store's
/// `inventory` node, and versions it — the same load→merge→publish→version shape as the other node
/// publishes.
async fn admin_publish_inventory<Inv, Cfg, A, C>(
    State(state): State<ConfigInventoryState<Inv, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishInventoryRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let (ingredients, recipes, suppliers) =
        match list_inventory_parts(&state.inventory, tenant_id).await {
            Ok(parts) => parts,
            Err(error) => return inventory_error_response(&error),
        };
    let summary = InventoryPublishSummary {
        ingredients: ingredients.len(),
        recipes: recipes.len(),
        suppliers: suppliers.len(),
    };
    let Ok(inventory_value) =
        serde_json::to_value(inventory_to_node(ingredients, recipes, suppliers))
    else {
        tracing::error!("could not serialise an inventory node");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the inventory service is unavailable",
        )
            .into_response();
    };

    // Set the `inventory` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys (`menu`, `tax`, `campaigns`, `permissions`, `floor`, capability flags).
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let store_layer = store_layer_with(state_before.as_ref(), "inventory", inventory_value);

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
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.inventory.publish",
                "store",
                &store_id.to_string(),
                None,
                serde_json::to_value(summary).ok(),
            )
            .await;
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

// --- Campaign publish (`/admin/config/campaigns`, ADR-0077, Track M3) --------------------------

/// The collaborators the campaign-publish route needs: the campaign store the promotions are read
/// from, the config-tree store the `campaigns` node is written onto, plus the admin/clock/audit every
/// write carries.
#[derive(Clone)]
struct ConfigCampaignsState<Camp, Cfg, A, C> {
    campaigns: Camp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `PUT /admin/config/campaigns` body: the `(tenant, store)` to push the tenant's authored campaigns
/// to. Like every other node publish, the campaigns come from what is authored, not the body.
#[derive(Debug, Clone, Deserialize)]
struct PublishCampaignsRequest {
    tenant_id: String,
    store_id: String,
}

/// Builds the campaign-publish sub-router ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// One route: assemble the tenant's authored campaigns into a `PublishedCampaigns`, write it as the
/// store's `campaigns` config node, and version it through the config tree — the same node-merge the
/// tax/floor/menu publishes use, so the other Store-level keys survive. Behind
/// [`ConsolePermission::PublishConfig`]. The edge applies the node to `EdgeSession::campaigns`.
pub fn config_campaigns_router<Camp, Cfg, A, C>(
    campaigns: Camp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/campaigns",
            axum::routing::put(admin_publish_campaigns::<Camp, Cfg, A, C>),
        )
        .route(
            "/admin/config/campaigns/preview",
            post(preview_publish_campaigns::<Camp, Cfg, A, C>),
        )
        .with_state(ConfigCampaignsState {
            campaigns,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Assembles a tenant's authored campaigns into a `PublishedCampaigns`, writes it as the store's
/// `campaigns` node, and versions it — the same load→merge→publish→version shape as the other node
/// publishes.
async fn admin_publish_campaigns<Camp, Cfg, A, C>(
    State(state): State<ConfigCampaignsState<Camp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishCampaignsRequest>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => campaigns,
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(campaigns_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the campaign service is unavailable",
        )
            .into_response();
    };

    // Set the `campaigns` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys (`menu`, `tax`, `permissions`, `floor`, capability flags).
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert("campaigns".to_owned(), campaigns_value);
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
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.campaigns.publish",
                "store",
                &store_id.to_string(),
                None,
                serde_json::to_value(campaigns.len()).ok(),
            )
            .await;
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

/// What publishing a candidate node *would* change, computed without minting a version or saving:
/// the RFC 7386 merge patch from the store's current effective document to the candidate, so the
/// console can show a real before/after before anyone commits ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
#[derive(Debug, Clone, serde::Serialize)]
struct ConfigPreview {
    /// The version the diff is computed against — the store's current version — or `null` when the
    /// store has no published configuration yet (the candidate is then entirely new).
    from_version_id: Option<String>,
    /// The RFC 7386 merge patch a publish would apply to the effective document. An empty object
    /// means the candidate composes to exactly today's effective document.
    diff: serde_json::Value,
    /// True when `diff` is empty — nothing would change, so there is nothing to publish.
    unchanged: bool,
}

/// The dry-run behind a node publish: composes the store's effective document with `node_key` set to
/// `node_value` on the Store layer (index 2) and returns the merge patch from the store's current
/// effective document to that candidate. Mints no version and saves nothing — the tree is composed in
/// memory and dropped.
///
/// The candidate is validated with exactly the [`CapabilityValidator`] a real publish uses, so
/// `Err` carries the violations a publish would reject it with — the preview reports them verbatim
/// rather than a bland "invalid". `diff` mirrors the delta an up-to-date store would receive on the
/// next sync: current published effective → candidate effective.
fn preview_config_node(
    state_before: Option<&ConfigTreeState>,
    node_key: &str,
    node_value: serde_json::Value,
) -> Result<ConfigPreview, Vec<String>> {
    use serde_json::Value;
    let empty = || Value::Object(serde_json::Map::new());
    // The four authored layers as stored (all empty when the store has no tree yet).
    let layers: [Value; 4] = state_before.map_or_else(
        || [empty(), empty(), empty(), empty()],
        |s| s.layers.clone(),
    );
    // The effective document the store currently holds — what a sync delta is computed against.
    let (from_version_id, current) = state_before
        .and_then(|s| s.history.last())
        .map_or((None, empty()), |v| {
            (Some(v.id.to_string()), v.effective.clone())
        });

    // Set `node_key` on the Store layer, leaving its other keys intact, exactly as the publish route
    // does before composing.
    let mut store_layer = layers[2].clone();
    if !store_layer.is_object() {
        store_layer = empty();
    }
    if let Value::Object(map) = &mut store_layer {
        map.insert(node_key.to_owned(), node_value);
    }

    let candidate = merge_layers(&[&layers[0], &layers[1], &store_layer, &layers[3]]);
    CapabilityValidator.validate(&candidate)?;

    let patch = diff(&current, &candidate);
    let unchanged = matches!(&patch, Value::Object(map) if map.is_empty());
    Ok(ConfigPreview {
        from_version_id,
        diff: patch,
        unchanged,
    })
}

/// `POST /admin/config/campaigns/preview` — the dry-run of the campaigns publish. Assembles the
/// tenant's authored campaigns into the `campaigns` node exactly as the publish route would, then
/// returns the merge patch it would apply to the store's effective document — without minting a
/// version, saving, or auditing (it changes nothing). `422` with the same violations a real publish
/// would reject the candidate with. Behind [`ConsolePermission::PublishConfig`], the same audience
/// that can actually publish.
async fn preview_publish_campaigns<Camp, Cfg, A, C>(
    State(state): State<ConfigCampaignsState<Camp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishCampaignsRequest>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => campaigns,
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(campaigns_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the campaign service is unavailable",
        )
            .into_response();
    };
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    match preview_config_node(state_before.as_ref(), "campaigns", campaigns_value) {
        Ok(preview) => (StatusCode::OK, Json(preview)).into_response(),
        Err(violations) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ConfigViolations { violations }),
        )
            .into_response(),
    }
}

// --- OTA rollout levers (`/admin/config/ota`, ADR-0078, Track O3) --------------------------------

/// The collaborators the OTA rollout routes need: the config-tree store the `fleet_update` node is
/// written onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct OtaConfigState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `PUT /admin/config/ota` body: the `(tenant, store)` and the rollout to publish. There is no
/// `halted` field — a fresh publish is live; the kill switch is `POST /admin/config/ota/halt`.
#[derive(Debug, Clone, Deserialize)]
struct PublishRolloutRequest {
    tenant_id: String,
    store_id: String,
    target_version: String,
    min_ring: String,
    rollout_percent: u8,
    signing_key_id: String,
    #[serde(default)]
    revoked_key_ids: Vec<String>,
}

/// A `POST /admin/config/ota/halt` body: the `(tenant, store)` whose rollout to halt (`true`) or
/// resume (`false`) — the kill switch, without re-typing the whole rollout.
#[derive(Debug, Clone, Deserialize)]
struct HaltRolloutRequest {
    tenant_id: String,
    store_id: String,
    halted: bool,
}

/// A `GET /admin/config/ota?tenant_id=&store_id=` query: which store's published rollout to read.
#[derive(Debug, Clone, Deserialize)]
struct OtaRolloutQuery {
    tenant_id: String,
    store_id: String,
}

/// Builds the OTA rollout sub-router ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md), O3).
///
/// The first-class levers that replace hand-editing a `fleet_update` node: `PUT /admin/config/ota`
/// publishes a rollout from typed fields, `POST /admin/config/ota/halt` flips its kill switch, and
/// `GET /admin/config/ota` reads the currently-published rollout. The writes compose the `fleet_update`
/// node through the same config tree and the same `CapabilityValidator` (its `ota_violations`) the
/// generic publish used, so a malformed rollout is a `422` with the exact violations. Writes are behind
/// [`ConsolePermission::PublishOta`] and audited; the read is behind [`ConsolePermission::Read`].
pub fn ota_config_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/ota",
            get(admin_get_rollout::<Cfg, A, C>).put(admin_publish_rollout::<Cfg, A, C>),
        )
        .route(
            "/admin/config/ota/halt",
            post(admin_halt_rollout::<Cfg, A, C>),
        )
        .with_state(OtaConfigState {
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Composes a `fleet_update` node onto a store's Store layer and publishes it — the same
/// load→merge→publish→version shape as the campaigns/tax node publishes, so the other Store-level keys
/// survive. The node is validated by the config tree's `CapabilityValidator` before it commits.
async fn admin_publish_rollout<Cfg, A, C>(
    State(state): State<OtaConfigState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishRolloutRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishOta,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    // A fresh publish is live: `halted` is omitted so it defaults false in the node.
    let node = serde_json::json!({
        "target_version": request.target_version,
        "min_ring": request.min_ring,
        "rollout_percent": request.rollout_percent,
        "signing_key_id": request.signing_key_id,
        "revoked_key_ids": request.revoked_key_ids,
    });
    let audit_detail = serde_json::json!({
        "target_version": request.target_version,
        "min_ring": request.min_ring,
        "rollout_percent": request.rollout_percent,
    });
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        node,
        "config.ota.publish",
        audit_detail,
    )
    .await
}

/// Flips the kill switch on a store's published rollout: loads its authored `fleet_update`, sets
/// `halted`, and re-publishes — preserving the rest of the rollout, so an operator halts a bad rollout
/// (or resumes a paused one) without re-typing the target, ring, and key. `400` if the store has no
/// rollout to halt.
async fn admin_halt_rollout<Cfg, A, C>(
    State(state): State<OtaConfigState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<HaltRolloutRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishOta,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let Some(mut node) = state_before
        .as_ref()
        .and_then(|s| s.layers[2].get("fleet_update"))
        .cloned()
    else {
        return (
            StatusCode::BAD_REQUEST,
            "the store has no published rollout to halt",
        )
            .into_response();
    };
    if let serde_json::Value::Object(map) = &mut node {
        map.insert("halted".to_owned(), serde_json::Value::Bool(request.halted));
    }
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        node,
        "config.ota.halt",
        serde_json::json!({ "halted": request.halted }),
    )
    .await
}

/// The shared load→set-`fleet_update`→publish→save→audit tail behind both the publish and the halt
/// levers. `node` is the `fleet_update` value to set; `action`/`detail` are the audit record.
async fn publish_ota_node<Cfg, A, C>(
    state: &OtaConfigState<Cfg, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    store_id: StoreId,
    node: serde_json::Value,
    action: &str,
    detail: serde_json::Value,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert("fleet_update".to_owned(), node);
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
            audit_action(
                &state.audit,
                &state.clock,
                context,
                Some(tenant_id),
                action,
                "store",
                &store_id.to_string(),
                None,
                Some(detail),
            )
            .await;
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

/// Reads a store's currently-published rollout — the authored `fleet_update` node — or `null` if none
/// is published. Behind [`ConsolePermission::Read`].
async fn admin_get_rollout<Cfg, A, C>(
    State(state): State<OtaConfigState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<OtaRolloutQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
        query.store_id.parse::<Ulid>().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match state.config_trees.load(tenant_id, store_id).await {
        Ok(state_before) => {
            let rollout = state_before
                .as_ref()
                .and_then(|s| s.layers[2].get("fleet_update"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            (StatusCode::OK, Json(rollout)).into_response()
        }
        Err(error) => config_store_error_response(&error),
    }
}

// --- Vouchers (ADR-0077, Track M3): mint and list the codes a voucher-kind campaign redeems --------

/// The largest batch of vouchers one request may mint. Generous for a real promotion drop, bounded so
/// a typo cannot ask the store to mint millions in one call.
const MAX_VOUCHER_BATCH: u32 = 10_000;

/// The state the voucher routes share: the voucher store, the campaign store (to check the campaign
/// exists and is a voucher-kind), and the admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct VoucherState<V, Camp, A, C> {
    vouchers: V,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `POST /admin/campaigns/{id}/vouchers` body: the tenant and how many codes to mint.
#[derive(Debug, Clone, Deserialize)]
struct GenerateVouchersRequest {
    tenant_id: String,
    count: u32,
}

/// One minted or listed voucher on the wire: the id, the code, and its status.
#[derive(Debug, Clone, serde::Serialize)]
struct VoucherView {
    voucher_id: String,
    code: String,
    status: crate::vouchers::VoucherStatus,
}

/// Builds the voucher sub-router ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// `POST /admin/campaigns/{id}/vouchers` mints a batch of codes for a voucher-kind campaign and returns
/// them once; `GET` lists a campaign's codes. Both behind [`ConsolePermission::ManageCampaigns`] — a
/// voucher code carries redeemable value, so even listing it is a manage action, not a plain read. The
/// mint audits `voucher.batch.generate` with the count only, never the codes.
pub fn voucher_router<V, Camp, A, C>(
    vouchers: V,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    V: VoucherStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/campaigns/{campaign_id}/vouchers",
            get(admin_list_vouchers::<V, Camp, A, C>)
                .post(admin_generate_vouchers::<V, Camp, A, C>),
        )
        .with_state(VoucherState {
            vouchers,
            campaigns,
            admin,
            clock,
            audit,
        })
}

/// A super-admin mints a batch of voucher codes for a voucher-kind campaign.
async fn admin_generate_vouchers<V, Camp, A, C>(
    State(state): State<VoucherState<V, Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Json(request): Json<GenerateVouchersRequest>,
) -> Response
where
    V: VoucherStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCampaigns,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Some(tenant_id), Some(campaign_id)) = (
        request.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        campaign_id.parse::<Ulid>().ok().map(CampaignId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or campaign_id is not a ULID",
        )
            .into_response();
    };
    if request.count == 0 || request.count > MAX_VOUCHER_BATCH {
        return (StatusCode::BAD_REQUEST, "count must be between 1 and 10000").into_response();
    }
    // The campaign must exist and be a voucher-kind — a code only makes sense for one the engine
    // evaluates as a voucher.
    match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(campaign)) if campaign.kind == PublishedCampaignKind::Voucher => {}
        Ok(Some(_other)) => {
            return (
                StatusCode::BAD_REQUEST,
                "campaign is not a voucher-kind campaign",
            )
                .into_response();
        }
        Ok(None) => return (StatusCode::NOT_FOUND, "no such campaign").into_response(),
        Err(error) => return campaign_error_response(&error),
    }
    // Mint the batch — a fresh id and code each, failing closed if OS entropy is unavailable.
    let mut minted = Vec::with_capacity(request.count as usize);
    for _ in 0..request.count {
        let (Some(voucher_id), Some(code)) = (
            mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(VoucherId::new),
            generate_code(),
        ) else {
            return campaign_entropy_unavailable();
        };
        minted.push(NewVoucher {
            voucher_id,
            campaign_id,
            code,
        });
    }
    match state.vouchers.insert_batch(tenant_id, &minted).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "voucher.batch.generate",
                "campaign",
                &campaign_id.to_string(),
                None,
                // The count only — never the codes, which carry redeemable value.
                serde_json::to_value(minted.len()).ok(),
            )
            .await;
            let view: Vec<VoucherView> = minted
                .iter()
                .map(|voucher| VoucherView {
                    voucher_id: voucher.voucher_id.to_string(),
                    code: voucher.code.clone(),
                    status: crate::vouchers::VoucherStatus::Active,
                })
                .collect();
            (StatusCode::CREATED, Json(view)).into_response()
        }
        Err(error) => voucher_error_response(&error),
    }
}

/// A super-admin lists a campaign's minted voucher codes.
async fn admin_list_vouchers<V, Camp, A, C>(
    State(state): State<VoucherState<V, Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    V: VoucherStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCampaigns,
    )
    .await
    {
        return denied;
    }
    let (Some(tenant_id), Some(campaign_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        campaign_id.parse::<Ulid>().ok().map(CampaignId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or campaign_id is not a ULID",
        )
            .into_response();
    };
    match state
        .vouchers
        .list_by_campaign(tenant_id, campaign_id)
        .await
    {
        Ok(records) => {
            let view: Vec<VoucherView> = records
                .into_iter()
                .map(|record| VoucherView {
                    voucher_id: record.voucher_id,
                    code: record.code,
                    status: record.status,
                })
                .collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => voucher_error_response(&error),
    }
}

/// Maps a voucher store failure to a retryable `503`, logging the detail rather than leaking it.
fn voucher_error_response(error: &VoucherStoreError) -> Response {
    tracing::error!(%error, "a voucher store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the voucher service is unavailable",
    )
        .into_response()
}

// --- Scheduled publishes (ADR-0077, Track M3): the effective-dated / Tết-menu mechanism -----------

/// The state the scheduled-publish routes share: the scheduled-publish store, the campaign store (to
/// snapshot the campaigns node at schedule time), and the admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct ScheduledPublishState<Sch, Camp, A, C> {
    scheduled: Sch,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `POST /admin/config/campaigns/schedule` body: the `(tenant, store)` and the future instant the
/// snapshot should publish, Unix milliseconds.
#[derive(Debug, Clone, Deserialize)]
struct ScheduleCampaignsRequest {
    tenant_id: String,
    store_id: String,
    effective_at_ms: i64,
}

/// A scheduled publish as listed — metadata only; the snapshotted node value is not returned (it can
/// be large, and the console shows what and when, not the payload).
#[derive(Debug, Clone, serde::Serialize)]
struct ScheduledPublishView {
    id: String,
    node_key: String,
    effective_at_ms: i64,
    status: ScheduledPublishStatus,
    created_at_ms: i64,
}

/// A `GET /admin/config/scheduled` query: the `(tenant, store)` whose pending publishes to list.
#[derive(Debug, Clone, Deserialize)]
struct ScheduledListQuery {
    tenant_id: String,
    store_id: String,
}

/// Builds the scheduled-publish sub-router ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// `POST /admin/config/campaigns/schedule` snapshots the tenant's campaigns and schedules them to
/// publish to a store at a future instant (the Tết-menu case) — behind [`ConsolePermission::PublishConfig`],
/// the permission an immediate publish uses. `GET /admin/config/scheduled` lists a store's pending
/// publishes ([`ConsolePermission::Read`]); `DELETE /admin/config/scheduled/{id}` cancels one
/// (`PublishConfig`). A background activator applies them at their time.
pub fn scheduled_publish_router<Sch, Camp, A, C>(
    scheduled: Sch,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/campaigns/schedule",
            post(admin_schedule_campaigns::<Sch, Camp, A, C>),
        )
        .route(
            "/admin/config/scheduled",
            get(admin_list_scheduled::<Sch, Camp, A, C>),
        )
        .route(
            "/admin/config/scheduled/{id}",
            delete(admin_cancel_scheduled::<Sch, Camp, A, C>),
        )
        .with_state(ScheduledPublishState {
            scheduled,
            campaigns,
            admin,
            clock,
            audit,
        })
}

/// A super-admin schedules the tenant's campaigns to publish to a store at a future instant. The node
/// is snapshotted now (what is authored and reviewed), so later edits do not leak into the publish.
async fn admin_schedule_campaigns<Sch, Camp, A, C>(
    State(state): State<ScheduledPublishState<Sch, Camp, A, C>>,
    headers: HeaderMap,
    Json(request): Json<ScheduleCampaignsRequest>,
) -> Response
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    if request.effective_at_ms <= state.clock.now().as_milliseconds_since_epoch() {
        return (
            StatusCode::BAD_REQUEST,
            "effective_at_ms must be in the future",
        )
            .into_response();
    }
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => campaigns,
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(node_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node to schedule");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the campaign service is unavailable",
        )
            .into_response();
    };
    let Some(id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()) else {
        return campaign_entropy_unavailable();
    };
    let publish = NewScheduledPublish {
        id: id.to_string(),
        tenant_id,
        store_id,
        node_key: "campaigns".to_owned(),
        node_value,
        effective_at_ms: request.effective_at_ms,
        created_by: context.admin.id.clone(),
    };
    match state.scheduled.schedule(&publish).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.campaigns.schedule",
                "store",
                &store_id.to_string(),
                None,
                serde_json::to_value(serde_json::json!({
                    "campaigns": campaigns.len(),
                    "effective_at_ms": request.effective_at_ms,
                }))
                .ok(),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": publish.id,
                    "effective_at_ms": request.effective_at_ms,
                })),
            )
                .into_response()
        }
        Err(error) => scheduled_error_response(&error),
    }
}

/// A super-admin lists a store's pending scheduled publishes.
async fn admin_list_scheduled<Sch, Camp, A, C>(
    State(state): State<ScheduledPublishState<Sch, Camp, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ScheduledListQuery>,
) -> Response
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    let (Some(tenant_id), Some(store_id)) = (
        query.tenant_id.parse::<Ulid>().ok().map(TenantId::new),
        query.store_id.parse::<Ulid>().ok().map(StoreId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or store_id is not a ULID",
        )
            .into_response();
    };
    match state.scheduled.list_for_store(tenant_id, store_id).await {
        Ok(rows) => {
            let view: Vec<ScheduledPublishView> = rows
                .into_iter()
                .map(|row| ScheduledPublishView {
                    id: row.id,
                    node_key: row.node_key,
                    effective_at_ms: row.effective_at_ms,
                    status: row.status,
                    created_at_ms: row.created_at_ms,
                })
                .collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => scheduled_error_response(&error),
    }
}

/// A super-admin cancels a pending scheduled publish.
async fn admin_cancel_scheduled<Sch, Camp, A, C>(
    State(state): State<ScheduledPublishState<Sch, Camp, A, C>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    match state.scheduled.cancel(tenant_id, &id).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.schedule.cancel",
                "scheduled_publish",
                &id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such pending scheduled publish").into_response(),
        Err(error) => scheduled_error_response(&error),
    }
}

/// Maps a scheduled-publish store failure to a retryable `503`, logging the detail rather than leaking it.
fn scheduled_error_response(error: &ScheduledPublishError) -> Response {
    tracing::error!(%error, "a scheduled-publish store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the scheduling service is unavailable",
    )
        .into_response()
}

// --- Media (ADR-0075, Track M5): upload, serve, list, and delete image renditions -----------------

/// The largest upload the media route accepts before re-encoding. A generous cap on the *original*
/// bytes (the body limit rejects anything larger with `413`); the stored renditions are bounded far
/// smaller by the pipeline's ≤30 KB / ≤150 KB budgets.
const MAX_MEDIA_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
struct MediaState<M, A, C> {
    media: M,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// One media asset as listed — id, content type, the detail rendition's size, and when it was stored.
#[derive(Debug, Clone, serde::Serialize)]
struct MediaSummaryView {
    media_id: String,
    content_type: String,
    detail_bytes: u64,
    created_at_ms: i64,
}

/// The `POST /admin/media` response: the id the caller references the new asset by, and its size.
#[derive(Debug, Clone, serde::Serialize)]
struct UploadedMedia {
    media_id: String,
    detail_bytes: u64,
}

/// Builds the media sub-router ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5).
///
/// Four routes on the tenant's media library. `POST` uploads an image as a raw binary body (under an
/// 8 MB limit), re-encodes it through the ADR-0042 pipeline, and stores the two bounded renditions;
/// `GET /admin/media` lists summaries; `GET .../thumbnail` and `.../detail` stream one rendition as
/// `image/jpeg`; `DELETE` removes an asset. Upload and delete need [`ConsolePermission::ManageMedia`]
/// and are audited; reads and the two serve routes need only [`ConsolePermission::Read`]. The original
/// upload is never stored — only the renditions.
pub fn media_router<M, A, C>(media: M, admin: A, clock: C, audit: Arc<dyn AuditRecorder>) -> Router
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/media",
            get(admin_list_media::<M, A, C>).post(admin_upload_media::<M, A, C>),
        )
        .route(
            "/admin/media/{media_id}",
            delete(admin_delete_media::<M, A, C>),
        )
        .route(
            "/admin/media/{media_id}/thumbnail",
            get(admin_get_media_thumbnail::<M, A, C>),
        )
        .route(
            "/admin/media/{media_id}/detail",
            get(admin_get_media_detail::<M, A, C>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_MEDIA_UPLOAD_BYTES))
        .with_state(MediaState {
            media,
            admin,
            clock,
            audit,
        })
}

/// A super-admin lists a tenant's media assets (summaries, without the bytes).
async fn admin_list_media<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
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
    match state.media.list(tenant_id).await {
        Ok(rows) => {
            let view: Vec<MediaSummaryView> = rows
                .iter()
                .map(|row| MediaSummaryView {
                    media_id: row.media_id.to_string(),
                    content_type: row.content_type.clone(),
                    detail_bytes: u64::try_from(row.detail_bytes).unwrap_or(u64::MAX),
                    created_at_ms: row.created_at_ms,
                })
                .collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => media_error_response(&error),
    }
}

/// A super-admin uploads an image: it is re-encoded to two bounded JPEG renditions and stored. The raw
/// upload is never persisted. `?tenant_id=` names the owner.
async fn admin_upload_media<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    body: axum::body::Bytes,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageMedia,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let renditions = match images::render(&body) {
        Ok(renditions) => renditions,
        Err(ImagePipelineError::Decode(_)) => {
            return (
                StatusCode::BAD_REQUEST,
                "the upload is not a decodable image",
            )
                .into_response();
        }
        Err(ImagePipelineError::Budget { .. }) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "the image could not be reduced within the size budget",
            )
                .into_response();
        }
        Err(ImagePipelineError::Encode(_)) => {
            tracing::error!("encoding a media rendition failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not encode the image",
            )
                .into_response();
        }
    };
    let Some(media_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MediaId::new)
    else {
        return media_entropy_unavailable();
    };
    let detail_bytes = u64::try_from(renditions.detail.len()).unwrap_or(u64::MAX);
    let asset = NewMediaAsset {
        media_id,
        tenant_id,
        content_type: "image/jpeg".to_owned(),
        thumbnail: renditions.thumbnail,
        detail: renditions.detail,
    };
    match state.media.put(&asset).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "media.upload",
                "media_asset",
                &media_id.to_string(),
                None,
                Some(serde_json::json!({ "content_type": "image/jpeg", "detail_bytes": detail_bytes })),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(UploadedMedia {
                    media_id: media_id.to_string(),
                    detail_bytes,
                }),
            )
                .into_response()
        }
        Err(error) => media_error_response(&error),
    }
}

/// Streams a stored rendition as `image/jpeg`, or `404` if the tenant has no such asset.
async fn serve_media_rendition<M, A, C>(
    state: &MediaState<M, A, C>,
    headers: &HeaderMap,
    tenant: &str,
    media_id: &str,
    rendition: Rendition,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&state.admin, &state.clock, headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let Ok(tenant_id) = tenant.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Ok(media_id) = media_id.parse::<Ulid>().map(MediaId::new) else {
        return (StatusCode::BAD_REQUEST, "media id is not a ULID").into_response();
    };
    match state.media.get(tenant_id, media_id, rendition).await {
        Ok(Some(bytes)) => (
            [
                (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                // Renditions are immutable (content is replaced by a new id), so cache hard, privately.
                (
                    axum::http::header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such media asset").into_response(),
        Err(error) => media_error_response(&error),
    }
}

/// A super-admin (or any reader) fetches an asset's thumbnail rendition.
async fn admin_get_media_thumbnail<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Path(media_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    serve_media_rendition(
        &state,
        &headers,
        &query.tenant_id,
        &media_id,
        Rendition::Thumbnail,
    )
    .await
}

/// A super-admin (or any reader) fetches an asset's detail rendition.
async fn admin_get_media_detail<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Path(media_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    serve_media_rendition(
        &state,
        &headers,
        &query.tenant_id,
        &media_id,
        Rendition::Detail,
    )
    .await
}

/// A super-admin deletes a media asset.
async fn admin_delete_media<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Path(media_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageMedia,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let Ok(media_id) = media_id.parse::<Ulid>().map(MediaId::new) else {
        return (StatusCode::BAD_REQUEST, "media id is not a ULID").into_response();
    };
    match state.media.delete(tenant_id, media_id).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "media.delete",
                "media_asset",
                &media_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such media asset").into_response(),
        Err(error) => media_error_response(&error),
    }
}

/// The `503` a media-store failure becomes.
fn media_error_response(error: &MediaStoreError) -> Response {
    tracing::error!(%error, "a media store operation failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the media service is unavailable",
    )
        .into_response()
}

/// The `503` a failure to mint a media id becomes (OS entropy unavailable).
fn media_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a media id");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the media service is unavailable",
    )
        .into_response()
}

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

/// Builds the subject-request sub-router ([ADR-0076](../../../docs/adr/0076-subject-request-tooling.md),
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

/// Parses `?tenant_id=` and the `{subject_id}` path into their ids, or `None` if either is not a ULID.
/// The caller turns `None` into the `400`. Shared by the three subject routes.
fn parse_subject_target(tenant_id: &str, subject_id: &str) -> Option<(TenantId, SubjectId)> {
    let tenant = tenant_id.parse::<Ulid>().map(TenantId::new).ok()?;
    let subject = subject_id.parse::<Ulid>().map(SubjectId::new).ok()?;
    Some((tenant, subject))
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
    let Some((tenant, subject)) = parse_subject_target(&query.tenant_id, &subject_id) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or subject_id is not a ULID",
        )
            .into_response();
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
        Ok(None) => (StatusCode::NOT_FOUND, "no such subject for this tenant").into_response(),
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
    let Some((tenant, subject)) = parse_subject_target(&query.tenant_id, &subject_id) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or subject_id is not a ULID",
        )
            .into_response();
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
        Ok(None) => (StatusCode::NOT_FOUND, "no such subject for this tenant").into_response(),
        Err(error) => subject_error_response(&error),
    }
}

/// Erases a subject — masks every personal field value in place ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)),
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
    let Some((tenant, subject)) = parse_subject_target(&query.tenant_id, &subject_id) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id or subject_id is not a ULID",
        )
            .into_response();
    };
    let record = match state.subjects.fetch(tenant, subject).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "no such subject for this tenant").into_response();
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
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the subject service is unavailable",
    )
        .into_response()
}

// --- Countries & locales (read-only master data, ADR-0074, Track M4) ------------------------------

/// One compiled country module as the console reads it: the code, human name, currency, preferred
/// language, number format, and the default retention period. What the platform can serve — not a
/// per-store setting, and not fiscalization.
#[derive(Debug, Clone, serde::Serialize)]
struct CountryView {
    code: String,
    display_name: String,
    currency_code: String,
    default_language: String,
    decimal_separator: String,
    group_separator: String,
    digits_per_group: u8,
    default_retention_days: u16,
}

/// The state the country/locale reads share: the views computed once at start-up from the compiled
/// registry, plus the admin/clock the permission check needs.
#[derive(Clone)]
struct CountryState<A, C> {
    countries: Arc<Vec<CountryView>>,
    locales: Arc<Vec<String>>,
    admin: A,
    clock: C,
}

/// Builds the countries/locales sub-router ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// `GET /admin/countries` lists the compiled country modules; `GET /admin/locales` lists the content
/// locales the platform can serve (each module's preferred language plus the enforced `en` fallback,
/// which feeds the translation grid's column set). Both are read-only master data behind
/// [`ConsolePermission::Read`], computed once from the registry at start-up.
pub fn country_router<A, C>(registry: &CountryRegistry, admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let mut countries = Vec::new();
    // `en` is always present as the fallback (docs/pos-spec.md §9); a module's preferred language adds
    // to the catalogue.
    let mut locales: BTreeSet<String> = BTreeSet::new();
    locales.insert("en".to_owned());
    for module in registry.modules() {
        let pack = module.locale_pack();
        locales.insert(pack.default_language.as_str().to_owned());
        countries.push(CountryView {
            code: module.country_code().as_str().to_owned(),
            display_name: module.display_name().to_owned(),
            currency_code: pack.currency_code.as_str().to_owned(),
            default_language: pack.default_language.as_str().to_owned(),
            decimal_separator: pack.number_format.decimal_separator.to_string(),
            group_separator: pack.number_format.group_separator.to_string(),
            digits_per_group: pack.number_format.digits_per_group,
            default_retention_days: pack.default_retention_days,
        });
    }
    Router::new()
        .route("/admin/countries", get(admin_list_countries::<A, C>))
        .route("/admin/locales", get(admin_list_locales::<A, C>))
        .with_state(CountryState {
            countries: Arc::new(countries),
            locales: Arc::new(locales.into_iter().collect()),
            admin,
            clock,
        })
}

/// A super-admin lists the compiled country modules.
async fn admin_list_countries<A, C>(
    State(state): State<CountryState<A, C>>,
    headers: HeaderMap,
) -> Response
where
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
    (StatusCode::OK, Json(state.countries.as_ref().clone())).into_response()
}

/// A super-admin lists the content locales the platform can serve.
async fn admin_list_locales<A, C>(
    State(state): State<CountryState<A, C>>,
    headers: HeaderMap,
) -> Response
where
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
    (StatusCode::OK, Json(state.locales.as_ref().clone())).into_response()
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

/// Parses an optional media id (an item's/brand's `image_ref`, ADR-0075), with the same
/// empty-is-`None` rule as [`parse_optional_category`].
fn parse_optional_media(value: Option<&str>) -> Result<Option<MediaId>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(MediaId::new(ulid)))
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

/// A CSV download response ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5): the
/// bytes with `text/csv` and a `content-disposition` naming the file so the browser saves it. `filename`
/// is a fixed, server-chosen literal per domain — never tenant-supplied — so it needs no escaping.
fn csv_download_response(filename: &str, body: Vec<u8>) -> Response {
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/csv; charset=utf-8".to_owned(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

/// A super-admin exports a tenant's catalog items as CSV ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5). Behind [`ConsolePermission::ManageCatalog`] and audited — the entry records who exported
/// which domain and how many rows, never the row contents. No price is exported (prices are per-channel
/// placements, a deferred T2 export); this is the item master only.
async fn admin_export_items<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let items = match state.catalog.list_items(tenant_id).await {
        Ok(items) => items,
        Err(error) => return catalog_error_response(&error),
    };
    let Ok(body) = export::items_csv(&items) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not build the CSV").into_response();
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "catalog.export_items",
        "catalog_export",
        &tenant_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": "items", "rows": items.len() })),
    )
    .await;
    csv_download_response("items.csv", body)
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let Ok(image_ref) = parse_optional_media(request.image_ref.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "image_ref is not a ULID").into_response();
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
        name_translations: clean_name_translations(request.name_translations),
        tax_class_id,
        item_category_id,
        item_subcategory_id,
        image_ref,
        status: EntityStatus::Active,
    };
    match state.catalog.create_item(&record).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_item.create",
                "menu_item",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let Ok(image_ref) = parse_optional_media(request.image_ref.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "image_ref is not a ULID").into_response();
    };
    let record = CatalogItem {
        menu_item_id,
        tenant_id,
        name: request.name,
        name_translations: clean_name_translations(request.name_translations),
        tax_class_id,
        item_category_id,
        item_subcategory_id,
        image_ref,
        status,
    };
    match state.catalog.update_item(&record).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_item.update",
                "menu_item",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "tax_class.create",
                "tax_class",
                &record.tax_class_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "tax_class.update",
                "tax_class",
                &record.tax_class_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_category.create",
                "item_category",
                &record.item_category_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_category.update",
                "item_category",
                &record.item_category_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_subcategory.create",
                "item_subcategory",
                &record.item_subcategory_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_subcategory.update",
                "item_subcategory",
                &record.item_subcategory_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_category.create",
                "display_category",
                &record.display_category_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_category.update",
                "display_category",
                &record.display_category_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_subcategory.create",
                "display_subcategory",
                &record.display_subcategory_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_subcategory.update",
                "display_subcategory",
                &record.display_subcategory_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            // A layout button's identity is (tenant, channel, item); the item id is its entity key,
            // and the full row (channel, category, position) is recorded as `after`.
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "layout_button.set",
                "layout_button",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "layout_button.remove",
                "layout_button",
                &menu_item_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "modifier_group.create",
                "modifier_group",
                &record.modifier_group_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "modifier_group.update",
                "modifier_group",
                &record.modifier_group_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu.create",
                "menu",
                &record.menu_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu.update",
                "menu",
                &record.menu_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_section.create",
                "menu_section",
                &record.menu_section_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_section.update",
                "menu_section",
                &record.menu_section_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            // A placement's identity is the (menu, item) pair; its per-channel prices are the
            // price-change journal (ADR-0069, G2), recorded as `after`.
            let entity_id = format!("{}/{}", record.menu_id, record.menu_item_id);
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "placement.set",
                "menu_placement",
                &entity_id,
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            (StatusCode::OK, Json(record)).into_response()
        }
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            let entity_id = format!("{menu_id}/{menu_item_id}");
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "placement.remove",
                "menu_placement",
                &entity_id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
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
    audit: Arc<dyn AuditRecorder>,
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
    audit: Arc<dyn AuditRecorder>,
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
            audit,
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
            // Records that a menu was compiled and published to a store — the menu compiled and the
            // config version it produced, keyed to the store. The compiled book itself rides the
            // config tree; the config version history (a later G2 slice) is where it is diffed.
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "catalog.publish",
                "store",
                &store_id.to_string(),
                None,
                Some(serde_json::json!({
                    "menu_id": menu_id.to_string(),
                    "config_version_id": id.to_string(),
                })),
            )
            .await;
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

// --- People publish (`/admin/people/publish`, ADR-0070 slice 5) ---------------------------------

/// The collaborators the people-publish route needs: the people store (the employee, role, and
/// assignment seams plus the trusted PIN-hash read), the config-tree store the compiled node is written
/// onto, and the admin/clock/audit every write carries. Separate from [`PeopleState`] because
/// publishing also needs the config-tree seam, exactly as catalog publish ([`CatalogPublishState`]) is
/// separate from catalog authoring.
#[derive(Clone)]
struct PeoplePublishState<P, Cfg, A, C> {
    people: P,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the people-publish sub-router ([ADR-0070](../../../docs/adr/0070-people-and-access.md) slice 5).
///
/// One route: compile a store's people + roles + assignments into the edge-shaped `permissions`
/// document and write it onto the store's `permissions` config node, versioned through the config tree
/// like every other publish. Behind [`ConsolePermission::PublishConfig`], the same gate as catalog and
/// config publish.
pub fn people_publish_router<P, Cfg, A, C>(
    people: P,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/people/publish",
            post(admin_publish_permissions::<P, Cfg, A, C>),
        )
        .with_state(PeoplePublishState {
            people,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// A super-admin selects the (tenant, store) whose `permissions` node to compile and publish.
#[derive(Debug, Clone, Deserialize)]
struct PublishPermissionsRequest {
    tenant_id: String,
    store_id: String,
}

/// Compiles a store's people into its `permissions` config node and versions it through the config
/// tree — the same load→compile→write→version shape as [`admin_publish_menu`], onto the `permissions`
/// key instead of `menu`/`layout`. The PIN hash rides in the node (the edge verifies against it,
/// ADR-0030); the audit records only the config version and staff count, never a name or PIN.
#[expect(
    clippy::too_many_lines,
    reason = "one publish is a single linear transaction — load assignments/employees/roles + each \
              assigned employee's PIN hash, compile the document, set the `permissions` node on the \
              Store layer and version it; splitting the load-compile-write flow would scatter the \
              config-tree state the final publish needs"
)]
async fn admin_publish_permissions<P, Cfg, A, C>(
    State(state): State<PeoplePublishState<P, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishPermissionsRequest>,
) -> Response
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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

    // Load the domain the compiler needs. `list` is on two of P's traits, so the calls are
    // fully-qualified to say which seam each one is.
    let assignments =
        match AssignmentStore::list_for_store(&state.people, tenant_id, store_id).await {
            Ok(assignments) => assignments,
            Err(error) => return people_error_response(&error),
        };
    let employees = match EmployeeStore::list(&state.people, tenant_id).await {
        Ok(employees) => employees,
        Err(error) => return people_error_response(&error),
    };
    let roles = match RoleTemplateStore::list(&state.people, tenant_id).await {
        Ok(roles) => roles,
        Err(error) => return people_error_response(&error),
    };
    // The stored PIN hash for each assigned employee, read only here (the trusted publish path) and
    // never returned over the API. Deduplicated: a store assigns a person at most once, but reading
    // by a set keeps it robust.
    let mut pins = std::collections::BTreeMap::new();
    for assignment in &assignments {
        if pins.contains_key(&assignment.employee_id.to_string()) {
            continue;
        }
        let hash =
            match EmployeeStore::pin_phc(&state.people, tenant_id, assignment.employee_id).await {
                Ok(hash) => hash,
                Err(error) => return people_error_response(&error),
            };
        pins.insert(assignment.employee_id.to_string(), hash);
    }

    let document = compile_permissions(store_id, &employees, &roles, &assignments, &pins);
    let staff_count = document.staff.len();
    let Ok(document_value) = serde_json::to_value(&document) else {
        tracing::error!("could not serialise a compiled permissions document");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the people service is unavailable",
        )
            .into_response();
    };

    // Set the `permissions` key on the store's Store layer (index 2 in Tenant→Brand→Store→Device) and
    // re-publish that layer, preserving any other Store-level keys (`menu`, `layout`, …).
    let state_before = match state.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert("permissions".to_owned(), document_value);
    } else {
        store_layer = serde_json::json!({ "permissions": document_value });
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
            // The trail records the config version and how many staff the node carries — never a name
            // or a PIN (ADR-0070).
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "permissions.publish",
                "store",
                &store_id.to_string(),
                None,
                Some(serde_json::json!({
                    "config_version_id": id.to_string(),
                    "staff_count": staff_count,
                })),
            )
            .await;
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
    audit: Arc<dyn AuditRecorder>,
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
pub fn translation_router<Tr, A, C>(
    translations: Tr,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
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
        .route(
            "/admin/translations/export",
            get(export_translations::<Tr, A, C>),
        )
        .route(
            "/admin/translations/import/dry-run",
            post(import_translations_dry_run::<Tr, A, C>),
        )
        .route(
            "/admin/translations/import/apply",
            post(import_translations_apply::<Tr, A, C>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_CSV_IMPORT_BYTES))
        .with_state(TranslationState {
            translations,
            admin,
            clock,
            audit,
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
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(()) => {
            // A whole-grid replace: the entity is the tenant's grid, keyed by the tenant id. The grid
            // is business copy (menu/UI strings), not personal data, so it is recorded as `after`.
            let after = serde_json::to_value(&grid).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "translations.save",
                "translation_grid",
                &tenant_id.to_string(),
                None,
                after,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => translation_error_response(&error),
    }
}

/// A super-admin exports a tenant's translation grid as CSV ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5). Behind [`ConsolePermission::ManageTranslations`] and audited (who exported which domain
/// and how many rows, never the row contents). The grid is business copy (menu/UI strings), not
/// personal data.
async fn export_translations<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let grid = match state.translations.load(tenant_id).await {
        Ok(grid) => grid.unwrap_or_default(),
        Err(error) => return translation_error_response(&error),
    };
    let Ok(body) = export::translations_csv(&grid) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not build the CSV").into_response();
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "translations.export",
        "translation_export",
        &tenant_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": "translations", "rows": grid.as_map().len() })),
    )
    .await;
    csv_download_response("translations.csv", body)
}

/// The hard cap on a CSV import body ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md)): a
/// translation grid is small copy, so 4 MB is generous while bounding the parse.
const MAX_CSV_IMPORT_BYTES: usize = 4 * 1024 * 1024;

/// A super-admin dry-runs a translation-grid CSV import ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5): the server parses and classifies every row (would-create / would-update / rejected) and
/// returns the report, **writing nothing**. Behind [`ConsolePermission::ManageTranslations`]; not
/// audited (it changes nothing). The confirm step (`.../apply`) does the write.
async fn import_translations_dry_run<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
    body: axum::body::Bytes,
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
    let existing = match state.translations.load(tenant_id).await {
        Ok(grid) => grid.unwrap_or_default(),
        Err(error) => return translation_error_response(&error),
    };
    match import::parse_translations_csv(&body, &existing) {
        Ok((_, report)) => (StatusCode::OK, Json(report)).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

/// A super-admin applies a translation-grid CSV import ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5) — the confirm after a dry-run. The valid rows are merged onto the tenant's grid (existing
/// keys not in the file are preserved; rejected rows are skipped) and saved; rejected rows are reported,
/// never written. Behind [`ConsolePermission::ManageTranslations`] and audited (the row counts, never
/// the contents). The merged grid still satisfies the `en`-fallback rule by construction.
async fn import_translations_apply<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
    body: axum::body::Bytes,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(tenant_id) = query.tenant_id.parse::<Ulid>().map(TenantId::new) else {
        return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
    };
    let existing = match state.translations.load(tenant_id).await {
        Ok(grid) => grid.unwrap_or_default(),
        Err(error) => return translation_error_response(&error),
    };
    let (merged, report) = match import::parse_translations_csv(&body, &existing) {
        Ok(parsed) => parsed,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    match state.translations.save(tenant_id, &merged).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "translations.import",
                "translation_grid",
                &tenant_id.to_string(),
                None,
                Some(serde_json::json!({
                    "created": report.create_count,
                    "updated": report.update_count,
                    "rejected": report.reject_count,
                })),
            )
            .await;
            (StatusCode::OK, Json(report)).into_response()
        }
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
    Query(window): Query<RollupWindowQuery>,
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
    let window = match window.into_window() {
        Ok(window) => window,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    // The tenant is the grant's, not the request's — this is the isolation boundary.
    match dashboard(&app.rollups, grant.tenant(), store_id, &window).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// The date-range window params shared by the rollup read routes (ADR-0081, Track O4): an inclusive
/// `from`/`to` business-date range and a `limit` cap. All optional — absent gives the default
/// [`crate::dashboard::rollup::DEFAULT_WINDOW_DAYS`]-day window, never the store's whole history.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RollupWindowQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

impl RollupWindowQuery {
    /// Validates the params into a [`RollupWindow`], or returns a `400` message.
    fn into_window(self) -> Result<RollupWindow, &'static str> {
        RollupWindow::new(self.from, self.to, self.limit)
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
    // Record the store's contact and the version it reported holding, for the fleet view
    // ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)). Best-effort telemetry: a liveness-write
    // failure is logged and swallowed, never failing the config pull the store actually needs.
    if let Err(error) = app
        .config_trees
        .record_store_seen(grant.tenant(), store_id, held, app.clock.now())
        .await
    {
        tracing::warn!(%error, "recording store liveness on a config pull failed");
    }
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

/// `POST /sync/stores/{store_id}/heartbeat` — a store's lightweight liveness ping
/// ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 2). Authenticated with the same scoped
/// API key the config pull uses (`read_config`), it advances the store's `last_seen_at` and nothing
/// else, so a store that is up but not currently pulling config (a parked long-poll, a quiet period
/// between publishes) still registers as online. `204` on success; unlike the config-pull capture,
/// recording is this request's whole purpose, so a store-write failure is a `503` the edge retries,
/// not a swallowed best-effort write.
async fn edge_heartbeat<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
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
    // The tenant is the grant's, not the path's — a store reaches only its own tenant's liveness row.
    match app
        .config_trees
        .record_store_heartbeat(grant.tenant(), store_id, app.clock.now())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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

/// How many proxies in front of this process are trusted to have appended to `X-Forwarded-For`,
/// when the configuration does not say.
///
/// One: the Caddy that terminates TLS ([ADR-0044](../../../docs/adr/0044-fork-and-deploy.md)), which
/// is every TLS posture except `TLS_MODE=external`. A deployment whose own load balancer or ingress
/// terminates upstream has two, and sets
/// [`crate::config::CloudConfig::trusted_proxy_hops`] accordingly —
/// [ADR-0090](../../../docs/adr/0090-tls-postures.md) derives it from the posture in `bootstrap.sh`
/// so it is not a value anyone has to remember. Under-counting is the safe direction to be wrong in
/// (the client can choose its bucket); over-counting collapses everyone behind one proxy into a
/// single bucket. Neither is acceptable, which is why this is configuration and not a guess.
const DEFAULT_TRUSTED_PROXY_HOPS: usize = 1;

/// The client IP for an admin request, taken from the **trusted** tail of `X-Forwarded-For`.
///
/// # Why the tail, and not the head
///
/// `X-Forwarded-For` is an append-only chain, and Caddy's `reverse_proxy` appends rather than
/// replaces. So a request that arrives carrying its own `X-Forwarded-For: 203.0.113.9` reaches this
/// process as `203.0.113.9, <the address Caddy actually saw>`. Everything to the left of the hops we
/// put there ourselves is **attacker-supplied text**, not an address.
///
/// This function used to return the *first* hop, and its own documentation said the value was "never
/// trusted for authorization" — true when it was written, because it only decorated the session list.
/// It stopped being true when [`admin_login`] began using it as the sole sliding-window rate-limit
/// key: a guesser could then send a fresh fake header per attempt, land in a fresh bucket every time,
/// and never meet the throttle at all. Counting from the right fixes that, because the rightmost
/// `trusted_hops` entries are the only ones this deployment wrote.
///
/// # Why the count is a parameter
///
/// It is a property of the *deployment*, not of the code: one hop behind the bundled Caddy, two when
/// a load balancer terminates TLS in front of it. It arrives from
/// [`crate::config::CloudConfig::trusted_proxy_hops`], which `bootstrap.sh` derives from `TLS_MODE`
/// ([ADR-0090](../../../docs/adr/0090-tls-postures.md)). A value of `0` is refused at config load;
/// were it to reach here it would resolve to `None` for every request, which over-throttles rather
/// than exempts.
///
/// `None` when the header is absent, which is what a request that did not come through the proxy
/// looks like. Callers key that as one shared bucket — over-throttling a direct caller rather than
/// exempting it, so an absent header cannot be a way through either.
///
/// `X-Real-IP` is deliberately **not** consulted as a fallback: it is a single client-settable value
/// with no chain to count back along, so honouring it would reopen exactly the hole this closes.
fn client_ip(headers: &HeaderMap, trusted_hops: usize) -> Option<&str> {
    let chain = header_str(headers, "x-forwarded-for")?;
    let hops: Vec<&str> = chain.split(',').map(str::trim).collect();
    // Take the leftmost hop this deployment is responsible for: with one trusted proxy that is the
    // last entry, the address Caddy itself observed. `saturating_sub` keeps a short chain (a proxy
    // that sent no chain, or a hop count larger than the chain) at the first entry rather than
    // panicking — the conservative end, since the first entry of a too-short chain is the only
    // address there is.
    hops.get(hops.len().saturating_sub(trusted_hops))
        .copied()
        .filter(|ip| !ip.is_empty())
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

// --- Console audit read (`GET /admin/audit`, ADR-0069 slice 4) ----------------------------------

/// The collaborators the audit read needs, stated independently of [`CloudApp`]: the concrete audit
/// store (the [`AuditRecorder`] the write routes carry exposes only `record`, so the read carries the
/// store itself), plus the admin and clock the session guard uses.
#[derive(Clone)]
struct AuditReadState<Au, A, C> {
    audit: Au,
    admin: A,
    clock: C,
}

/// The default and maximum number of audit rows one read returns.
const AUDIT_READ_DEFAULT_LIMIT: u32 = 200;
const AUDIT_READ_MAX_LIMIT: u32 = 500;

/// The filters the Audit screen names on the query string. Every field is optional; an absent field
/// does not filter. `tenant_id` absent is the fleet-wide read (every tenant, including tenant-global
/// entries); present scopes to one tenant.
#[derive(Debug, Clone, Deserialize)]
struct AuditReadQuery {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    entity_type: Option<String>,
    #[serde(default)]
    entity_id: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    actor_admin_id: Option<String>,
    #[serde(default)]
    since_ms: Option<i64>,
    #[serde(default)]
    until_ms: Option<i64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// One audit entry as the console reads it: ids as strings (the screen shows names/labels, not raw
/// ULIDs front-and-centre), the actor snapshot flattened, and the instant as Unix ms.
#[derive(Debug, Clone, serde::Serialize)]
struct AuditEntryView {
    id: String,
    tenant_id: Option<String>,
    actor_admin_id: String,
    actor_email: String,
    actor_role: String,
    action: String,
    entity_type: String,
    entity_id: String,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
    request_id: Option<String>,
    at_ms: i64,
}

impl AuditEntryView {
    fn from_entry(entry: AuditEntry) -> Self {
        Self {
            id: entry.id.to_string(),
            tenant_id: entry.tenant_id.map(|tenant| tenant.to_string()),
            actor_admin_id: entry.actor.admin_id,
            actor_email: entry.actor.email,
            actor_role: entry.actor.role.as_token().to_owned(),
            action: entry.action,
            entity_type: entry.entity_type,
            entity_id: entry.entity_id,
            before: entry.before,
            after: entry.after,
            request_id: entry.request_id,
            at_ms: entry.at.as_milliseconds_since_epoch(),
        }
    }
}

/// Builds the audit-read sub-router ([ADR-0069](../../../docs/adr/0069-audit-trail.md) slice 4).
///
/// One read, `GET /admin/audit`, behind [`ConsolePermission::Read`] (every console role may read the
/// trail). It names its tenant the admin-is-global way — a `?tenant_id=` query
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)); absent, it is the fleet-wide
/// read. Like the other reads it carries its own state and is merged into the main router.
pub fn audit_router<Au, A, C>(audit: Au, admin: A, clock: C) -> Router
where
    Au: AuditStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/audit", get(admin_list_audit::<Au, A, C>))
        .with_state(AuditReadState {
            audit,
            admin,
            clock,
        })
}

/// A super-admin (any console role) reads the audit trail, filtered by the query string.
async fn admin_list_audit<Au, A, C>(
    State(state): State<AuditReadState<Au, A, C>>,
    headers: HeaderMap,
    Query(query): Query<AuditReadQuery>,
) -> Response
where
    Au: AuditStore + Clone + Send + Sync + 'static,
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
    let tenant = match query.tenant_id.as_deref() {
        Some(text) => match text.parse::<Ulid>().map(TenantId::new) {
            Ok(tenant) => Some(tenant),
            Err(_ignored) => {
                return (StatusCode::BAD_REQUEST, "tenant_id is not a ULID").into_response();
            }
        },
        None => None,
    };
    let limit = query
        .limit
        .unwrap_or(AUDIT_READ_DEFAULT_LIMIT)
        .clamp(1, AUDIT_READ_MAX_LIMIT);
    let filter = crate::audit::AuditQuery {
        tenant,
        entity_type: query.entity_type,
        entity_id: query.entity_id,
        action: query.action,
        actor_admin_id: query.actor_admin_id,
        since_ms: query.since_ms,
        until_ms: query.until_ms,
        limit,
    };
    match state.audit.query(&filter).await {
        Ok(entries) => {
            let view: Vec<AuditEntryView> = entries
                .into_iter()
                .map(AuditEntryView::from_entry)
                .collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "an audit read failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the audit service is unavailable",
            )
                .into_response()
        }
    }
}

/// Records one console mutation to the audit trail ([ADR-0069](../../../docs/adr/0069-audit-trail.md)),
/// best-effort and after the mutation has already succeeded: mints the entry id from `clock`, snapshots
/// the acting admin from `actor`, and hands the entry to the recorder (which logs, never propagates, a
/// store failure). A mint failure is logged and the entry dropped rather than failing the caller's
/// completed mutation. `before` is the entity's prior value (`None` for a create), `after` its new
/// value (`None` for a delete); `tenant` scopes the entry (`None` for a tenant-global action).
#[expect(
    clippy::too_many_arguments,
    reason = "one audit entry is genuinely this many independent facts — recorder, clock, actor, \
              tenant scope, action, entity type+id, and the before/after values; bundling them into \
              a struct at every one of the many call sites would only add ceremony, not clarity"
)]
async fn audit_action<C>(
    audit: &Arc<dyn AuditRecorder>,
    clock: &C,
    actor: &AdminContext,
    tenant: Option<TenantId>,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) where
    C: ClockSource,
{
    let now = clock.now();
    let Some(ulid) = mint_ulid(now.as_milliseconds_since_epoch()) else {
        tracing::error!(
            action,
            "could not mint an audit id; this action is not recorded"
        );
        return;
    };
    let entry = AuditEntry {
        id: AuditId::new(ulid),
        tenant_id: tenant,
        actor: AuditActor {
            admin_id: actor.admin.id.clone(),
            email: actor.admin.email.clone(),
            role: actor.admin.role,
        },
        action: action.to_owned(),
        entity_type: entity_type.to_owned(),
        entity_id: entity_id.to_owned(),
        before,
        after,
        request_id: None,
        at: now,
    };
    audit.record(entry).await;
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageApiKeys,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    // Records the grant, never the secret: the scopes and expiry are the auditable fact; the token
    // is shown once to the caller and never written to the trail.
    let after = serde_json::json!({
        "scopes": request.scopes,
        "expires_at_ms": request.expires_at_ms,
    });
    audit_action(
        &app.audit,
        &app.clock,
        &context,
        Some(tenant_id),
        "api_key.create",
        "api_key",
        &id.to_string(),
        None,
        Some(after),
    )
    .await;
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageApiKeys,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Ok(id) = id.parse::<Ulid>().map(ApiKeyId::new) else {
        return (StatusCode::BAD_REQUEST, "the key id is not a ULID").into_response();
    };
    match app.keys.revoke(id).await {
        Ok(found) => {
            // Only a revoke that actually retired a live key is worth a trail entry; revoking an
            // already-gone id is a no-op `204` and records nothing. The revoke seam is by-id only, so
            // the entry has no tenant scope (a fleet-wide read still surfaces it).
            if found {
                audit_action(
                    &app.audit,
                    &app.clock,
                    &context,
                    None,
                    "api_key.revoke",
                    "api_key",
                    &id.to_string(),
                    None,
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
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
        invited_by: context.admin.id.clone(),
        expires_at,
    };
    match app.admin.create_invite(invite).await {
        Ok(()) => {
            // The role granted is the auditable fact; the invite email and single-use token are not
            // written to the trail. Admin management is tenant-global, so the entry carries no tenant.
            audit_action(
                &app.audit,
                &app.clock,
                &context,
                None,
                "admin.invite",
                "admin_invite",
                &id.to_string(),
                None,
                Some(serde_json::json!({ "role": role.as_token() })),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(InviteAdminResponse {
                    invite_id: id.to_string(),
                    token,
                    expires_at_ms: expires_at.as_milliseconds_since_epoch(),
                }),
            )
                .into_response()
        }
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
    match app.admin.revoke_invite(&id).await {
        Ok(removed) => {
            if removed {
                audit_action(
                    &app.audit,
                    &app.clock,
                    &context,
                    None,
                    "admin.invite_revoke",
                    "admin_invite",
                    &id,
                    None,
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageAdmins,
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
    if let Err(response) = guard_last_owner_change(&app.admin, &id, role != AdminRole::Owner).await
    {
        return response;
    }
    match app.admin.set_admin_user_role(&id, role).await {
        Ok(true) => {
            audit_action(
                &app.audit,
                &app.clock,
                &context,
                None,
                "admin.role_set",
                "admin",
                &id,
                None,
                Some(serde_json::json!({ "role": role.as_token() })),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageAdmins,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
        Ok(true) => {
            audit_action(
                &app.audit,
                &app.clock,
                &context,
                None,
                "admin.status_set",
                "admin",
                &id,
                None,
                Some(serde_json::json!({ "status": status.as_token() })),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
            // Records that a new version was published and at which level — not the config document
            // itself; the full old→new history is the config version list (a later G2 slice).
            let level_token = match level {
                ConfigLevel::Tenant => "tenant",
                ConfigLevel::Brand => "brand",
                ConfigLevel::Store => "store",
                ConfigLevel::Device => "device",
            };
            audit_action(
                &app.audit,
                &app.clock,
                &context,
                Some(tenant_id),
                "config.publish",
                "config",
                &store_id.to_string(),
                None,
                Some(serde_json::json!({
                    "level": level_token,
                    "config_version_id": id.to_string(),
                })),
            )
            .await;
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

// --- Config version history (`/admin/stores/{id}/config/versions` + `/config/rollback`, ADR-0069) --

/// One published config version as the console lists it: the version id, when it was published (from
/// the ULID's own timestamp, Unix ms), and whether it is the store's current version.
#[derive(Debug, Clone, serde::Serialize)]
struct ConfigVersionView {
    version_id: String,
    at_ms: i64,
    current: bool,
}

/// A super-admin rolls a store's config back to a past version.
#[derive(Debug, Clone, Deserialize)]
struct RollbackConfigRequest {
    /// The version to restore (a ULID); its effective document becomes the new current version.
    version_id: String,
}

/// Lists a store's published config versions newest-first (super-admin only). The version history is
/// the config tree's own append-only log; the `at` of each is read from its ULID id, so no separate
/// timestamp column is needed. `404` if the store has no tree yet.
async fn admin_config_versions<S, R, K, C, A, T, W>(
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
            let current = tree.current_version();
            let mut views: Vec<ConfigVersionView> = tree
                .version_ids()
                .into_iter()
                .map(|id| ConfigVersionView {
                    version_id: id.to_string(),
                    at_ms: i64::try_from(id.as_ulid().timestamp_ms()).unwrap_or(i64::MAX),
                    current: current == Some(id),
                })
                .collect();
            views.reverse(); // history is oldest-first; the console reads newest-first.
            (StatusCode::OK, Json(views)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            "the store has no published configuration",
        )
            .into_response(),
        Err(error) => config_store_error_response(&error),
    }
}

/// The effective (composed, validated) document of one past config version (super-admin only), for
/// the console's diff view. `404` if the store or the named version is unknown.
async fn admin_config_version_effective<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path((store_id, version_id)): Path<(String, String)>,
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
    let (Ok(tenant_id), Ok(store_id), Ok(version_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
        version_id.parse::<Ulid>().map(ConfigVersionId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, store_id, or the version id is not a ULID",
        )
            .into_response();
    };
    match app.config_trees.load(tenant_id, store_id).await {
        Ok(Some(state)) => {
            let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
            match tree.effective_at(version_id) {
                Some(effective) => (StatusCode::OK, Json(effective.clone())).into_response(),
                None => (StatusCode::NOT_FOUND, "no such config version").into_response(),
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

/// Rolls a store's config back to a past version (super-admin only). Append-only: the chosen version's
/// effective document is re-published as a *new* current version, so nothing in the history is altered
/// or removed and the store pulls the restored config on its next sync. The action is audited as
/// `config.rollback` ([ADR-0069](../../../docs/adr/0069-audit-trail.md)). `404` if the store or the
/// named version is unknown.
async fn admin_config_rollback<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigTenantQuery>,
    Json(request): Json<RollbackConfigRequest>,
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (Ok(tenant_id), Ok(store_id), Ok(version_id)) = (
        query.tenant_id.parse::<Ulid>().map(TenantId::new),
        store_id.parse::<Ulid>().map(StoreId::new),
        request.version_id.parse::<Ulid>().map(ConfigVersionId::new),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "tenant_id, store_id, or the version id is not a ULID",
        )
            .into_response();
    };
    let Some(state) = (match app.config_trees.load(tenant_id, store_id).await {
        Ok(state) => state,
        Err(error) => return config_store_error_response(&error),
    }) else {
        return (
            StatusCode::NOT_FOUND,
            "the store has no published configuration",
        )
            .into_response();
    };
    let mut tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
    let Some(new_version_id) = mint_version_id(app.clock.now().as_milliseconds_since_epoch())
    else {
        tracing::error!("could not read OS entropy to mint a config version id");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the configuration service is unavailable",
        )
            .into_response();
    };
    let Some(new_id) = tree.restore(version_id, new_version_id) else {
        return (StatusCode::NOT_FOUND, "no such config version").into_response();
    };
    if let Err(error) = app
        .config_trees
        .save(tenant_id, store_id, &tree.state())
        .await
    {
        return config_store_error_response(&error);
    }
    audit_action(
        &app.audit,
        &app.clock,
        &context,
        Some(tenant_id),
        "config.rollback",
        "config",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({
            "restored_from": version_id.to_string(),
            "config_version_id": new_id.to_string(),
        })),
    )
    .await;
    (
        StatusCode::OK,
        Json(PublishedConfig {
            config_version_id: new_id.to_string(),
        }),
    )
        .into_response()
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
    Query(query): Query<AdminRollupWindowQuery>,
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
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    // The tenant is named explicitly here (the admin is global), unlike the `/v1` read where it is
    // the API key's. It is a read of the materialised rollup — event counts only, no PII.
    match dashboard(&app.rollups, tenant_id, store_id, &window).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// The `/admin` daily-rollup query: the explicit `tenant_id` (the console admin is global) plus the
/// shared [`RollupWindowQuery`] date range and cap (ADR-0081, Track O4).
#[derive(Debug, Clone, Deserialize)]
struct AdminRollupWindowQuery {
    tenant_id: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// The materialised **revenue** rollup for one store, windowed exactly as [`admin_daily_rollups`].
///
/// Prices are **T2** (ADR-0081), so this is gated behind `console.reports.revenue` (Owner/Admin) —
/// narrower than the counts read's `console.data.read`. The rollup carries no customer or employee
/// identifier; the tenant is named explicitly (the console admin is global).
async fn admin_revenue_daily<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
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
        ConsolePermission::ReadRevenue,
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
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match revenue(&app.rollups, tenant_id, store_id, &window).await {
        Ok(revenue) => (StatusCode::OK, Json(revenue)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// An **X or Z report** for one store's trading day (ADR-0081, resolving spec gap D10). `business_date`
/// is optional — absent, the current (latest) day is reported as an X; a past day reads back as a Z.
/// T2 (it bundles revenue and cash), so gated behind `console.reports.revenue`.
async fn admin_xz_report<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<XzReportQuery>,
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
        ConsolePermission::ReadRevenue,
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
    match xz_report(&app.rollups, tenant_id, store_id, query.business_date).await {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// The X/Z report query: the explicit `tenant_id` plus an optional `business_date` (`YYYY-MM-DD`);
/// absent, the current day is reported.
#[derive(Debug, Clone, Deserialize)]
struct XzReportQuery {
    tenant_id: String,
    #[serde(default)]
    business_date: Option<String>,
}

/// Exports a store's daily activity rollups (counts) as a CSV download, windowed like the read
/// (ADR-0081, Track O4; reuses the ADR-0075 rail). Counts only, so `console.data.read`; audited by
/// row count.
async fn admin_export_rollups<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
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
    let context =
        match require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await {
            Ok(context) => context,
            Err(denied) => return denied,
        };
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
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let days = match dashboard(&app.rollups, tenant_id, store_id, &window).await {
        Ok(days) => days,
        Err(error) => return rollup_error_response(&error),
    };
    let Ok(body) = export::rollups_csv(&days) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not build the CSV").into_response();
    };
    audit_action(
        &app.audit,
        &app.clock,
        &context,
        Some(tenant_id),
        "reports.export_rollups",
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": "rollups", "rows": days.len() })),
    )
    .await;
    csv_download_response("rollups.csv", body)
}

/// Exports a store's daily revenue rollups as a CSV download, windowed like the read (ADR-0081, Track
/// O4). Prices are **T2**, so `console.reports.revenue`; audited by row count only — never contents.
async fn admin_export_revenue<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
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
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
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
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    let days = match revenue(&app.rollups, tenant_id, store_id, &window).await {
        Ok(days) => days,
        Err(error) => return rollup_error_response(&error),
    };
    let Ok(body) = export::revenue_csv(&days) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not build the CSV").into_response();
    };
    audit_action(
        &app.audit,
        &app.clock,
        &context,
        Some(tenant_id),
        "reports.export_revenue",
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": "revenue", "rows": days.len() })),
    )
    .await;
    csv_download_response("revenue.csv", body)
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

#[cfg(test)]
mod client_ip_tests {
    //! The trusted-hop rule behind [`client_ip`]. Worth pinning directly because the value feeds the
    //! `/admin/login` sliding-window rate limit, and the first version of this function read the
    //! *leftmost* `X-Forwarded-For` hop — a value the client writes — so a guesser could mint a fresh
    //! bucket per attempt and never meet the throttle. These are the tests that would have caught it.
    //!
    //! The hop count is now configuration ([ADR-0090](../../../docs/adr/0090-tls-postures.md)), so
    //! both postures are covered: `ONE_PROXY` is the bundled Caddy alone, `TWO_PROXIES` is
    //! `TLS_MODE=external`, where a load balancer terminates TLS in front of it.
    use super::{DEFAULT_TRUSTED_PROXY_HOPS, client_ip};
    use axum::http::{HeaderMap, HeaderName};

    /// The bundled Caddy alone — every posture but `external`.
    const ONE_PROXY: usize = 1;
    /// An upstream terminator in front of Caddy: `TLS_MODE=external`.
    const TWO_PROXIES: usize = 2;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                name.parse::<HeaderName>()
                    .expect("a valid test header name"),
                value.parse().expect("a valid test header value"),
            );
        }
        map
    }

    #[test]
    fn one_proxy_hop_is_the_address_the_proxy_saw() {
        let map = headers(&[("x-forwarded-for", "203.0.113.9")]);
        assert_eq!(client_ip(&map, ONE_PROXY), Some("203.0.113.9"));
    }

    #[test]
    fn a_client_supplied_leading_hop_is_ignored() {
        // The attack the old behaviour allowed: Caddy appends rather than replaces, so anything the
        // client puts in the header arrives to the LEFT of the address Caddy actually observed.
        // Trusting the left end let a password guesser rotate buckets at will.
        let map = headers(&[("x-forwarded-for", "1.1.1.1, 203.0.113.9")]);
        assert_eq!(
            client_ip(&map, ONE_PROXY),
            Some("203.0.113.9"),
            "the trusted hop is the one this deployment appended, not the one the caller claimed"
        );
    }

    #[test]
    fn a_long_forged_chain_still_resolves_to_the_real_peer() {
        let map = headers(&[("x-forwarded-for", "9.9.9.9, 8.8.8.8, 7.7.7.7, 203.0.113.9")]);
        assert_eq!(client_ip(&map, ONE_PROXY), Some("203.0.113.9"));
    }

    #[test]
    fn surrounding_whitespace_does_not_change_the_hop() {
        let map = headers(&[("x-forwarded-for", "1.1.1.1 ,  203.0.113.9  ")]);
        assert_eq!(client_ip(&map, ONE_PROXY), Some("203.0.113.9"));
    }

    #[test]
    fn no_forwarding_header_is_none_rather_than_a_guess() {
        // A request that did not come through the proxy. Callers key this as one shared bucket, which
        // over-throttles a direct caller instead of exempting it — an absent header must not be a way
        // through either.
        assert_eq!(client_ip(&headers(&[]), ONE_PROXY), None);
    }

    #[test]
    fn x_real_ip_is_deliberately_not_a_fallback() {
        // Single-valued and client-settable, with no chain to count back along: honouring it would
        // reopen the hole this function closes.
        let map = headers(&[("x-real-ip", "1.1.1.1")]);
        assert_eq!(client_ip(&map, ONE_PROXY), None);
    }

    #[test]
    fn an_empty_header_is_none() {
        let map = headers(&[("x-forwarded-for", "")]);
        assert_eq!(client_ip(&map, ONE_PROXY), None);
    }

    #[test]
    fn the_default_hop_count_is_one_trusted_proxy() {
        // The default has to match the deployment the repository ships (ADR-0044): one Caddy.
        let map = headers(&[("x-forwarded-for", "1.1.1.1, 203.0.113.9")]);
        assert_eq!(DEFAULT_TRUSTED_PROXY_HOPS, ONE_PROXY);
        assert_eq!(
            client_ip(&map, DEFAULT_TRUSTED_PROXY_HOPS),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn two_trusted_proxies_reach_past_the_upstream_terminator() {
        // TLS_MODE=external: the terminator appends the client, then Caddy appends the terminator.
        // The real client is two entries back, and this is the case that made the count
        // configuration — at one hop this would key every request on the terminator's address.
        let map = headers(&[("x-forwarded-for", "203.0.113.9, 10.0.0.7")]);
        assert_eq!(
            client_ip(&map, TWO_PROXIES),
            Some("203.0.113.9"),
            "with two trusted hops the client is the entry before the two we appended"
        );
        assert_eq!(
            client_ip(&map, ONE_PROXY),
            Some("10.0.0.7"),
            "and at one hop it would be the terminator — the misconfiguration this pins"
        );
    }

    #[test]
    fn two_trusted_proxies_still_ignore_a_forged_prefix() {
        let map = headers(&[("x-forwarded-for", "1.1.1.1, 2.2.2.2, 203.0.113.9, 10.0.0.7")]);
        assert_eq!(client_ip(&map, TWO_PROXIES), Some("203.0.113.9"));
    }

    #[test]
    fn a_chain_shorter_than_the_hop_count_falls_back_to_its_first_entry() {
        // A misconfigured deployment, or an upstream that sent no chain of its own. Saturating at the
        // first entry is the conservative answer: it is the only address present, and it is one this
        // deployment or its trusted proxy wrote — never a value further left that a client chose.
        let map = headers(&[("x-forwarded-for", "10.0.0.7")]);
        assert_eq!(client_ip(&map, TWO_PROXIES), Some("10.0.0.7"));
    }
}

#[cfg(test)]
mod preview_diff_tests {
    //! The pure dry-run behind the campaigns-publish preview ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)),
    //! covered directly since it, not the thin route around it, holds the logic worth pinning.
    use super::preview_config_node;
    use crate::config_tree::{ConfigTreeState, PublishedVersion};
    use pos_proto::ids::ConfigVersionId;
    use pos_proto::ulid::Ulid;
    use serde_json::{Value, json};

    fn version(raw: &str) -> ConfigVersionId {
        ConfigVersionId::new(raw.parse::<Ulid>().expect("a valid test ULID"))
    }

    /// A store whose Store layer already carries a `tax` node and an old `campaigns` node, with a
    /// single published version whose effective document is that Store layer composed alone.
    fn state_with(store_layer: Value) -> ConfigTreeState {
        let empty = || Value::Object(serde_json::Map::new());
        ConfigTreeState {
            layers: [empty(), empty(), store_layer.clone(), empty()],
            history: vec![PublishedVersion {
                id: version("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
                effective: store_layer,
            }],
            k: 8,
        }
    }

    #[test]
    fn a_changed_node_diffs_to_only_that_node_and_leaves_the_others_alone() {
        let state = state_with(json!({
            "tax": {"rate": 8},
            "campaigns": {"campaigns": [{"id": "old"}]},
        }));
        let candidate = json!({"campaigns": [{"id": "new"}]});

        let preview = preview_config_node(Some(&state), "campaigns", candidate)
            .expect("the candidate validates");

        assert!(!preview.unchanged);
        assert_eq!(
            preview.from_version_id.as_deref(),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        );
        // Only the campaigns node moves; `tax` never appears in the patch.
        assert_eq!(
            preview.diff,
            json!({"campaigns": {"campaigns": [{"id": "new"}]}}),
            "the patch replaces just the campaigns node",
        );
    }

    #[test]
    fn republishing_the_same_node_is_an_empty_patch() {
        let node = json!({"campaigns": [{"id": "keep"}]});
        let state = state_with(json!({"campaigns": node}));

        let preview =
            preview_config_node(Some(&state), "campaigns", node).expect("the candidate validates");

        assert!(preview.unchanged);
        assert_eq!(preview.diff, json!({}), "nothing to publish");
    }

    #[test]
    fn a_store_with_no_tree_yet_previews_the_whole_node_as_new() {
        let candidate = json!({"campaigns": [{"id": "first"}]});

        let preview =
            preview_config_node(None, "campaigns", candidate).expect("the candidate validates");

        assert!(!preview.unchanged);
        assert!(
            preview.from_version_id.is_none(),
            "no version to diff against yet",
        );
        assert_eq!(
            preview.diff,
            json!({"campaigns": {"campaigns": [{"id": "first"}]}}),
        );
    }

    #[test]
    fn a_candidate_that_would_not_validate_reports_the_publish_time_violations() {
        // Two mutually exclusive capability flags — the same conflict a real publish rejects (§10).
        let candidate = json!(true);
        let state = state_with(json!({
            "pay_first_enabled": true,
            "tables_enabled": true,
        }));

        let violations = preview_config_node(Some(&state), "unrelated", candidate)
            .expect_err("the composed document must fail validation");
        assert!(
            violations
                .iter()
                .any(|v| v.contains("pay_first_enabled") && v.contains("tables_enabled")),
            "the preview surfaces the real conflict: {violations:?}",
        );
    }
}
