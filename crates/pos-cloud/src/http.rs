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
//!    store operation rather than an integrator API — absent from the *integrator* OpenAPI
//!    document, like `/internal`.
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
//!    replace a tenant's localized strings, `en`-validated. Not part of the *integrator* contract,
//!    so it is absent from `/v1/openapi.json` — but it has a generated document of its own at
//!    `/admin/openapi.json` ([`crate::openapi_admin`], roadmap v3 B5), because a fork writing its
//!    own console needs the routes, parameters and outcomes written down.
//!
//! # One error shape, arriving in slices
//!
//! Every failure should answer the AIP-193 envelope `pos-proto` defines — `{"error":{code,status,
//! message,details}}` — and [`api_error`] is the only way this module builds one, so the status
//! line and the body cannot disagree. ADR-0026 §27 chose that shape when this surface was small;
//! the surface then grew to roughly six hundred error paths writing plain text, which is what
//! roadmap v3 Q3 is converting.
//!
//! It converts in reviewable groups rather than one commit, so **for the duration some handlers
//! answer the envelope and the rest answer plain text**. That is a known intermediate state, not a
//! disagreement about the target, and it is safe because no consumer depends on the shape: the
//! console's `failure()` reads the envelope, the config publish's `{"violations":[…]}` and raw text
//! alike, and `cloud-sync-http` maps cloud failures on HTTP status alone and never parses a body.
//! Converted so far: every per-domain error helper (`*_error_response`, `*_entropy_unavailable`,
//! [`activation_refused`], [`too_many_login_attempts`], [`admin_service_unavailable`]) and
//! [`error_response`]. Remaining: the inline validation refusals written at each handler — also the
//! ones that will carry field-level `details`, which is why they are their own slice.
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
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_MATCH, REFERRER_POLICY, RETRY_AFTER,
    SET_COOKIE, USER_AGENT, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pos_ports::PortError;
use pos_ports::blob_store::{BlobKey, BlobStore};
use pos_ports::config_store::ConfigUpdate;
use pos_ports::event_store::EventStore;
use pos_proto::campaign::{
    PublishedAction, PublishedCampaign, PublishedCampaignKind, PublishedConditions,
};
use pos_proto::channels::{
    PublishedChannels, PublishedTender, PublishedVendorPolicies, PublishedVendorPolicy,
};
use pos_proto::determinism::ClockSource;
use pos_proto::devices::{DeviceConnection, PublishedDevice, PublishedDevices};
use pos_proto::display::GridPosition;
use pos_proto::enums::{EdgePlacement, PaymentMethod, SalesChannel, UnitOfMeasure};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{
    AreaId, CampaignId, ConfigVersionId, CourseId, DeviceId, DisplayCategoryId,
    DisplaySubcategoryId, EventId, IngredientId, MenuItemId, StationId, StoreId, SubjectId,
    SupplierId, TableId, TaxClassId, TenantId, VoucherId,
};
use pos_proto::inventory::{
    PublishedIngredient, PublishedRecipe, PublishedRecipeLine, PublishedSupplier,
};
use pos_proto::locale::{CountryCode, TaxComponent, TaxRate};
use pos_proto::money::CurrencyCode;
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::{Open, WireEnum};
use pos_proto::{ErrorResponse, ErrorStatus};

use pos_country::CountryRegistry;

use pos_core::activation::{ActivationCode, Redemption, redeem};
use pos_core::business_date::{CutoffHour, StoreTimeZone};

use crate::activation::{ActivationCodeStore, hash_code, mint_device_credential};
use crate::alerts::{AlertRecord, AlertStore, AlertStoreError};
use crate::audit::{
    AuditActor, AuditEntry, AuditId, AuditRecorder, AuditStore, NoopAuditRecorder, TrailOrder,
};
use crate::auth::admin::{
    AdminContext, AdminRole, AdminStatus, AdminStore, IMPLICIT_OWNER_EMAIL, IMPLICIT_OWNER_ID,
    LoginRequest, NewAdminInvite, NewAdminUser, NewRecoveryCode, SessionDenied, SessionMint,
    SessionSummary, authenticate_session, authenticated_admin, current_session_token_hash,
    hash_recovery_code, hash_session_token, login, logout,
};
use crate::auth::apikey::{ApiKeyAdminStore, ApiKeyId, ApiKeyStore, Scope, issue};
use crate::auth::bearer::{authenticate, confine_to_store, require_scope, require_store};
use crate::auth::console_rbac::{ConsolePermission, role_grants};
use crate::auth::enrol::{
    MIN_PASSWORD_LEN, SetupRequest, TOTP_SECRET_BYTES, build_enrolment, constant_time_eq,
};
use crate::auth::password::hash_password;
use crate::auth::rate_limit::SlidingRateLimiter;
use crate::auth::session::{clear_cookie, set_cookie};
use crate::campaigns::{CampaignStore, CampaignStoreError, to_node as campaigns_to_node};
use crate::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, ChannelPrice, DisplayCategory,
    DisplaySubcategory, ItemCategory, ItemCategoryId, ItemListFilter, ItemSort, ItemSubcategory,
    ItemSubcategoryId, LayoutButton, Menu, MenuId, MenuPlacement, MenuSection, MenuSectionId,
    ModifierGroup, ModifierGroupId, TaxClass,
};
use crate::catalog_compiler::{compile_layout_book, compile_menu};
use crate::cloud::{Cloud, DailyRollup};
use crate::config::InternalSecret;
use crate::config_tree::{
    CapabilityValidator, ConfigError, ConfigLevel, ConfigTree, ConfigTreeState, ConfigTreeStore,
    ConfigValidator, SyncOutcome,
    merge::{diff, merge_layers},
};
use crate::dashboard::rollup::WindowError;
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
use crate::lease::{LeaseStore, LeaseStoreError, lease_node};
use crate::media::{MediaId, MediaStore, MediaStoreError, NewMediaAsset, Rendition};
use crate::openapi::ApiDoc;
use crate::openapi_admin::AdminApiDoc;
use base64::Engine as _;

use crate::ota::{
    ArtifactKind, RecordOutcome, ReleaseArtifact, ReleaseStore, ReleaseStoreError, TargetTriple,
    artifact_key, validate_release_tag,
};
use crate::paging::{MAX_PAGE_LIMIT, MAX_PAGE_OFFSET, Page, PageRequest, PageRequestError};
use crate::people::{
    Assignment, AssignmentId, AssignmentStore, Employee, EmployeeId, EmployeeListFilter,
    EmployeeSort, EmployeeStore, EmployeeUpdate, NewAssignment, NewEmployee, NewRoleTemplate,
    PermissionInfo, RoleTemplate, RoleTemplateId, RoleTemplateStore, RoleTemplateUpdate,
    is_known_permission, permission_catalogue,
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
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned, records};
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

/// How many `/v1/orders` calls one tenant may make within the window before a `429`, when the binary
/// does not override it (roadmap **Q5**). Sized for a marketplace at a real lunch rush with room to
/// spare — a busy store takes single-digit orders a minute, and this is a whole tenant's allowance.
const DEFAULT_ORDERS_MAX_REQUESTS: usize = 300;

/// The sliding rate-limit window for `/v1/orders`, in seconds, when the config does not say — one
/// minute, so [`DEFAULT_ORDERS_MAX_REQUESTS`] reads as "five orders a second per integrator".
const DEFAULT_ORDERS_WINDOW_SECS: u64 = 60;

/// How many store-facing `/sync/*` requests one client connection may make within the window before
/// a `429`, when the binary does not override it (roadmap **Q5**).
const DEFAULT_SYNC_MAX_REQUESTS: usize = 600;

/// The sliding rate-limit window for `/sync/*`, in seconds, when the config does not say — one
/// minute. With [`DEFAULT_SYNC_MAX_REQUESTS`] that is ten requests a second sustained from one
/// connection: roughly fifty times what a healthy store generates (a five-second relay poll plus
/// occasional config and heartbeat traffic), and far below what a tight retry loop does.
const DEFAULT_SYNC_WINDOW_SECS: u64 = 60;

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
    /// The `/internal` shared secret (ADR-0097). `Option` only so `CloudApp::new` can be built
    /// before it is supplied; `CloudConfig::validate` refuses to boot without it, and a `None` here
    /// refuses every `/internal` request rather than admitting one.
    internal_shared_secret: Option<InternalSecret>,
    login_rate_limiter: SlidingRateLimiter,
    /// The `/sync/*` limiter (roadmap **Q5**), keyed on the client connection and checked before
    /// authentication. Its own limiter, not the login one: a store polling hard must not be able to
    /// lock an admin out of the console.
    sync_rate_limiter: SlidingRateLimiter,
    /// The `/v1/orders` limiter (roadmap **Q5**), keyed on the caller's tenant and checked after
    /// authentication. Handed to the intake sub-router through [`Self::orders_rate_limiter`].
    orders_rate_limiter: SlidingRateLimiter,
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
            internal_shared_secret: self.internal_shared_secret.clone(),
            login_rate_limiter: self.login_rate_limiter.clone(),
            sync_rate_limiter: self.sync_rate_limiter.clone(),
            orders_rate_limiter: self.orders_rate_limiter.clone(),
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
            internal_shared_secret: None,
            login_rate_limiter: SlidingRateLimiter::new(
                DEFAULT_ADMIN_LOGIN_MAX_ATTEMPTS,
                DEFAULT_ADMIN_LOGIN_WINDOW_SECS,
            ),
            sync_rate_limiter: SlidingRateLimiter::new(
                DEFAULT_SYNC_MAX_REQUESTS,
                DEFAULT_SYNC_WINDOW_SECS,
            ),
            orders_rate_limiter: SlidingRateLimiter::new(
                DEFAULT_ORDERS_MAX_REQUESTS,
                DEFAULT_ORDERS_WINDOW_SECS,
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
        self.login_rate_limiter = SlidingRateLimiter::new(max_attempts, window_secs);
        self
    }

    /// Sets the `/sync/*` rate limit — at most `max_requests` store-facing requests per client
    /// connection within a `window_secs` sliding window (roadmap **Q5**); the binary threads the
    /// configured values in ([`crate::config::CloudConfig::sync_max_requests`],
    /// [`crate::config::CloudConfig::sync_window_secs`]).
    ///
    /// Sized for a **wedged** store, not a busy one: a healthy box polls the relay every five
    /// seconds and pulls config far less often, so the default leaves an order of magnitude of
    /// headroom. What it stops is a box stuck in a tight retry loop — the shape the fork checklist
    /// already documents as "`403` on every poll, every five seconds" — costing the cloud a
    /// database round trip per iteration for every shop that ends up in that state.
    /// Sets the `/v1/orders` rate limit — at most `max_requests` intake calls per **tenant** within
    /// a `window_secs` sliding window (roadmap **Q5**); the binary threads the configured values in
    /// ([`crate::config::CloudConfig::orders_max_requests`],
    /// [`crate::config::CloudConfig::orders_window_secs`]).
    ///
    /// Per tenant rather than per connection because the intake is a shared resource between
    /// integrators: the thing worth preventing is one marketplace's runaway retry loop consuming the
    /// capacity the others need, and a tenant is the identity the bearer key proves.
    #[must_use]
    pub fn with_orders_rate_limit(mut self, max_requests: usize, window_secs: u64) -> Self {
        self.orders_rate_limiter = SlidingRateLimiter::new(max_requests, window_secs);
        self
    }

    /// Sets the `/sync/*` rate limit — see [`Self::with_orders_rate_limit`] for why the two
    /// surfaces are keyed differently.
    #[must_use]
    pub fn with_sync_rate_limit(mut self, max_requests: usize, window_secs: u64) -> Self {
        self.sync_rate_limiter = SlidingRateLimiter::new(max_requests, window_secs);
        self
    }

    /// The state [`throttle_sync`] runs on, for the binary to layer over the composed service.
    ///
    /// Handed out rather than layered inside [`router`] because the `/sync` routes are merged into a
    /// service the binary assembles: layering here would throttle only the routes this router owns
    /// and silently miss the relay's, which live in their own sub-router.
    #[must_use]
    pub fn sync_throttle(&self) -> SyncThrottle<C>
    where
        C: Clone,
    {
        SyncThrottle {
            limiter: self.sync_rate_limiter.clone(),
            trusted_proxy_hops: self.trusted_proxy_hops,
            clock: self.clock.clone(),
        }
    }

    /// The `/v1/orders` limiter, for [`crate::orders::orders_router`] — the intake carries its own
    /// state, so it takes a clone rather than reaching into [`CloudApp`].
    #[must_use]
    pub fn orders_rate_limiter(&self) -> SlidingRateLimiter {
        self.orders_rate_limiter.clone()
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

    /// The shared secret `/internal/ingest` requires
    /// ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)).
    #[must_use]
    pub fn with_internal_shared_secret(mut self, secret: Option<InternalSecret>) -> Self {
        self.internal_shared_secret = secret;
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
        .route("/admin/openapi.json", get(admin_openapi))
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
        // serves — which is *only* this router. `main.rs` applies the same layer to the
        // fully-composed service, which is what actually reaches the SPA fallback and the merged
        // `/admin` sub-routers; this line covered neither, and until production-readiness **S3** the
        // comment here claimed otherwise while `main.rs` carried no such layer. Both are kept: the
        // headers are `insert`ed, so applying the middleware twice sets the same values once.
        .layer(axum::middleware::from_fn(security_headers))
}

/// The collaborators the reconciliation routes share: the diff-and-history store, the admin store the
/// admin read authenticates against, the clock that stamps each recorded run and drives the session
/// guard, and the scoped keys the store-facing route authenticates a box with.
#[derive(Clone)]
struct ReconcileState<Rec, A, C, K> {
    store: Rec,
    admin: A,
    clock: C,
    /// The `/internal` shared secret (ADR-0097). It guards the **handler**, not this router: the
    /// same router serves `/admin/reconcile`, which is behind a console permission and must not
    /// start demanding an internal header.
    internal_shared_secret: Option<InternalSecret>,
    /// The scoped keys the **store-facing** route authenticates with, so a box that is off the
    /// cloud's own network can reconcile at all (production-readiness **R3**).
    keys: K,
}

/// Builds the reconciliation sub-router, stated independently of [`CloudApp`].
///
/// Reconciliation is the cloud's half of ADR-0040's missing-id diff: a store sends the ids it holds
/// over some window, and the cloud answers with the subset it lacks — the ids to re-push. Every diff
/// also records a run into the history ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// so `GET /admin/reconcile` can show the console that reconciliation happened and what it caught.
///
/// # Two doors, because a store is not on the cloud's network
///
/// `POST /sync/stores/{store_id}/reconcile` is the **store's** door, authenticated by its own scoped
/// key and bound to its own store, exactly like the config pull, the heartbeat and the OTA report
/// beside it. It exists because ADR-0040 called reconciliation *edge-initiated* and put it on
/// `/internal/*` — and the shipped proxy denies `/internal/*` to every off-box caller, which a store
/// is by definition. The deferred edge poller would have been written against a route it could never
/// reach (production-readiness **R3**); this is the third route to make that same move.
///
/// `POST /internal/reconcile` stays for a caller that genuinely is on the cloud's own network — an
/// operator's one-off, or a future cloud-side sweep. It names its tenant and store in the body and is
/// guarded by the `/internal` shared secret (ADR-0097); the store's door takes neither, because its
/// identity comes from the key it presents.
///
/// The admin read is behind [`ConsolePermission::Read`]. Stated independently and merged in `main`,
/// rather than threading an extra collaborator through every `CloudApp` handler.
pub fn reconcile_router<Rec, A, C, K>(
    store: Rec,
    admin: A,
    clock: C,
    internal_shared_secret: Option<InternalSecret>,
    keys: K,
) -> Router
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/reconcile",
            post(store_reconcile::<Rec, A, C, K>),
        )
        .route("/internal/reconcile", post(reconcile::<Rec, A, C, K>))
        .route(
            "/admin/reconcile",
            get(admin_reconcile_runs::<Rec, A, C, K>),
        )
        .with_state(ReconcileState {
            store,
            admin,
            clock,
            internal_shared_secret,
            keys,
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

/// A store's own reconciliation manifest: just the ids, because the tenant and the store come from
/// the key it presented and the path it called (production-readiness **R3**).
///
/// Deliberately **not** [`ReconcileRequest`] with two ignored members: a body that carries a
/// `tenant_id` the server discards is a body a caller will eventually believe is honoured.
#[derive(Debug, Clone, Deserialize)]
struct StoreReconcileRequest {
    /// The event ids this store holds over the window it is reconciling.
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
/// `/internal/ingest`), so it is absent from the public OpenAPI and requires the `X-Pos-Internal-Key`
/// shared secret ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)) on top of the
/// proxy denies both deploy lanes apply.
async fn reconcile<Rec, A, C, K>(
    State(state): State<ReconcileState<Rec, A, C, K>>,
    headers: HeaderMap,
    Json(request): Json<ReconcileRequest>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
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
    run_reconcile(&state, tenant_id, store_id, &request.event_ids).await
}

/// `POST /sync/stores/{store_id}/reconcile` — the **store's** door onto the same diff.
///
/// Identity comes from the scoped key the box presents, not from a body it writes: the tenant is the
/// grant's and the store is the path's, checked against the key's binding. That is the same shape the
/// config pull, the heartbeat and the OTA report have, and it is why this route needs no shared
/// secret — a store cannot reach the cloud's private network, and never could
/// (production-readiness **R3**).
async fn store_reconcile<Rec, A, C, K>(
    State(state): State<ReconcileState<Rec, A, C, K>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<StoreReconcileRequest>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
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
    run_reconcile(&state, grant.tenant(), store_id, &request.event_ids).await
}

/// The half both reconcile routes share: parse the manifest, run the diff, record the run.
///
/// Extracted for the reason the OTA report's shared body was — the whole difference between the two
/// routes is meant to be *where identity comes from*, and one body makes that true rather than
/// merely intended. A drift here would be a store and an operator getting different answers to the
/// same question.
async fn run_reconcile<Rec, A, C, K>(
    state: &ReconcileState<Rec, A, C, K>,
    tenant_id: TenantId,
    store_id: StoreId,
    event_ids: &[String],
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    let request_event_ids = event_ids;
    let mut candidates = Vec::with_capacity(request_event_ids.len());
    for raw in request_event_ids {
        match raw.parse::<EventId>() {
            Ok(id) => candidates.push(id),
            Err(_) => {
                return api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    "an event id is not a ULID",
                    &[("event_ids", "NOT_A_ULID")],
                );
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
            service_unavailable("reconciliation")
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
async fn admin_reconcile_runs<Rec, A, C, K>(
    State(state): State<ReconcileState<Rec, A, C, K>>,
    headers: HeaderMap,
    Query(query): Query<ReconcileHistoryQuery>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let store = match query.store_id.as_deref() {
        Some(raw) => match raw.parse::<Ulid>().map(StoreId::new) {
            Ok(store) => Some(store),
            Err(_) => return ulid_refusal(&["store_id"]),
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
            service_unavailable("reconciliation")
        }
    }
}

/// The state the OTA-report ingest carries: the write seam and the server clock that stamps the
/// report's arrival instant.
#[derive(Clone)]
struct OtaReportState<R, C, K> {
    reports: R,
    clock: C,
    /// The key store the **store-facing** route authenticates against. The `/internal` route beside
    /// it does not use this: it is trusted-network and guarded by the shared secret instead.
    keys: K,
    /// The `/internal` shared secret (ADR-0097). Guarded at the handler like the other two, for one
    /// shape across all three, even though this router carries only the one route.
    internal_shared_secret: Option<InternalSecret>,
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
    /// Whether the post-install self-test passed, absent when the store has never self-tested
    /// (ADR-0078 Amendment 1). Optional on the wire, so an edge built before the amendment posts the
    /// same body and its `true`/`false` is read unchanged.
    #[serde(default)]
    self_test_passed: Option<bool>,
}

/// What a **store** reports about itself ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// on the `/sync` route that replaced the `/internal` one for store-originated reporting
/// ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)).
///
/// No `tenant_id` and no `store_id`: the tenant comes from the scoped key and the store from the
/// path. That is the entire point of the move — the `/internal` shape took both in the body, so it
/// trusted the caller's claim about which store it was, and a shared secret could change *who could
/// reach* the route without making a report attributable.
#[derive(Debug, Clone, Deserialize)]
struct StoreReportRequest {
    /// The release the store is now running.
    installed: String,
    /// Whether the post-install self-test passed; absent when the store has never self-tested
    /// (ADR-0078 Amendment 1).
    #[serde(default)]
    self_test_passed: Option<bool>,
}

/// Builds the OTA-report ingest sub-router ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// stated independently of [`CloudApp`] like [`reconcile_router`].
///
/// `POST /internal/ota/report` records the version a store is running and its last self-test outcome
/// onto the fleet-liveness read model, so the cloud can see rollout-ring progress. Internal,
/// private-network, and absent from the public OpenAPI, exactly like `/internal/ingest` and
/// `/internal/reconcile`; the server stamps the arrival instant from its own clock.
pub fn ota_report_router<R, C, K>(
    reports: R,
    clock: C,
    keys: K,
    internal_shared_secret: Option<InternalSecret>,
) -> Router
where
    R: OtaReportStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/report",
            post(receive_store_report::<R, C, K>),
        )
        .route("/internal/ota/report", post(ingest_ota_report::<R, C, K>))
        .with_state(OtaReportState {
            reports,
            clock,
            keys,
            internal_shared_secret,
        })
}

/// The response header carrying an artifact's detached signature, as lowercase hex
/// ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)).
///
/// It must name the same header as `cloud-sync-http`'s `SIGNATURE_HEADER` — the spellings differ in
/// case only, which HTTP treats as the same name. The edge refuses an
/// artifact whose signature header is missing, so a rename on one side alone stops every update in
/// the fleet — loudly, but only once a rollout reaches a real box.
const ARTIFACT_SIGNATURE_HEADER: &str = "x-pos-artifact-signature";

/// The collaborators the artifact route needs: the key store and clock that authenticate a store, the
/// object store the bytes live in, and the registry that says a release exists.
#[derive(Clone)]
struct ArtifactState<K, C, B, L> {
    keys: K,
    clock: C,
    blobs: B,
    releases: L,
}

/// What a store asks for: a release tag, and the architecture it runs.
///
/// `arch` is the additive field [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Correction 2 added. R1's workflow cross-compiles two targets, so a request without one cannot say
/// which binary it means — and guessing would hand an `aarch64` box an `x86_64` executable that fails
/// its self-test after the install, which is the expensive place to find out.
#[derive(Debug, Clone, Deserialize)]
struct ArtifactRequest {
    release: String,
    arch: String,
}

/// Builds the OTA artifact sub-router, stated independently of [`CloudApp`]
/// ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 1).
///
/// Like [`device_router`], it carries its own state and is merged into the main router rather than
/// adding more `CloudApp` generics for two collaborators one route uses.
///
/// **On `/sync`, not `/internal`.** [ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)
/// pinned this at `/internal/ota/artifact` when `/internal` was believed to be the store-facing
/// surface. It is not: the proxy answers `404` to every `/internal/*` request from outside the box,
/// and a store reaches its cloud through that proxy, so a handler there would be unreachable by its
/// only caller. Amendment 1 moves it here, where the tenant comes from the scoped key rather than a
/// body field.
pub fn ota_artifact_router<K, C, B, L>(keys: K, clock: C, blobs: B, releases: L) -> Router
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/artifact",
            post(serve_ota_artifact::<K, C, B, L>),
        )
        .with_state(ArtifactState {
            keys,
            clock,
            blobs,
            releases,
        })
}

// ---------------------------------------------------------------------------------------------
// The `/admin` release surface: putting an artifact in, and reading what is hosted
// ---------------------------------------------------------------------------------------------

/// The largest release binary the upload accepts.
///
/// A `pos-edge` build with its embedded UI is a few tens of megabytes; 128 MiB leaves room to grow
/// while still refusing an upload that could only be a mistake. The limit sits on this route rather
/// than on the reverse proxy so that a fork terminating TLS elsewhere ([ADR-0090](../../../docs/adr/0090-tls-postures.md)'s
/// `external` posture) still gets it.
const MAX_RELEASE_UPLOAD_BYTES: usize = 128 * 1024 * 1024;

/// The header the upload carries the signature in: the **first base64 line** of the `.minisig` file
/// minisign wrote (line 2 of the file — line 1 is the untrusted comment).
///
/// Deliberately a different name from [`ARTIFACT_SIGNATURE_HEADER`], which the *serve* route answers
/// with in lowercase hex. Same bytes, two encodings, two directions: reusing one name for both would
/// make a mis-encoded upload look like a valid download header.
const UPLOAD_SIGNATURE_HEADER: &str = "x-pos-minisig";

/// Bytes in a minisign signature blob: `algorithm(2) ∥ key_id(8) ∥ ed25519_signature(64)`.
///
/// Mirrors `updater-minisign`'s `SIGNATURE_LEN`, which is private to that adapter. Checking the
/// length here is a **shape** check and emphatically not verification — the cloud never verifies, and
/// [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) is explicit that it stays a dumb host.
/// What it buys is that a signature which could never verify is refused at the one moment a human is
/// watching, instead of at every box in the ring hours later.
const MINISIGN_SIGNATURE_LEN: usize = 74;

/// The collaborators the `/admin` release routes need: the object store the bytes go to, the registry
/// that records them, and the admin/clock/audit trio every console write carries.
#[derive(Clone)]
struct ReleaseAdminState<B, L, A, C> {
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Which release and target the uploaded bytes are.
///
/// `release` is spelled the way a rollout's `target_version` and the binary's own version spell it —
/// bare, without a leading `v` ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2). The uploader passes the string it will promote.
#[derive(Debug, Clone, Deserialize)]
struct ReleaseUploadQuery {
    release: String,
    arch: String,
}

/// One hosted artifact, as the console reads it.
#[derive(Debug, Clone, Serialize)]
struct HostedArtifactResponse {
    arch: String,
    size_bytes: i64,
    sha256: String,
    recorded_at_ms: i64,
}

/// Builds the `/admin` release sub-router — the half that puts an artifact *into* the cloud.
///
/// Merged only when `[artifacts]` is configured, exactly like [`ota_artifact_router`]: without an
/// object store there is nowhere for the bytes to go, and a route that always answers `503` is worse
/// than a route that is honestly absent.
///
/// `POST /admin/releases?release=&arch=` takes the **bare executable** as its body and minisign's
/// signature line in [`UPLOAD_SIGNATURE_HEADER`]. The bare executable, not the release tarball,
/// because `UpdateInstaller::apply` writes the bytes it is handed as the next binary — so the
/// signature the edge checks has to cover *these* bytes ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2). Unpacking a tarball server-side would hand `apply` bytes nobody signed.
pub fn release_admin_router<B, L, A, C>(
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/releases", post(admin_upload_release::<B, L, A, C>))
        .route(
            "/admin/releases/{release}",
            get(admin_list_release::<B, L, A, C>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_RELEASE_UPLOAD_BYTES,
        ))
        .with_state(ReleaseAdminState {
            blobs,
            releases,
            admin,
            clock,
            audit,
        })
}

/// Decodes minisign's base64 signature line into the raw 74-byte blob the edge verifies against.
///
/// [ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md) Correction 2 put the decode here: every
/// type in the tree holds raw signature bytes, minisign's on-disk form is base64, and the boundary
/// where one becomes the other is this upload. The edge never sees base64.
///
/// # Errors
///
/// The [`Response`] to send — a `400` naming the header, for a value that is not base64 or does not
/// decode to a minisign blob's length.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape the other route helpers carry"
)]
fn decode_minisig_line(headers: &HeaderMap) -> Result<Vec<u8>, Response> {
    let Some(line) = headers
        .get(UPLOAD_SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the upload must carry minisign's signature line",
            &[(
                UPLOAD_SIGNATURE_HEADER,
                "absent — send the second line of the .minisig file",
            )],
        ));
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(line.trim()) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "minisign's signature line is not valid base64",
            &[(UPLOAD_SIGNATURE_HEADER, "not base64")],
        ));
    };
    if bytes.len() != MINISIGN_SIGNATURE_LEN {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "that is not a minisign signature",
            &[(
                UPLOAD_SIGNATURE_HEADER,
                "a minisign signature decodes to seventy-four bytes",
            )],
        ));
    }
    Ok(bytes)
}

/// Writes an artifact's two blobs, signature first.
///
/// The order is the contract, not a preference: the serve route treats "the registry says this
/// release exists but a blob does not" as the cloud's own inconsistency and answers `500`. Recording
/// the row only after **both** blobs are stored means that state is never reachable through this
/// route — a half-finished upload leaves no row, and the release simply does not exist yet.
///
/// # Errors
///
/// The [`Response`] to send: `503`, because an object store that cannot be written is a dependency
/// being down, and the same upload retried later is the right thing for the caller to do.
async fn store_artifact_blobs<B: BlobStore>(
    blobs: &B,
    binary_key: &BlobKey,
    signature_key: &BlobKey,
    binary: &[u8],
    signature: &[u8],
) -> Result<(), Response> {
    for (key, bytes, part) in [
        (signature_key, signature, "signature"),
        (binary_key, binary, "binary"),
    ] {
        if let Err(error) = blobs.put(key, bytes).await {
            tracing::error!(%error, part, key = key.as_str(), "storing a release blob failed");
            return Err(service_unavailable("the release artifact store"));
        }
    }
    Ok(())
}

/// Checks everything about an upload that can be judged from the request alone: that the release and
/// target could be storage keys, that the body is not empty, and that the signature header decodes to
/// a minisign blob.
///
/// Separated from the handler so that "is this request well-formed" and "did storing it work" stay
/// legible as two steps — and because every refusal here is a `400` naming its field, while every
/// failure after it is a dependency being down.
///
/// # Errors
///
/// The [`Response`] to send: a `400` naming `release`, `arch`, `body`, or the signature header.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape the other route helpers carry"
)]
fn parse_release_upload(
    query: &ReleaseUploadQuery,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(TargetTriple, Vec<u8>), Response> {
    if let Err(error) = validate_release_tag(&query.release) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        ));
    }
    let target = TargetTriple::parse(&query.arch).map_err(|error| {
        api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("arch: {error}"),
            &[("arch", "not a target triple")],
        )
    })?;
    if body.is_empty() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the upload body is the release executable, and it is empty",
            &[("body", "empty")],
        ));
    }
    let signature = decode_minisig_line(headers)?;
    Ok((target, signature))
}

/// A super-admin uploads one release artifact: the bare executable, plus its minisign signature.
///
/// Idempotent by [`admit_artifact`]'s rule — re-uploading the same release/target with identical
/// bytes answers `200` rather than failing, so a re-run of a release step does not wedge; different
/// bytes for a release a ring may already have installed are refused, because a version that can
/// change under a fleet is not a version.
async fn admin_upload_release<B, L, A, C>(
    State(state): State<ReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ReleaseUploadQuery>,
    body: axum::body::Bytes,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (target, signature) = match parse_release_upload(&query, &headers, &body) {
        Ok(parsed) => parsed,
        Err(refusal) => return refusal,
    };
    let (Ok(binary_key), Ok(signature_key)) = (
        artifact_key(&query.release, &target, ArtifactKind::Binary),
        artifact_key(&query.release, &target, ArtifactKind::Signature),
    ) else {
        return api_error(ErrorStatus::Internal, "could not compose the artifact keys");
    };
    let artifact = ReleaseArtifact {
        release: query.release.clone(),
        target,
        size_bytes: i64::try_from(body.len()).unwrap_or(i64::MAX),
        sha256: hex_digest(&body),
        recorded_at: state.clock.now(),
    };
    if let Err(refusal) =
        store_artifact_blobs(&state.blobs, &binary_key, &signature_key, &body, &signature).await
    {
        return refusal;
    }
    let outcome = match state.releases.record_artifact(&artifact).await {
        Ok(outcome) => outcome,
        Err(ReleaseStoreError::Immutable { release, target }) => {
            return api_error_with_details(
                ErrorStatus::AlreadyExists,
                format!("release {release} for {target} is already hosted with different bytes"),
                &[("release", "already hosted with a different digest")],
            );
        }
        Err(error @ ReleaseStoreError::Unavailable(_)) => {
            tracing::error!(%error, "recording a release artifact failed");
            return service_unavailable("the release registry");
        }
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        None,
        "release.upload",
        "release_artifact",
        &format!("{}/{}", artifact.release, artifact.target),
        None,
        Some(serde_json::json!({
            "arch": artifact.target.as_str(),
            "size_bytes": artifact.size_bytes,
            "sha256": artifact.sha256,
        })),
    )
    .await;
    let status = match outcome {
        RecordOutcome::Recorded => StatusCode::CREATED,
        RecordOutcome::AlreadyRecorded => StatusCode::OK,
    };
    (
        status,
        Json(serde_json::json!({
            "release": artifact.release,
            "arch": artifact.target.as_str(),
            "size_bytes": artifact.size_bytes,
            "sha256": artifact.sha256,
        })),
    )
        .into_response()
}

/// Which targets a release is hosted for — what an operator checks before promoting it, and what the
/// promote guard consults on their behalf.
async fn admin_list_release<B, L, A, C>(
    State(state): State<ReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Path(release): Path<String>,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    if let Err(error) = validate_release_tag(&release) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        );
    }
    match state.releases.list_artifacts(&release).await {
        Ok(artifacts) => Json(serde_json::json!({
            "release": release,
            "artifacts": artifacts
                .into_iter()
                .map(|artifact| HostedArtifactResponse {
                    arch: artifact.target.to_string(),
                    size_bytes: artifact.size_bytes,
                    sha256: artifact.sha256,
                    recorded_at_ms: artifact.recorded_at.as_milliseconds_since_epoch(),
                })
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "listing a release's artifacts failed");
            service_unavailable("the release registry")
        }
    }
}

/// Reads a release's two blobs — the executable and its raw detached signature.
///
/// Buffered, not streamed: [`BlobStore::get`] returns a `Vec<u8>`, so a 30 MB artifact is held in
/// memory for the length of the response. [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// flags that deliberately — a streaming `get` is a port change with its own contract-suite work and
/// belongs to the performance wave. Tolerable for a ring of a few boxes, wrong for a fleet-wide push.
///
/// # Errors
///
/// The [`Response`] to send. A blob that is **absent** while the registry says the release exists is
/// the cloud's own inconsistency: not the caller's mistake, and not something a retry mends. So it is
/// `Internal` rather than a `404` (which would tell the edge there is nothing to install and hide the
/// fault) or a `503` (which would ask it to try again forever). A backend that cannot be *reached* is
/// the retryable case, and stays `503`.
async fn read_artifact_blobs<B: BlobStore>(
    blobs: &B,
    binary_key: &BlobKey,
    signature_key: &BlobKey,
) -> Result<(Vec<u8>, Vec<u8>), Response> {
    let bytes = read_one_artifact_blob(blobs, binary_key, "artifact").await?;
    // Refused rather than served: bytes with no signature are bytes the edge has nothing to judge
    // by, and it would be right to refuse them anyway.
    let signature = read_one_artifact_blob(blobs, signature_key, "signature").await?;
    Ok((bytes, signature))
}

/// One blob, or the refusal to send. See [`read_artifact_blobs`] for why an absent blob is
/// `Internal`; `part` names which half is missing so the log and the message say the same thing.
async fn read_one_artifact_blob<B: BlobStore>(
    blobs: &B,
    key: &BlobKey,
    part: &str,
) -> Result<Vec<u8>, Response> {
    match blobs.get(key).await {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => {
            tracing::error!(
                key = key.as_str(),
                part,
                "a recorded release is missing a blob"
            );
            Err(api_error(
                ErrorStatus::Internal,
                format!("the release is recorded but its {part} is missing"),
            ))
        }
        Err(error) => {
            tracing::warn!(%error, key = key.as_str(), part, "reading a release blob failed");
            Err(service_unavailable("artifact store"))
        }
    }
}

/// `POST /sync/stores/{store_id}/artifact` — hands a store the signed release bytes it decided to
/// install ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)).
///
/// **The cloud is a dumb host.** It stores bytes an operator uploaded and hands them back; it never
/// signs, and it never verifies the minisign signature. The edge verifies against a trust anchor
/// baked into its own binary ([ADR-0047](../../../docs/adr/0047-minisign-verification.md),
/// [ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)), so a compromised cloud, a swapped
/// blob or a spoofed host can make an update *fail* — never make a box install code.
///
/// **`read_config`, the owner's call (2026-09-04).** Every provisioned box already carries that
/// scope, so the OTA path works with no re-provisioning; a dedicated `fetch_update` scope would be
/// cleaner and would cost a visit to every live store before a single update could ship. The scope
/// is here to stop the VPS being an open binary-distribution host, not to keep a signed artifact
/// secret.
///
/// The signature rides `X-Pos-Artifact-Signature` as lowercase hex and the body stays the raw
/// artifact: a JSON envelope would mean encoding tens of megabytes to carry a few hundred bytes. The
/// blob holds **raw** signature bytes — ADR-0092 Correction 2 put minisign's base64 decode at the
/// `/admin` upload, once, under operator supervision, so the edge never sees base64 for a signature.
async fn serve_ota_artifact<K, C, B, L>(
    State(state): State<ArtifactState<K, C, B, L>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<ArtifactRequest>,
) -> Response
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadConfig) {
        return forbidden.into_response();
    }
    // A release is fleet-wide, so there is no per-store row to isolate here — but the key presenting
    // itself is still a *store's* credential, and one store must not fetch a release under another
    // store's name (it is how the OTA progress read model attributes an install). So the id is parsed
    // and then held to the grant, exactly as the config pull beside it does (S1).
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    if let Err(error) = validate_release_tag(&request.release) {
        return api_error(ErrorStatus::InvalidArgument, format!("release: {error}"));
    }
    // A *shape* check only. `TargetTriple::parse` says whether this could name a target, not
    // whether the cloud hosts one for it — a fork that cross-compiles beyond the two targets R1
    // builds records artifacts for them, and the registry stays the authority on what exists. So a
    // well-formed triple with nothing recorded falls through to the `404` below, exactly like an
    // unknown release tag; only a triple that could never name anything is refused here.
    let target = match TargetTriple::parse(&request.arch) {
        Ok(target) => target,
        Err(error) => return api_error(ErrorStatus::InvalidArgument, format!("arch: {error}")),
    };
    let artifact = match state
        .releases
        .find_artifact(&request.release, &target)
        .await
    {
        Ok(Some(artifact)) => artifact,
        // Not an error: a store asking for a release this cloud does not host is told so, and
        // installs nothing. The adapter maps this onto `PortError::not_found`.
        Ok(None) => return not_found("release artifact"),
        Err(error) => {
            tracing::warn!(%error, "reading the release registry failed");
            return service_unavailable("release registry");
        }
    };
    let (Ok(binary_key), Ok(signature_key)) = (
        artifact_key(&request.release, &target, ArtifactKind::Binary),
        artifact_key(&request.release, &target, ArtifactKind::Signature),
    ) else {
        // Unreachable: the tag validated above and the triple parsed, and those are the only two
        // inputs. Reported rather than unwrapped, because a panic here would take the process down
        // for a case the type system nearly rules out but does not.
        return api_error(
            ErrorStatus::Internal,
            "the release's storage key could not be built",
        );
    };
    let (bytes, signature) =
        match read_artifact_blobs(&state.blobs, &binary_key, &signature_key).await {
            Ok(pair) => pair,
            Err(refusal) => return refusal,
        };
    // The registry's digest is an integrity check against a truncated upload or a corrupted blob, and
    // is deliberately not a trust boundary — only the minisign signature makes an artifact safe to
    // install, and only the edge verifies it. Checking it here stops the cloud shipping bytes it can
    // already tell are not the ones it recorded. `Internal`, not `Unavailable`: a corrupt blob does
    // not fix itself on retry, and telling the edge to back off and try again would be a lie.
    let digest = hex_digest(&bytes);
    if digest != artifact.sha256 {
        tracing::error!(
            key = binary_key.as_str(),
            recorded = artifact.sha256,
            found = digest,
            "an artifact blob does not match its recorded digest"
        );
        return api_error(
            ErrorStatus::Internal,
            "the stored artifact does not match its recorded digest",
        );
    }
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    match HeaderValue::from_str(&hex_encode(&signature)) {
        Ok(value) => {
            headers.insert(ARTIFACT_SIGNATURE_HEADER, value);
        }
        Err(error) => {
            // Unreachable: hex is header-safe by construction. Refused rather than served, because a
            // 2xx without the signature is bytes the edge has nothing to judge by.
            tracing::error!(%error, "the artifact signature could not become a header value");
            return api_error(
                ErrorStatus::Internal,
                "the artifact signature could not be sent",
            );
        }
    }
    response
}

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
async fn receive_store_report<R, C, K>(
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
async fn ingest_ota_report<R, C, K>(
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
    /// How an **approved** device is attached: `usb`, `network` or `serial`
    /// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)). Required on approve,
    /// ignored on reject. Discovery cannot find this out, and it decides whether a cash drawer may
    /// be opened at all, so approval is where a human states it.
    #[serde(default)]
    connection: Option<String>,
    /// The kitchen station an **approved** device serves (a ULID). Absent means the counter's
    /// receipt printer, which serves the bill rather than a station. Ignored on reject.
    #[serde(default)]
    station_id: Option<String>,
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
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let Some(kind) = DeviceKind::from_wire(&request.kind) else {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "kind must be one of printer, kds",
            &[("kind", "UNKNOWN_VALUE")],
        );
    };
    // Reported per field rather than as one "name and address are required", because a caller that
    // sent a name and forgot an address should be told which of the two to fix. The condition is
    // unchanged; only the answer got more specific.
    if request.name.trim().is_empty() || request.address.trim().is_empty() {
        let mut missing: Vec<(&str, &str)> = Vec::with_capacity(2);
        if request.name.trim().is_empty() {
            missing.push(("name", "REQUIRED"));
        }
        if request.address.trim().is_empty() {
            missing.push(("address", "REQUIRED"));
        }
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name and address are required",
            &missing,
        );
    }
    let Some(id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DeviceProposalId::new)
    else {
        tracing::error!("could not read OS entropy to mint a device-proposal id");
        return service_unavailable("device");
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
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
    let (tenant_id, id) = match parse_ulid_fields([("tenant_id", &query.tenant_id), ("id", id)]) {
        Ok([tenant_id, id]) => (TenantId::new(tenant_id), DeviceProposalId::new(id)),
        Err(refusal) => return refusal,
    };
    // Approval carries the two facts discovery cannot find (ADR-0100). A rejection carries neither:
    // they describe a device the store will address, and a rejected one never will.
    let (connection, station) = if approved {
        let Some(raw) = query.connection.as_deref() else {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "connection is required to approve a device: usb, network, or serial",
                &[("connection", "REQUIRED")],
            );
        };
        let Some(connection) = DeviceConnection::from_short_name(raw) else {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "connection must be one of usb, network, serial",
                &[("connection", "INVALID_ENUM_VALUE")],
            );
        };
        let station = match query.station_id.as_deref() {
            None => None,
            Some(raw) => match parse_ulid_fields([("station_id", raw)]) {
                Ok([station]) => Some(StationId::new(station)),
                Err(refusal) => return refusal,
            },
        };
        (Some(connection), station)
    } else {
        (None, None)
    };
    match state
        .devices
        .resolve(tenant_id, id, approved, connection, station)
        .await
    {
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
    api_error(
        ErrorStatus::Unavailable,
        "the device service is unavailable",
    )
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

/// `GET /admin/people/employees`: the tenant, plus the optional paging bounds.
///
/// Separate from [`PeopleTenantQuery`] rather than adding the two fields to it, because that struct
/// is shared by five routes and only this one reads a page. A `?limit=` on the other four would be
/// accepted and ignored — a query string that looks honoured and is not.
///
/// The `limit`/`offset` pair is repeated per route for the reason [`VoucherListQuery`] gives.
#[derive(Debug, Clone, Deserialize)]
struct EmployeeListQuery {
    /// The tenant whose employees to list (a 26-character ULID).
    tenant_id: String,
    /// How many employees to return. **Absent means unpaged**: the whole roster, as an array.
    #[serde(default)]
    limit: Option<String>,
    /// How many employees to skip. Only meaningful with `limit`.
    #[serde(default)]
    offset: Option<String>,
    /// A case-insensitive substring the person's name or staff code must contain.
    ///
    /// Only meaningful with `limit`: on the unpaged read the route refuses rather than quietly
    /// returning the whole roster, the same as every other search on this surface.
    ///
    #[serde(default)]
    q: Option<String>,
    /// Which order to return the page in — one of [`EmployeeSort::tokens`]. Only with `limit`.
    #[serde(default)]
    sort: Option<String>,
    /// `asc` or `desc`, inverting `sort`'s natural direction. Only with `limit`.
    #[serde(default)]
    order: Option<String>,
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
    api_error(
        ErrorStatus::Unavailable,
        "the people service is unavailable",
    )
}

/// `503` when OS entropy is unavailable to mint an id or hash a PIN — the request cannot proceed
/// safely, and it is transient.
fn people_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy for a people & access write");
    api_error(
        ErrorStatus::Unavailable,
        "the people service is unavailable",
    )
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
    Query(query): Query<EmployeeListQuery>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit. The unpaged one is not legacy: the
    // permission node is compiled from the whole roster, and a node built from a page would be
    // missing whoever fell off it (ADR-0098). The console's table is what wants a page — and gets
    // strictly less T1 data per response than the read beside it (ADR-0070).
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        // `q`/`sort`/`order` shape a *page*. Honouring them on the whole-roster read would answer a
        // different question than the caller asked; ignoring them would answer the wrong one
        // silently. The route names the missing parameter instead.
        for (field, value) in [
            ("q", query.q.as_deref()),
            ("sort", query.sort.as_deref()),
            ("order", query.order.as_deref()),
        ] {
            if present_param(value).is_some() {
                return page_shaping_needs_a_limit_refusal(field);
            }
        }
        return match state.people.list(tenant_id).await {
            Ok(employees) => (StatusCode::OK, Json(employees)).into_response(),
            Err(error) => people_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    let filter = match employee_list_filter(
        query.q.as_deref(),
        query.sort.as_deref(),
        query.order.as_deref(),
    ) {
        Ok(filter) => filter,
        Err(refusal) => return refusal,
    };
    match state.people.list_page(tenant_id, page, &filter).await {
        Ok(read) => paged_ok(read, page),
        Err(error) => people_error_response(&error),
    }
}

/// Reads `?q=`, `?sort=` and `?order=` into an [`EmployeeListFilter`], refusing a token outside the
/// route's own closed sets.
///
/// The refusal names the field and lists what it accepts (ADR-0096's shape), so a caller that sent
/// `?sort=hired` is told `sort` must be `newest`, `name` or `code` rather than being handed the
/// default order and left to wonder why nothing moved.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape `item_list_filter` and the other route helpers carry"
)]
fn employee_list_filter(
    search: Option<&str>,
    sort: Option<&str>,
    order: Option<&str>,
) -> Result<EmployeeListFilter, Response> {
    let search = present_param(search).map(str::to_owned);
    let sort = match present_param(sort) {
        None => EmployeeSort::default(),
        Some(token) => match EmployeeSort::from_token(token) {
            Some(sort) => sort,
            None => {
                return Err(enum_refusal("sort", EmployeeSort::tokens().iter().copied()));
            }
        },
    };
    let descending = match present_param(order) {
        // Absent is ascending — the same reading `?order=asc` has, so a caller that omits the
        // parameter and one that spells out the default get the same page.
        None | Some("asc") => false,
        Some("desc") => true,
        Some(_unknown) => return Err(enum_refusal("order", ["asc", "desc"])),
    };
    Ok(EmployeeListFilter {
        search,
        sort,
        descending,
    })
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
    let (tenant_id, employee_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("employee_id", &employee_id),
    ]) {
        Ok([tenant_id, employee_id]) => (TenantId::new(tenant_id), EmployeeId::new(employee_id)),
        Err(refusal) => return refusal,
    };
    match state.people.get(tenant_id, employee_id).await {
        Ok(Some(employee)) => versioned_ok(employee.record, &employee.etag),
        Ok(None) => not_found("employee"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // Two required fields, so name the ones actually blank rather than both — the same per-field
    // rule the ULID and opening-hours refusals follow.
    let mut missing: Vec<(&str, &str)> = Vec::with_capacity(2);
    if request.code.trim().is_empty() {
        missing.push(("code", "REQUIRED"));
    }
    if request.name.trim().is_empty() {
        missing.push(("name", "REQUIRED"));
    }
    if !missing.is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "code and name are required",
            &missing,
        );
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
        Ok(version) => {
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
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": employee_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
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
    let (tenant_id, employee_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("employee_id", &employee_id),
    ]) {
        Ok([tenant_id, employee_id]) => (TenantId::new(tenant_id), EmployeeId::new(employee_id)),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    // Read the current row for the audit `before` (id/code/status, never the name) and to answer 404.
    let existing = match state.people.get(tenant_id, employee_id).await {
        Ok(Some(employee)) => employee,
        Ok(None) => return not_found("employee"),
        Err(error) => return people_error_response(&error),
    };
    let update = EmployeeUpdate {
        employee_id,
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.people.update(&update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let before = serde_json::json!({
                "id": employee_id.to_string(),
                "code": existing.record.code,
                "status": existing.record.status.as_str(),
            });
            let after = serde_json::json!({
                "id": employee_id.to_string(),
                "code": existing.record.code,
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
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("employee"),
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
    let (tenant_id, employee_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("employee_id", &employee_id),
    ]) {
        Ok([tenant_id, employee_id]) => (TenantId::new(tenant_id), EmployeeId::new(employee_id)),
        Err(refusal) => return refusal,
    };
    if !pin_is_well_formed(&request.pin) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the PIN must be 4 to 8 digits",
            &[("pin", "OUT_OF_RANGE")],
        );
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
        Ok(false) => not_found("employee"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.people.list(tenant_id).await {
        Ok(roles) => (StatusCode::OK, Json(roles)).into_response(),
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
    let (tenant_id, role_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("role_id", &role_id)]) {
            Ok([tenant_id, role_id]) => (TenantId::new(tenant_id), RoleTemplateId::new(role_id)),
            Err(refusal) => return refusal,
        };
    match state.people.get(tenant_id, role_id).await {
        Ok(Some(role)) => versioned_ok(role.record, &role.etag),
        Ok(None) => not_found("role"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    if let Some(unknown) = first_unknown_permission(&request.permissions) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("unknown permission id: {unknown}"),
            &[("permissions", "INVALID_ENUM_VALUE")],
        );
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
        Ok(version) => {
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
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": role_template_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
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
    let (tenant_id, role_template_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("role_id", &role_id)]) {
            Ok([tenant_id, role_template_id]) => (
                TenantId::new(tenant_id),
                RoleTemplateId::new(role_template_id),
            ),
            Err(refusal) => return refusal,
        };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    if let Some(unknown) = first_unknown_permission(&request.permissions) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("unknown permission id: {unknown}"),
            &[("permissions", "INVALID_ENUM_VALUE")],
        );
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let update = RoleTemplateUpdate {
        role_template_id,
        tenant_id,
        name: request.name.clone(),
        permissions: request.permissions.clone(),
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.people.update(&update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("role"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let result = match (query.store_id.as_deref(), query.employee_id.as_deref()) {
        (Some(store), None) => {
            let store_id = match parse_ulid_fields([("store_id", store)]) {
                Ok([store_id]) => StoreId::new(store_id),
                Err(refusal) => return refusal,
            };
            state.people.list_for_store(tenant_id, store_id).await
        }
        (None, Some(employee)) => {
            let employee_id = match parse_ulid_fields([("employee_id", employee)]) {
                Ok([employee_id]) => EmployeeId::new(employee_id),
                Err(refusal) => return refusal,
            };
            state.people.list_for_employee(tenant_id, employee_id).await
        }
        _ => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "name exactly one of store_id or employee_id",
                &[
                    ("store_id", "MUTUALLY_EXCLUSIVE"),
                    ("employee_id", "MUTUALLY_EXCLUSIVE"),
                ],
            );
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
    let (tenant_id, employee_id, store_id, role_template_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("employee_id", &request.employee_id),
        ("store_id", &request.store_id),
        ("role_template_id", &request.role_template_id),
    ]) {
        Ok([tenant_id, employee_id, store_id, role_template_id]) => (
            TenantId::new(tenant_id),
            EmployeeId::new(employee_id),
            StoreId::new(store_id),
            RoleTemplateId::new(role_template_id),
        ),
        Err(refusal) => return refusal,
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
    let (tenant_id, assignment_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("assignment_id", &assignment_id),
    ]) {
        Ok([tenant_id, assignment_id]) => {
            (TenantId::new(tenant_id), AssignmentId::new(assignment_id))
        }
        Err(refusal) => return refusal,
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
        Ok(false) => not_found("assignment"),
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    // Every key must be a known §10 capability flag — the form only sends catalogue keys, and this
    // keeps a typo from writing a stray boolean into the config document.
    for key in request.flags.keys() {
        if !pos_core::capability::Capability::ALL
            .iter()
            .any(|capability| capability.meta().key == key)
        {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("unknown capability flag: {key}"),
                &[("flags", "INVALID_ENUM_VALUE")],
            );
        }
    }

    // Load the store's tree, set the flag keys on its Store layer (index 2), and re-publish that layer,
    // preserving the other Store-level keys (`menu`, `layout`, `permissions`).
    let nodes: Vec<(String, serde_json::Value)> = request
        .flags
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::Bool(*value)))
        .collect();

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    // A read, not a write: the publish composes the `tax` node from whatever the table holds now, so
    // the version it was read at is nothing this path can use.
    let entries = match state.tax_rates.list_tax_rates(tenant_id).await {
        Ok((entries, _version)) => entries,
        Err(error) => return tax_rate_error_response(&error),
    };
    let Ok(tax_value) = serde_json::to_value(to_table(&entries)) else {
        tracing::error!("could not serialise a tax rate table");
        return service_unavailable("tax-rate");
    };

    // Set the `tax` key on the store's Store layer (index 2) and re-publish it, preserving the other
    // Store-level keys (`menu`, `layout`, `permissions`, `floor`, capability flags).
    let nodes = vec![("tax".to_owned(), tax_value)];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
}

// --- Store profile publish (`/admin/config/store-profile`, ADR-0106) ----------------------------

/// A `PUT /admin/config/store-profile` body: the `(tenant, store)` and who that store legally is.
///
/// Every field is text the operator supplies. A registered address is written differently in every
/// country, so the framework imposes no shape on it; the one field with a checkable shape is the tax
/// registration number, and the check is the **country module's**
/// ([ADR-0106](../../../docs/adr/0106-the-store-is-a-legal-person.md)).
#[derive(Debug, Clone, Deserialize)]
struct PublishStoreProfileRequest {
    tenant_id: String,
    store_id: String,
    /// The registered name — what the law wants on the paper.
    #[serde(default)]
    legal_name: String,
    #[serde(default)]
    trading_name: Option<String>,
    #[serde(default)]
    address_lines: Vec<String>,
    #[serde(default)]
    tax_registration_number: Option<String>,
    #[serde(default)]
    tax_registration_label: Option<String>,
    #[serde(default)]
    contact_lines: Vec<String>,
    #[serde(default)]
    footer_lines: Vec<String>,
    /// Which country's rule to check the registration number's *shape* against, as an ISO 3166-1
    /// alpha-2 code. Optional: a fork whose country module is not compiled into this cloud still
    /// publishes a profile, with the number stored unchecked rather than the publish refused.
    #[serde(default)]
    country_code: Option<String>,
}

/// The collaborators the store-profile publish needs.
#[derive(Clone)]
struct ConfigStoreProfileState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    countries: Arc<CountryRegistry>,
}

/// Builds the store-profile publish sub-router ([ADR-0106](../../../docs/adr/0106-the-store-is-a-legal-person.md)).
///
/// One route, behind [`ConsolePermission::PublishConfig`]: write the store's registered identity as
/// its `store_profile` config node. The edge applies it to `EdgeSession::profile` and the receipt is
/// composed from it — which is what turns the store's paper into a document a Japanese or Indian
/// auditor accepts.
pub fn config_store_profile_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    countries: Arc<CountryRegistry>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/store-profile",
            axum::routing::put(admin_publish_store_profile::<Cfg, A, C>),
        )
        .with_state(ConfigStoreProfileState {
            config_trees,
            admin,
            clock,
            audit,
            countries,
        })
}

/// Trims a field, and treats blank as absent — a store that typed a space must not get a receipt
/// with a blank line where its address should be.
fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Trims a list of printed lines and drops the blank ones.
fn trimmed_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Validates and writes a store's registered identity as its `store_profile` node.
async fn admin_publish_store_profile<Cfg, A, C>(
    State(state): State<ConfigStoreProfileState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishStoreProfileRequest>,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };

    let registration = trimmed(request.tax_registration_number.as_deref());
    // Format only, never registration: whether a number *exists* is a call to a tax authority and
    // belongs behind `Fiscalization` (ADR-0027). This catches the typo that would otherwise reach an
    // invoice — and only when this cloud carries the country's module, so a fork serving a market it
    // has not written a pack for still publishes.
    if let (Some(number), Some(module)) = (
        registration.as_deref(),
        trimmed(request.country_code.as_deref())
            .and_then(|code| CountryCode::parse(&code).ok())
            .and_then(|code| state.countries.get(code)),
    ) && !module.is_valid_tax_code(number)
    {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "tax_registration_number is not the shape {} issues",
                module.display_name()
            )
            .as_str(),
            &[("tax_registration_number", "INVALID_FORMAT")],
        );
    }

    let profile = StoreProfile {
        legal_name: request.legal_name.trim().to_owned(),
        trading_name: trimmed(request.trading_name.as_deref()),
        address_lines: trimmed_lines(&request.address_lines),
        tax_registration_number: registration,
        tax_registration_label: trimmed(request.tax_registration_label.as_deref()),
        contact_lines: trimmed_lines(&request.contact_lines),
        footer_lines: trimmed_lines(&request.footer_lines),
    };
    let Ok(profile_value) = serde_json::to_value(&profile) else {
        tracing::error!("could not serialise a store profile");
        return service_unavailable("config-tree");
    };

    let nodes = vec![("store_profile".to_owned(), profile_value)];
    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.store_profile.publish",
        "store",
        &store_id.to_string(),
        None,
        // The registered identity is the store's own business data, not a person's, so the trail
        // records what was published — which is the point of an audit trail on a legal document.
        serde_json::to_value(&profile).ok(),
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
    /// Whether this store's menu prices already contain their tax
    /// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)): Japan's 税込 and
    /// India's MRP are `true`, Vietnam's `++` is `false`. Defaults to `false`, which is what every
    /// store did before the field existed.
    #[serde(default)]
    prices_include_tax: bool,
    /// What the grand total is rounded to in cash, in minor units — `1000` for Vietnam's thousand
    /// đồng, `100` for India's rupee, absent for Japan
    /// ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
    #[serde(default)]
    cash_rounding_increment: Option<i64>,
    /// The notes the till offers as quick-cash keys, in minor units. Absent or empty means the exact
    /// amount only, which is the honest answer rather than a guess.
    #[serde(default)]
    cash_denominations: Vec<i64>,
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

/// Checks the till-money half of a locale publish and returns the denominations to store
/// ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
///
/// Both refusals are about a number that cannot mean what it says. A rounding increment of zero or
/// less is a typo rather than a posture — `round_to_increment` needs a non-zero step, and "round to
/// nothing" is expressed by omitting the field — and a note worth nothing is not a note. Refused
/// here rather than dropped at the edge, so the person who typed it finds out.
///
/// The denominations come back sorted and de-duplicated rather than as given: the till lays its keys
/// out in this order, and a repeated note would be a repeated button.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn checked_denominations(request: &PublishLocaleRequest) -> Result<Vec<i64>, Response> {
    if request
        .cash_rounding_increment
        .is_some_and(|increment| increment <= 0)
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "cash_rounding_increment must be a positive number of minor units, or absent for no \
             rounding",
            &[("cash_rounding_increment", "OUT_OF_RANGE")],
        ));
    }
    if request
        .cash_denominations
        .iter()
        .any(|denomination| *denomination <= 0)
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "cash_denominations are note values in minor units and must all be positive",
            &[("cash_denominations", "OUT_OF_RANGE")],
        ));
    }
    let mut denominations = request.cash_denominations.clone();
    denominations.sort_unstable();
    denominations.dedup();
    Ok(denominations)
}

/// Validates a store's locale settings and writes them as its `locale` node, versioned — the same
/// load→merge→publish→version shape as the other node publishes. Each field is checked with its domain
/// constructor before anything is written (a real IANA timezone against the tz database, a 3-letter
/// currency, an hour in `0..=23`), so a bad value is a `400` naming it rather than a stored error.
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if CurrencyCode::parse(&request.currency_code).is_err() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "currency_code is not a 3-letter code",
            &[("currency_code", "INVALID_FORMAT")],
        );
    }
    if StoreTimeZone::from_iana_name(&request.timezone).is_err() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "timezone is not a valid IANA name",
            &[("timezone", "INVALID_FORMAT")],
        );
    }
    if CutoffHour::new(request.cutoff_hour).is_err() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "cutoff_hour must be in 0..=23",
            &[("cutoff_hour", "OUT_OF_RANGE")],
        );
    }
    let denominations = match checked_denominations(&request) {
        Ok(denominations) => denominations,
        Err(refusal) => return refusal,
    };

    let mut locale_value = serde_json::json!({
        "currency_code": request.currency_code,
        "timezone": request.timezone,
        "cutoff_hour": request.cutoff_hour,
        "prices_include_tax": request.prices_include_tax,
        "cash_rounding_increment": request.cash_rounding_increment,
        "cash_denominations": denominations,
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
    let nodes = vec![("locale".to_owned(), locale_value.clone())];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns, the shape `parse_ulid_fields` already carries this expectation for"
)]
fn parse_known_tokens<E: WireEnum>(
    field: &str,
    tokens: &[String],
) -> Result<Vec<Open<E>>, Response> {
    let mut out = Vec::with_capacity(tokens.len());
    for token in tokens {
        match E::from_wire(token) {
            Some(known) if known != E::UNSPECIFIED => out.push(Open::from_known(known)),
            // One refusal for two causes — an unrecognised token, and an explicit `*_UNSPECIFIED`
            // that parses and is still not a choice — because the answer is the same either way:
            // here is the set you may pick from, and neither is in it. The old message
            // (`"{token} is not a recognised value"`) named the field for neither and the set for
            // no one.
            _ => return Err(enum_refusal(field, accepted_tokens::<E>())),
        }
    }
    Ok(out)
}

/// The tokens a caller may actually send for `E`: every wire token except `UNSPECIFIED`.
///
/// `WireEnum::ALL` leads with `UNSPECIFIED`, which exists so an older client can read a newer
/// server's value and is never a choice a caller makes. Listing it as accepted would invite one.
fn accepted_tokens<E: WireEnum>() -> impl Iterator<Item = &'static str> {
    E::ALL
        .iter()
        .copied()
        .filter(|value| *value != E::UNSPECIFIED)
        .map(WireEnum::as_wire)
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
    match config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
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
    let nodes = vec![(node_key.to_owned(), node_value.clone())];
    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let enabled = match parse_known_tokens::<SalesChannel>("enabled", &request.enabled) {
        Ok(tokens) => tokens,
        Err(refusal) => return refusal,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let accepted = match parse_known_tokens::<PaymentMethod>("accepted", &request.accepted) {
        Ok(tokens) => tokens,
        Err(refusal) => return refusal,
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
    api_error(
        ErrorStatus::Unavailable,
        "the configuration service is unavailable",
    )
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let mut node = serde_json::json!({
        "enabled": request.enabled,
        "staff_confirmation_required": request.staff_confirmation_required,
        "per_table_limit": request.per_table_limit,
        "rate_window_secs": request.rate_window_secs,
    });
    if let Some(hours) = request.business_hours {
        let mut out_of_range: Vec<(&str, &str)> = Vec::with_capacity(2);
        if hours.open_hour > 23 {
            out_of_range.push(("open_hour", "OUT_OF_RANGE"));
        }
        if hours.close_hour > 23 {
            out_of_range.push(("close_hour", "OUT_OF_RANGE"));
        }
        if !out_of_range.is_empty() {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "open_hour and close_hour must be in 0..=23",
                &out_of_range,
            );
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if request
        .policies
        .iter()
        .any(|policy| policy.availability.is_unspecified() || policy.availability.is_unrecognised())
    {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a vendor policy names an unknown availability (open/busy/closed)",
            &[("policies", "INVALID_ENUM_VALUE")],
        );
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
    api_error(ErrorStatus::Unavailable, "the floor service is unavailable")
}

/// `503` when OS entropy is unavailable to mint a floor/kitchen id.
fn floor_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy for a floor & kitchen write");
    api_error(ErrorStatus::Unavailable, "the floor service is unavailable")
}

/// Parses an optional id field: an absent field, an empty string, or whitespace is `Ok(None)`; a
/// present value must be a ULID or the caller gets a `400` naming the field.
///
/// **The only place that rule is written.** There were seven typed wrappers around this, and two of
/// them — `brand_id` on a store and `parent_menu_id` on a menu — matched on `Some(text)` without
/// trimming or filtering, so an empty string meant "malformed" for those two fields and "unset" for
/// the other five. Whether `""` clears an optional reference is not a per-field question, and a
/// caller cannot be expected to know which of two identical-looking fields it is asking. Clearing a
/// select box sends `""`, and the console only avoided the `400` because every call site happened to
/// map it to `null` first — one screen forgetting that would have produced a refusal an operator
/// could not act on. So the wrappers are gone and every caller passes its own constructor
/// (`BrandId::new`, `MenuId::new`, …) here.
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
    let [tenant] = parse_ulid_fields([("tenant_id", tenant_id)])?;
    Ok(TenantId::new(tenant))
}

/// Reads a (tenant, store) list query, or returns the `400`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn floor_tenant_store(query: &FloorListQuery) -> Result<(TenantId, StoreId), Response> {
    let [tenant, store] = parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ])?;
    Ok((TenantId::new(tenant), StoreId::new(store)))
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
        Ok(areas) => (StatusCode::OK, Json(areas)).into_response(),
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
    let area_id = match parse_ulid_fields([("area_id", &area_id)]) {
        Ok([area_id]) => AreaId::new(area_id),
        Err(refusal) => return refusal,
    };
    match AreaStore::get(&state.floor, tenant_id, area_id).await {
        Ok(Some(area)) => versioned_ok(area.record, &area.etag),
        Ok(None) => not_found("area"),
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
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
        Ok(version) => {
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
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": area_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
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
    let (tenant_id, area_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("area_id", &area_id)]) {
            Ok([tenant_id, area_id]) => (TenantId::new(tenant_id), AreaId::new(area_id)),
            Err(refusal) => return refusal,
        };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let update = AreaUpdate {
        area_id,
        tenant_id,
        name: request.name.clone(),
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match AreaStore::update(&state.floor, &update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("area"),
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
        Ok(tables) => (StatusCode::OK, Json(tables)).into_response(),
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
    let table_id = match parse_ulid_fields([("table_id", &table_id)]) {
        Ok([table_id]) => TableId::new(table_id),
        Err(refusal) => return refusal,
    };
    match TableStore::get(&state.floor, tenant_id, table_id).await {
        Ok(Some(table)) => versioned_ok(table.record, &table.etag),
        Ok(None) => not_found("table"),
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
    let (tenant_id, store_id, area_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("area_id", &request.area_id),
    ]) {
        Ok([tenant_id, store_id, area_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            AreaId::new(area_id),
        ),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
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
        Ok(version) => {
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
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": table_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
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
    let (tenant_id, table_id, area_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("table_id", &table_id),
        ("area_id", &request.area_id),
    ]) {
        Ok([tenant_id, table_id, area_id]) => (
            TenantId::new(tenant_id),
            TableId::new(table_id),
            AreaId::new(area_id),
        ),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
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
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match TableStore::update(&state.floor, &update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("table"),
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
        Ok(stations) => (StatusCode::OK, Json(stations)).into_response(),
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
    let station_id = match parse_ulid_fields([("station_id", &station_id)]) {
        Ok([station_id]) => StationId::new(station_id),
        Err(refusal) => return refusal,
    };
    match StationStore::get(&state.floor, tenant_id, station_id).await {
        Ok(Some(station)) => versioned_ok(station.record, &station.etag),
        Ok(None) => not_found("station"),
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Ok(backup_station_id) =
        parse_optional_ulid(request.backup_station_id.as_deref(), StationId::new)
    else {
        return ulid_refusal(&["backup_station_id"]);
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
        Ok(version) => {
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
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": station_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
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
    let (tenant_id, station_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("station_id", &station_id),
    ]) {
        Ok([tenant_id, station_id]) => (TenantId::new(tenant_id), StationId::new(station_id)),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Ok(backup_station_id) =
        parse_optional_ulid(request.backup_station_id.as_deref(), StationId::new)
    else {
        return ulid_refusal(&["backup_station_id"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let update = StationUpdate {
        station_id,
        tenant_id,
        name: request.name.clone(),
        backup_station_id,
        is_default: request.is_default,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match StationStore::update(&state.floor, &update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("station"),
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
    let (tenant_id, store_id, station_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("station_id", &request.station_id),
    ]) {
        Ok([tenant_id, store_id, station_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            StationId::new(station_id),
        ),
        Err(refusal) => return refusal,
    };
    let Ok(menu_item_id) = parse_optional_ulid(request.menu_item_id.as_deref(), MenuItemId::new)
    else {
        return ulid_refusal(&["menu_item_id"]);
    };
    let Ok(course_id) = parse_optional_ulid(request.course_id.as_deref(), CourseId::new) else {
        return ulid_refusal(&["course_id"]);
    };
    // A rule must match exactly one of an item or a course — the same rule the §10 validator enforces
    // at publish, surfaced here so the console cannot store a rule that matches nothing or both.
    if menu_item_id.is_some() == course_id.is_some() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a routing rule must match exactly one of menu_item_id or course_id",
            &[
                ("menu_item_id", "MUTUALLY_EXCLUSIVE"),
                ("course_id", "MUTUALLY_EXCLUSIVE"),
            ],
        );
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
    let (tenant_id, rule_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("rule_id", &rule_id)]) {
            Ok([tenant_id, rule_id]) => (TenantId::new(tenant_id), RoutingRuleId::new(rule_id)),
            Err(refusal) => return refusal,
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
        Ok(false) => not_found("routing rule"),
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

/// The device-publish sub-router's state: the approval queue to read, the config tree to write.
#[derive(Clone)]
struct DevicePublishState<D, Cfg, A, C> {
    devices: D,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin selects the (tenant, store) whose `devices` node to compile and publish.
#[derive(Debug, Clone, Deserialize)]
struct PublishDevicesRequest {
    /// The tenant that owns the store (a 26-character ULID).
    tenant_id: String,
    /// The store whose approved devices to publish (a 26-character ULID).
    store_id: String,
}

/// Builds the device-publish sub-router
/// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md), C2 slice 2b).
///
/// Separate from [`device_router`] because it needs the config tree, which the propose/approve
/// surface does not — the same split [`floor_publish_router`] makes for the same reason.
pub fn device_publish_router<D, Cfg, A, C>(
    devices: D,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/devices/publish",
            post(admin_publish_devices::<D, Cfg, A, C>),
        )
        .with_state(DevicePublishState {
            devices,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Compiles a store's **approved** devices into its `devices` config node and versions it
/// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)).
///
/// The same load→compile→write→version shape as the floor publish beside it, onto the `devices` key.
/// A device whose stored `kind` or `connection` this build does not know keeps its token through
/// `Open` rather than failing the publish — the store on the other end applies the same rule, and a
/// node that refused to compile over one unfamiliar device would take a shop's receipt printer with
/// it.
///
/// A proposal with **no** connection is skipped, not published as a guess. That state is only
/// reachable for a row approved before ADR-0100 (the route now requires it), and publishing it as
/// `network` would silently disable the cash drawer on a USB printer. Skipping is visible: the
/// response says how many were published and how many were held back.
async fn admin_publish_devices<D, Cfg, A, C>(
    State(state): State<DevicePublishState<D, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishDevicesRequest>,
) -> Response
where
    D: DeviceProposalStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };

    let approved = match state
        .devices
        .list(tenant_id, Some(store_id), DeviceProposalStatus::Approved)
        .await
    {
        Ok(approved) => approved,
        Err(error) => {
            tracing::warn!(%error, "could not read the approved devices to publish");
            return service_unavailable("devices");
        }
    };
    let considered = approved.len();
    let node = compile_devices(&approved);
    let published_count = node.devices().len();

    let Ok(devices_value) = serde_json::to_value(&node) else {
        tracing::error!("could not serialise a compiled device node");
        return service_unavailable("devices");
    };

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        vec![("devices".to_owned(), devices_value)],
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "devices.publish",
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({
            "config_version_id": id.to_string(),
            "device_count": published_count,
            "skipped_count": considered.saturating_sub(published_count),
        })),
    )
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "config_version_id": id.to_string(),
            "device_count": published_count,
            "skipped_count": considered.saturating_sub(published_count),
        })),
    )
        .into_response()
}

/// Turns approved proposals into the published node.
///
/// The proposal's ULID becomes the device's — the approval is what turns a proposal into a device,
/// and reusing the identifier means the audit trail on the proposal and the device the store
/// addresses are the same thing, rather than two ids an operator has to correlate by hand.
///
/// A row with no connection is dropped rather than guessed at; see [`admin_publish_devices`].
fn compile_devices(approved: &[DeviceProposalSummary]) -> PublishedDevices {
    PublishedDevices::new(
        approved
            .iter()
            .filter_map(|row| {
                let device_id = row.id.parse::<Ulid>().ok().map(DeviceId::new)?;
                let connection = row.connection.as_deref()?;
                let station_id = match row.station_id.as_deref() {
                    None => None,
                    // A station id that will not parse drops the *station*, not the device: the
                    // printer still exists and still prints, it simply falls back to serving no
                    // station until the approval is corrected.
                    Some(raw) => raw.parse::<Ulid>().ok().map(StationId::new),
                };
                Some(PublishedDevice {
                    device_id,
                    kind: Open::parse(&device_kind_token(&row.kind)),
                    connection: Open::parse(&device_connection_token(connection)),
                    address: row.address.clone(),
                    name: DisplayName::new(row.name.as_str()),
                    station_id,
                })
            })
            .collect(),
    )
}

/// The node's prefixed token for a stored short kind name (`printer` → `DEVICE_KIND_PRINTER`).
///
/// A name this build does not know still produces a token, which `Open` then retains — the
/// forward-compatibility the node promises has to start here, at the one place the two spellings
/// meet, or an unfamiliar device is lost before it reaches the store.
fn device_kind_token(short: &str) -> String {
    format!("DEVICE_KIND_{}", short.to_ascii_uppercase())
}

/// As [`device_kind_token`], for a connection (`usb` → `DEVICE_CONNECTION_USB`).
fn device_connection_token(short: &str) -> String {
    format!("DEVICE_CONNECTION_{}", short.to_ascii_uppercase())
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };

    // Load the authoring rows. `list` is on all four seams, so each call is fully-qualified. The
    // version each row was read at is a writer's concern (ADR-0094); a compiled plan carries the
    // floor, not the console's edit history, so the versions are dropped here.
    let areas: Vec<Area> = match AreaStore::list(&state.floor, tenant_id, store_id).await {
        Ok(areas) => areas.into_iter().map(|area| area.record).collect(),
        Err(error) => return floor_error_response(&error),
    };
    let tables: Vec<Table> = match TableStore::list(&state.floor, tenant_id, store_id).await {
        Ok(tables) => tables.into_iter().map(|table| table.record).collect(),
        Err(error) => return floor_error_response(&error),
    };
    let stations: Vec<Station> = match StationStore::list(&state.floor, tenant_id, store_id).await {
        Ok(stations) => stations.into_iter().map(|station| station.record).collect(),
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
        return unprocessable_violations(&violations);
    }

    let (Ok(floor_value), Ok(stations_value)) = (
        serde_json::to_value(&floor_plan),
        serde_json::to_value(&station_plan),
    ) else {
        tracing::error!("could not serialise a compiled floor or station plan");
        return service_unavailable("floor");
    };

    // Set the `floor` and `stations` keys on the store's Store layer (index 2) and re-publish it,
    // preserving the other Store-level keys (`menu`, `layout`, `permissions`, capability flags).
    let nodes = vec![
        ("floor".to_owned(), floor_value),
        ("stations".to_owned(), stations_value),
    ];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
    // Minting a QR token is a read of the floor, not a write to it, so the version each row was
    // read at (ADR-0094) is dropped rather than carried into the printable sheet.
    let tokens = tables
        .into_iter()
        .map(|table| table.record)
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
    /// How many events the store had committed and not yet published as of its last heartbeat, or
    /// `null` if it has never reported one. The mirror of `relay_backlog`: that counts orders held
    /// *for* the store, this counts sales held *at* it. `null` is not zero — a store that never said
    /// is not a store that is caught up — so the console renders the two differently.
    outbox_depth: Option<u64>,
    /// Unix ms of the heartbeat that reported `outbox_depth`, or `null`.
    outbox_reported_at_ms: Option<i64>,
    /// The lease generation the box last reported holding
    /// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)), or `null` if it
    /// has never said.
    lease_generation_held: Option<u64>,
    /// Unix ms of the heartbeat that reported it, or `null`.
    lease_reported_at_ms: Option<i64>,
    /// The store's authoritative lease generation, or `null` if the cloud has never issued it one.
    lease_generation_authoritative: Option<u64>,
    /// Whether this box has been **superseded**: it holds a generation the store has moved past, so
    /// it refuses over-the-air updates and is no longer the machine the store runs on.
    ///
    /// Derived here rather than stored, exactly as `online` and `config_current` are: it is a
    /// comparison of two numbers the row already carries, and deriving it in one place keeps the
    /// console from re-implementing `lease_standing` in TypeScript. `false` when either number is
    /// absent — a store with no lease in force, or a box that has not reported one, is not
    /// *superseded*; it is simply not under the lease yet, and saying otherwise would put a red
    /// badge on every store the day this shipped.
    lease_superseded: bool,
    /// Where the machine holding the authoritative generation runs — the `EDGE_PLACEMENT_*` token,
    /// or `null` ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// `null` covers two different things on purpose, and the console must not read it as a mode:
    /// the cloud has never bumped this store, or the stored token is one this build cannot decode.
    /// Neither is `EDGE_PLACEMENT_UNSPECIFIED` — on the wire that token means *this message did not
    /// say*, and a server that emitted it would be saying something. A field that is simply absent
    /// is the honest shape for both.
    ///
    /// What the two absences mean for *urgency* is deliberately not on this view, because the
    /// console does not decide it: the alert engine reads the same `FleetRow` and scores an
    /// undecodable token with the hosted case rather than the in-store one.
    edge_placement: Option<&'static str>,
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
        // `lease_standing` is the domain's, and this is the console's read of it: a box behind its
        // store's authority has been replaced. A box *ahead* of it (`Invalid`) also refuses, and is
        // deliberately not shown as superseded — that is corruption or a restored backup, which a
        // "replaced" badge would mislabel; it surfaces as the two numbers disagreeing.
        let lease_superseded = matches!(
            (row.lease_generation_held, row.lease_generation_authoritative),
            (Some(held), Some(authoritative)) if held < authoritative
        );
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
            outbox_depth: row.outbox_depth,
            outbox_reported_at_ms: row
                .outbox_reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            lease_generation_held: row.lease_generation_held,
            lease_reported_at_ms: row
                .lease_reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            lease_generation_authoritative: row.lease_generation_authoritative,
            lease_superseded,
            edge_placement: row.edge_placement.as_wire(),
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
    api_error(ErrorStatus::Unavailable, "the fleet service is unavailable")
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
    let tenant = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant]) => TenantId::new(tenant),
        Err(refusal) => return refusal,
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
    let (tenant, store) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant, store]) => (TenantId::new(tenant), StoreId::new(store)),
            Err(refusal) => return refusal,
        };
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    match state.fleet.store_detail(tenant, store).await {
        Ok(Some(row)) => {
            (StatusCode::OK, Json(FleetStoreView::from_row(row, now_ms))).into_response()
        }
        Ok(None) => not_found("store"),
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
    api_error(ErrorStatus::Unavailable, "the alert service is unavailable")
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
    api_error(
        ErrorStatus::Unavailable,
        "the health service is unavailable",
    )
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

/// `GET /admin/campaigns/{id}/vouchers`: the tenant, plus the optional paging bounds.
///
/// `limit`/`offset` are declared here rather than flattened in from a shared struct — see
/// [`parse_page`] for why that is not a choice — but they are only *read* there, so the rule for what
/// they accept lives in one place even though the field names are repeated per route.
#[derive(Debug, Clone, Deserialize)]
struct VoucherListQuery {
    /// The tenant whose vouchers to list (a 26-character ULID).
    tenant_id: String,
    /// How many codes to return. **Absent means unpaged**: every code, as an array.
    #[serde(default)]
    limit: Option<String>,
    /// How many codes to skip. Only meaningful with `limit`.
    #[serde(default)]
    offset: Option<String>,
}

/// `GET /admin/catalog/items`: the tenant, plus the optional paging bounds.
///
/// The `limit`/`offset` pair is repeated per route for the reason [`VoucherListQuery`] gives.
#[derive(Debug, Clone, Deserialize)]
struct ItemListQuery {
    /// The tenant whose items to list (a 26-character ULID).
    tenant_id: String,
    /// How many items to return. **Absent means unpaged**: the whole item master, as an array —
    /// which is what the menu compiler and five of the six console pickers ask for.
    #[serde(default)]
    limit: Option<String>,
    /// How many items to skip. Only meaningful with `limit`.
    #[serde(default)]
    offset: Option<String>,
    /// A case-insensitive substring the item's name or any per-locale name must contain.
    ///
    /// Only meaningful with `limit`: on the unpaged read it is ignored, and the route says so by
    /// refusing rather than by quietly returning the whole master.
    #[serde(default)]
    q: Option<String>,
    /// Which order to return the page in — one of [`ItemSort::tokens`]. Only meaningful with `limit`.
    #[serde(default)]
    sort: Option<String>,
    /// `asc` or `desc`, inverting `sort`'s natural direction. Only meaningful with `limit`.
    #[serde(default)]
    order: Option<String>,
}

/// `GET /admin/media`: the tenant, plus the optional paging bounds.
///
/// The `limit`/`offset` pair is repeated per route for the reason [`VoucherListQuery`] gives.
#[derive(Debug, Clone, Deserialize)]
struct MediaListQuery {
    /// The tenant whose assets to list (a 26-character ULID).
    tenant_id: String,
    /// How many summaries to return. **Absent means unpaged**: every asset, as an array — which is
    /// what the item image picker asks for.
    #[serde(default)]
    limit: Option<String>,
    /// How many summaries to skip. Only meaningful with `limit`.
    #[serde(default)]
    offset: Option<String>,
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
    api_error(
        ErrorStatus::Unavailable,
        "the registry service is unavailable",
    )
}

/// The `503` returned when OS entropy is unavailable to mint an id.
fn registry_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a registry id");
    api_error(
        ErrorStatus::Unavailable,
        "the registry service is unavailable",
    )
}

/// Parses a status word from a request body; `None` (a `400`) for anything but the two known values.
fn parse_entity_status(value: &str) -> Option<EntityStatus> {
    EntityStatus::ALL
        .iter()
        .copied()
        .find(|status| status.as_str() == value)
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
        Ok(tenants) => (StatusCode::OK, Json(tenants)).into_response(),
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = TenantRecord {
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_tenant(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("tenant"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.registry.list_brands(tenant_id).await {
        Ok(brands) => (StatusCode::OK, Json(brands)).into_response(),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (brand_id, tenant_id) =
        match parse_ulid_fields([("brand_id", &brand_id), ("tenant_id", &request.tenant_id)]) {
            Ok([brand_id, tenant_id]) => (BrandId::new(brand_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = BrandRecord {
        brand_id,
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_brand(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("brand"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.registry.list_stores(tenant_id).await {
        Ok(stores) => (StatusCode::OK, Json(stores)).into_response(),
        Err(error) => registry_error_response(&error),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Ok(brand_id) = parse_optional_ulid(request.brand_id.as_deref(), BrandId::new) else {
        return ulid_refusal(&["brand_id"]);
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (store_id, tenant_id) =
        match parse_ulid_fields([("store_id", &store_id), ("tenant_id", &request.tenant_id)]) {
            Ok([store_id, tenant_id]) => (StoreId::new(store_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    let Ok(brand_id) = parse_optional_ulid(request.brand_id.as_deref(), BrandId::new) else {
        return ulid_refusal(&["brand_id"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = StoreRecord {
        store_id,
        tenant_id,
        brand_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_store(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("store"),
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    match state.registry.list_devices(tenant_id, store_id).await {
        Ok(devices) => (StatusCode::OK, Json(devices)).into_response(),
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (tenant_id, store_id, device_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &store_id),
        ("device_id", &device_id),
    ]) {
        Ok([tenant_id, store_id, device_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            DeviceId::new(device_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = DeviceRecord {
        device_id,
        tenant_id,
        store_id,
        name: request.name,
        kind: request.kind,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_device(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("device"),
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
            get(admin_list_layout_buttons::<Cat, A, C>)
                .post(admin_create_layout_button::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/layout-buttons/{sales_channel}/{menu_item_id}",
            axum::routing::put(admin_update_layout_button::<Cat, A, C>)
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
            get(admin_list_placements::<Cat, A, C>).post(admin_create_placement::<Cat, A, C>),
        )
        .route(
            "/admin/catalog/menus/{menu_id}/placements/{menu_item_id}",
            axum::routing::put(admin_update_placement::<Cat, A, C>)
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

/// A `POST /admin/catalog/layout-buttons` body: a button's presentation plus the `(channel, item)`
/// slot it goes in, which the collection URI has no room for and the `PUT` takes from its path.
///
/// The identity fields are spelled out here rather than `#[serde(flatten)]`-ing the update body:
/// flatten pulls in serde's buffering `Content` enum, whose `f32`/`f64` variants `clippy.toml` bans
/// outright (`docs/adr/0013`: money is never a float). `into_write` keeps the two shapes from
/// drifting instead.
#[derive(Debug, Clone, Deserialize)]
struct CreateLayoutButtonRequest {
    sales_channel: String,
    menu_item_id: String,
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

impl CreateLayoutButtonRequest {
    /// The presentation half, as the shared builder takes it.
    fn into_write(self) -> SetLayoutButtonRequest {
        SetLayoutButtonRequest {
            tenant_id: self.tenant_id,
            display_category_id: self.display_category_id,
            display_subcategory_id: self.display_subcategory_id,
            label: self.label,
            grid_column: self.grid_column,
            grid_row: self.grid_row,
            sort: self.sort,
        }
    }
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

/// A `POST /admin/catalog/menus/{menu_id}/placements` body: a placement plus the `(menu, item)` pair
/// it is keyed by. The menu is also in the path; both are read from the body so this route and the
/// `PUT` share one builder.
///
/// Flat rather than `#[serde(flatten)]` for the same reason as [`CreateLayoutButtonRequest`].
#[derive(Debug, Clone, Deserialize)]
struct CreatePlacementRequest {
    menu_id: String,
    menu_item_id: String,
    tenant_id: String,
    #[serde(default)]
    menu_section_id: Option<String>,
    prices: Vec<ChannelPrice>,
    available: bool,
}

impl CreatePlacementRequest {
    /// The placement half, as the shared builder takes it.
    fn into_write(self) -> SetPlacementRequest {
        SetPlacementRequest {
            tenant_id: self.tenant_id,
            menu_section_id: self.menu_section_id,
            prices: self.prices,
            available: self.available,
        }
    }
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
    api_error(
        ErrorStatus::Unavailable,
        "the catalog service is unavailable",
    )
}

/// The `503` returned when OS entropy is unavailable to mint a catalog id.
fn catalog_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a catalog id");
    api_error(
        ErrorStatus::Unavailable,
        "the catalog service is unavailable",
    )
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

/// One authored tax-rate row on the wire: the class id, the channel's wire token, the rate in basis
/// points (10% is `1000`), and how that rate is broken out on the invoice.
#[derive(Debug, Clone, serde::Serialize)]
struct TaxRateView {
    tax_class_id: String,
    sales_channel: String,
    rate_bps: u32,
    /// The named parts the invoice prints, which must sum to `rate_bps`
    /// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)). Empty for a rate
    /// printed as one line, which is most of the world.
    components: Vec<TaxComponentView>,
}

/// One named part of a rate, on the wire.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct TaxComponentView {
    /// What the invoice calls it — `CGST`, `SGST`, `IGST`.
    name: String,
    /// This part's share, in basis points.
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
    /// How the rate is broken out on the invoice. `#[serde(default)]`, so a console that predates
    /// ADR-0104 still saves a grid and every row keeps printing as one line.
    #[serde(default)]
    components: Vec<TaxComponentView>,
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
        components: entry
            .components
            .iter()
            .map(|component| TaxComponentView {
                name: component.name.clone(),
                rate_bps: component.rate.basis_points(),
            })
            .collect(),
    }
}

/// Checks one row's breakdown and returns the components to store
/// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
///
/// The invariant is that the parts sum to the row's own rate, and it is checked **here**, where the
/// table is authored, rather than at the till. A row that fails it would print an invoice whose
/// lines do not add up to the tax charged — which is the one way this feature can produce a document
/// an auditor rejects, and the refusal says which row and by how much so the operator can fix it
/// rather than hunt for it.
///
/// An empty list always passes: it is "no breakdown", not "a breakdown summing to zero".
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn checked_components(row: &TaxRateRowRequest) -> Result<Vec<TaxComponent>, Response> {
    if row.components.is_empty() {
        return Ok(Vec::new());
    }
    if row
        .components
        .iter()
        .any(|component| component.name.trim().is_empty())
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a tax component has no name, and the name is what the invoice prints",
            &[("rates", "MISSING_FIELD")],
        ));
    }
    let total: u64 = row
        .components
        .iter()
        .map(|component| u64::from(component.rate_bps))
        .sum();
    if total != u64::from(row.rate_bps) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "a rate's components sum to {total} basis points but the rate is {}; the invoice \
                 would print lines that do not add up to the tax charged",
                row.rate_bps
            )
            .as_str(),
            &[("rates", "OUT_OF_RANGE")],
        ));
    }
    Ok(row
        .components
        .iter()
        .map(|component| {
            TaxComponent::new(
                component.name.trim(),
                TaxRate::from_basis_points(component.rate_bps),
            )
        })
        .collect())
}

/// Validates a whole submitted grid into the entries to store, or the one refusal that stops it.
///
/// Every check is on the row rather than on the table: an unknown class or channel, a rate above
/// 100 %, a `(class, channel)` pair submitted twice, and a breakdown that does not sum to its own
/// rate. The first failure returns, because a partly-applied grid is not a state this resource has —
/// a save replaces the tenant's whole table or none of it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn checked_entries(
    rows: &[TaxRateRowRequest],
    known: &BTreeSet<TaxClassId>,
) -> Result<Vec<TaxRateEntry>, Response> {
    let mut entries = Vec::with_capacity(rows.len());
    let mut seen: BTreeSet<(TaxClassId, SalesChannel)> = BTreeSet::new();
    for row in rows {
        let tax_class_id = match parse_ulid_fields([("tax_class_id", &row.tax_class_id)]) {
            Ok([tax_class_id]) => TaxClassId::new(tax_class_id),
            Err(refusal) => return Err(refusal),
        };
        if !known.contains(&tax_class_id) {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tax rate names an unknown tax class",
                &[("rates", "UNKNOWN_REFERENCE")],
            ));
        }
        let Some(sales_channel) = SalesChannel::from_wire(&row.sales_channel) else {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tax rate names an unknown sales channel",
                &[("rates", "INVALID_ENUM_VALUE")],
            ));
        };
        if row.rate_bps > MAX_TAX_RATE_BPS {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tax rate exceeds 100%",
                &[("rates", "OUT_OF_RANGE")],
            ));
        }
        if !seen.insert((tax_class_id, sales_channel)) {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a (tax class, channel) pair is repeated",
                &[("rates", "DUPLICATE")],
            ));
        }
        entries.push(
            TaxRateEntry::new(
                tax_class_id,
                sales_channel,
                TaxRate::from_basis_points(row.rate_bps),
            )
            .with_components(checked_components(row)?),
        );
    }
    Ok(entries)
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.tax_rates.list_tax_rates(tenant_id).await {
        Ok((rows, version)) => {
            let view: Vec<TaxRateView> = rows.iter().map(tax_rate_view).collect();
            let response = (StatusCode::OK, Json(view)).into_response();
            // The collection is the entity, so its version rides on the response's `ETag` rather
            // than inside the body — the body is a JSON array, which has nowhere to put a field
            // ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)). A tenant
            // that has never saved rates has no version, and sends `If-Match: *` to say so.
            match version {
                Some(version) => with_etag(response, &version),
                None => response,
            }
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // The version the console read the grid at, or `None` from `If-Match: *` for a tenant that has
    // never saved rates ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
    let expected = match if_match_collection(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let known: BTreeSet<TaxClassId> = match state.catalog.list_tax_classes(tenant_id).await {
        Ok(classes) => classes
            .iter()
            .map(|class| class.record.tax_class_id)
            .collect(),
        Err(error) => return catalog_error_response(&error),
    };
    let entries = match checked_entries(&request.rates, &known) {
        Ok(entries) => entries,
        Err(refusal) => return refusal,
    };
    match state
        .tax_rates
        .set_tax_rates(tenant_id, &entries, expected.as_ref())
        .await
    {
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        // No rate table for this tenant to replace, and the caller named a version for one. There is
        // no other edit to have lost, so a `412` telling them to reload would send them looking for
        // a conflict nobody caused.
        Ok(UpdateOutcome::NotFound) => not_found("tax rate table"),
        Ok(UpdateOutcome::Updated(version)) => {
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
            with_etag((StatusCode::OK, Json(view)).into_response(), &version)
        }
        Err(error) => tax_rate_error_response(&error),
    }
}

/// Maps a tax-rate store failure to a retryable `503`, logging the detail rather than leaking it.
fn tax_rate_error_response(error: &TaxRateStoreError) -> Response {
    tracing::error!(%error, "a tax-rate store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the tax-rate service is unavailable",
    )
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
) -> Result<PublishedCampaign, FieldRefusal> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(FieldRefusal::new("name is required", "name", "REQUIRED"));
    }
    // Split, where this used to be one `if` ORing both bounds and naming neither. The old message
    // was the ULID-slice pathology in its worse form: not "names every field it looked at" but
    // "names none of them", so a caller who sent both minutes could not tell which was refused.
    if let Some(schedule) = &request.conditions.schedule {
        if schedule.start_minute >= MINUTES_PER_DAY {
            return Err(FieldRefusal::new(
                "a schedule start minute must be within the day (0..1440)",
                "conditions.schedule.start_minute",
                "OUT_OF_RANGE",
            ));
        }
        if schedule.end_minute >= MINUTES_PER_DAY {
            return Err(FieldRefusal::new(
                "a schedule end minute must be within the day (0..1440)",
                "conditions.schedule.end_minute",
                "OUT_OF_RANGE",
            ));
        }
    }
    if let Some(channels) = &request.conditions.channels
        && channels
            .iter()
            .any(|channel| channel.is_unrecognised() || channel.is_unspecified())
    {
        // The collection, not an index: no shipped detail in this file names an element position,
        // and the tax-rate loop sets the precedent for naming the list.
        return Err(FieldRefusal::new(
            "a campaign names an unknown sales channel",
            "conditions.channels",
            "INVALID_ENUM_VALUE",
        ));
    }
    match request.action {
        PublishedAction::Percentage { rate } if rate.numerator() < 0 => {
            return Err(FieldRefusal::new(
                "a percentage rate cannot be negative",
                "action.rate",
                "OUT_OF_RANGE",
            ));
        }
        PublishedAction::AmountOff { amount } if amount.is_negative() => {
            return Err(FieldRefusal::new(
                "an amount off cannot be negative",
                "action.amount",
                "OUT_OF_RANGE",
            ));
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(campaign)) => (StatusCode::OK, Json(campaign)).into_response(),
        Ok(None) => not_found("campaign"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(campaign_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(CampaignId::new)
    else {
        return campaign_entropy_unavailable();
    };
    let campaign = match build_campaign(&request, campaign_id) {
        Ok(campaign) => campaign,
        Err(refusal) => return refusal.into_response(),
    };
    match state.campaigns.create_campaign(tenant_id, &campaign).await {
        Ok(CreateOutcome::Created(version)) => {
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
            versioned_created(campaign, &version)
        }
        // Unreachable while the id is minted here rather than sent by the caller, and answered
        // anyway: the seam cannot know that, and a route that mints its own id today may take one
        // tomorrow. Silently overwriting is what this slice removed.
        Ok(CreateOutcome::AlreadyExists) => already_exists("campaign"),
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
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    let before = match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return not_found("campaign"),
        Err(error) => return campaign_error_response(&error),
    };
    let campaign = match build_campaign(&request, campaign_id) {
        Ok(campaign) => campaign,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .campaigns
        .update_campaign(tenant_id, &campaign, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "campaign.update",
                "campaign",
                &campaign_id.to_string(),
                serde_json::to_value(CampaignAuditSummary::of(&before.record)).ok(),
                serde_json::to_value(CampaignAuditSummary::of(&campaign)).ok(),
            )
            .await;
            versioned_ok(campaign, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("campaign"),
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
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    let before = match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return not_found("campaign"),
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
                serde_json::to_value(CampaignAuditSummary::of(&before.record)).ok(),
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
    api_error(
        ErrorStatus::Unavailable,
        "the campaign service is unavailable",
    )
}

/// Maps a campaign store failure to a retryable `503`, logging the detail rather than leaking it.
fn campaign_error_response(error: &CampaignStoreError) -> Response {
    tracing::error!(%error, "a campaign store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the campaign service is unavailable",
    )
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

/// A `POST /admin/inventory/recipes` body: a recipe plus the item it makes.
///
/// The item id is in the body here because the collection URI has no room for it, and the `PUT`
/// keeps taking it from the path. A struct of its own rather than an `Option` on `RecipeRequest`,
/// so neither route carries a field the other requires — and flat rather than
/// `#[serde(flatten)]` for the same reason as [`CreateLayoutButtonRequest`].
#[derive(Debug, Clone, Deserialize)]
struct CreateRecipeRequest {
    item_id: String,
    tenant_id: String,
    #[serde(default)]
    lines: Vec<PublishedRecipeLine>,
    #[serde(default)]
    auto_86_threshold: i64,
}

impl CreateRecipeRequest {
    /// The recipe half, as the shared handler body takes it.
    fn into_write(self) -> RecipeRequest {
        RecipeRequest {
            tenant_id: self.tenant_id,
            lines: self.lines,
            auto_86_threshold: self.auto_86_threshold,
        }
    }
}

/// The body of a recipe write. The item it makes is the URL key on the `PUT` — a recipe references
/// an existing menu item or modifier, so its id is client-owned, unlike an ingredient's
/// server-minted id. `lines` (the bill of materials) and `auto_86_threshold` are the wire recipe's
/// own fields.
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
            get(admin_list_recipes::<Inv, A, C>).post(admin_create_recipe::<Inv, A, C>),
        )
        .route(
            "/admin/inventory/recipes/{item_id}",
            get(admin_get_recipe::<Inv, A, C>)
                .put(admin_update_recipe::<Inv, A, C>)
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
) -> Result<PublishedIngredient, FieldRefusal> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(FieldRefusal::new("name is required", "name", "REQUIRED"));
    }
    if request.unit.is_unspecified() || request.unit.is_unrecognised() {
        // `INVALID_ENUM_VALUE`, not `REQUIRED`: `unit` carries no `#[serde(default)]`, so an absent
        // key never reaches here — what does is a present token that is unrecognised or explicitly
        // `UNSPECIFIED`, and both are enum-value faults.
        return Err(FieldRefusal::new(
            "an ingredient names an unknown unit of measure",
            "unit",
            "INVALID_ENUM_VALUE",
        ));
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
) -> Result<PublishedRecipe, FieldRefusal> {
    if request.auto_86_threshold < 0 {
        return Err(FieldRefusal::new(
            "an auto-86 threshold cannot be negative",
            "auto_86_threshold",
            "OUT_OF_RANGE",
        ));
    }
    if request
        .lines
        .iter()
        .any(|line| line.per_unit.as_milli() <= 0)
    {
        return Err(FieldRefusal::new(
            "a recipe line must consume a positive amount",
            "lines",
            "OUT_OF_RANGE",
        ));
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
) -> Result<PublishedSupplier, FieldRefusal> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(FieldRefusal::new("name is required", "name", "REQUIRED"));
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
    let (tenant_id, ingredient_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("ingredient_id", &ingredient_id),
    ]) {
        Ok([tenant_id, ingredient_id]) => {
            (TenantId::new(tenant_id), IngredientId::new(ingredient_id))
        }
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .get_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(Some(row)) => versioned_ok(row.record, &row.etag),
        Ok(None) => not_found("ingredient"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(ingredient_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(IngredientId::new)
    else {
        return inventory_entropy_unavailable();
    };
    let ingredient = match build_ingredient(&request, ingredient_id) {
        Ok(ingredient) => ingredient,
        Err(refusal) => return refusal.into_response(),
    };
    match state
        .inventory
        .create_ingredient(tenant_id, &ingredient)
        .await
    {
        Ok(CreateOutcome::Created(version)) => {
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
            versioned_created(ingredient, &version)
        }
        // Unreachable while the id is minted here rather than sent by the caller, and answered
        // anyway — the seam cannot know that, and silently overwriting is what this slice removed.
        Ok(CreateOutcome::AlreadyExists) => already_exists("ingredient"),
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
    let (tenant_id, ingredient_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("ingredient_id", &ingredient_id),
    ]) {
        Ok([tenant_id, ingredient_id]) => {
            (TenantId::new(tenant_id), IngredientId::new(ingredient_id))
        }
        Err(refusal) => return refusal,
    };
    let before = match state
        .inventory
        .get_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("ingredient");
    };
    let ingredient = match build_ingredient(&request, ingredient_id) {
        Ok(ingredient) => ingredient,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .update_ingredient(tenant_id, &ingredient, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(ingredient, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("ingredient"),
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
    let (tenant_id, ingredient_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("ingredient_id", &ingredient_id),
    ]) {
        Ok([tenant_id, ingredient_id]) => {
            (TenantId::new(tenant_id), IngredientId::new(ingredient_id))
        }
        Err(refusal) => return refusal,
    };
    let before = match state
        .inventory
        .get_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("ingredient");
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
    let (tenant_id, item) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("item_id", &item_id)]) {
            Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
            Err(refusal) => return refusal,
        };
    match state.inventory.get_recipe(tenant_id, item).await {
        Ok(Some(row)) => versioned_ok(row.record, &row.etag),
        Ok(None) => not_found("recipe"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin creates the recipe for an item, refusing if that item already has one.
///
/// The route this replaced was a single `PUT` upsert that re-derived create-versus-update at
/// runtime: it listed the tenant's recipes to pick between `201`/`200` and between the
/// `create`/`update` audit action, then wrote unconditionally. That read raced its own write — a
/// concurrent create or delete in between made *both* the status code and the audit entry wrong —
/// and a caller asking to add a recipe silently replaced the bill of materials of one already
/// there. [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md) §(ii).
async fn admin_create_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateRecipeRequest>,
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
    let (tenant_id, item) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("item_id", &request.item_id),
    ]) {
        Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
        Err(refusal) => return refusal,
    };
    let recipe = match build_recipe(&request.into_write(), item) {
        Ok(recipe) => recipe,
        Err(refusal) => return refusal.into_response(),
    };
    match state.inventory.create_recipe(tenant_id, &recipe).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.recipe.create",
                "recipe",
                &item.to_string(),
                None,
                serde_json::to_value(RecipeAuditSummary::of(&recipe)).ok(),
            )
            .await;
            versioned_created(recipe, &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("recipe"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin replaces the recipe for the path item, only at the version they read.
async fn admin_update_recipe<Inv, A, C>(
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
    let (tenant_id, item) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("item_id", &item_id)]) {
            Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
            Err(refusal) => return refusal,
        };
    // Read for the audit's "before" only. It no longer decides the status code or the action name:
    // the version comparison in the write does that, so a race here can no longer mislabel either.
    let before = match state.inventory.get_recipe(tenant_id, item).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let recipe = match build_recipe(&request, item) {
        Ok(recipe) => recipe,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .update_recipe(tenant_id, &recipe, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.recipe.update",
                "recipe",
                &item.to_string(),
                before
                    .as_ref()
                    .and_then(|r| serde_json::to_value(RecipeAuditSummary::of(r)).ok()),
                serde_json::to_value(RecipeAuditSummary::of(&recipe)).ok(),
            )
            .await;
            versioned_ok(recipe, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("recipe"),
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
    let (tenant_id, item) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("item_id", &item_id)]) {
            Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
            Err(refusal) => return refusal,
        };
    let before = match state.inventory.get_recipe(tenant_id, item).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("recipe");
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
    let (tenant_id, supplier_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("supplier_id", &supplier_id),
    ]) {
        Ok([tenant_id, supplier_id]) => (TenantId::new(tenant_id), SupplierId::new(supplier_id)),
        Err(refusal) => return refusal,
    };
    match state.inventory.get_supplier(tenant_id, supplier_id).await {
        Ok(Some(row)) => versioned_ok(row.record, &row.etag),
        Ok(None) => not_found("supplier"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(supplier_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(SupplierId::new)
    else {
        return inventory_entropy_unavailable();
    };
    let supplier = match build_supplier(&request, supplier_id) {
        Ok(supplier) => supplier,
        Err(refusal) => return refusal.into_response(),
    };
    match state.inventory.create_supplier(tenant_id, &supplier).await {
        Ok(CreateOutcome::Created(version)) => {
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
            versioned_created(supplier, &version)
        }
        // Unreachable while the id is minted here rather than sent by the caller, and answered
        // anyway — the seam cannot know that, and silently overwriting is what this slice removed.
        Ok(CreateOutcome::AlreadyExists) => already_exists("supplier"),
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
    let (tenant_id, supplier_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("supplier_id", &supplier_id),
    ]) {
        Ok([tenant_id, supplier_id]) => (TenantId::new(tenant_id), SupplierId::new(supplier_id)),
        Err(refusal) => return refusal,
    };
    let before = match state.inventory.get_supplier(tenant_id, supplier_id).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("supplier");
    };
    let supplier = match build_supplier(&request, supplier_id) {
        Ok(supplier) => supplier,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .update_supplier(tenant_id, &supplier, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(supplier, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("supplier"),
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
    let (tenant_id, supplier_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("supplier_id", &supplier_id),
    ]) {
        Ok([tenant_id, supplier_id]) => (TenantId::new(tenant_id), SupplierId::new(supplier_id)),
        Err(refusal) => return refusal,
    };
    let before = match state.inventory.get_supplier(tenant_id, supplier_id).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("supplier");
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
    api_error(
        ErrorStatus::Unavailable,
        "the inventory service is unavailable",
    )
}

/// Maps an inventory store failure to a retryable `503`, logging the detail rather than leaking it.
fn inventory_error_response(error: &InventoryStoreError) -> Response {
    tracing::error!(%error, "an inventory store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the inventory service is unavailable",
    )
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
    // A publish wants the records, not the versions the read saw: the node is assembled from what is
    // authored now, and a conditional write against any one row would say nothing about the set.
    Ok((
        records(inventory.list_ingredients(tenant_id).await?),
        records(inventory.list_recipes(tenant_id).await?),
        records(inventory.list_suppliers(tenant_id).await?),
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

/// Drops the version a config-tree read was handed, keeping the state.
///
/// A read of the tree wants the four layers and the history; the version the row was read at is a
/// *writer's* precondition ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
/// Read paths say so by calling this, rather than silently discarding a field.
pub(crate) fn strip_tree_version(
    loaded: Option<Versioned<ConfigTreeState>>,
) -> Option<ConfigTreeState> {
    loaded.map(|versioned| versioned.record)
}

/// A store's config tree as it was read, with the version the store handed out.
///
/// The two travel together because a conditional write needs both: the state to compose the next
/// publish from, and the version to write against.
struct LoadedTree {
    state: Option<ConfigTreeState>,
    version: Option<Version>,
}

/// Loads a store's tree for a write, keeping the version it was read at.
///
/// Split from [`publish_store_layer`] so each route can compose its own layer in between, which is
/// the only part of the twelve node-publish routes that actually differs.
async fn load_tree_for_write<Cfg>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<LoadedTree, Response>
where
    Cfg: ConfigTreeStore,
{
    match config_trees.load(tenant_id, store_id).await {
        Ok(Some(versioned)) => Ok(LoadedTree {
            state: Some(versioned.record),
            version: Some(versioned.etag),
        }),
        Ok(None) => Ok(LoadedTree {
            state: None,
            version: None,
        }),
        Err(error) => Err(config_store_error_response(&error)),
    }
}

/// How many times a write that composes on a read will re-read and re-apply before giving up.
///
/// Bounded so a pathological hot store cannot spin. Three is far beyond what a console's publish
/// rate can lose; exhausting them means the caller is contending with something other than a racing
/// operator, and a `412` is then the honest answer
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
const CONDITIONAL_WRITE_ATTEMPTS: u8 = 3;

/// Sets `nodes` on a store's tree at `level` and saves it, retrying if another publish lands first.
///
/// Twelve routes used to hand-roll the load→compose→publish→save cycle, and each copy was a place
/// the conditional write could be forgotten. It is one function now
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
///
/// # Why this retries instead of refusing
///
/// A layer is a **map of nodes**, and this writes only the keys in `nodes`. Two publishes of
/// *different* nodes — a menu and a floor, say — do not conflict in any way an operator would
/// recognise: nothing either of them authored is lost. What the old code lost was the *document*,
/// because each publish wrote back a whole tree composed from a stale read.
///
/// So the precondition here is the store's, evaluated at write time, and losing it is not the
/// caller's problem: reload, re-apply this write's own keys onto what is now there, and save again.
/// Both nodes land and neither operator sees a conflict. Refusing instead would hand someone a
/// "the configuration changed" error for an edit that never touched their key, which is friction
/// with no correctness behind it.
///
/// Concurrent edits to the *same* node are a different question, and they are answered a layer up:
/// the authoring records a node is compiled from are themselves conditionally written (slices 2–4),
/// so two people cannot silently overwrite each other's catalog or floor edits. By the time a
/// publish runs, it is compiling committed data.
///
/// The two routes where a human edits the document itself — `PUT /admin/config` and the rollback —
/// are the exception and go through [`publish_authored_layer`] instead.
async fn publish_config_nodes<Cfg, C>(
    config_trees: &Cfg,
    clock: &C,
    tenant_id: TenantId,
    store_id: StoreId,
    level: ConfigLevel,
    nodes: Vec<(String, serde_json::Value)>,
) -> Result<ConfigVersionId, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let mut last_conflict = false;
    for _ in 0..CONDITIONAL_WRITE_ATTEMPTS {
        let loaded = load_tree_for_write(config_trees, tenant_id, store_id).await?;
        let layer = layer_with_nodes(loaded.state.as_ref(), level, &nodes);
        let mut tree = match loaded.state {
            Some(existing) => ConfigTree::from_state(store_id, CapabilityValidator, existing),
            None => ConfigTree::new(store_id, CapabilityValidator),
        };
        let Some(version_id) = mint_version_id(clock.now().as_milliseconds_since_epoch()) else {
            tracing::error!("could not read OS entropy to mint a config version id");
            return Err(service_unavailable("configuration"));
        };
        let id = match tree.publish(level, layer, version_id) {
            Ok(id) => id,
            Err(ConfigError::Invalid(violations)) => {
                return Err(unprocessable_violations(&violations));
            }
        };
        match config_trees
            .save(tenant_id, store_id, &tree.state(), loaded.version.as_ref())
            .await
        {
            Ok(UpdateOutcome::Updated(_)) => return Ok(id),
            Ok(UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => {
                last_conflict = true;
                tracing::debug!("a concurrent config publish won; re-applying this node");
            }
            Err(error) => return Err(config_store_error_response(&error)),
        }
    }
    if last_conflict {
        tracing::warn!(
            "a config publish lost {CONDITIONAL_WRITE_ATTEMPTS} races in a row and gave up"
        );
    }
    Err(version_mismatch())
}

/// Sets `nodes` on the layer at `level`, preserving every other key.
///
/// A missing or non-object prior layer starts from an empty object, so a store's first publish and a
/// corrupt layer both compose cleanly.
fn layer_with_nodes(
    state_before: Option<&ConfigTreeState>,
    level: ConfigLevel,
    nodes: &[(String, serde_json::Value)],
) -> serde_json::Value {
    let mut layer = state_before.map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layer(level).clone(),
    );
    if !layer.is_object() {
        layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut layer {
        for (key, value) in nodes {
            map.insert(key.clone(), value.clone());
        }
    }
    layer
}

/// Publishes a whole authored `layer` at `level`, refusing a write made against a stale read.
///
/// This is the other half of [`publish_config_nodes`], and the difference is what the caller is
/// doing. A node publish sets one key and commutes with the others, so it retries. This one replaces
/// a layer an operator *typed* — every key in it, including keys they never looked at — so a second
/// writer really does destroy work. That earns a refusal the operator has to see, carrying the
/// `ConfigVersionId` they were editing so the history screen can show them what changed.
///
/// Used only by `PUT /admin/config` and `POST /admin/config/versions/{id}/restore`.
async fn publish_authored_layer<Cfg, C>(
    config_trees: &Cfg,
    clock: &C,
    headers: &HeaderMap,
    tenant_id: TenantId,
    store_id: StoreId,
    loaded: LoadedTree,
    level: ConfigLevel,
    layer: serde_json::Value,
) -> Result<ConfigVersionId, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let mut tree = match loaded.state {
        Some(existing) => ConfigTree::from_state(store_id, CapabilityValidator, existing),
        None => ConfigTree::new(store_id, CapabilityValidator),
    };
    if_match_config(headers, tree.current_version())?;
    let Some(version_id) = mint_version_id(clock.now().as_milliseconds_since_epoch()) else {
        tracing::error!("could not read OS entropy to mint a config version id");
        return Err(service_unavailable("configuration"));
    };
    match tree.publish(level, layer, version_id) {
        Ok(id) => match config_trees
            .save(tenant_id, store_id, &tree.state(), loaded.version.as_ref())
            .await
        {
            // The `If-Match` above compared against this handler's own read, which another publish
            // may already have overtaken. This is the precondition the store evaluates at write
            // time, and it is the one that actually prevents the interleave.
            Ok(UpdateOutcome::Updated(_)) => Ok(id),
            Ok(UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => Err(version_mismatch()),
            Err(error) => Err(config_store_error_response(&error)),
        },
        Err(ConfigError::Invalid(violations)) => Err(unprocessable_violations(&violations)),
    }
}

/// The `If-Match` a config publish must carry, compared against the version the tree holds.
///
/// A store that has never been published to has no current version, and says so: `If-Match: *` is
/// how a caller asserts "there is nothing here yet". That is the one place a wildcard is accepted in
/// this tree — the record-shaped routes refuse it, because for them it means "overwrite whatever is
/// there", which is the behaviour ADR-0094 exists to remove. Here it means the opposite: *only* if
/// nothing is there.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
fn if_match_config(headers: &HeaderMap, current: Option<ConfigVersionId>) -> Result<(), Response> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "this write must carry the version it was read at, as an If-Match header",
            &[("if-match", "REQUIRED")],
        ));
    };
    let Ok(raw) = raw.to_str() else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be a strong entity-tag",
            &[("if-match", "INVALID_FORMAT")],
        ));
    };
    let trimmed = raw.trim();

    let Some(current) = current else {
        return if trimmed == "*" {
            Ok(())
        } else {
            Err(version_mismatch())
        };
    };
    match parse_entity_tag(trimmed) {
        Ok(expected) if expected.as_str() == current.to_string() => Ok(()),
        // A wildcard here claims the store has never been published to, and it has: same refusal as
        // naming the wrong version, because it is the same wrong claim about the same tree.
        Ok(_) | Err(EntityTagRefusal::Wildcard) => Err(version_mismatch()),
        Err(EntityTagRefusal::Malformed) => Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be a strong entity-tag",
            &[("if-match", "INVALID_FORMAT")],
        )),
    }
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
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
        return service_unavailable("inventory");
    };

    // Set the `inventory` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys (`menu`, `tax`, `campaigns`, `permissions`, `floor`, capability flags).
    let nodes = vec![("inventory".to_owned(), inventory_value)];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => records(campaigns),
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(campaigns_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node");
        return service_unavailable("campaign");
    };

    // Set the `campaigns` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys (`menu`, `tax`, `permissions`, `floor`, capability flags).
    let nodes = vec![("campaigns".to_owned(), campaigns_value)];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => records(campaigns),
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(campaigns_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node");
        return service_unavailable("campaign");
    };
    let loaded = match load_tree_for_write(&state.config_trees, tenant_id, store_id).await {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    let state_before = &loaded.state;
    match preview_config_node(state_before.as_ref(), "campaigns", campaigns_value) {
        Ok(preview) => (StatusCode::OK, Json(preview)).into_response(),
        Err(violations) => unprocessable_violations(&violations),
    }
}

// --- OTA rollout levers (`/admin/config/ota`, ADR-0078, Track O3) --------------------------------

/// The collaborators the OTA rollout routes need: the config-tree store the `fleet_update` node is
/// written onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct OtaConfigState<Cfg, A, C, L> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    releases: L,
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
/// Also serves `GET /admin/config/ota/placement` — the same `(tenant, store)` question about the
/// other OTA node.
#[derive(Debug, Clone, Deserialize)]
struct OtaRolloutQuery {
    tenant_id: String,
    store_id: String,
}

/// A `POST /admin/config/lease/bump` body: the `(tenant, store)` whose next generation to issue, and
/// optionally where the machine taking it runs
/// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// `edge_placement` absent means "replace the machine where the store already is" — ADR-0003's swap,
/// and every bump this route served before the field existed, which is why it defaults rather than
/// being required. Present, it means the store is **moving**, and this is the only route in the tree
/// that writes the value.
#[derive(Debug, Clone, Deserialize)]
struct BumpLeaseRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    edge_placement: Option<String>,
}

/// A `PUT /admin/config/ota/placement` body: the `(tenant, store)` and where in the rollout it sits —
/// its ring, and the stable canary bucket that fixes its place in the fleet ramp.
#[derive(Debug, Clone, Deserialize)]
struct PublishPlacementRequest {
    tenant_id: String,
    store_id: String,
    ring: String,
    canary_bucket: u8,
}

/// Builds the OTA rollout sub-router ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md), O3).
///
/// The first-class levers that replace hand-editing a `fleet_update` node: `PUT /admin/config/ota`
/// publishes a rollout from typed fields, `POST /admin/config/ota/halt` flips its kill switch, and
/// `GET /admin/config/ota` reads the currently-published rollout. The writes compose the `fleet_update`
/// node through the same config tree and the same `CapabilityValidator` (its `ota_violations`) the
/// generic publish used, so a malformed rollout is a `422` with the exact violations. Writes are behind
/// [`ConsolePermission::PublishOta`] and audited; the read is behind [`ConsolePermission::Read`].
///
/// `PUT`/`GET /admin/config/ota/placement` are the other half of the rollout decision. A rollout says
/// *which* devices are eligible; a placement says *where this store sits* — its ring and its stable
/// canary bucket. A store with no placement is placed nowhere, and `decide_rollout` finds it eligible
/// for nothing: safe, but it never updates. Both halves have to be authored for a fleet to move
/// ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md),
/// [ADR-0052](../../../docs/adr/0052-ota-rollout-config.md)).
pub fn ota_config_router<Cfg, A, C, L>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    releases: L,
) -> Router
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/ota",
            get(admin_get_rollout::<Cfg, A, C, L>).put(admin_publish_rollout::<Cfg, A, C, L>),
        )
        .route(
            "/admin/config/ota/halt",
            post(admin_halt_rollout::<Cfg, A, C, L>),
        )
        .route(
            "/admin/config/ota/placement",
            get(admin_get_placement::<Cfg, A, C, L>).put(admin_publish_placement::<Cfg, A, C, L>),
        )
        // The lease (ADR-0108) is not an OTA lever, and it sits here anyway: this router already owns
        // the load→set-node→publish→audit machinery a store-scoped config node needs, and the lease's
        // whole delivery mechanism is that node. It is deliberately not a `PUT` beside the two above
        // — there is nothing to author, only a counter to advance.
        .route("/admin/config/lease", get(admin_get_lease::<Cfg, A, C, L>))
        .route(
            "/admin/config/lease/bump",
            post(admin_bump_lease::<Cfg, A, C, L>),
        )
        .with_state(OtaConfigState {
            config_trees,
            admin,
            clock,
            audit,
            releases,
        })
}

/// Refuses a rollout whose `target_version` the cloud does not host
/// ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 2).
///
/// Without this, the console's happy path was: publish a typo, and every store in the ring fetches,
/// gets a `404`, and stays on the old version. A `404` on the artifact route means "install nothing",
/// so that failure is silent by design — the fleet just never moves, and no log says why. The check
/// turns it into a `422` at the moment a human is looking at the field they got wrong, and lists what
/// *is* hosted so they can see the spelling they meant.
///
/// It reads the registry, not the object store, so it also works on a deployment with no
/// `[artifacts]` block: such a cloud can never upload, therefore hosts nothing, therefore cannot
/// promote — the correct posture for one that ships no edge releases.
///
/// # Errors
///
/// The [`Response`] to send: `422` naming `target_version` when nothing is hosted for it, or `503`
/// when the registry could not be read (refusing to publish is right either way — a rollout is not
/// worth guessing about).
async fn require_hosted_release<L: ReleaseStore>(
    releases: &L,
    target_version: &str,
) -> Result<(), Response> {
    if validate_release_tag(target_version).is_err() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "target_version: not usable as a storage key, so no artifact could be hosted for it",
            &[("target_version", "not usable as a storage key")],
        ));
    }
    match releases.list_artifacts(target_version).await {
        Ok(hosted) if hosted.is_empty() => Err(api_error_with_details(
            ErrorStatus::Unprocessable,
            format!(
                "no release artifact is hosted for {target_version}, so every store in the ring \
                 would fetch nothing — upload it first (POST /admin/releases)"
            ),
            &[("target_version", "no artifact hosted for this version")],
        )),
        Ok(_hosted) => Ok(()),
        Err(error) => {
            tracing::error!(%error, "checking hosted release artifacts failed");
            Err(service_unavailable("the release registry"))
        }
    }
}

/// Composes a `fleet_update` node onto a store's Store layer and publishes it — the same
/// load→merge→publish→version shape as the campaigns/tax node publishes, so the other Store-level keys
/// survive. The node is validated by the config tree's `CapabilityValidator` before it commits.
async fn admin_publish_rollout<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<PublishRolloutRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = require_hosted_release(&state.releases, &request.target_version).await {
        return refusal;
    }
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
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "fleet_update",
            node,
            action: "config.ota.publish",
            detail: audit_detail,
        },
    )
    .await
}

/// Flips the kill switch on a store's published rollout: loads its authored `fleet_update`, sets
/// `halted`, and re-publishes — preserving the rest of the rollout, so an operator halts a bad rollout
/// (or resumes a paused one) without re-typing the target, ring, and key. `400` if the store has no
/// rollout to halt.
async fn admin_halt_rollout<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<HaltRolloutRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let loaded = match load_tree_for_write(&state.config_trees, tenant_id, store_id).await {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    let state_before = &loaded.state;
    let Some(mut node) = state_before
        .as_ref()
        .map(|s| s.layer(ConfigLevel::Store))
        .and_then(|layer| layer.get("fleet_update"))
        .cloned()
    else {
        return api_error(
            ErrorStatus::InvalidArgument,
            "the store has no published rollout to halt",
        );
    };
    if let serde_json::Value::Object(map) = &mut node {
        map.insert("halted".to_owned(), serde_json::Value::Bool(request.halted));
    }
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "fleet_update",
            node,
            action: "config.ota.halt",
            detail: serde_json::json!({ "halted": request.halted }),
        },
    )
    .await
}

/// One OTA node write: where it lands and how the trail names it. Grouped because the five travel
/// together and mean nothing apart — a `key` on the wrong `level` is exactly the mistake
/// [`ConfigTreeState::layer`] exists to prevent, so they are chosen at one place.
struct OtaNodeWrite<'a> {
    /// The config layer the node belongs on: Store for a rollout, Device for a placement.
    level: ConfigLevel,
    /// The node's key in that layer — `fleet_update` or `device_ota`.
    key: &'a str,
    /// The value to set.
    node: serde_json::Value,
    /// The audit action id.
    action: &'a str,
    /// The audit detail, which carries no personal data — a rollout is fleet configuration.
    detail: serde_json::Value,
}

/// The shared load→set-node→publish→save→audit tail behind every OTA lever.
async fn publish_ota_node<Cfg, A, C, L>(
    state: &OtaConfigState<Cfg, A, C, L>,
    context: &AdminContext,
    tenant_id: TenantId,
    store_id: StoreId,
    write: OtaNodeWrite<'_>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
{
    let nodes = vec![(write.key.to_owned(), write.node)];
    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        write.level,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(tenant_id),
        write.action,
        "store",
        &store_id.to_string(),
        None,
        Some(write.detail),
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

/// Reads a store's currently-published rollout — the authored `fleet_update` node — or `null` if none
/// is published. Behind [`ConsolePermission::Read`].
async fn admin_get_rollout<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Query(query): Query<OtaRolloutQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    match state
        .config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(state_before) => {
            let rollout = state_before
                .as_ref()
                .map(|s| s.layer(ConfigLevel::Store))
                .and_then(|layer| layer.get("fleet_update"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            (StatusCode::OK, Json(rollout)).into_response()
        }
        Err(error) => config_store_error_response(&error),
    }
}

/// Composes a `device_ota` node onto a store's **Device** layer and publishes it — the placement half
/// of the rollout decision ([ADR-0052](../../../docs/adr/0052-ota-rollout-config.md)). Same
/// load→merge→publish→version shape as the rollout lever, on a different layer and key, so the store's
/// other Device-level settings survive. The config tree's `CapabilityValidator` checks the node before
/// it commits, so a ring that names no ring or a bucket past 99 is a `422` carrying the reasons.
///
/// The placement is per **store**, not per terminal, despite the key's name and the Device layer it
/// sits on: a config tree is keyed by `StoreId` and its Device layer is one document the store's
/// terminals share, so they all take the same ring and bucket. That is the granularity the delivery
/// mechanism has, and it is also the granularity a shop wants — a counter running two releases at once
/// is worse than a counter a week behind. ADR-0052 Correction 1 records this against that ADR's
/// original "per-device" wording.
async fn admin_publish_placement<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<PublishPlacementRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let node = serde_json::json!({
        "ring": request.ring,
        "canary_bucket": request.canary_bucket,
    });
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Device,
            key: "device_ota",
            node: node.clone(),
            action: "config.ota.placement",
            detail: node,
        },
    )
    .await
}

/// Reads a store's published placement — the authored `device_ota` node — or `null` if the store has
/// never been placed. Behind [`ConsolePermission::Read`].
async fn admin_get_placement<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Query(query): Query<OtaRolloutQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    match state
        .config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(state_before) => {
            let placement = state_before
                .as_ref()
                .map(|s| s.layer(ConfigLevel::Device))
                .and_then(|layer| layer.get("device_ota"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            (StatusCode::OK, Json(placement)).into_response()
        }
        Err(error) => config_store_error_response(&error),
    }
}

/// `POST /admin/config/lease/bump` — issues this store's next lease generation and publishes it
/// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)).
///
/// This is the act of saying **"a different machine is the store now"**. One request does two things
/// that must not drift apart: it advances the authoritative counter in `store_lease`, and it
/// publishes the resulting number as the Store-layer `lease` node — the only rail that reaches a
/// till. The node is *derived* here, never authored: no route accepts a `lease` body, because a
/// generation a person could type would not be an authority.
///
/// A store's **first** bump lands on generation `0` and supersedes nobody — ADR-0049's "the first
/// lease a store ever issues is generation `0`" — so it is the act of putting a store under the
/// lease at all. Every bump after that supersedes whatever box holds the previous number: it stops
/// installing updates on its next config pull, and stays stopped across reboots, because the edge
/// takes its generation once and thereafter only compares.
///
/// Behind [`ConsolePermission::ManageStores`] rather than `PublishOta`: replacing the machine that
/// *is* a store is store management, and the person who does it is the person who provisions
/// hardware, not the person who runs an upgrade campaign. The audit entry names the generation
/// issued, so "who replaced this box, and when" has an answer.
///
/// # The third thing that must not drift apart
///
/// [ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md) makes *where* the store
/// runs an attribute of the store, and this route the only writer of it. A body naming
/// `edge_placement` is a **move**; a body omitting it is a swap in place, and keeps whatever the
/// store had. Either way the value is written inside the bump's own statement, so a reader can never
/// catch the two disagreeing — which is not fussiness but the difference between a console that says
/// "Offline-capable: yes" about a hosted store and one that does not.
///
/// An explicit `EDGE_PLACEMENT_UNSPECIFIED` is refused rather than accepted as "no change". On the
/// wire that token means *this message did not say*, so a caller that sends it has said nothing in a
/// field that looks like it said something; omitting the field is how you mean that, and a refusal
/// that says so beats a request that appears to move a store and does not.
async fn admin_bump_lease<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<BumpLeaseRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let requested_placement = match request.edge_placement.as_deref() {
        None => None,
        Some(token) => match EdgePlacement::from_wire(token) {
            Some(EdgePlacement::Unspecified) | None => {
                return api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    "edge_placement must be one of EDGE_PLACEMENT_IN_STORE, \
                     EDGE_PLACEMENT_HOSTED_BY_OPERATOR, EDGE_PLACEMENT_HOSTED_BY_PLATFORM — \
                     omit the field to keep the store where it is",
                    &[("edge_placement", "UNKNOWN_VALUE")],
                );
            }
            Some(placement) => Some(placement),
        },
    };
    // The counter moves first. If the publish then fails the store is on a generation no till has
    // been told about, which is the safe half of the split: every box keeps the generation it holds
    // and keeps updating, and the next bump (or a re-publish) closes it. The opposite order would
    // publish a generation the authority never issued.
    //
    // The placement rides inside that same write rather than following it, so there is no interval
    // in which the store record and the lease disagree (ADR-0110).
    let bump = match state
        .config_trees
        .bump(tenant_id, store_id, state.clock.now(), requested_placement)
        .await
    {
        Ok(bump) => bump,
        Err(error) => return lease_store_error_response(&error),
    };
    let node = lease_node(bump.generation);
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "lease",
            node,
            action: "config.lease.bump",
            // `edge_placement` is what the store now is; `edge_placement_moved` says whether this
            // bump chose it. Without the second flag the trail cannot tell a store that was moved
            // to a hosted machine from one whose already-hosted box was swapped — the same row
            // either way, and only one of them is a change of what the store can promise.
            detail: serde_json::json!({
                "generation": bump.generation.value(),
                "edge_placement": bump.edge_placement.as_wire(),
                "edge_placement_moved": requested_placement.is_some(),
            }),
        },
    )
    .await
}

/// `GET /admin/config/lease` — the store's authoritative lease generation, or `null` if it has never
/// been issued one. Behind [`ConsolePermission::Read`].
///
/// Read from the `store_lease` row, not from the published node: the row is the authority and the
/// node is its delivery, and a console that read the node would be reporting what the *tills* were
/// told rather than what is true.
async fn admin_get_lease<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Query(query): Query<OtaRolloutQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ReleaseStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    match state.config_trees.current(tenant_id, store_id).await {
        Ok(current) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "generation": current.map(pos_core::lease::LeaseGeneration::value),
            })),
        )
            .into_response(),
        Err(error) => lease_store_error_response(&error),
    }
}

/// Turns a lease-store fault into the same `503` every other store outage returns. There is no
/// caller-fixable case: a bump takes no value and a read takes no filter, so anything that fails
/// here is the database, not the request.
fn lease_store_error_response(error: &LeaseStoreError) -> Response {
    tracing::error!(%error, "the lease store failed");
    service_unavailable("the lease store is unavailable")
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
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    if request.count == 0 || request.count > MAX_VOUCHER_BATCH {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "count must be between 1 and 10000",
            &[("count", "OUT_OF_RANGE")],
        );
    }
    // The campaign must exist and be a voucher-kind — a code only makes sense for one the engine
    // evaluates as a voucher.
    match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(campaign)) if campaign.record.kind == PublishedCampaignKind::Voucher => {}
        Ok(Some(_other)) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "campaign is not a voucher-kind campaign",
                &[("campaign_id", "WRONG_KIND")],
            );
        }
        Ok(None) => return not_found("campaign"),
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
    Query(query): Query<VoucherListQuery>,
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
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit. Not a default and not a migration: an
    // operator printing a promotion's flyer run wants every code, and the console's table wants
    // twenty-five of them (ADR-0098).
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        return match state
            .vouchers
            .list_by_campaign(tenant_id, campaign_id)
            .await
        {
            Ok(records) => (StatusCode::OK, Json(voucher_views(records))).into_response(),
            Err(error) => voucher_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    match state
        .vouchers
        .list_by_campaign_page(tenant_id, campaign_id, page)
        .await
    {
        Ok(read) => paged_ok(Page::new(voucher_views(read.items), read.total), page),
        Err(error) => voucher_error_response(&error),
    }
}

/// The wire view of a batch of vouchers.
///
/// Shared by the paged and unpaged reads, so a row cannot render one way on the table and another on
/// the flyer run — and so neither form can start leaking a field the other withholds.
fn voucher_views(records: Vec<crate::vouchers::VoucherRecord>) -> Vec<VoucherView> {
    records
        .into_iter()
        .map(|record| VoucherView {
            voucher_id: record.voucher_id,
            code: record.code,
            status: record.status,
        })
        .collect()
}

/// Maps a voucher store failure to a retryable `503`, logging the detail rather than leaking it.
fn voucher_error_response(error: &VoucherStoreError) -> Response {
    tracing::error!(%error, "a voucher store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the voucher service is unavailable",
    )
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if request.effective_at_ms <= state.clock.now().as_milliseconds_since_epoch() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "effective_at_ms must be in the future",
            &[("effective_at_ms", "OUT_OF_RANGE")],
        );
    }
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => records(campaigns),
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(node_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node to schedule");
        return service_unavailable("campaign");
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
        Ok(false) => not_found("pending scheduled publish"),
        Err(error) => scheduled_error_response(&error),
    }
}

/// Maps a scheduled-publish store failure to a retryable `503`, logging the detail rather than leaking it.
fn scheduled_error_response(error: &ScheduledPublishError) -> Response {
    tracing::error!(%error, "a scheduled-publish store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the scheduling service is unavailable",
    )
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
    Query(query): Query<MediaListQuery>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit (ADR-0098). The image picker attaches a
    // photograph to an item and needs the whole library to find one in; the Media screen's table
    // wants twenty-five rows and a count. Neither is a default for the other.
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        return match state.media.list(tenant_id).await {
            Ok(rows) => (StatusCode::OK, Json(media_views(rows))).into_response(),
            Err(error) => media_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    match state.media.list_page(tenant_id, page).await {
        Ok(read) => paged_ok(Page::new(media_views(read.items), read.total), page),
        Err(error) => media_error_response(&error),
    }
}

/// The wire view of a batch of media summaries.
///
/// Shared by the paged and unpaged reads, so an asset cannot render one way on the table and another
/// in the picker.
fn media_views(rows: Vec<crate::media::MediaSummary>) -> Vec<MediaSummaryView> {
    rows.into_iter()
        .map(|row| MediaSummaryView {
            media_id: row.media_id.to_string(),
            content_type: row.content_type.clone(),
            detail_bytes: u64::try_from(row.detail_bytes).unwrap_or(u64::MAX),
            created_at_ms: row.created_at_ms,
        })
        .collect()
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let renditions = match images::render(&body) {
        Ok(renditions) => renditions,
        Err(ImagePipelineError::Decode(_)) => {
            return api_error(
                ErrorStatus::InvalidArgument,
                "the upload is not a decodable image",
            );
        }
        Err(ImagePipelineError::Budget { .. }) => {
            // The pipeline understood the image and could not fit it in the budget: well-formed
            // request, unprocessable content (ADR-0096). There is no field to name — the upload is
            // the whole body — so this carries no `details`.
            return api_error(
                ErrorStatus::Unprocessable,
                "the image could not be reduced within the size budget",
            );
        }
        Err(ImagePipelineError::Encode(_)) => {
            tracing::error!("encoding a media rendition failed");
            return api_error(ErrorStatus::Internal, "could not encode the image");
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
    let tenant_id = match parse_ulid_fields([("tenant_id", tenant)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let media_id = match parse_ulid_fields([("media_id", media_id)]) {
        Ok([media_id]) => MediaId::new(media_id),
        Err(refusal) => return refusal,
    };
    match state.media.get(tenant_id, media_id, rendition).await {
        Ok(Some(bytes)) => (
            [
                (CONTENT_TYPE, "image/jpeg"),
                // Renditions are immutable (content is replaced by a new id), so cache hard, privately.
                (
                    axum::http::header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => not_found("media asset"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let media_id = match parse_ulid_fields([("media_id", &media_id)]) {
        Ok([media_id]) => MediaId::new(media_id),
        Err(refusal) => return refusal,
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
        Ok(false) => not_found("media asset"),
        Err(error) => media_error_response(&error),
    }
}

/// The `503` a media-store failure becomes.
fn media_error_response(error: &MediaStoreError) -> Response {
    tracing::error!(%error, "a media store operation failed");
    api_error(ErrorStatus::Unavailable, "the media service is unavailable")
}

/// The `503` a failure to mint a media id becomes (OS entropy unavailable).
fn media_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a media id");
    api_error(ErrorStatus::Unavailable, "the media service is unavailable")
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

// --- Countries & locales (read-only master data, ADR-0074, Track M4) ------------------------------

/// One compiled country module as the console reads it: the code, human name, currency, preferred
/// language, number format, the default retention period, and the till facts its coinage and its
/// quoting habit fix. What the platform can serve — not a per-store setting, and not fiscalization.
///
/// The last three are what make a country **choosable** rather than merely listed
/// ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)): a store-settings form reads them
/// to fill its own fields in, so provisioning a Japanese shop does not depend on somebody remembering
/// that Japanese prices are tax-inclusive.
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
    prices_include_tax: bool,
    cash_rounding_increment: Option<i64>,
    cash_denominations: Vec<i64>,
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
            prices_include_tax: pack.prices_include_tax,
            cash_rounding_increment: pack.cash_rounding_increment,
            cash_denominations: pack.cash_denominations.clone(),
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
    Query(query): Query<ItemListQuery>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit (ADR-0098). This route has the most
    // consumers of any list in the console and only one of them is a table: the menu compiler and
    // the item pickers need every item, or a menu compiles without whatever fell off the page. That
    // is the reason an absent `?limit=` can never come to mean a default page size.
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        // `q`/`sort`/`order` shape a *page*. Accepting them on the whole-set read and ignoring them
        // would answer a different question than the caller asked and say nothing about it — so the
        // route names the one parameter that is missing instead.
        for (field, value) in [
            ("q", query.q.as_deref()),
            ("sort", query.sort.as_deref()),
            ("order", query.order.as_deref()),
        ] {
            if present_param(value).is_some() {
                return page_shaping_needs_a_limit_refusal(field);
            }
        }
        return match state.catalog.list_items(tenant_id).await {
            Ok(items) => (StatusCode::OK, Json(items)).into_response(),
            Err(error) => catalog_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    let filter = match item_list_filter(
        query.q.as_deref(),
        query.sort.as_deref(),
        query.order.as_deref(),
    ) {
        Ok(filter) => filter,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .list_items_page(tenant_id, page, &filter)
        .await
    {
        Ok(read) => paged_ok(read, page),
        Err(error) => catalog_error_response(&error),
    }
}

/// Reads `?q=`, `?sort=` and `?order=` into an [`ItemListFilter`], refusing a token outside the
/// route's own closed sets.
///
/// The refusal names the field and lists what it accepts (ADR-0096's shape), so a caller that sent
/// `?sort=price` is told `sort` must be `newest`, `name`, or `status` rather than being handed the
/// default order and left to wonder why nothing moved.
///
/// A blank or whitespace `q` is *absence*, not a search for nothing: clearing a search box sends
/// `?q=`, and the one reading of that which is never right is "match only items whose name contains
/// the empty string", which is every item — the same answer as no search, arrived at by accident.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal is built where the field names are \
              known, exactly as the other query parsers on this router do"
)]
fn item_list_filter(
    search: Option<&str>,
    sort: Option<&str>,
    order: Option<&str>,
) -> Result<ItemListFilter, Response> {
    let search = present_param(search).map(str::to_owned);
    let sort = match present_param(sort) {
        None => ItemSort::default(),
        Some(token) => match ItemSort::from_token(token) {
            Some(sort) => sort,
            None => return Err(enum_refusal("sort", ItemSort::tokens().iter().copied())),
        },
    };
    let descending = match present_param(order) {
        // Absent is ascending — the same reading `?order=asc` has, so a caller that omits the
        // parameter and one that spells out the default get the same page.
        None | Some("asc") => false,
        Some("desc") => true,
        Some(_unknown) => return Err(enum_refusal("order", ["asc", "desc"])),
    };
    Ok(ItemListFilter {
        search,
        sort,
        descending,
    })
}

/// The refusal for a page-shaping parameter sent to a read that was not asked to page.
///
/// Its own sentence rather than [`offset_without_limit_refusal`]'s, because the fix differs: an
/// `offset` without a `limit` is a caller that forgot half a page, while a `q` without one is a
/// caller expecting a filtered *set* — a thing this API does not offer, and will not, because the
/// whole-set read exists for consumers that need every row.
fn page_shaping_needs_a_limit_refusal(field: &str) -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        format!(
            "{field} shapes a page and needs a limit: without one this read returns every row, \
             unfiltered and in its own order"
        ),
        &[(field, "MISSING_DEPENDENT_FIELD")],
    )
}

/// A CSV download response ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5): the
/// bytes with `text/csv` and a `content-disposition` naming the file so the browser saves it. `filename`
/// is a fixed, server-chosen literal per domain — never tenant-supplied — so it needs no escaping.
fn csv_download_response(filename: &str, body: Vec<u8>) -> Response {
    (
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // The version each row was read at is for a writer (ADR-0094); an export is a read, so the
    // records go out as they always did.
    let items: Vec<CatalogItem> = match state.catalog.list_items(tenant_id).await {
        Ok(items) => items.into_iter().map(|item| item.record).collect(),
        Err(error) => return catalog_error_response(&error),
    };
    let Ok(body) = export::items_csv(&items) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
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
    let (tenant_id, tax_class_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("tax_class_id", &request.tax_class_id),
    ]) {
        Ok([tenant_id, tax_class_id]) => (TenantId::new(tenant_id), TaxClassId::new(tax_class_id)),
        Err(refusal) => return refusal,
    };
    let Ok(item_category_id) =
        parse_optional_ulid(request.item_category_id.as_deref(), ItemCategoryId::new)
    else {
        return ulid_refusal(&["item_category_id"]);
    };
    let Ok(item_subcategory_id) = parse_optional_ulid(
        request.item_subcategory_id.as_deref(),
        ItemSubcategoryId::new,
    ) else {
        return ulid_refusal(&["item_subcategory_id"]);
    };
    let Ok(image_ref) = parse_optional_ulid(request.image_ref.as_deref(), MediaId::new) else {
        return ulid_refusal(&["image_ref"]);
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (menu_item_id, tenant_id, tax_class_id) = match parse_ulid_fields([
        ("menu_item_id", &menu_item_id),
        ("tenant_id", &request.tenant_id),
        ("tax_class_id", &request.tax_class_id),
    ]) {
        Ok([menu_item_id, tenant_id, tax_class_id]) => (
            MenuItemId::new(menu_item_id),
            TenantId::new(tenant_id),
            TaxClassId::new(tax_class_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let Ok(item_category_id) =
        parse_optional_ulid(request.item_category_id.as_deref(), ItemCategoryId::new)
    else {
        return ulid_refusal(&["item_category_id"]);
    };
    let Ok(item_subcategory_id) = parse_optional_ulid(
        request.item_subcategory_id.as_deref(),
        ItemSubcategoryId::new,
    ) else {
        return ulid_refusal(&["item_subcategory_id"]);
    };
    let Ok(image_ref) = parse_optional_ulid(request.image_ref.as_deref(), MediaId::new) else {
        return ulid_refusal(&["image_ref"]);
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
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_item(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("item"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_tax_classes(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (tax_class_id, tenant_id) = match parse_ulid_fields([
        ("tax_class_id", &tax_class_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([tax_class_id, tenant_id]) => (TaxClassId::new(tax_class_id), TenantId::new(tenant_id)),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = TaxClass {
        tax_class_id,
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_tax_class(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("tax class"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_item_categories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (item_category_id, tenant_id) = match parse_ulid_fields([
        ("item_category_id", &item_category_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([item_category_id, tenant_id]) => (
            ItemCategoryId::new(item_category_id),
            TenantId::new(tenant_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = ItemCategory {
        item_category_id,
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_item_category(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("item category"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_item_subcategories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
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
    let (tenant_id, item_category_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("item_category_id", &request.item_category_id),
    ]) {
        Ok([tenant_id, item_category_id]) => (
            TenantId::new(tenant_id),
            ItemCategoryId::new(item_category_id),
        ),
        Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (item_subcategory_id, tenant_id, item_category_id) = match parse_ulid_fields([
        ("item_subcategory_id", &item_subcategory_id),
        ("tenant_id", &request.tenant_id),
        ("item_category_id", &request.item_category_id),
    ]) {
        Ok([item_subcategory_id, tenant_id, item_category_id]) => (
            ItemSubcategoryId::new(item_subcategory_id),
            TenantId::new(tenant_id),
            ItemCategoryId::new(item_category_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = ItemSubcategory {
        item_subcategory_id,
        tenant_id,
        item_category_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_item_subcategory(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("item sub-category"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_display_categories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (display_category_id, tenant_id) = match parse_ulid_fields([
        ("display_category_id", &display_category_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([display_category_id, tenant_id]) => (
            DisplayCategoryId::new(display_category_id),
            TenantId::new(tenant_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = DisplayCategory {
        display_category_id,
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_display_category(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("display category"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_display_subcategories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
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
    let (tenant_id, display_category_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("display_category_id", &request.display_category_id),
    ]) {
        Ok([tenant_id, display_category_id]) => (
            TenantId::new(tenant_id),
            DisplayCategoryId::new(display_category_id),
        ),
        Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (display_subcategory_id, tenant_id, display_category_id) = match parse_ulid_fields([
        ("display_subcategory_id", &display_subcategory_id),
        ("tenant_id", &request.tenant_id),
        ("display_category_id", &request.display_category_id),
    ]) {
        Ok([display_subcategory_id, tenant_id, display_category_id]) => (
            DisplaySubcategoryId::new(display_subcategory_id),
            TenantId::new(tenant_id),
            DisplayCategoryId::new(display_category_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = DisplaySubcategory {
        display_subcategory_id,
        tenant_id,
        display_category_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_display_subcategory(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("display sub-category"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_layout_buttons(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<Versioned<LayoutButton>>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// Assembles a [`LayoutButton`] from a request plus the channel and item that identify it.
///
/// Shared by the create and update routes so the record is assembled in one place: the two differ
/// only in where the identity comes from (a body on `POST`, the path on `PUT`) and in what the
/// write does with it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns, the shape `parse_ulid_fields` already carries this expectation for"
)]
fn build_layout_button(
    request: SetLayoutButtonRequest,
    sales_channel: &str,
    menu_item_id: &str,
) -> Result<LayoutButton, Response> {
    let (tenant_id, menu_item_id, display_category_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("menu_item_id", menu_item_id),
        ("display_category_id", &request.display_category_id),
    ]) {
        Ok([tenant_id, menu_item_id, display_category_id]) => (
            TenantId::new(tenant_id),
            MenuItemId::new(menu_item_id),
            DisplayCategoryId::new(display_category_id),
        ),
        Err(refusal) => return Err(refusal),
    };
    let Ok(display_subcategory_id) = parse_optional_ulid(
        request.display_subcategory_id.as_deref(),
        DisplaySubcategoryId::new,
    ) else {
        return Err(ulid_refusal(&["display_subcategory_id"]));
    };
    // A grid slot exists only when both column and row are given; otherwise the button flows by order.
    let position = match (request.grid_column, request.grid_row) {
        (Some(column), Some(row)) => Some(GridPosition { column, row }),
        _ => None,
    };
    Ok(LayoutButton {
        tenant_id,
        sales_channel: Open::<SalesChannel>::parse(sales_channel),
        display_category_id,
        display_subcategory_id,
        menu_item_id,
        label: request.label,
        position,
        sort: request.sort,
    })
}

/// A super-admin places an item's button on a channel's layout, refusing if it is already there.
///
/// The route this replaced was a single `PUT` upsert. A button's identity is entirely
/// caller-supplied — `(tenant, channel, item)` — so "place this button" silently replaced the
/// label, grid slot and sort order of one already on that channel, and the audit trail recorded
/// the overwrite as a `set` with no `before`.
async fn admin_create_layout_button<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateLayoutButtonRequest>,
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
    let (sales_channel, menu_item_id) =
        (request.sales_channel.clone(), request.menu_item_id.clone());
    let record = match build_layout_button(request.into_write(), &sales_channel, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    match state.catalog.create_layout_button(&record).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "layout_button.create",
                "layout_button",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("layout button"),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin changes a button's presentation on a channel, only at the version they read.
async fn admin_update_layout_button<Cat, A, C>(
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
    let record = match build_layout_button(request, &sales_channel, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_layout_button(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            // A layout button's identity is (tenant, channel, item); the item id is its entity key,
            // and the full row (channel, category, position) is recorded as `after`.
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "layout_button.update",
                "layout_button",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("layout button"),
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
    let (tenant_id, menu_item_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("menu_item_id", &menu_item_id),
    ]) {
        Ok([tenant_id, menu_item_id]) => (TenantId::new(tenant_id), MenuItemId::new(menu_item_id)),
        Err(refusal) => return refusal,
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
        Ok(false) => not_found("layout button"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_modifier_groups(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Ok(member_item_ids) = parse_item_id_list(&request.member_item_ids) else {
        return ulid_refusal(&["member_item_ids"]);
    };
    let Ok(attached_item_ids) = parse_item_id_list(&request.attached_item_ids) else {
        return ulid_refusal(&["attached_item_ids"]);
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (modifier_group_id, tenant_id) = match parse_ulid_fields([
        ("modifier_group_id", &modifier_group_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([modifier_group_id, tenant_id]) => (
            ModifierGroupId::new(modifier_group_id),
            TenantId::new(tenant_id),
        ),
        Err(refusal) => return refusal,
    };
    let Ok(member_item_ids) = parse_item_id_list(&request.member_item_ids) else {
        return ulid_refusal(&["member_item_ids"]);
    };
    let Ok(attached_item_ids) = parse_item_id_list(&request.attached_item_ids) else {
        return ulid_refusal(&["attached_item_ids"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
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
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_modifier_group(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("modifier group"),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.catalog.list_menus(tenant_id).await {
        Ok(menus) => (StatusCode::OK, Json(menus)).into_response(),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Ok(parent_menu_id) = parse_optional_ulid(request.parent_menu_id.as_deref(), MenuId::new)
    else {
        return ulid_refusal(&["parent_menu_id"]);
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (menu_id, tenant_id) =
        match parse_ulid_fields([("menu_id", &menu_id), ("tenant_id", &request.tenant_id)]) {
            Ok([menu_id, tenant_id]) => (MenuId::new(menu_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    let Ok(parent_menu_id) = parse_optional_ulid(request.parent_menu_id.as_deref(), MenuId::new)
    else {
        return ulid_refusal(&["parent_menu_id"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = Menu {
        menu_id,
        tenant_id,
        name: request.name,
        parent_menu_id,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_menu(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("menu"),
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
    let (tenant_id, menu_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("menu_id", &menu_id)]) {
            Ok([tenant_id, menu_id]) => (TenantId::new(tenant_id), MenuId::new(menu_id)),
            Err(refusal) => return refusal,
        };
    match state.catalog.list_menu_sections(tenant_id, menu_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
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
    let (tenant_id, menu_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("menu_id", &menu_id)]) {
            Ok([tenant_id, menu_id]) => (TenantId::new(tenant_id), MenuId::new(menu_id)),
            Err(refusal) => return refusal,
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
        Ok(version) => {
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
            versioned_created(record, &version)
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
    let (tenant_id, menu_id, menu_section_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("menu_id", &menu_id),
        ("menu_section_id", &menu_section_id),
    ]) {
        Ok([tenant_id, menu_id, menu_section_id]) => (
            TenantId::new(tenant_id),
            MenuId::new(menu_id),
            MenuSectionId::new(menu_section_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = MenuSection {
        menu_section_id,
        tenant_id,
        menu_id,
        name: request.name,
        sort: request.sort,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_menu_section(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
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
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("menu section"),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a menu's placements (tenant named on the query).
///
/// Behind [`ConsolePermission::ReadRevenue`], not plain `Read` (production-readiness **S5**): a
/// [`MenuPlacement`] carries `prices` — the item's price on every channel of this menu — which is
/// exactly the commercially sensitive (**T2**) data `ReadRevenue` was carved out of `Read` to hold
/// back. Until this row was verified the console handed a store's full per-channel price book to any
/// Viewer, while refusing the same figures on a revenue report.
///
/// The consequence is deliberate and worth stating: Ops and Viewer can still list the menus, their
/// sections and the item master, and can no longer read what anything costs.
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
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, menu_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("menu_id", &menu_id)]) {
            Ok([tenant_id, menu_id]) => (TenantId::new(tenant_id), MenuId::new(menu_id)),
            Err(refusal) => return refusal,
        };
    match state.catalog.list_placements(tenant_id, menu_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<Versioned<MenuPlacement>>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// Assembles a [`MenuPlacement`] from a request plus the menu and item that identify it.
///
/// Shared by the create and update routes, for the same reason as `build_layout_button`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns, the shape `parse_ulid_fields` already carries this expectation for"
)]
fn build_placement(
    request: SetPlacementRequest,
    menu_id: &str,
    menu_item_id: &str,
) -> Result<MenuPlacement, Response> {
    let (tenant_id, menu_id, menu_item_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("menu_id", menu_id),
        ("menu_item_id", menu_item_id),
    ]) {
        Ok([tenant_id, menu_id, menu_item_id]) => (
            TenantId::new(tenant_id),
            MenuId::new(menu_id),
            MenuItemId::new(menu_item_id),
        ),
        Err(refusal) => return Err(refusal),
    };
    let Ok(menu_section_id) =
        parse_optional_ulid(request.menu_section_id.as_deref(), MenuSectionId::new)
    else {
        return Err(ulid_refusal(&["menu_section_id"]));
    };
    Ok(MenuPlacement {
        tenant_id,
        menu_id,
        menu_item_id,
        menu_section_id,
        prices: request.prices,
        available: request.available,
    })
}

/// A super-admin adds an item to a menu, refusing if it is already on it.
///
/// The route this replaced was a single `PUT` upsert. A placement's identity is the caller-supplied
/// `(menu, item)` pair, so "add this item" silently replaced the prices, section and availability
/// of one already on the menu — and because the per-channel prices are the price-change journal
/// (ADR-0069, G2), the overwrite was recorded as a `set` with no `before` to compare against.
async fn admin_create_placement<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreatePlacementRequest>,
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
    let (menu_id, menu_item_id) = (request.menu_id.clone(), request.menu_item_id.clone());
    let record = match build_placement(request.into_write(), &menu_id, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    match state.catalog.create_placement(&record).await {
        Ok(CreateOutcome::Created(version)) => {
            let entity_id = format!("{}/{}", record.menu_id, record.menu_item_id);
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "placement.create",
                "menu_placement",
                &entity_id,
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("menu placement"),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin changes an item's prices and availability on a menu, only at the version they read.
async fn admin_update_placement<Cat, A, C>(
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
    let record = match build_placement(request, &menu_id, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_placement(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            // A placement's identity is the (menu, item) pair; its per-channel prices are the
            // price-change journal (ADR-0069, G2), recorded as `after`.
            let entity_id = format!("{}/{}", record.menu_id, record.menu_item_id);
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "placement.update",
                "menu_placement",
                &entity_id,
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("menu placement"),
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
    let (tenant_id, menu_id, menu_item_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("menu_id", &menu_id),
        ("menu_item_id", &menu_item_id),
    ]) {
        Ok([tenant_id, menu_id, menu_item_id]) => (
            TenantId::new(tenant_id),
            MenuId::new(menu_id),
            MenuItemId::new(menu_item_id),
        ),
        Err(refusal) => return refusal,
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
        Ok(false) => not_found("placement"),
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
    let (tenant_id, store_id, menu_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("menu_id", &request.menu_id),
    ]) {
        Ok([tenant_id, store_id, menu_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            MenuId::new(menu_id),
        ),
        Err(refusal) => return refusal,
    };

    // Load the tenant's authoring model. Placements are gathered across every menu; the compiler
    // filters to the requested menu's inheritance chain, so extra rows are harmless.
    // The compiler takes the authoring records themselves; the version each was read at is a
    // writer's concern (ADR-0094) and would only be noise in a compiled book.
    let items: Vec<CatalogItem> = match state.catalog.list_items(tenant_id).await {
        Ok(items) => items.into_iter().map(|item| item.record).collect(),
        Err(error) => return catalog_error_response(&error),
    };
    let menus: Vec<Menu> = match state.catalog.list_menus(tenant_id).await {
        Ok(menus) => menus.into_iter().map(|menu| menu.record).collect(),
        Err(error) => return catalog_error_response(&error),
    };
    let mut placements = Vec::new();
    for menu in &menus {
        match state.catalog.list_placements(tenant_id, menu.menu_id).await {
            Ok(rows) => placements.extend(records(rows)),
            Err(error) => return catalog_error_response(&error),
        }
    }

    // Compile the price book. A refusal here is a configuration error the operator must fix, not a
    // store failure.
    let book = match compile_menu(&items, &menus, &placements, menu_id) {
        Ok(book) => book,
        Err(error) => return api_error(ErrorStatus::Unprocessable, error.to_string()),
    };
    let Ok(book_value) = serde_json::to_value(&book) else {
        tracing::error!("could not serialise a compiled menu book");
        return service_unavailable("catalog");
    };

    // Compile the presentation layout alongside the price book (ADR-0066): the display taxonomy plus
    // the tenant's layout buttons resolve to a per-channel `LayoutBook`, delivered on a separate
    // `layout` node so a button moving reprices nothing. The layout compiler is forgiving (a stale
    // button is skipped), so this never fails a publish that the price compile accepted.
    let display_categories: Vec<DisplayCategory> =
        match state.catalog.list_display_categories(tenant_id).await {
            Ok(rows) => rows.into_iter().map(|row| row.record).collect(),
            Err(error) => return catalog_error_response(&error),
        };
    let display_subcategories: Vec<DisplaySubcategory> =
        match state.catalog.list_display_subcategories(tenant_id).await {
            Ok(rows) => rows.into_iter().map(|row| row.record).collect(),
            Err(error) => return catalog_error_response(&error),
        };
    let layout_buttons = match state.catalog.list_layout_buttons(tenant_id).await {
        Ok(rows) => records(rows),
        Err(error) => return catalog_error_response(&error),
    };
    let layout = compile_layout_book(&display_categories, &display_subcategories, &layout_buttons);
    let Ok(layout_value) = serde_json::to_value(&layout) else {
        tracing::error!("could not serialise a compiled layout book");
        return service_unavailable("catalog");
    };

    // Load the store's tree (or start one), set the `menu` and `layout` keys on its Store layer, and
    // re-publish that layer. The Store layer is index 2 in the Tenant→Brand→Store→Device order
    // (`ConfigLevel::ORDER`); writing the whole layer back preserves any other Store-level keys there.
    let nodes = vec![
        ("menu".to_owned(), book_value),
        ("layout".to_owned(), layout_value),
    ];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };

    // Load the domain the compiler needs. `list` is on two of P's traits, so the calls are
    // fully-qualified to say which seam each one is.
    let assignments =
        match AssignmentStore::list_for_store(&state.people, tenant_id, store_id).await {
            Ok(assignments) => assignments,
            Err(error) => return people_error_response(&error),
        };
    // As with the floor, the compiler takes the authoring records; the version each was read at is
    // a writer's concern (ADR-0094) and has no place in a published permissions document.
    let employees: Vec<Employee> = match EmployeeStore::list(&state.people, tenant_id).await {
        Ok(employees) => employees
            .into_iter()
            .map(|employee| employee.record)
            .collect(),
        Err(error) => return people_error_response(&error),
    };
    let roles: Vec<RoleTemplate> = match RoleTemplateStore::list(&state.people, tenant_id).await {
        Ok(roles) => roles.into_iter().map(|role| role.record).collect(),
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
        return service_unavailable("people");
    };

    // Set the `permissions` key on the store's Store layer (index 2 in Tenant→Brand→Store→Device) and
    // re-publish that layer, preserving any other Store-level keys (`menu`, `layout`, …).
    let nodes = vec![("permissions".to_owned(), document_value)];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
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
    let Some(code) = mint_activation_code() else {
        tracing::error!("could not read OS entropy to mint an activation code");
        return service_unavailable("activation");
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
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the activation code is malformed",
            &[("code", "MALFORMED")],
        );
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
                return service_unavailable("activation");
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
    api_error(ErrorStatus::PermissionDenied, "activation refused")
}

/// Maps an activation-store failure to a retryable `503`, logging the detail rather than leaking it.
fn activation_error_response(error: &crate::activation::ActivationStoreError) -> Response {
    tracing::error!(%error, "an activation store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the activation service is unavailable",
    )
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.translations.load(tenant_id).await {
        // A tenant with no grid yet is an empty grid to edit, not a 404 — and no `ETag`, because
        // there is no version yet. That absence is what the console turns into `If-Match: *`.
        Ok(None) => (StatusCode::OK, Json(TranslationGrid::default())).into_response(),
        Ok(Some(loaded)) => {
            let response = (StatusCode::OK, Json(loaded.record)).into_response();
            with_etag(response, &loaded.etag)
        }
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // The version the console read the grid at, or `None` from `If-Match: *` for a tenant that has
    // authored no grid yet ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
    // This route replaces a grid an operator edited cell by cell, so a second writer really does
    // destroy their work — unlike the CSV import below, which merges and can retry.
    let expected = match if_match_collection(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let missing = grid.keys_missing_fallback();
    if !missing.is_empty() {
        // The one refusal in this file that knows exactly which fields are at fault, so it is the
        // one that most needs `details` — and the one that had the worst behaviour before ADR-0096:
        // its bespoke `{"missing_fallback":[…]}` body had no reader in the console at all, so an
        // operator's toast rendered the raw JSON.
        //
        // `<key>.en` rather than `<key>`: it is the `en` value that is missing, and a path a reader
        // can act on beats a name they have to interpret.
        let fields: Vec<String> = missing.iter().map(|key| format!("{key}.en")).collect();
        let details: Vec<(&str, &str)> = fields
            .iter()
            .map(|field| (field.as_str(), "REQUIRED"))
            .collect();
        return api_error_with_details(
            ErrorStatus::Unprocessable,
            format!(
                "every key needs a non-empty `en` fallback; these do not: {}",
                missing.join(", ")
            ),
            &details,
        );
    }
    match state
        .translations
        .save(tenant_id, &grid, expected.as_ref())
        .await
    {
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        // No grid to replace, and the caller named a version for one: an absence, not a conflict.
        Ok(UpdateOutcome::NotFound) => not_found("translation grid"),
        Ok(UpdateOutcome::Updated(version)) => {
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
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let grid = match state.translations.load(tenant_id).await {
        Ok(grid) => grid.map(|loaded| loaded.record).unwrap_or_default(),
        Err(error) => return translation_error_response(&error),
    };
    let Ok(body) = export::translations_csv(&grid) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let existing = match state.translations.load(tenant_id).await {
        Ok(grid) => grid.map(|loaded| loaded.record).unwrap_or_default(),
        Err(error) => return translation_error_response(&error),
    };
    match import::parse_translations_csv(&body, &existing) {
        Ok((_, report)) => (StatusCode::OK, Json(report)).into_response(),
        // No `details`: the parser addresses a row and byte offset in an opaque uploaded body, and
        // the only named input this request has is the `tenant_id` query, which is not what failed.
        // Inventing a field here would send a client looking for a JSON member it never sent.
        Err(error) => api_error(ErrorStatus::InvalidArgument, error.to_string()),
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // An import **merges** its rows into whatever grid is stored: the keys in the CSV are set, every
    // other key is left alone. That makes it a delta, not a replace, so losing a race to another
    // writer is not a lost update — it is a stale read, and re-reading fixes it. Retrying is the
    // right answer where refusing would be for `PUT /admin/translations`, because there is no
    // operator holding a version to be told about, and the rows to apply are in the request
    // ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
    let mut applied = None;
    for _ in 0..CONDITIONAL_WRITE_ATTEMPTS {
        let loaded = match state.translations.load(tenant_id).await {
            Ok(loaded) => loaded,
            Err(error) => return translation_error_response(&error),
        };
        let (existing, expected) = loaded.map_or_else(
            || (TranslationGrid::default(), None),
            |loaded| (loaded.record, Some(loaded.etag)),
        );
        // Re-parsed against the grid this attempt actually read, so the report counts creates and
        // updates relative to the winner's grid rather than to one that no longer exists.
        let (merged, report) = match import::parse_translations_csv(&body, &existing) {
            Ok(parsed) => parsed,
            Err(error) => return api_error(ErrorStatus::InvalidArgument, error.to_string()),
        };
        match state
            .translations
            .save(tenant_id, &merged, expected.as_ref())
            .await
        {
            Ok(UpdateOutcome::Updated(version)) => {
                applied = Some((report, version));
                break;
            }
            Ok(UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => {}
            Err(error) => return translation_error_response(&error),
        }
    }
    let Some((report, version)) = applied else {
        return version_mismatch();
    };
    {
        {
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
            with_etag((StatusCode::OK, Json(report)).into_response(), &version)
        }
    }
}

/// Maps a translation-store failure to a retryable `503`, logging the detail rather than leaking it.
fn translation_error_response(error: &crate::translations::TranslationStoreError) -> Response {
    tracing::error!(%error, "a translation store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the translation service is unavailable",
    )
}

/// The generated OpenAPI document for the public `/v1` surface.
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Serves the generated `/admin` console API document (roadmap v3 B5).
///
/// Unauthenticated, like `/v1/openapi.json`: it describes routes and carries no tenant data, and
/// the console SPA this server already ships publicly names every one of these paths in its own
/// JavaScript — so a session guard here would withhold nothing an unauthenticated caller cannot
/// already read, while blocking the fork developer the document exists for.
async fn admin_openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(AdminApiDoc::openapi())
}

/// Ingests a batch of event envelopes, idempotently. Internal (the reconciliation re-push target),
/// so it is deliberately absent from the public OpenAPI document and requires the
/// `X-Pos-Internal-Key` shared secret
/// ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)) on top of the proxy denies
/// both deploy lanes apply — neither control replaces the other.
async fn ingest<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
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
    if let Err(refusal) = internal_guard(app.internal_shared_secret.as_ref(), &headers) {
        return refusal;
    }
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
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = confine_to_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let window = match window.into_window() {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
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
    fn into_window(self) -> Result<RollupWindow, WindowError> {
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
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let held = match query.held_version {
        None => None,
        Some(ref raw) => match parse_ulid_fields([("held_version", raw)]) {
            Ok([version]) => Some(ConfigVersionId::new(version)),
            Err(refusal) => return refusal,
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
    match app
        .config_trees
        .load(grant.tenant(), store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(Some(state)) => {
            let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
            let response = match tree.update_for(held) {
                SyncOutcome::UpToDate => ConfigSyncResponse::UpToDate,
                SyncOutcome::Deliver(update) => ConfigSyncResponse::Update { update },
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Ok(None) => no_published_configuration(),
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
    body: axum::body::Bytes,
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
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let report = match heartbeat_body(&body) {
        Ok(report) => report,
        // A body that is present and will not parse is the caller's mistake; swallowing it would let
        // a store report nothing forever while believing it reported.
        Err(error) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("the heartbeat body is not a heartbeat report: {error}"),
                &[
                    ("outbox_depth", "INVALID_VALUE"),
                    ("lease_generation", "INVALID_VALUE"),
                ],
            );
        }
    };
    // The tenant is the grant's, not the path's — a store reaches only its own tenant's liveness row.
    match app
        .config_trees
        .record_store_heartbeat(
            grant.tenant(),
            store_id,
            app.clock.now(),
            report.outbox_depth,
            report.lease_generation,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => config_store_error_response(&error),
    }
}

/// What a heartbeat may carry beyond "I am here" ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)).
///
/// Every field is optional and the whole body is optional, because the body is younger than the
/// route: a store running an older binary posts nothing at all, and must keep being recorded as
/// alive rather than refused for sending the empty body it has always sent.
#[derive(Debug, Default, serde::Deserialize)]
struct HeartbeatBody {
    /// How many events the store has committed and not yet published — its own publish backlog, the
    /// opposite direction from the relay backlog the fleet row already carries. `None` when the store
    /// did not say, which is a different answer from zero and leaves the recorded depth alone.
    #[serde(default)]
    outbox_depth: Option<u64>,
    /// The lease generation the box holds
    /// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)), so the console can
    /// tell a store that has been *replaced* from one that is merely quiet. `None` when the store did
    /// not say — an older edge, or one whose lease row could not be read — and that must not be
    /// recorded as `0`, because `0` is a store's real first generation.
    #[serde(default)]
    lease_generation: Option<u64>,
}

/// Reads a heartbeat's optional body. An empty body is the older edge and yields the default —
/// nothing reported, which the store layer keeps distinct from a reported zero.
///
/// # Errors
///
/// The decode error, for the caller to phrase as a refusal.
fn heartbeat_body(body: &[u8]) -> Result<HeartbeatBody, serde_json::Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(HeartbeatBody::default());
    }
    serde_json::from_slice(body)
}

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
/// The header the three `/internal/*` routes require
/// ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)).
///
/// Its own header rather than `Authorization`, because that space is `pos_<ULID>_<secret>` and
/// resolves to a `Grant` carrying the tenant every `/sync` handler reads. A tenantless shared secret
/// has no `Grant` to be, so reusing the header would put a second parse branch inside the one
/// function that answers *who is calling*. The `X-Pos-` spelling is the edge↔cloud family's, kept
/// because both sides must agree on it; the **published** header table drops the `X-` prefix
/// (roadmap **Q5**, RFC 6648), and a header an integrator writes against belongs to that table.
pub(crate) const INTERNAL_KEY_HEADER: &str = "X-Pos-Internal-Key";

/// Refuses a request to an `/internal` route that does not carry the shared secret.
///
/// The refusal is a `404` with one wording for every failure — absent header, wrong value, and (once
/// `CloudConfig::validate` guarantees the secret is set) any future "not configured" case are
/// indistinguishable to a caller. That is not tidiness: `403` confirms the route is there, which is
/// exactly what the two proxy denies refuse to confirm and what ADR-0050 refuses on activation. A
/// caller who belongs here has the key.
///
/// The comparison is `constant_time_eq`, the same one `/admin/setup` uses.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 404 the handler returns, the shape `parse_ulid_fields` already carries this expectation for"
)]
fn internal_guard(expected: Option<&InternalSecret>, headers: &HeaderMap) -> Result<(), Response> {
    // `None` is unreachable in a booted process — `CloudConfig::validate` refuses to start without
    // the secret — but it refuses rather than admits, so a fork that wires the router by hand gets a
    // closed route instead of an open one.
    let Some(expected) = expected else {
        tracing::warn!("refused an /internal request: no shared secret is configured");
        return Err(api_error(ErrorStatus::NotFound, "no such route"));
    };
    let presented = header_str(headers, INTERNAL_KEY_HEADER).unwrap_or_default();
    if constant_time_eq(presented, expected.expose()) {
        return Ok(());
    }
    // The route, never the header value or the body: the bodies here carry tenant and store ids.
    tracing::warn!("refused an /internal request without a valid key");
    Err(api_error(ErrorStatus::NotFound, "no such route"))
}

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
    too_many_requests(
        "too many sign-in attempts; try again later",
        retry_after_secs,
    )
}

/// A `429` on the AIP-193 envelope with a `Retry-After` the caller can honour.
///
/// One helper for every throttled surface, so a client that learns to back off on one learns it for
/// all of them: same `RESOURCE_EXHAUSTED` status, same header, only the message differs. The
/// `Retry-After` is whole seconds and never below one, so a sub-second wait still tells a caller to
/// hold rather than retry immediately.
pub(crate) fn too_many_requests(message: &str, retry_after_secs: u64) -> Response {
    let mut response = api_error(ErrorStatus::ResourceExhausted, message);
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

/// The prefix [`throttle_sync`] guards. Every store-facing route sits under it (ADR-0097 moved the
/// two that did not), so one prefix check covers config-pull, the heartbeat, the update report, the
/// artifact fetch, the device proposals and the order relay.
const SYNC_PREFIX: &str = "/sync/";

/// Throttles the store-facing `/sync/*` surface before authentication (roadmap **Q5**).
///
/// A layer rather than a check in each handler: there are six `/sync` routes and "the store-facing
/// surface has a budget" is one behaviour, not six. It runs before the credential is verified,
/// which is the point — a wedged or hostile box should cost a header comparison, not a key lookup
/// and a database round trip, per iteration.
///
/// Keyed on the client connection, **not** the store. The store id is in the caller-supplied path,
/// so keying on it would let anyone spell a shop's id and exhaust that shop's budget; the
/// connection is the only identity that exists this early. Behind the P8 reverse proxy the real
/// client arrives in `X-Forwarded-For`, which is why the key goes through [`client_ip`] and its
/// `trusted_proxy_hops` ([ADR-0090](../../../docs/adr/0090-tls-postures.md)) rather than the socket
/// peer — otherwise every store would share the proxy's one budget.
///
/// Requests to any other prefix pass straight through, so the layer can be applied to the whole
/// composed service without touching `/admin`, `/v1` or the console SPA.
pub async fn throttle_sync<C>(
    State(throttle): State<SyncThrottle<C>>,
    request: Request,
    next: Next,
) -> Response
where
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if !request.uri().path().starts_with(SYNC_PREFIX) {
        return next.run(request).await;
    }
    let ip = client_ip(request.headers(), throttle.trusted_proxy_hops);
    let keys = [format!("ip:{}", ip.unwrap_or("unknown"))];
    if let Err(retry_after_secs) = throttle
        .limiter
        .check_and_record(&keys, throttle.clock.now())
    {
        return too_many_requests(
            "too many requests to the store sync surface; try again later",
            retry_after_secs,
        );
    }
    next.run(request).await
}

/// What [`throttle_sync`] needs: the limiter, how many proxies to trust when reading the client
/// address, and the clock.
///
/// The clock is the port, read once per request, not a captured instant — a captured one would
/// freeze the window at start-up and the limit would never drain.
#[derive(Clone, Debug)]
pub struct SyncThrottle<C> {
    limiter: SlidingRateLimiter,
    trusted_proxy_hops: usize,
    clock: C,
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
        Err(api_error(
            ErrorStatus::PermissionDenied,
            "insufficient permissions",
        ))
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
/// `GET /admin/audit`: the filters, plus the bound.
///
/// `limit` is a `String` rather than a `u32` so that one parser owns it and an unparseable value
/// gets a refusal naming the field (ADR-0096's shape) instead of axum's own query rejection.
///
/// `offset` is what opts a caller into the paged form here, where every other paged read keys on
/// `limit` — see [`parse_audit_page`], which explains why this route could not use the same trigger.
///
/// `order` belongs to the paged form only, and the windowed read refuses it rather than ignoring it
/// — see [`windowed_read_cannot_be_ordered_refusal`].
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
    limit: Option<String>,
    #[serde(default)]
    offset: Option<String>,
    #[serde(default)]
    order: Option<String>,
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
                return ulid_refusal(&["tenant_id"]);
            }
        },
        None => None,
    };
    let filter = crate::audit::AuditQuery {
        tenant,
        entity_type: query.entity_type,
        entity_id: query.entity_id,
        action: query.action,
        actor_admin_id: query.actor_admin_id,
        since_ms: query.since_ms,
        until_ms: query.until_ms,
    };
    // Two reads over the same filters, chosen by whether the caller named an offset (ADR-0098). The
    // per-entity audit panel wants the newest few for one entity and no count; the Audit screen's
    // table wants a page and a total.
    let Some(page) = parse_audit_page(query.limit.as_deref(), query.offset.as_deref()) else {
        if present_param(query.order.as_deref()).is_some() {
            return windowed_read_cannot_be_ordered_refusal();
        }
        let limit = match windowed_audit_limit(query.limit.as_deref()) {
            Ok(limit) => limit,
            Err(refusal) => return refusal,
        };
        return match state.audit.query(&filter, limit).await {
            Ok(entries) => (StatusCode::OK, Json(audit_views(entries))).into_response(),
            Err(error) => audit_read_failure(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    let order = match parse_trail_order(query.order.as_deref()) {
        Ok(order) => order,
        Err(refusal) => return refusal,
    };
    match state.audit.query_page(&filter, page, order).await {
        Ok(read) => paged_ok(Page::new(audit_views(read.items), read.total), page),
        Err(error) => audit_read_failure(&error),
    }
}

/// Reads `?order=` off the paged audit read, defaulting to the trail's own order.
///
/// # Errors
///
/// A closed-set refusal naming `order` and the tokens it accepts, if the value names no order.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal is built where the field names are \
              known, exactly as the other query parsers on this router do"
)]
fn parse_trail_order(order: Option<&str>) -> Result<TrailOrder, Response> {
    match present_param(order) {
        None => Ok(TrailOrder::default()),
        Some(token) => TrailOrder::from_token(token)
            .ok_or_else(|| enum_refusal("order", TrailOrder::tokens().iter().copied())),
    }
}

/// The refusal for `?order=` sent to the *windowed* audit read.
///
/// Its own sentence rather than [`page_shaping_needs_a_limit_refusal`]'s, because on this route the
/// missing parameter is the `offset` and the reason is sharper than "it would be ignored": on the
/// windowed read `limit` already means "the most recent this many", so `?order=oldest&limit=200`
/// has two honest readings — the newest two hundred entries shown earliest-first, or the earliest
/// two hundred entries — and they are different sets. The route will not guess between them.
///
/// The paged read has no such ambiguity, which is why the order lives there: `LIMIT`/`OFFSET`
/// window an already-ordered set, so the order is applied first and the window second, always.
fn windowed_read_cannot_be_ordered_refusal() -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        "order shapes a page and needs an offset: on this read `limit` means \"the most recent this \
         many\", so an order would have two readings — the newest entries earliest-first, or the \
         earliest entries — and those are different sets",
        &[("order", "MISSING_DEPENDENT_FIELD")],
    )
}

/// The wire view of a batch of audit entries, shared by the windowed and paged reads.
fn audit_views(entries: Vec<AuditEntry>) -> Vec<AuditEntryView> {
    entries
        .into_iter()
        .map(AuditEntryView::from_entry)
        .collect()
}

/// Maps an audit read failure to a retryable `503`, logging the detail rather than leaking it.
fn audit_read_failure(error: &crate::audit::AuditStoreError) -> Response {
    tracing::error!(%error, "an audit read failed");
    service_unavailable("audit")
}

/// The bound for the *windowed* audit read: how many of the newest matching entries to return.
///
/// Defaulted and clamped, which is ADR-0069's behaviour for this read and is deliberately kept.
/// `limit` here does not mean "one page of this size" — it means "the most recent this many" — so
/// pulling an over-large value into range answers the question the caller asked, just less of it.
/// That is the opposite of the paged form, where a clamp would answer a *different* question (see
/// [`crate::paging`]).
///
/// A value that is not a number is still refused: there is no honest reading of `?limit=lots`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal is built where the field names are \
              known, exactly as the other query parsers on this router do"
)]
fn windowed_audit_limit(limit: Option<&str>) -> Result<u32, Response> {
    match present_param(limit) {
        None => Ok(AUDIT_READ_DEFAULT_LIMIT),
        Some(text) => match text.parse::<u32>() {
            Ok(value) => Ok(value.clamp(1, AUDIT_READ_MAX_LIMIT)),
            Err(_ignored) => Err(page_bound_refusal("limit")),
        },
    }
}

/// Reads the paging bounds off `/admin/audit`, where the trigger is `offset` and not `limit`.
///
/// `None` means "not a paged request" and the caller should serve the windowed read.
///
/// # Why this route's trigger differs
///
/// Everywhere else, naming a `limit` is what asks for a page, because `limit` did not previously
/// exist on those reads. On `/admin/audit` it did: ADR-0069 gave it a *different* meaning — "the most
/// recent this many", defaulted to 200 and clamped at 500 — and the console sends it today. Making
/// `?limit=200` return a paged envelope instead of a bare array would change the response shape of a
/// request already in flight, which is the one thing ADR-0098 exists to prevent. So on this route the
/// second read is asked for by naming an `offset`.
///
/// ADR-0098 put `audit` in the paged cohort without drawing this consequence from its own
/// measurement that five query structs already carry a `limit`. That is recorded as a correction in
/// the ADR rather than left as a surprise here.
///
/// Within the paged form the `limit` is *checked*, not clamped, exactly as elsewhere: it is a page
/// size, and a caller stitching pages together must be told when the size it used was not the size
/// it asked for.
fn parse_audit_page(
    limit: Option<&str>,
    offset: Option<&str>,
) -> Option<Result<PageRequest, Response>> {
    let offset_text = present_param(offset)?;
    let Some(limit_text) = present_param(limit) else {
        // A page is a limit and an offset together. Defaulting the limit here would make the page
        // size invisible to the caller stitching pages, which decision 1 rules out.
        return Some(Err(offset_without_limit_refusal()));
    };
    let Ok(limit) = limit_text.parse::<u32>() else {
        return Some(Err(page_bound_refusal("limit")));
    };
    let Ok(offset) = offset_text.parse::<u32>() else {
        return Some(Err(page_bound_refusal("offset")));
    };
    Some(match PageRequest::new(limit, offset) {
        Ok(request) => Ok(request),
        Err(PageRequestError::LimitOutOfRange) => Err(page_bound_refusal("limit")),
        Err(PageRequestError::OffsetOutOfRange) => Err(page_bound_refusal("offset")),
    })
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
    /// The one store the key will act for (a ULID), or absent for a tenant-wide key.
    ///
    /// A store's own credential — the key its edge presents on `/sync/stores/{store_id}/…` — must
    /// name its store here: those routes serve one store's configuration, including its employee
    /// roster and PIN hashes, and refuse a key that is not bound to the store in the path (S1). A
    /// tenant-wide key stays right for an integration reading a whole tenant's rollups.
    #[serde(default)]
    store_id: Option<String>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let store_id = match request.store_id.as_deref() {
        None => None,
        Some(raw) => match parse_ulid_fields([("store_id", raw)]) {
            Ok([store_id]) => Some(StoreId::new(store_id)),
            Err(refusal) => return refusal,
        },
    };
    // Strict: an unknown scope name is a `400`, not a silent drop — the admin is granting explicitly,
    // so a typo must not quietly issue a key that authorises nothing.
    let scopes = match parse_scopes(&request.scopes) {
        Ok(scopes) => scopes,
        Err(unknown) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("unknown scope: {unknown}"),
                &[("scopes", "INVALID_ENUM_VALUE")],
            );
        }
    };
    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(secret)) = (mint_api_key_id(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint an API key");
        return service_unavailable("provisioning");
    };
    let expires_at = match request.expires_at_ms {
        Some(ms) => match pos_proto::time::Timestamp::from_milliseconds_since_epoch(ms) {
            Ok(timestamp) => Some(timestamp),
            Err(_) => {
                return api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    "expires_at_ms is out of range",
                    &[("expires_at_ms", "OUT_OF_RANGE")],
                );
            }
        },
        None => None,
    };
    let (stored, token) = issue(id, tenant_id, store_id, scopes, &secret, expires_at);
    if let Err(error) = app.keys.insert(&stored).await {
        tracing::error!(%error, "persisting a new API key failed");
        return service_unavailable("provisioning");
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match app.keys.list_for_tenant(tenant_id).await {
        Ok(summaries) => (StatusCode::OK, Json(summaries)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing API keys failed");
            service_unavailable("provisioning")
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
    let id = match parse_ulid_fields([("id", &id)]) {
        Ok([id]) => ApiKeyId::new(id),
        Err(refusal) => return refusal,
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
            service_unavailable("provisioning")
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

/// Lower-case hex of arbitrary bytes.
///
/// Two callers, both of which need the *same* encoding: the opaque session handle the console sees
/// (a 32-byte hash, not reversible to the token, so exposing it grants no capability), and the
/// artifact signature header, whose lowercase-ness the edge's decoder enforces
/// ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Lower-case hex of the SHA-256 of `bytes`, in the shape the release registry stores.
///
/// An **integrity** check only. `ota_releases.sha256` exists to catch a truncated upload or a
/// corrupted blob; what makes an artifact safe to install is the minisign signature, which only the
/// edge verifies ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)).
fn hex_digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex_encode(&sha2::Sha256::digest(bytes))
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
    if let Err(denied) = authenticated_admin(&app.admin, &app.clock, &headers).await {
        return denied.into_response();
    }
    let credential = match app.admin.load_credential().await {
        Ok(Some(credential)) => credential,
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
    match app.admin.rotate_totp_secret(secret.to_vec()).await {
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

// --- Console admin management and invitations ([ADR-0067]) --------------------------------------

/// A `503` when the admin store is unreachable — the transient, retryable failure for the admin
/// surface, with the detail kept to the server's log rather than the client.
fn admin_service_unavailable() -> Response {
    api_error(ErrorStatus::Unavailable, "the admin service is unavailable")
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
            return Err(api_error(
                ErrorStatus::FailedPrecondition,
                "cannot remove the last active owner",
            ));
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
        return enum_refusal("role", AdminRole::ALL.iter().map(|role| role.as_token()));
    };
    // No privilege escalation: only an owner may mint another owner.
    if role == AdminRole::Owner && context.admin.role != AdminRole::Owner {
        return api_error(
            ErrorStatus::PermissionDenied,
            "only an owner may invite an owner",
        );
    }
    let email = request.email.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a valid email is required",
            &[("email", "INVALID_FORMAT")],
        );
    }
    match app.admin.find_admin_user_by_email(&email).await {
        Ok(Some(_)) => {
            return api_error_with_details(
                ErrorStatus::AlreadyExists,
                "an admin with that email already exists",
                &[("email", "ALREADY_EXISTS")],
            );
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
        // One field, out of range — a `400`, not the `422` this answered before ADR-0096. The
        // difference is what it tells the reader to do: there *is* a field to go and fix, which is
        // exactly what separates `INVALID_ARGUMENT` from `UNPROCESSABLE`.
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("the password must be at least {MIN_PASSWORD_LEN} characters"),
            &[("password", "OUT_OF_RANGE")],
        );
    }
    let now = app.clock.now();
    let invite = match app
        .admin
        .find_pending_invite_by_token(hash_session_token(&request.token), now)
        .await
    {
        Ok(Some(invite)) => invite,
        Ok(None) => {
            return api_error(
                ErrorStatus::Unauthenticated,
                "the invite is invalid or has expired",
            );
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
            return api_error(
                ErrorStatus::Unauthenticated,
                "the invite is invalid or has expired",
            );
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
        Ok(false) => api_error_with_details(
            ErrorStatus::AlreadyExists,
            "an admin with that email already exists",
            &[("email", "ALREADY_EXISTS")],
        ),
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
        return enum_refusal("role", AdminRole::ALL.iter().map(|role| role.as_token()));
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
        Ok(false) => not_found("admin"),
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
        return enum_refusal(
            "status",
            AdminStatus::ALL.iter().map(|status| status.as_token()),
        );
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
        Ok(false) => not_found("admin"),
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let Some(level) = parse_config_level(&level) else {
        return enum_refusal(
            "level",
            ConfigLevel::ORDER.iter().map(|level| level.as_str()),
        );
    };

    // Rehydrate the store's tree (or start a fresh one), authoring against the §10-aware validator.
    let loaded = match load_tree_for_write(&app.config_trees, tenant_id, store_id).await {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    let id = match publish_authored_layer(
        &app.config_trees,
        &app.clock,
        &headers,
        tenant_id,
        store_id,
        loaded,
        level,
        document,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
        {
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
    let context =
        match require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await {
            Ok(context) => context,
            Err(denied) => return denied,
        };
    let prices = role_grants(context.admin.role, ConsolePermission::ReadRevenue);
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    match app
        .config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(Some(state)) => {
            let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
            match tree.current_effective() {
                // Never the staff PIN hashes (S7), and never a price to a role without
                // `ReadRevenue` (S8) — see `without_staff_credentials` and `without_prices`.
                Some(effective) => {
                    let document = without_staff_credentials(effective);
                    let document = if prices {
                        document
                    } else {
                        without_prices(&document)
                    };
                    (StatusCode::OK, Json(document)).into_response()
                }
                None => no_published_configuration(),
            }
        }
        Ok(None) => no_published_configuration(),
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    match app
        .config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
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
        Ok(None) => no_published_configuration(),
        Err(error) => config_store_error_response(&error),
    }
}

/// Removes the credentials a console read must never carry (production-readiness **S7**).
///
/// The `permissions` node the People publish writes contains each staff member's Argon2id PIN hash —
/// **T1**, the credential the edge verifies a sign-in against. It belongs on the `/sync` route a store
/// reads with its own scoped key, and nowhere else. Until this existed, `GET /admin/stores/{id}/config`
/// handed the whole effective document to anyone holding `console.data.read` — which every role down
/// to Viewer holds — so a read-only console account could lift the PIN hash of every member of staff
/// in the fleet, one store at a time.
///
/// Stripped unconditionally rather than gated on a permission: no console screen reads a PIN hash, no
/// console screen could sensibly do anything with one, and a value nobody needs is not worth a role
/// check that could later be widened by mistake. The field is *removed*, not blanked, so a reader
/// cannot mistake an empty string for "this member has no PIN set" — which is a real and different
/// state (they cannot sign in).
fn without_staff_credentials(document: &serde_json::Value) -> serde_json::Value {
    let mut document = document.clone();
    if let Some(staff) = document
        .get_mut("permissions")
        .and_then(|node| node.get_mut("staff"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for member in staff {
            if let Some(member) = member.as_object_mut() {
                member.remove("pin_phc");
            }
        }
    }
    document
}

/// The config nodes whose published shape carries money, and which a console caller therefore reads
/// only with [`ConsolePermission::ReadRevenue`] (production-readiness **S8**).
///
/// A node earns a place here by carrying a **price a competitor would want** — `Money` in the wire
/// type the publish path writes — not merely by being commercially interesting. Today that is two:
///
/// - **`menu`** — the compiled per-channel price book. This is the node S8 was raised for: closing
///   `GET /admin/catalog/menus/{id}/placements` under **S5** left the same price book readable one
///   route over, through the config screen.
/// - **`campaigns`** — combo prices, discount amounts and minimum-bill thresholds
///   ([ADR-0077](../../../docs/adr/0077-campaigns.md)). Found while fixing S8, and included because a
///   redaction that closes one price surface and leaves its sibling open is not a fix; a promotion's
///   economics are the same **T2** class as a menu price.
///
/// `layout` is deliberately absent — buttons and their order carry no money — and so are `tax`
/// (published rates, which a receipt shows the guest anyway) and `permissions` (whose credential is
/// removed unconditionally by [`without_staff_credentials`]).
const PRICED_CONFIG_NODES: &[&str] = &["menu", "campaigns"];

/// Removes the priced nodes from a console read for a caller without `ReadRevenue`
/// (production-readiness **S8**).
///
/// **Why redact rather than refuse the whole read.** The alternative was to gate
/// `GET /admin/stores/{id}/config` on `ReadRevenue` outright, which is one line and costs Ops the
/// config screen — the screen they use to see a store's capabilities, floor, stations and printers.
/// The point of carving `ReadRevenue` out of `Read` was that prices are narrower than data, so the
/// narrow thing is what should be withheld.
///
/// The node is **removed, not blanked**, for the reason S7 gives: an empty object would read as
/// "this store has no menu published", which is a real and different state. The console tells the
/// operator that something is hidden by their role rather than letting them infer it from an absence.
fn without_prices(document: &serde_json::Value) -> serde_json::Value {
    let mut document = document.clone();
    if let Some(nodes) = document.as_object_mut() {
        for node in PRICED_CONFIG_NODES {
            nodes.remove(*node);
        }
    }
    document
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
    let context =
        match require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await {
            Ok(context) => context,
            Err(denied) => return denied,
        };
    let prices = role_grants(context.admin.role, ConsolePermission::ReadRevenue);
    let (tenant_id, store_id, version_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &store_id),
        ("version_id", &version_id),
    ]) {
        Ok([tenant_id, store_id, version_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            ConfigVersionId::new(version_id),
        ),
        Err(refusal) => return refusal,
    };
    match app
        .config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(Some(state)) => {
            let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
            match tree.effective_at(version_id) {
                // The diff view reads a past version, and a past version holds past PIN hashes (S7)
                // and past prices (S8) — a price is not less sensitive for being last month's.
                Some(effective) => {
                    let document = without_staff_credentials(effective);
                    let document = if prices {
                        document
                    } else {
                        without_prices(&document)
                    };
                    (StatusCode::OK, Json(document)).into_response()
                }
                None => not_found("config version"),
            }
        }
        Ok(None) => no_published_configuration(),
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
    let (tenant_id, store_id, version_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &store_id),
        ("version_id", &request.version_id),
    ]) {
        Ok([tenant_id, store_id, version_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            ConfigVersionId::new(version_id),
        ),
        Err(refusal) => return refusal,
    };
    let loaded = match load_tree_for_write(&app.config_trees, tenant_id, store_id).await {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    let Some(state) = loaded.state else {
        return no_published_configuration();
    };
    let mut tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
    // A rollback is a write, and it composes on what it read: refuse it if someone published in
    // between, so an operator cannot roll back over an edit they never saw (ADR-0095).
    if let Err(refusal) = if_match_config(&headers, tree.current_version()) {
        return refusal;
    }
    let Some(new_version_id) = mint_version_id(app.clock.now().as_milliseconds_since_epoch())
    else {
        tracing::error!("could not read OS entropy to mint a config version id");
        return service_unavailable("configuration");
    };
    let Some(new_id) = tree.restore(version_id, new_version_id) else {
        return not_found("config version");
    };
    match app
        .config_trees
        .save(tenant_id, store_id, &tree.state(), loaded.version.as_ref())
        .await
    {
        Ok(UpdateOutcome::Updated(_)) => {}
        Ok(UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => return version_mismatch(),
        Err(error) => return config_store_error_response(&error),
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    let days = match dashboard(&app.rollups, tenant_id, store_id, &window).await {
        Ok(days) => days,
        Err(error) => return rollup_error_response(&error),
    };
    let Ok(body) = export::rollups_csv(&days) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    let days = match revenue(&app.rollups, tenant_id, store_id, &window).await {
        Ok(days) => days,
        Err(error) => return rollup_error_response(&error),
    };
    let Ok(body) = export::revenue_csv(&days) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
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
            // The full rejection goes to the log and a coarser sentence to the caller, because two
            // of the six variants are decided by *this server's* resolver rather than by the URL
            // the caller sent: rendering them told a caller which internal names resolve and to
            // what address, one name per request, which is the reconnaissance the SSRF block exists
            // to refuse. `SsrfRejection::caller_message` draws that line; the delivery path already
            // logs the same way (`webhook/runner.rs`).
            //
            // The submitted URL is deliberately absent from the log line: a webhook URL routinely
            // carries its own bearer token in the path or query, and this is a log.
            tracing::warn!(reason = %rejection, "refused to register a webhook URL");
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                rejection.caller_message(),
                &[("url", rejection.caller_reason())],
            );
        }
        Err(join_error) => {
            tracing::error!(%join_error, "the SSRF vetting task failed to join");
            return service_unavailable("webhook");
        }
    };

    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(secret)) = (mint_webhook_id(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint a webhook endpoint");
        return service_unavailable("webhook");
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
        return service_unavailable("webhook");
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match app.webhooks.list_for_tenant(tenant_id).await {
        Ok(summaries) => (StatusCode::OK, Json::<Vec<WebhookSummary>>(summaries)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing webhook endpoints failed");
            service_unavailable("webhook")
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
    let (tenant_id, id) = match parse_ulid_fields([("tenant_id", &query.tenant_id), ("id", &id)]) {
        Ok([tenant_id, id]) => (TenantId::new(tenant_id), WebhookEndpointId::new(id)),
        Err(refusal) => return refusal,
    };
    match app.webhooks.delete(tenant_id, id).await {
        Ok(_removed) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "deleting a webhook endpoint failed");
            service_unavailable("webhook")
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
    let (tenant_id, endpoint_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("id", &id)]) {
            Ok([tenant_id, endpoint_id]) => (
                TenantId::new(tenant_id),
                WebhookEndpointId::new(endpoint_id),
            ),
            Err(refusal) => return refusal,
        };
    // Confirm the endpoint is this tenant's before clearing its flag: `set_disabled` is not itself
    // tenant-scoped, so the scope is enforced here against the tenant's own listing.
    match app.webhooks.list_for_tenant(tenant_id).await {
        Ok(summaries) => {
            if !summaries
                .iter()
                .any(|summary| summary.id == endpoint_id.to_string())
            {
                return not_found("webhook endpoint for this tenant");
            }
        }
        Err(error) => {
            tracing::error!(%error, "listing webhook endpoints failed");
            return service_unavailable("webhook");
        }
    }
    match app.webhooks.set_disabled(endpoint_id, false).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "re-enabling a webhook endpoint failed");
            service_unavailable("webhook")
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
    api_error(
        ErrorStatus::Unavailable,
        "the configuration service is unavailable",
    )
}

/// Parses a config-tree level from its path segment, or `None` for an unknown one.
fn parse_config_level(level: &str) -> Option<ConfigLevel> {
    ConfigLevel::ORDER
        .into_iter()
        .find(|candidate| candidate.as_str() == level)
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

/// Every error this surface sends, in the one AIP-193 shape
/// [`pos_proto::error`](../../../pos-proto/src/error.rs) defines
/// ([ADR-0026](../../../docs/adr/0026-cloud-http-surface.md) §27, `docs/naming-and-api.md` §4).
///
/// The HTTP status is **not** a parameter: it is derived from `status` through
/// [`ErrorStatus::http_code`], which is where the mapping is stated once. A caller that could pick
/// its own code could pick one that disagrees with the body it is sending, and a client branching on
/// either would then be reading a different error from the one the server meant.
///
/// Field-level detail has no caller yet — the inline validation sites that will carry it are the
/// next slice — so it is not a parameter either. When it arrives, `ErrorResponse::with_detail` is
/// already fluent and the door is a second constructor, not a widened signature.
pub(crate) fn api_error(status: ErrorStatus, message: impl Into<String>) -> Response {
    (
        http_status(status),
        Json(ErrorResponse::new(status, message)),
    )
        .into_response()
}

/// The same envelope, naming **which** field was wrong and why
/// (`docs/naming-and-api.md` §4).
///
/// This is the door [`api_error`] deliberately left open rather than widening its own signature: a
/// refusal that is not about a particular field cannot grow an empty `details` array by accident,
/// and a caller cannot pass one where none belongs.
///
/// `reason` is a stable `SCREAMING_SNAKE` token; `message` is prose for a person. They are separate
/// because they change at different rates — the message can be reworded or translated freely, while
/// the reason is what a client branches on, and wording a client depends on is wording that can no
/// longer be improved.
pub(crate) fn api_error_with_details(
    status: ErrorStatus,
    message: impl Into<String>,
    details: &[(&str, &str)],
) -> Response {
    let body = details.iter().fold(
        ErrorResponse::new(status, message),
        |body, (field, reason)| body.with_detail(*field, *reason),
    );
    (http_status(status), Json(body)).into_response()
}

/// The `422` a composed document earns when every field in it is valid and the document they make
/// together is not ([ADR-0096](../../../docs/adr/0096-unprocessable-status.md)).
///
/// The violations arrive from `pos-core` as prose a person can read — "the routing rule for station
/// `S01` names a backup station that does not exist" — not as field paths, so they join into
/// `message` rather than becoming `details`. Inventing a `field` for them would be fabricating
/// structure the domain never produced, and a client that branched on the fabricated `reason` would
/// be branching on a guess.
///
/// Separated from [`api_error`] so the four sites that report violations cannot each pick their own
/// separator: before this they could not even agree on a *body*, which is how the fifth shape —
/// the translation grid's — ended up with no reader in the console at all.
fn unprocessable_violations(violations: &[String]) -> Response {
    api_error(ErrorStatus::Unprocessable, violations.join("; "))
}

/// Parses `N` named ULID fields at once, refusing with a detail per field that **actually** failed.
///
/// Half of this file's refusals were this one sentence written by hand, and writing it by hand let
/// it drift three ways at once: 63 different phrasings of "not a ULID"; no `details` array, so a
/// console could not mark the offending input; and — the one that misinforms a caller — a tuple
/// parse whose message named *every* field it had looked at (`"tenant_id or store_id is not a
/// ULID"`) instead of the one that was wrong. About 120 sites did that. A caller reading it has to
/// guess which half of its request to fix.
///
/// Taking the fields as an argument is what makes that ambiguity *unwriteable* rather than merely
/// fixed here: there is no signature in this module that accepts a message about fields it did not
/// check, so the next id parse cannot reintroduce the shape.
///
/// Returning an array rather than a slice is what keeps the ids typed at the call site — a caller
/// destructures positionally, `Ok([tenant, store]) => (TenantId::new(tenant), StoreId::new(store))`,
/// so nothing is indexed and the newtype stays where it belongs, in the handler.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(crate) fn parse_ulid_fields<const N: usize>(
    fields: [(&str, &str); N],
) -> Result<[Ulid; N], Response> {
    let mut bad: Vec<&str> = Vec::new();
    // The placeholder for a field that did not parse never escapes: `bad` is non-empty in exactly
    // that case, and a non-empty `bad` returns the refusal instead of the array.
    let parsed = fields.map(|(field, raw)| match raw.parse::<Ulid>() {
        Ok(id) => id,
        Err(_ignored) => {
            bad.push(field);
            Ulid::NIL
        }
    });
    if bad.is_empty() {
        Ok(parsed)
    } else {
        Err(ulid_refusal(&bad))
    }
}

/// The refusal [`parse_ulid_fields`] returns: `INVALID_ARGUMENT`, a `NOT_A_ULID` detail per failed
/// field, and prose naming those fields in the order the caller listed them.
///
/// A single bad field reads `"tenant_id is not a ULID"` — the wording 51 of these sites already
/// used — so for the common case only the `details` array is new, and no client that was reading
/// the message sees it change.
fn ulid_refusal(bad: &[&str]) -> Response {
    let details: Vec<(&str, &str)> = bad.iter().map(|field| (*field, "NOT_A_ULID")).collect();
    let message = match bad {
        [field] => format!("{field} is not a ULID"),
        [head @ .., last] => format!("{} and {last} are not ULIDs", head.join(", ")),
        // Unreachable: the only caller refuses to build a refusal with nothing to refuse.
        [] => "a field is not a ULID".to_owned(),
    };
    api_error_with_details(ErrorStatus::InvalidArgument, message, &details)
}

/// The `400` a malformed rollup window earns, naming the query parameter at fault.
///
/// The mapping lives here rather than in `dashboard::rollup` because only the route knows the wire
/// names. `Inverted` carries **no** `details`: neither `from` nor `to` is wrong on its own, and the
/// standing rule is to name only the field that actually was — attributing it to `from` would send
/// a caller who mistyped `to` to the wrong input. The message already names both.
fn window_refusal(error: WindowError) -> Response {
    match error {
        WindowError::MalformedFrom => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "from must be a YYYY-MM-DD business date",
            &[("from", "INVALID_FORMAT")],
        ),
        WindowError::MalformedTo => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "to must be a YYYY-MM-DD business date",
            &[("to", "INVALID_FORMAT")],
        ),
        WindowError::Inverted => {
            api_error(ErrorStatus::InvalidArgument, "from must not be after to")
        }
        WindowError::LimitTooSmall => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "limit must be at least 1",
            &[("limit", "OUT_OF_RANGE")],
        ),
    }
}

/// Why a request-to-record builder refused, and which field to send the caller to.
///
/// The four builders returned a bare `&'static str`, and their own doc comments said why: a
/// `Result<_, Response>` trips `clippy::result_large_err`. That is a real constraint and this
/// respects it — three `&'static str`s is 48 bytes, well under the threshold — while fixing what
/// the bare string cost, which is that a refusal naming no field is one no console can mark.
///
/// `field` carries the full path for a nested value (`action.rate`, not `rate`), following
/// ADR-0096's reasoning that a path a reader can act on beats a name they have to interpret.
#[derive(Clone, Copy)]
struct FieldRefusal {
    message: &'static str,
    field: &'static str,
    reason: &'static str,
}

impl FieldRefusal {
    const fn new(message: &'static str, field: &'static str, reason: &'static str) -> Self {
        Self {
            message,
            field,
            reason,
        }
    }

    /// The `400` this refusal becomes at the handler that called the builder.
    fn into_response(self) -> Response {
        api_error_with_details(
            ErrorStatus::InvalidArgument,
            self.message,
            &[(self.field, self.reason)],
        )
    }
}

/// The refusal for a field whose value is outside a **closed set**: `INVALID_ARGUMENT`, an
/// `INVALID_ENUM_VALUE` detail naming the field, and prose listing what is accepted.
///
/// The set is passed in rather than spelled into the message, so every caller derives it from the
/// enum that owns it — `EntityStatus::ALL`, `AdminRole::ALL`, `AdminStatus::ALL`,
/// `ConfigLevel::ORDER`. Adding a variant then updates the refusal, where before it would have left
/// a sentence listing the old set: `"status must be active or archived"` was written out at eighteen
/// routes, so a third status would have needed eighteen edits and got none.
///
/// The prose it builds matches what those sites already said, so only the `details` array is new.
fn enum_refusal<'token>(field: &str, accepted: impl IntoIterator<Item = &'token str>) -> Response {
    let tokens: Vec<&str> = accepted.into_iter().collect();
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        format!("{field} must be {}", accepted_list(&tokens)),
        &[(field, "INVALID_ENUM_VALUE")],
    )
}

/// Reads a closed set as English: `"active or archived"`, `"owner, admin, ops, or viewer"`.
///
/// Separate from [`enum_refusal`] because this is the part with arities to get wrong, and a `String`
/// can be asserted where a `Response` body needs an async read. Both spellings match what the
/// hand-written sentences used, which is why generating them changed no message.
fn accepted_list(tokens: &[&str]) -> String {
    match tokens {
        [only] => (*only).to_owned(),
        [first, last] => format!("{first} or {last}"),
        [head @ .., last] => format!("{}, or {last}", head.join(", ")),
        // Unreachable: a closed set with nothing in it accepts nothing, so no value could be
        // refused *against* it.
        [] => "one of the accepted values".to_owned(),
    }
}

/// The refusal for a `status` field that is not an [`EntityStatus`] token.
///
/// Eighteen routes read this one field, which is why it gets a name of its own: the eighteen call
/// sites now say *which* refusal they mean rather than each restating the accepted set.
fn entity_status_refusal() -> Response {
    enum_refusal(
        "status",
        EntityStatus::ALL.iter().map(|status| status.as_str()),
    )
}

/// The refusal for a named thing that is not there: `NOT_FOUND`, `"no such {entity}"`, no details.
///
/// Sixty-one sites wrote this sentence out, thirty-five spellings of it, and the value of one home
/// here is smaller than for the ULID and closed-set refusals: nothing was *wrong*, only repeated.
/// What it does buy is that the next absent entity cannot arrive with a different phrasing or a
/// different status.
///
/// No `details` array, deliberately. `details` names a field the caller got wrong, and a caller
/// asking after an employee who does not exist got its fields right — the employee is absent.
/// Naming one here would send a client to fix an input that was fine.
fn not_found(entity: &str) -> Response {
    api_error(ErrorStatus::NotFound, format!("no such {entity}"))
}

/// The refusal for a store's configuration that has not been published yet.
///
/// Its own name because six routes answer it and it is not the `"no such X"` shape: the store
/// exists, and so does its config tree; what is missing is a *published version* of it.
fn no_published_configuration() -> Response {
    api_error(
        ErrorStatus::NotFound,
        "the store has no published configuration",
    )
}

/// A query param that was actually sent: absent, empty, and whitespace all read as absent.
///
/// The same rule `parse_optional_ulid` applies to optional id fields (#280), for the same reason —
/// clearing a form control sends `""`, and a caller that sent nothing meaningful should be treated as
/// having sent nothing.
fn present_param(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// Reads a list route's paging params: `None` for an unpaged read, `Some(Ok(_))` for a page,
/// `Some(Err(_))` for a refusal.
///
/// The three-way answer is the shape of ADR-0098's central decision. An absent `limit` is not a
/// missing value to default — it is the caller asking for the whole set, which is what a picker and
/// a compiler want and what these routes have always returned. Only a caller that names a limit gets
/// a page.
///
/// **`offset` without `limit` is refused**, not ignored. On its own it selects nothing coherent — the
/// unpaged read returns everything regardless — so honouring it silently would answer a question
/// nobody asked, and dropping it silently would lose one somebody did. Both are the kind of silence
/// ADR-0098 exists to remove.
///
/// **Two `&str` parameters rather than one flattened struct.** A `PageParams` carrying the pair and
/// `#[serde(flatten)]`-ed into each query struct would read better and does not compile here: the
/// flatten attribute makes serde's derive generate its buffering `Content` enum, whose `f32`/`f64`
/// variants `clippy.toml` bans outright (ADR-0013 — money is never a float), and it does so
/// regardless of the field types being flattened. So each list query declares its own
/// `Option<String>` pair and passes them here, and this function stays the only place the rule is
/// written. Measured, not assumed: flattening two `Option<String>` fields fails the `lints` gate with
/// `use of a disallowed type f32`.
///
/// The params arrive as text, not `u32`, for the same reason [`parse_ulid_fields`] takes strings:
/// typed as a number, axum's `Query` extractor would reject `?limit=abc` itself with its own
/// plain-text body, and every refusal on this surface is an AIP-193 envelope naming the field
/// (Q3a/Q3b).
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal a route returns as-is"
)]
fn parse_page(limit: Option<&str>, offset: Option<&str>) -> Option<Result<PageRequest, Response>> {
    let Some(limit_text) = present_param(limit) else {
        // No limit, so no page. An offset with nothing to offset into is a caller mistake worth
        // naming rather than dropping.
        return present_param(offset).map(|_offset| Err(offset_without_limit_refusal()));
    };
    let Ok(limit) = limit_text.parse::<u32>() else {
        return Some(Err(page_bound_refusal("limit")));
    };
    let offset = match present_param(offset) {
        None => 0,
        Some(text) => match text.parse::<u32>() {
            Ok(value) => value,
            Err(_ignored) => return Some(Err(page_bound_refusal("offset"))),
        },
    };
    Some(match PageRequest::new(limit, offset) {
        Ok(request) => Ok(request),
        Err(PageRequestError::LimitOutOfRange) => Err(page_bound_refusal("limit")),
        Err(PageRequestError::OffsetOutOfRange) => Err(page_bound_refusal("offset")),
    })
}

/// The refusal for a `limit` or `offset` that is absent-shaped, unparseable, or out of range.
///
/// One message for all three because they are one mistake from the caller's side: the value it sent
/// is not a page bound this API accepts, and the sentence says what one is. Splitting "not a number"
/// from "too large" would give a client two strings to branch on where the fix is identical.
fn page_bound_refusal(field: &str) -> Response {
    let accepted = if field == "limit" {
        format!("an integer from 1 to {MAX_PAGE_LIMIT}")
    } else {
        format!("an integer from 0 to {MAX_PAGE_OFFSET}")
    };
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        format!("{field} must be {accepted}"),
        &[(field, "OUT_OF_RANGE")],
    )
}

/// The refusal for an `offset` sent without a `limit`.
///
/// Its own sentence because the fix is not "correct the offset" but "add a limit", and a caller told
/// only that its offset was refused would go looking at the wrong parameter.
fn offset_without_limit_refusal() -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        "offset needs a limit: paging is a limit and an offset together, and without a limit this \
         read answers with its unpaged form instead",
        &[("offset", "MISSING_DEPENDENT_FIELD")],
    )
}

/// A page on the wire: the rows, the size of the whole set, and the bounds that produced them.
///
/// The bounds are echoed because a pager needs them to render "1–25 of 812" and to build the next
/// request, and because the alternative — the client tracking what it sent — breaks the moment a
/// link is shared or a page is reloaded.
#[derive(Debug, Clone, Serialize)]
struct PagedBody<T> {
    items: Vec<T>,
    total: u32,
    limit: u32,
    offset: u32,
}

/// Answers a paged read: `200` with the page, its total, and the bounds it used.
fn paged_ok<T: Serialize>(page: Page<T>, request: PageRequest) -> Response {
    (
        StatusCode::OK,
        Json(PagedBody {
            items: page.items,
            total: page.total,
            limit: request.limit(),
            offset: request.offset(),
        }),
    )
        .into_response()
}

/// The refusal for a dependency that is down: `SERVICE_UNAVAILABLE`, and no details.
///
/// `ErrorStatus::Unavailable`, not `InvalidArgument`, which is the distinction deriving the code
/// from [`ErrorStatus::http_code`] exists to keep: this refusal is **retryable** and the caller's
/// request was fine. A client that reads it as its own fault stops retrying something that would
/// have succeeded.
pub(crate) fn service_unavailable(service: &str) -> Response {
    api_error(
        ErrorStatus::Unavailable,
        format!("the {service} service is unavailable"),
    )
}

/// What was wrong with an `If-Match` value that is present but unusable.
///
/// Separate from the refusal it becomes so the parse can be tested as a pure function: the exact
/// strings a client branches on are worth asserting, and a test that only reads status codes would
/// pass on any of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EntityTagRefusal {
    /// `If-Match: *` — legal HTTP, and refused here. See [`if_match`].
    Wildcard,
    /// Not one strong entity-tag: a weak `W/"…"`, a comma-separated list, or unquoted.
    Malformed,
}

/// Reads one strong entity-tag out of a raw header value.
///
/// **Strong only.** RFC 9110 §13.1.1 evaluates `If-Match` with the strong comparison function,
/// under which a weak validator never matches — so accepting `W/"…"` would accept a token that can
/// only ever fail, which is worse than refusing it, because the caller would read the resulting
/// `412` as a real conflict and go looking for an edit nobody made.
///
/// **One tag, not a list.** A list is legal HTTP and means "any of these"; here the version is a
/// single row's, so a list has no meaning a caller could intend. The inner-quote check is what
/// rejects it: `"a", "b"` unwraps to `a", "b`, which still contains a quote.
fn parse_entity_tag(raw: &str) -> Result<Version, EntityTagRefusal> {
    let trimmed = raw.trim();
    if trimmed == "*" {
        return Err(EntityTagRefusal::Wildcard);
    }
    let inner = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or(EntityTagRefusal::Malformed)?;
    if inner.contains('"') {
        return Err(EntityTagRefusal::Malformed);
    }
    Ok(Version::new(inner))
}

/// The version a mutating route is being asked to replace
/// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
///
/// **Required, not optional.** Treating an absent header as "no opinion" would leave the silent
/// clobber this exists to close, and leave it as the *default* — every caller that had not been
/// updated would keep overwriting. Absent is therefore an ordinary missing-field refusal, in the
/// shape Q3b gave every other one, rather than a status of its own.
///
/// `If-Match: *` is refused too. It is legal HTTP meaning "whatever is there now", which is
/// precisely last-write-wins under a different name, and no caller in this tree wants it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn if_match(headers: &HeaderMap) -> Result<Version, Response> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match is required: send the etag the record was read at",
            &[("if-match", "REQUIRED")],
        ));
    };
    let Ok(text) = raw.to_str() else {
        return Err(entity_tag_refusal(EntityTagRefusal::Malformed));
    };
    parse_entity_tag(text).map_err(entity_tag_refusal)
}

/// The `If-Match` a **collection** write must carry
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
///
/// Same requirement as [`if_match`] and one difference: a wildcard is accepted, and means something
/// the record routes have no use for. A record is created by a `POST` that names no version; a
/// collection is not created at all — the tenant's tax table and translation grid are always
/// *there*, conceptually, and the only question is whether anything has been saved into them yet.
/// `If-Match: *` is how a caller says "nothing has", and `Ok(None)` carries that down to the store,
/// which is the only party that can tell whether it is still true. It is an assertion, not a waiver:
/// a store that has been saved to refuses it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
fn if_match_collection(headers: &HeaderMap) -> Result<Option<Version>, Response> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match is required: send the etag the collection was read at, or * if it has never been saved",
            &[("if-match", "REQUIRED")],
        ));
    };
    let Ok(text) = raw.to_str() else {
        return Err(entity_tag_refusal(EntityTagRefusal::Malformed));
    };
    match parse_entity_tag(text) {
        Ok(version) => Ok(Some(version)),
        Err(EntityTagRefusal::Wildcard) => Ok(None),
        Err(refusal @ EntityTagRefusal::Malformed) => Err(entity_tag_refusal(refusal)),
    }
}

/// The refusal for an `If-Match` this surface will not act on.
fn entity_tag_refusal(refusal: EntityTagRefusal) -> Response {
    match refusal {
        EntityTagRefusal::Wildcard => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must name a version, not *",
            &[("if-match", "WILDCARD_NOT_ACCEPTED")],
        ),
        EntityTagRefusal::Malformed => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be one strong entity-tag, in double quotes",
            &[("if-match", "INVALID_FORMAT")],
        ),
    }
}

/// The refusal for a write against a version the record no longer holds: `412`, and no details.
///
/// No `details` array, for the same reason absence and outage carry none: the caller's fields were
/// all fine. What went stale is the caller's *copy*, not any one input, so naming a field would
/// send it to fix something that was right.
fn version_mismatch() -> Response {
    api_error(
        ErrorStatus::VersionMismatch,
        "the record changed since you read it: re-read it and try again",
    )
}

/// The `409` a create earns when the key it asked for is taken.
///
/// Names the entity, because the caller's next move depends on it: edit the one that is there, or
/// pick a different key. Before [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)
/// split the six keyed upserts, this answer did not exist — an `upsert` overwrote instead, and the
/// only way a duplicate could surface was as the store's own `503`, which tells a caller the server
/// is broken and invites the retry that can never succeed.
fn already_exists(entity: &str) -> Response {
    api_error(
        ErrorStatus::AlreadyExists,
        format!("that {entity} already exists: edit it instead of creating another"),
    )
}

/// A freshly created record and the version it starts at, as `201`.
///
/// A create answers with a version for the same reason a read does: without one the caller has to
/// list the collection again before it can edit what it just made, which is a round trip to learn
/// something the server already knew. RFC 9110 provides for exactly this on `201`.
fn versioned_created<T>(record: T, version: &Version) -> Response
where
    T: Serialize,
{
    let body = Json(Versioned::new(record, version.clone()));
    match HeaderValue::from_str(&format!("\"{version}\"")) {
        Ok(tag) => (StatusCode::CREATED, [(ETAG, tag)], body).into_response(),
        Err(_ignored) => (StatusCode::CREATED, body).into_response(),
    }
}

/// Stamps the version a write left the record at onto a response, without changing its shape.
///
/// Five entities — areas, tables, stations, employees, role templates — answer a write with `204`
/// or a bare `{"id": …}`, not the record. That is not an oversight to correct here: their update
/// payloads do not carry every field (an `AreaUpdate` has no `store_id`), so a returned
/// "representation" would be one this handler had to invent. The header is the whole contract for
/// them — RFC 9110 §8.8.3 puts `ETag` on exactly this response — so a caller that wants the record
/// re-reads it, and a caller that wants to keep writing already holds the token it needs.
///
/// A token that cannot be a header value is the adapter's fault, not the caller's, and the write
/// itself still succeeded; the response goes out unstamped rather than turning a completed write
/// into a `500`.
fn with_etag(mut response: Response, version: &Version) -> Response {
    if let Ok(tag) = HeaderValue::from_str(&format!("\"{version}\"")) {
        response.headers_mut().insert(ETAG, tag);
    }
    response
}

/// A record and the version it is now at, as `200`.
///
/// Carries the version **twice**, deliberately: as the `ETag` header RFC 9110 defines for a single
/// resource, and as the `etag` field a list row has to use because one header cannot describe many
/// rows. Byte-identical either way, so a client has one code path for "remember this version" and
/// no opportunity to reformat a token it must not parse.
fn versioned_ok<T>(record: T, version: &Version) -> Response
where
    T: Serialize,
{
    let Ok(tag) = HeaderValue::from_str(&format!("\"{version}\"")) else {
        // Unreachable through the adapters in this tree, which mint digits. A token that cannot be
        // a header value is the adapter's fault, not the caller's, and the record is still correct.
        return (
            StatusCode::OK,
            Json(Versioned::new(record, version.clone())),
        )
            .into_response();
    };
    (
        StatusCode::OK,
        [(ETAG, tag)],
        Json(Versioned::new(record, version.clone())),
    )
        .into_response()
}

/// The `axum` status code for an [`ErrorStatus`], over `pos-proto`'s authoritative map.
///
/// The fallback is unreachable — every code [`ErrorStatus::http_code`] returns is a valid status,
/// and `every_status_maps_to_a_valid_code` holds it to that — but an unmappable code would be a
/// server-side fault by elimination, which is what `500` says.
fn http_status(status: ErrorStatus) -> StatusCode {
    StatusCode::from_u16(status.http_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Maps a [`PortError`] to an HTTP response, so a caller retries the retryable statuses (`503`,
/// `429`) and not the terminal ones.
///
/// The status comes from [`ErrorStatus::http_code`] rather than a match written here. The match this
/// replaced sent four caller-fault statuses — `NotFound`, `AlreadyExists`, `PermissionDenied`,
/// `Unauthenticated` — to `500`, which would have told a client its own bad request was the
/// server's fault and invited a retry that could never succeed. Only one handler reaches this today
/// (`ingest`) and none of those four is reachable from it, so nothing was actually mis-answering;
/// the point of deriving the code is that the next caller cannot inherit the trap.
fn error_response(error: &PortError) -> Response {
    api_error(error.status(), error.to_string())
}

/// Maps a rollup-read failure to a `503`, logging the detail rather than returning it — a dashboard
/// read only fails when the store itself is unreachable, which is transient and the caller's cue to
/// retry, and the internal reason is not the client's business.
fn rollup_error_response(error: &RollupError) -> Response {
    tracing::error!(%error, "a dashboard rollup read failed");
    api_error(
        ErrorStatus::Unavailable,
        "the dashboard is temporarily unavailable",
    )
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

#[cfg(test)]
mod conditional_write_tests {
    //! What `If-Match` accepts, and — more usefully — what it refuses.
    //!
    //! Every case here is a header a real client could plausibly send. Two of them look correct and
    //! are not, which is why they are pinned rather than reasoned about: a weak validator, which
    //! `If-Match` can never satisfy, and a wildcard, which is legal HTTP for exactly the
    //! last-write-wins behaviour ADR-0094 removes. The parse is a pure function so the refusal each
    //! produces can be asserted by name rather than by status code alone.

    use super::{EntityTagRefusal, parse_entity_tag};

    #[test]
    fn a_strong_entity_tag_unwraps_to_the_token_the_adapter_minted() {
        // Byte-identical to what the read handed out, quotes stripped and nothing else touched —
        // the token is opaque, so any normalisation here would corrupt a fork's scheme.
        for (header, token) in [
            (r#""1847302""#, "1847302"),
            (r#""sha256:9f86d0818""#, "sha256:9f86d0818"),
            (r#"  "42"  "#, "42"),
            (r#""""#, ""),
        ] {
            assert_eq!(
                parse_entity_tag(header).map(|version| version.as_str().to_owned()),
                Ok(token.to_owned()),
                "parsing {header}"
            );
        }
    }

    #[test]
    fn a_weak_validator_is_refused_because_if_match_could_never_match_it() {
        // RFC 9110 §13.1.1: `If-Match` uses the *strong* comparison function, under which a weak
        // validator never matches. Accepting `W/"…"` would accept a tag that can only ever fail,
        // and the caller would read the resulting 412 as a real conflict and go hunting for an edit
        // that nobody made.
        assert_eq!(
            parse_entity_tag(r#"W/"1847302""#),
            Err(EntityTagRefusal::Malformed)
        );
    }

    #[test]
    fn a_wildcard_is_refused_because_it_is_last_write_wins_by_another_name() {
        // `If-Match: *` is legal HTTP meaning "whatever is there now" — precisely the behaviour
        // ADR-0094 exists to remove. It gets its own refusal rather than the malformed one, because
        // the caller's header is well-formed and the answer it needs is different.
        assert_eq!(parse_entity_tag("*"), Err(EntityTagRefusal::Wildcard));
        assert_eq!(parse_entity_tag("  *  "), Err(EntityTagRefusal::Wildcard));
    }

    #[test]
    fn a_list_of_tags_is_refused_because_one_row_has_one_version() {
        // A list is legal HTTP and means "any of these". A row has one version, so a list names no
        // intent this surface could honour. The inner-quote check is what catches it.
        assert_eq!(
            parse_entity_tag(r#""a", "b""#),
            Err(EntityTagRefusal::Malformed)
        );
    }

    #[test]
    fn an_unquoted_token_is_refused_rather_than_guessed_at() {
        // Sending the token bare is the likeliest client mistake. Guessing that the caller meant
        // `"1847302"` would work until the day a scheme mints a token that is itself quoted.
        for header in ["1847302", r#""unterminated"#, r#"unopened""#] {
            assert_eq!(
                parse_entity_tag(header),
                Err(EntityTagRefusal::Malformed),
                "parsing {header}"
            );
        }
    }
}

#[cfg(test)]
mod absence_tests {
    //! The two refusals that are *not* the caller's fault, and the one property that separates them.
    //!
    //! Unlike the ULID and closed-set refusals, nothing here was wrong before — sixty-one absences
    //! and forty-two outages were simply written out one at a time. So this earns two tests, not six:
    //! the status codes, because `NotFound` and `Unavailable` differ in whether a client should
    //! **retry**, and that is the distinction deriving the code from [`ErrorStatus::http_code`]
    //! exists to keep. A `503` answered as a `400` would stop a client retrying something that would
    //! have succeeded on its own.
    //!
    //! That neither carries a `details` array is asserted over HTTP in `tests/cloud.rs`, where the
    //! body can be read.

    use super::{no_published_configuration, not_found, service_unavailable};
    use axum::http::StatusCode;

    /// An absent entity is the caller's answer, not its fault, and it is terminal.
    #[test]
    fn an_absence_is_a_terminal_404() {
        assert_eq!(not_found("employee").status(), StatusCode::NOT_FOUND);
        assert_eq!(
            no_published_configuration().status(),
            StatusCode::NOT_FOUND,
            "the store exists; what is missing is a published version"
        );
    }

    /// An outage is **retryable**, which is the whole reason it is a different `ErrorStatus`.
    #[test]
    fn an_outage_is_a_retryable_503() {
        assert_eq!(
            service_unavailable("configuration").status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "not 400: the request was fine and retrying it may work"
        );
    }
}

#[cfg(test)]
mod closed_set_tests {
    //! What a closed-set refusal lists, and the property that keeps it true.
    //!
    //! `"status must be active or archived"` was written out at eighteen routes. The sentence and the
    //! parser were two separate statements of one set, so a third status would have needed eighteen
    //! edits and got none of them: eighteen routes would have gone on refusing a value they now
    //! accepted, naming a set that no longer existed.
    //!
    //! [`enum_refusal`] builds the prose from the set it is handed and every caller hands it the enum's
    //! own list, so the message is generated. These tests pin that: the prose still reads the way the
    //! eighteen hand-written sentences did, and every token any of the four refusals lists is a token
    //! its parser actually accepts — which is the half a reviewer cannot check by eye.

    use super::{
        AdminRole, AdminStatus, ConfigLevel, EntityStatus, PaymentMethod, SalesChannel,
        accepted_list, accepted_tokens, entity_status_refusal, parse_config_level,
        parse_entity_status,
    };
    use axum::http::StatusCode;
    use pos_proto::wire_enum::WireEnum as _;

    /// Every token the `status` refusal lists is one [`parse_entity_status`] accepts.
    #[test]
    fn every_status_token_listed_is_one_the_parser_accepts() {
        for status in EntityStatus::ALL {
            assert_eq!(
                parse_entity_status(status.as_str()),
                Some(*status),
                "{} is listed, so it must parse",
                status.as_str()
            );
        }
        assert_eq!(
            parse_entity_status("retired"),
            None,
            "and nothing else does"
        );
    }

    /// Every token the `level` refusal lists is one [`parse_config_level`] accepts.
    #[test]
    fn every_level_token_listed_is_one_the_parser_accepts() {
        for level in ConfigLevel::ORDER {
            assert_eq!(
                parse_config_level(level.as_str()),
                Some(level),
                "{} is listed, so it must parse",
                level.as_str()
            );
        }
        assert_eq!(parse_config_level("region"), None, "and nothing else does");
    }

    /// The two admin enums round-trip their own tokens, which is what the `role` and admin `status`
    /// refusals list.
    #[test]
    fn the_admin_enums_round_trip_the_tokens_their_refusals_list() {
        for role in AdminRole::ALL {
            assert_eq!(AdminRole::from_token(role.as_token()), Some(*role));
        }
        for status in AdminStatus::ALL {
            assert_eq!(AdminStatus::from_token(status.as_token()), Some(*status));
        }
    }

    /// The prose reads exactly as the hand-written sentences did, at both arities the four fields
    /// use — which is what makes generating it a no-op for every existing client.
    #[test]
    fn the_prose_reads_the_way_the_hand_written_sentences_did() {
        assert_eq!(
            accepted_list(
                &EntityStatus::ALL
                    .iter()
                    .map(|status| status.as_str())
                    .collect::<Vec<_>>()
            ),
            "active or archived",
            "the eighteen `status` routes said this"
        );
        assert_eq!(
            accepted_list(
                &AdminRole::ALL
                    .iter()
                    .map(|role| role.as_token())
                    .collect::<Vec<_>>()
            ),
            "owner, admin, ops, or viewer",
            "the two `role` routes said this"
        );
        assert_eq!(
            accepted_list(
                &AdminStatus::ALL
                    .iter()
                    .map(|s| s.as_token())
                    .collect::<Vec<_>>()
            ),
            "active or suspended"
        );
        assert_eq!(
            accepted_list(
                &ConfigLevel::ORDER
                    .iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
            ),
            "tenant, brand, store, or device",
            "reworded from \"one of tenant, brand, store, device\" onto the one shape"
        );
        assert_eq!(
            accepted_list(&["only"]),
            "only",
            "and a one-value set is not a list"
        );
        assert_eq!(
            entity_status_refusal().status(),
            StatusCode::BAD_REQUEST,
            "a closed-set refusal is the caller's fault"
        );
    }

    /// Every token a `parse_known_tokens` refusal offers is one that same parser accepts, and
    /// `UNSPECIFIED` is offered by neither.
    ///
    /// The exclusion is the half worth pinning. `WireEnum::ALL` leads with `UNSPECIFIED`, which
    /// exists so an older client can read a newer server's value — it is never a choice a caller
    /// makes. Listing it as accepted would invite one, and the parser refuses it, so the refusal
    /// would be naming a token that earns the same refusal again.
    #[test]
    fn unspecified_is_never_offered_as_a_choice() {
        for accepted in [
            accepted_tokens::<SalesChannel>().collect::<Vec<_>>(),
            accepted_tokens::<PaymentMethod>().collect::<Vec<_>>(),
        ] {
            assert!(!accepted.is_empty(), "the set is not empty");
            assert!(
                !accepted.contains(&SalesChannel::UNSPECIFIED.as_wire())
                    && !accepted.contains(&PaymentMethod::UNSPECIFIED.as_wire()),
                "got {accepted:?}"
            );
        }
        for token in accepted_tokens::<SalesChannel>() {
            assert_eq!(
                SalesChannel::from_wire(token),
                Some(
                    SalesChannel::ALL
                        .iter()
                        .copied()
                        .find(|value| value.as_wire() == token)
                        .expect("the token came from ALL")
                ),
                "{token} is listed, so it must parse"
            );
        }
    }
}

#[cfg(test)]
mod page_params_tests {
    //! The rule for `?limit=`/`?offset=`, pinned where a future list route cannot fork it.
    //!
    //! The load-bearing case is the first one: an absent `limit` must stay "the whole set". Give it
    //! a default and five item pickers and the menu compiler start reading a page and saying nothing
    //! about it (ADR-0098).

    use super::{parse_page, present_param};
    use crate::paging::{MAX_PAGE_LIMIT, MAX_PAGE_OFFSET};

    #[test]
    fn no_limit_means_the_whole_set_and_not_a_default_page() {
        assert!(
            parse_page(None, None).is_none(),
            "an absent limit is the unpaged read, not a value to default"
        );
        // Blank and whitespace are absent too: a console clearing its page-size box sends `""`.
        assert!(parse_page(Some(""), None).is_none());
        assert!(parse_page(Some("   "), None).is_none());
    }

    #[test]
    fn a_limit_alone_starts_at_the_head_of_the_set() {
        let page = parse_page(Some("25"), None)
            .expect("a limit means a page")
            .expect("25 is in range");
        assert_eq!(page.limit(), 25);
        assert_eq!(page.offset(), 0, "no offset starts at the beginning");
    }

    #[test]
    fn an_offset_without_a_limit_is_refused_rather_than_ignored() {
        // Silently dropping it loses a question the caller asked; silently honouring it answers one
        // nobody did, since the unpaged read returns everything regardless.
        let refusal = parse_page(None, Some("50"))
            .expect("an offset is not nothing")
            .expect_err("but it is not a page either");
        assert_eq!(refusal.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_bound_that_is_not_a_number_is_refused_by_this_layer() {
        // Not axum's `Query` extractor: these arrive as text precisely so the refusal is an AIP-193
        // envelope naming the field rather than a plain-text rejection body.
        for (limit, offset) in [
            (Some("abc"), None),
            (Some("25"), Some("-1")),
            (Some("2.5"), None),
        ] {
            let refusal = parse_page(limit, offset)
                .expect("a limit was named")
                .expect_err("but it does not parse");
            assert_eq!(refusal.status(), axum::http::StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn a_bound_past_its_cap_is_refused_rather_than_clamped() {
        // Clamping would answer a different question than the one asked, and leave the caller to
        // notice by diffing what it sent against what came back. Nothing does that.
        for (limit, offset) in [
            (format!("{}", MAX_PAGE_LIMIT + 1), "0".to_owned()),
            ("0".to_owned(), "0".to_owned()),
            ("25".to_owned(), format!("{}", MAX_PAGE_OFFSET + 1)),
        ] {
            let refusal = parse_page(Some(&limit), Some(&offset))
                .expect("a limit was named")
                .expect_err("but a bound is out of range");
            assert_eq!(
                refusal.status(),
                axum::http::StatusCode::BAD_REQUEST,
                "limit={limit} offset={offset}"
            );
        }
        // The caps themselves are accepted: the range is inclusive on both ends.
        assert!(
            parse_page(
                Some(&MAX_PAGE_LIMIT.to_string()),
                Some(&MAX_PAGE_OFFSET.to_string())
            )
            .expect("a limit was named")
            .is_ok()
        );
    }

    #[test]
    fn a_present_bound_is_trimmed_before_it_is_parsed() {
        let page = parse_page(Some(" 25 "), Some(" 50 "))
            .expect("a limit was named")
            .expect("both are in range once trimmed");
        assert_eq!((page.limit(), page.offset()), (25, 50));
    }

    #[test]
    fn absent_blank_and_whitespace_are_the_same_absence() {
        // The shared rule, asserted on the helper itself so both bounds inherit it and a third
        // param added later cannot spell it differently.
        assert_eq!(present_param(None), None);
        assert_eq!(present_param(Some("")), None);
        assert_eq!(present_param(Some(" \t ")), None);
        assert_eq!(present_param(Some(" 7 ")), Some("7"));
    }
}

#[cfg(test)]
mod optional_ulid_tests {
    //! The one rule for an optional id field, pinned so it cannot fork per field again.
    //!
    //! Seven typed wrappers used to sit on `parse_optional_ulid`, and two of them skipped the trim
    //! and the empty filter — so `""` meant "unset" for five fields and "malformed" for two, on the
    //! same surface, with nothing in the types to notice. This test is what a re-introduced wrapper
    //! has to get past.

    use super::parse_optional_ulid;
    use crate::catalog::{MenuId, MenuSectionId};
    use pos_proto::ids::BrandId;
    use pos_proto::ulid::Ulid;

    /// A ULID an operator never types, in the Crockford alphabet the parser accepts.
    const ID: &str = "01JAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn absent_blank_and_whitespace_all_mean_unset() {
        // The three ways a client says "no value": the field is missing, cleared to an empty string
        // (what a select box sends), or whitespace a form left behind. All three are the same
        // answer, because a caller cannot be expected to know which spelling a given field wants.
        for value in [None, Some(""), Some("   "), Some("\t")] {
            assert_eq!(
                parse_optional_ulid(value, BrandId::new),
                Ok(None),
                "{value:?} must read as unset"
            );
        }
    }

    #[test]
    fn a_present_value_must_be_a_ulid_and_is_trimmed() {
        assert_eq!(
            parse_optional_ulid(Some(ID), BrandId::new),
            Ok(Some(BrandId::new(ID.parse::<Ulid>().expect("a ULID")))),
            "a present value parses"
        );
        assert_eq!(
            parse_optional_ulid(Some(&format!("  {ID}  ")), BrandId::new),
            Ok(Some(BrandId::new(ID.parse::<Ulid>().expect("a ULID")))),
            "and surrounding whitespace is not the caller's mistake to pay for"
        );
        // A present-but-malformed value is the caller's error, and stays one: `Err` is what the
        // handlers turn into the `400` that names the field.
        for value in ["not-a-ulid", "01JAAAA", "01JAAAAAAAAAAAAAAAAAAAAAAI"] {
            assert_eq!(
                parse_optional_ulid(Some(value), BrandId::new),
                Err(()),
                "{value:?} must be refused"
            );
        }
    }

    #[test]
    fn the_rule_does_not_depend_on_which_id_type_is_being_parsed() {
        // The two fields that disagreed were `brand_id` on a store and `parent_menu_id` on a menu.
        // They are checked here against a third that always agreed, because the property under test
        // is that the id type is irrelevant — one function, one rule.
        assert_eq!(parse_optional_ulid(Some(""), BrandId::new), Ok(None));
        assert_eq!(parse_optional_ulid(Some(""), MenuId::new), Ok(None));
        assert_eq!(parse_optional_ulid(Some(""), MenuSectionId::new), Ok(None));
        assert_eq!(parse_optional_ulid(Some("nope"), BrandId::new), Err(()));
        assert_eq!(parse_optional_ulid(Some("nope"), MenuId::new), Err(()));
        assert_eq!(
            parse_optional_ulid(Some("nope"), MenuSectionId::new),
            Err(())
        );
    }
}

#[cfg(test)]
mod error_envelope_tests {
    //! The one error shape, and the two properties that keep it honest.
    //!
    //! Both are about *disagreement*, which is the failure mode an error path has that a success
    //! path does not: a body that says one thing while the status line says another. A client reads
    //! whichever it trusts, and is then acting on an error the server did not send.

    use super::{
        CampaignRequest, FieldRefusal, IngredientRequest, RecipeRequest, SalesChannel,
        SupplierRequest, WindowError, api_error, build_campaign, build_ingredient, build_recipe,
        build_supplier, error_response, http_status, parse_known_tokens, window_refusal,
    };
    use axum::http::StatusCode;
    use axum::response::Response;
    use pos_ports::{PortError, PortName};
    use pos_proto::error::ErrorBody;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::WireEnum as _;
    use pos_proto::{CampaignId, IngredientId, MenuItemId, SupplierId};
    use pos_proto::{ErrorResponse, ErrorStatus};

    /// Every canonical status, `Unspecified` included — a server never emits it, but if one ever
    /// leaked through `http_status` it must still produce a valid code rather than panic.
    fn every_status() -> impl Iterator<Item = ErrorStatus> {
        ErrorStatus::ALL.iter().copied()
    }

    /// Reads a response's status line and its parsed body together, because the point of every test
    /// here is that the two agree.
    async fn read(response: Response) -> (StatusCode, ErrorBody) {
        let status = response.status();
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json"),
            "an error body is JSON, or a client parsing the envelope reads plain text instead"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("collect the error body");
        let parsed: ErrorResponse = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "the body is not the AIP-193 envelope ({error}): {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, parsed.error)
    }

    #[test]
    fn every_status_maps_to_a_valid_code() {
        // `http_status` falls back to 500 on an unmappable code. This is the test that makes that
        // fallback dead rather than a silent reinterpretation of somebody's 4xx.
        for status in every_status() {
            let code = status.http_code();
            assert_eq!(
                StatusCode::from_u16(code)
                    .ok()
                    .map(|mapped| mapped.as_u16()),
                Some(code),
                "{status} maps to {code}, which is not a valid HTTP status code"
            );
            assert_eq!(http_status(status).as_u16(), code, "{status}");
        }
    }

    #[tokio::test]
    async fn the_status_line_and_the_body_cannot_disagree() {
        for status in every_status() {
            let (line, body) = read(api_error(status, "a message")).await;
            assert_eq!(
                line.as_u16(),
                status.http_code(),
                "{status}: the status line"
            );
            assert_eq!(body.code, status.http_code(), "{status}: the body's code");
            assert_eq!(
                body.status.as_wire(),
                status.as_wire(),
                "{status}: the token"
            );
            assert_eq!(body.message, "a message", "{status}: the message");
            assert!(body.details.is_empty(), "{status}: no detail was asked for");
        }
    }

    #[tokio::test]
    async fn a_port_error_answers_with_its_own_status_rather_than_five_hundred() {
        // The match this replaced sent NotFound, AlreadyExists, PermissionDenied and Unauthenticated
        // to 500 — telling a client the server had broken when the client's own request was at
        // fault, and inviting a retry that could never succeed. Only `ingest` reaches this today and
        // none of those four is reachable from it, so nothing was mis-answering; deriving the code
        // is what stops the *next* caller inheriting the trap. This test is that guarantee: it fails
        // if the match ever comes back.
        for status in every_status() {
            let error = PortError::new(PortName::EventStore, status, "the port refused");
            let (line, body) = read(error_response(&error)).await;
            assert_eq!(
                line.as_u16(),
                status.http_code(),
                "{status} answered {line}, not the code its own status implies"
            );
            assert_eq!(body.status.as_wire(), status.as_wire(), "{status}");
        }
    }

    #[tokio::test]
    async fn an_unrecognised_token_names_the_field_and_lists_the_accepted_set() {
        // The old refusal was `"{token} is not a recognised value"` as plain text: it named neither
        // the field nor the set, which between them are the only two things a caller can act on.
        let refusal = parse_known_tokens::<SalesChannel>("enabled", &["NOT_A_CHANNEL".to_owned()])
            .expect_err("an unknown token is refused");
        let (line, body) = read(refusal).await;
        assert_eq!(line, StatusCode::BAD_REQUEST);
        assert_eq!(body.status.as_wire(), "INVALID_ARGUMENT");
        assert_eq!(
            body.details
                .iter()
                .map(|detail| (detail.field.as_str(), detail.reason.as_str()))
                .collect::<Vec<_>>(),
            vec![("enabled", "INVALID_ENUM_VALUE")],
            "the field the caller sent, not the token it sent in it"
        );
        assert!(
            body.message.contains("DINE_IN"),
            "the message lists the set: {}",
            body.message
        );
        assert!(
            !body.message.contains("UNSPECIFIED"),
            "and does not offer the token the parser refuses: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn a_window_refusal_names_the_bound_unless_neither_is_wrong_alone() {
        // The split is the point: `RollupWindow::new` checked both bounds in one loop and threw away
        // which had failed, one line before the caller needed it.
        for (error, expected) in [
            (WindowError::MalformedFrom, Some(("from", "INVALID_FORMAT"))),
            (WindowError::MalformedTo, Some(("to", "INVALID_FORMAT"))),
            (WindowError::LimitTooSmall, Some(("limit", "OUT_OF_RANGE"))),
            // The exception, and the one worth stating: `from` and `to` are each fine and only their
            // order is wrong, so naming either would send half of callers to the wrong input.
            (WindowError::Inverted, None),
        ] {
            let (line, body) = read(window_refusal(error)).await;
            assert_eq!(line, StatusCode::BAD_REQUEST, "{error:?}");
            assert_eq!(
                body.details
                    .iter()
                    .map(|detail| (detail.field.as_str(), detail.reason.as_str()))
                    .collect::<Vec<_>>(),
                expected.into_iter().collect::<Vec<_>>(),
                "{error:?}"
            );
        }
    }

    /// Every field a builder names is one its request struct actually deserialises, at the path a
    /// caller would address it by.
    ///
    /// The request goes through `serde_json` rather than being constructed, which is the whole
    /// point: it pins the wire name, the struct and the refusal to each other. #150 found five
    /// shipped `details` entries naming fields no request carried (`tax_rates` where the struct has
    /// `rates`), and every one of them looked correct beside its neighbours — a console marking
    /// `details.field` would have marked an input the caller never sent.
    ///
    /// Nested values carry their **full path**, so `action.rate` rather than `rate`: the console
    /// addresses the input the reader sees, instead of guessing which parent a leaf belongs to.
    /// Reads a builder's refusal down to the `(field, reason)` pairs it carries.
    async fn refusal_details(refusal: FieldRefusal) -> Vec<(String, String)> {
        let (line, body) = read(refusal.into_response()).await;
        assert_eq!(line, StatusCode::BAD_REQUEST);
        body.details
            .into_iter()
            .map(|detail| (detail.field, detail.reason))
            .collect()
    }

    /// Reads a JSON body into a request struct the way axum does.
    ///
    /// From text rather than `from_value`, because `CurrencyCode` deserialises from a borrowed
    /// string — going through the owned tree would fail on the very field this is here to exercise.
    fn request<T: serde::de::DeserializeOwned>(body: &serde_json::Value) -> T {
        serde_json::from_str(&body.to_string()).unwrap_or_else(|error| panic!("{error}"))
    }

    #[tokio::test]
    async fn the_campaign_builder_names_a_field_its_own_request_carries() {
        let campaign_id = CampaignId::new(Ulid::from_u128(1));
        let base = serde_json::json!({
            "tenant_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "name": "Happy hour",
            "kind": "bill_level",
            "priority": 1,
            "action": { "type": "percentage", "rate": { "numerator": 10, "denominator": 100 } },
        });
        let with = |patch: &serde_json::Value| -> CampaignRequest {
            let mut body = base.clone();
            for (key, value) in patch.as_object().unwrap_or_else(|| unreachable!()) {
                body[key] = value.clone();
            }
            request(&body)
        };

        // Each schedule bound is named separately. Before this both were checked by one `if` that
        // OR'd them and named neither, so a caller who sent both could not tell which was refused.
        let cases = [
            (serde_json::json!({ "name": "  " }), "name", "REQUIRED"),
            (
                serde_json::json!({
                    "conditions": { "schedule": { "days": 127, "start_minute": 9999, "end_minute": 60 } },
                }),
                "conditions.schedule.start_minute",
                "OUT_OF_RANGE",
            ),
            (
                serde_json::json!({
                    "conditions": { "schedule": { "days": 127, "start_minute": 60, "end_minute": 9999 } },
                }),
                "conditions.schedule.end_minute",
                "OUT_OF_RANGE",
            ),
            (
                serde_json::json!({ "conditions": { "channels": ["NOT_A_CHANNEL"] } }),
                "conditions.channels",
                "INVALID_ENUM_VALUE",
            ),
            (
                serde_json::json!({
                    "action": { "type": "percentage", "rate": { "numerator": -1, "denominator": 100 } },
                }),
                "action.rate",
                "OUT_OF_RANGE",
            ),
            (
                serde_json::json!({
                    "action": {
                        "type": "amount_off",
                        "amount": { "currency_code": "VND", "amount_minor": -1 },
                    },
                }),
                "action.amount",
                "OUT_OF_RANGE",
            ),
        ];
        for (patch, field, reason) in cases {
            let refusal = build_campaign(&with(&patch), campaign_id)
                .err()
                .unwrap_or_else(|| panic!("{field} must be refused"));
            assert_eq!(
                refusal_details(refusal).await,
                vec![(field.to_owned(), reason.to_owned())],
                "{field}"
            );
        }
    }

    /// The inventory builders, same property. See
    /// [`the_campaign_builder_names_a_field_its_own_request_carries`] for why it is worth pinning.
    #[tokio::test]
    async fn the_inventory_builders_name_a_field_their_own_requests_carry() {
        for (body, field, reason) in [
            (
                serde_json::json!({
                    "tenant_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "name": " ",
                    "unit": "UNIT_OF_MEASURE_GRAM",
                }),
                "name",
                "REQUIRED",
            ),
            (
                serde_json::json!({
                    "tenant_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "name": "Flour",
                    "unit": "NOT_A_UNIT",
                }),
                "unit",
                "INVALID_ENUM_VALUE",
            ),
        ] {
            let ingredient: IngredientRequest = request(&body);
            let refusal = build_ingredient(&ingredient, IngredientId::new(Ulid::from_u128(1)))
                .err()
                .unwrap_or_else(|| panic!("{field} must be refused"));
            assert_eq!(
                refusal_details(refusal).await,
                vec![(field.to_owned(), reason.to_owned())],
                "{field}"
            );
        }

        for (body, field) in [
            (
                serde_json::json!({
                    "tenant_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "auto_86_threshold": -1,
                }),
                "auto_86_threshold",
            ),
            (
                serde_json::json!({
                    "tenant_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "lines": [{
                        "ingredient": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                        "per_unit": { "milli": 0 },
                    }],
                }),
                "lines",
            ),
        ] {
            let recipe: RecipeRequest = request(&body);
            let refusal = build_recipe(&recipe, MenuItemId::new(Ulid::from_u128(1)))
                .err()
                .unwrap_or_else(|| panic!("{field} must be refused"));
            assert_eq!(
                refusal_details(refusal).await,
                vec![(field.to_owned(), "OUT_OF_RANGE".to_owned())],
                "{field}"
            );
        }

        let supplier: SupplierRequest = request(&serde_json::json!({
            "tenant_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "name": "\t",
        }));
        let refusal = build_supplier(&supplier, SupplierId::new(Ulid::from_u128(1)))
            .err()
            .unwrap_or_else(|| unreachable!("a blank supplier name is refused"));
        assert_eq!(
            refusal_details(refusal).await,
            vec![("name".to_owned(), "REQUIRED".to_owned())]
        );
    }
}

#[cfg(test)]
mod console_config_redaction_tests {
    //! What a console read of a store's configuration must never carry.
    //!
    //! Two independent redactions run on the same document and the tests keep them independent: the
    //! staff credential goes unconditionally (**S7**), the priced nodes go by role (**S8**). A change
    //! that merged them would be a change in behaviour for one of the two.

    use super::{PRICED_CONFIG_NODES, without_prices, without_staff_credentials};
    use serde_json::json;

    /// A document shaped like a real effective config: a priced node, a free one, and the staff node.
    fn document() -> serde_json::Value {
        json!({
            "menu": { "channels": { "dine_in": { "items": [{ "price_amount_minor": 95_000 }] } } },
            "campaigns": { "rules": [{ "amount": { "currency": "VND", "minor": 20_000 } }] },
            "layout": { "buttons": ["margherita"] },
            "capabilities": { "tips_enabled": true },
            "permissions": { "staff": [{ "employee_id": "01J", "pin_phc": "$argon2id$v=19$..." }] },
        })
    }

    #[test]
    fn a_role_without_read_revenue_gets_no_priced_node() {
        let redacted = without_prices(&document());
        for node in PRICED_CONFIG_NODES {
            assert!(
                redacted.get(*node).is_none(),
                "`{node}` carries money and must not reach a caller without ReadRevenue (S8)"
            );
        }
    }

    #[test]
    fn the_rest_of_the_document_survives_the_price_redaction() {
        // The whole reason S8 redacts rather than gating the route: Ops keeps the config screen.
        let redacted = without_prices(&document());
        assert!(
            redacted.get("layout").is_some(),
            "buttons carry no money, so Ops still sees the till's layout"
        );
        assert!(
            redacted.get("capabilities").is_some(),
            "capabilities are what the config screen is *for*"
        );
        assert!(
            redacted.get("permissions").is_some(),
            "the staff node stays; its credential is removed separately (S7)"
        );
    }

    #[test]
    fn the_two_redactions_are_independent() {
        // A priced read still loses the PIN hash, and a redacted read still loses the prices —
        // neither is a side effect of the other.
        let priced = without_staff_credentials(&document());
        assert!(
            priced.get("menu").is_some(),
            "a caller with ReadRevenue keeps the price book"
        );
        let staff = priced["permissions"]["staff"][0]
            .as_object()
            .expect("a staff member");
        assert!(
            !staff.contains_key("pin_phc"),
            "the credential goes unconditionally (S7), whatever the role"
        );

        let both = without_prices(&priced);
        assert!(both.get("menu").is_none(), "and the prices go by role (S8)");
    }

    #[test]
    fn redacting_a_document_that_has_no_priced_node_changes_nothing() {
        // A store that has published capabilities and nothing else is the common case on day one.
        let plain = json!({ "capabilities": { "tips_enabled": false } });
        assert_eq!(without_prices(&plain), plain);
    }
}
