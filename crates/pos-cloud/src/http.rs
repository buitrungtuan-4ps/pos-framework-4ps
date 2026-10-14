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

use axum::extract::{Path, Query, Request, State};
use axum::http::HeaderName;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_MATCH, REFERRER_POLICY, RETRY_AFTER,
    SET_COOKIE, STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
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
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{ConfigVersionId, DeviceId, EventId, StoreId, SubjectId, TenantId};
use pos_proto::locale::CountryCode;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;

use crate::alerts::{AlertRecord, AlertStore, AlertStoreError};
use crate::archive::{
    ArchiveSecret, ArchiveStore, StoreArchive, StoreArchiveKey, archive_object_key,
};
use crate::audit::{AuditRecorder, NoopAuditRecorder};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::apikey::{ApiKeyAdminStore, ApiKeyId, ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::auth::console_rbac::{ConsolePermission, role_grants};
use crate::auth::enrol::{TOTP_SECRET_BYTES, constant_time_eq};
use crate::auth::password::hash_password;
use crate::auth::rate_limit::SlidingRateLimiter;
use crate::cloud::Cloud;
use crate::config::InternalSecret;
use crate::config_tree::prerequisites_for;
use crate::config_tree::{
    CapabilityValidator, ConfigLevel, ConfigTree, ConfigTreeState, ConfigTreeStore, SyncOutcome,
};
use crate::dashboard::{RollupStore, admitted_devices};
use crate::fleet::{FleetRow, FleetStore, FleetStoreError, OtaReportStore, PrintAgentStanding};
use crate::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use crate::lease::{
    HandoverFacts, HandoverState, LeaseBumpOutcome, LeaseStore, LeaseStoreError,
    RegionAcknowledgement, RegionAcknowledgementStore, RegionAgreement, RegionRefusal,
    RetireOutcome, SettleOutcome, handover_state, lease_node, region_agreement, resolve_region,
    standing_acknowledgement,
};
use crate::openapi::ApiDoc;

mod admin;
mod authored_layer;
mod campaigns;
mod catalog;
mod config_nodes;
mod config_publish;
mod countries;
mod devices;
mod fees;
mod floor;
mod integrations;
mod inventory;
mod lease_bump;
mod locale_node;
mod media;
mod people;
mod reason_codes;
mod release_schedule;
mod setting_refusal;
mod settings;
mod settings_confirm;
mod store_groups;
mod tax_rates;
mod translations;

pub use admin::audit_router;
use admin::{
    admin_accept_invite, admin_create_api_key, admin_invite_admin, admin_list_admins,
    admin_list_api_keys, admin_list_invites, admin_list_sessions, admin_revoke_api_key,
    admin_revoke_invite, admin_revoke_other_sessions, admin_revoke_session, admin_set_admin_role,
    admin_set_admin_status,
};
// The console's OpenAPI document names each documented handler by its path here.
pub(crate) use admin::{
    __path_admin_generate_recovery_codes, __path_admin_login, __path_admin_logout,
    __path_admin_recovery_codes_status, __path_admin_reenrol_totp, __path_admin_session,
    __path_admin_setup, __path_admin_whoami, admin_generate_recovery_codes, admin_login,
    admin_logout, admin_recovery_codes_status, admin_reenrol_totp, admin_session, admin_setup,
    admin_whoami,
};
use authored_layer::publish_authored_layer;
use campaigns::scheduled_error_response;
pub use campaigns::{
    campaign_router, config_campaigns_router, scheduled_publish_router, voucher_router,
};
use catalog::{catalog_error_response, clean_name_translations};
pub use catalog::{catalog_publish_router, catalog_router};
use config_publish::{
    LoadedTree, ReleaseNodeRequest, ReleaseState, ScheduleReleaseRequest, admin_config_nodes,
    admin_config_rollback, admin_config_version_effective, admin_config_versions, if_match_config,
    if_match_lease, layer_with_nodes, load_tree_for_write, only_settings, preview_config_node,
    publish_config_nodes, publish_nodes, release_error_response, release_report_response,
    release_target_stores, release_target_timezones, staff_redacted_for, unresolvable_moment,
    without_prices,
};
pub use config_publish::{config_preview_router, config_release_router};
pub(crate) use config_publish::{publish_config_nodes_with, strip_tree_version};
// The console's OpenAPI document names each documented handler by its path here.
pub use config_nodes::{
    config_channels_router, config_locale_router, config_store_profile_router, config_tax_router,
};
pub(crate) use config_publish::{
    __path_admin_cancel_release, __path_admin_config_nodes, __path_admin_create_release,
    __path_admin_list_releases, __path_admin_read_release, __path_admin_schedule_release,
};
pub use countries::country_router;
pub(crate) use devices::{
    __path_admin_revoke_device, __path_admin_set_printer_paper, __path_admin_set_terminal_float,
    __path_admin_set_terminal_receipt,
};
pub use devices::{
    activation_router, claim_router, device_publish_router, device_revoke_router, device_router,
};
pub use fees::fees_router;
use integrations::{
    admin_delete_webhook, admin_enable_webhook, admin_list_webhooks, admin_register_webhook,
};
pub use integrations::{config_integrations_router, connection_router, provider_catalogue_router};
// The console's OpenAPI document names each documented handler by its path here.
pub use floor::{floor_publish_router, floor_router, table_qr_router};
pub(crate) use integrations::{
    __path_admin_create_connection, __path_admin_delete_connection, __path_admin_get_connection,
    __path_admin_list_connections, __path_admin_list_providers, __path_admin_publish_integrations,
    __path_admin_update_connection,
};
// The console's OpenAPI document names each documented handler by its path here.
pub(crate) use fees::{
    __path_admin_create_fee, __path_admin_delete_fee, __path_admin_effective_fees,
    __path_admin_list_fees, __path_admin_preview_fees, __path_admin_publish_fees,
    __path_admin_put_fee,
};
pub use inventory::{config_inventory_router, inventory_router};
pub use media::media_router;
pub use reason_codes::{config_reason_codes_router, reason_code_router};
// The console's OpenAPI document names each documented handler by its path here.
pub(crate) use people::{__path_admin_permissions_readiness, republish_people};
pub use people::{
    capabilities_router, config_capabilities_router, people_publish_router, people_router,
    permissions_readiness_router,
};
pub(crate) use reason_codes::{
    __path_admin_create_reason_code, __path_admin_delete_reason_code, __path_admin_get_reason_code,
    __path_admin_list_reason_codes, __path_admin_publish_reason_codes,
    __path_admin_update_reason_code,
};
use setting_refusal::save_if_confirmed;
pub use settings::settings_router;
pub use store_groups::{BatchNode, BatchNodePlan, batch_nodes, store_group_router};
use store_groups::{BoxFuture, store_group_error_response, unknown_batch_node};
// The console's OpenAPI document names each documented handler by its path here.
pub(crate) use settings::{
    __path_admin_apply_setting_presets, __path_admin_clear_setting,
    __path_admin_effective_settings, __path_admin_list_settings, __path_admin_publish_settings,
    __path_admin_put_setting, __path_admin_settings_catalogue,
};
use settings::{TenantLayout, settings_store_error_response};
pub(crate) use store_groups::{
    __path_admin_create_store_group, __path_admin_list_store_group_batches,
    __path_admin_list_store_groups, __path_admin_publish_to_store_group,
    __path_admin_read_store_group_batch, __path_admin_set_store_group_members,
    __path_admin_update_store_group,
};
pub use tax_rates::tax_rate_router;
pub use translations::translation_router;

// The error envelope and its helpers live in pos-cloud-api; `crate::http::api_error` and the rest
// stay importable from here.
use crate::openapi_admin::AdminApiDoc;
use pos_cloud_reports::revenue::{
    admin_export_revenue, admin_export_revenue_fees, admin_export_revenue_tax, admin_revenue_daily,
};
use pos_cloud_reports::rollups::{
    admin_daily_rollups, admin_export_rollups, admin_rollups_reset, daily_rollups,
};
use pos_cloud_reports::state::ReportsState;
// The console's permission check, tenant query and audit entry live in pos-cloud-api, and the
// rollup refusals in pos-cloud-reports; `crate::http::require_permission` and the rest still work.
use crate::till_names::PublishedTillNames;
use base64::Engine as _;
pub(crate) use pos_cloud_api::console::{
    ConfigTenantQuery, audit_action, mint_ulid, require_permission,
};
pub(crate) use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable, ulid_refusal,
};
pub(crate) use pos_cloud_reports::refusal::rollup_error_response;
use pos_cloud_reports::xz::admin_xz_report;
use pos_core::lease::LeaseGeneration;

use crate::anchor::{AnchorConflict, AnchorLedger};
use crate::ota::{
    ArtifactKind, ArtifactStore, ArtifactStoreError, MAX_ARTIFACT_BYTES, MINISIGN_SIGNATURE_LEN,
    RecordOutcome, ReleaseArtifact, TargetTriple, WINDOWS_TARGET, admit_artifact, artifact_key,
    installer_file_name, validate_release_tag,
};
use crate::paging::{MAX_PAGE_LIMIT, MAX_PAGE_OFFSET, Page, PageRequest, PageRequestError};
use crate::reconcile::{ReconcileRun, ReconcileRunStore, ReconcileStore};
use crate::registry::{
    BrandId, BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError,
    StoreRecord, TenantRecord,
};
use crate::release_source::{ReleaseSource, ReleaseSourceError};
use crate::retention::{RetentionError, SubjectStore};
// These are ADR-0125 config releases — a named set of config publishes going out to a set of shops.
// They import under their own plain spelling, which they could not do while `crate::ota` also
// exported a `ReleaseStore`; that one is `ArtifactStore` now, so the alias this import used to carry
// is gone and the reader no longer has to hold two meanings of one word at once.
use crate::starting_roles::StartingRoles;
use crate::store_groups::StoreGroupStore;
use crate::version::{UpdateOutcome, Version, Versioned};
use crate::webhook::WebhookEndpointStore;
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
    pub(crate) rollups: R,
    pub(crate) keys: K,
    pub(crate) clock: C,
    pub(crate) admin: A,
    pub(crate) config_trees: T,
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
    pub(crate) audit: Arc<dyn AuditRecorder>,
    /// When QR ordering got one switch ([`crate::qr_ordering::OneSwitch::since`]): a rollback to a
    /// version published before then is settled. `None` restores every version as it is.
    qr_switch_since: Option<pos_proto::Timestamp>,
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
            qr_switch_since: self.qr_switch_since,
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
            qr_switch_since: None,
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
            activate_limiter: SlidingRateLimiter::new(ACTIVATE_MAX_ATTEMPTS, ACTIVATE_WINDOW_SECS),
            collect_limiter: SlidingRateLimiter::new(
                CLAIM_COLLECT_MAX_POLLS,
                CLAIM_COLLECT_WINDOW_SECS,
            ),
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

    /// Sets when QR ordering got one switch, which the binary reads from its one-time setting
    /// ([`crate::qr_ordering::run_once`]): a rollback to a version published before then gives it
    /// the switch it ran ([`crate::qr_ordering::settle`]). Left unset, a rollback restores every
    /// version as it is.
    #[must_use]
    pub const fn with_qr_switch_since(mut self, since: pos_proto::Timestamp) -> Self {
        self.qr_switch_since = Some(since);
        self
    }
}

/// The reporting routes' part of the application state (ADR-0170 decision 4): what a report handler
/// reads, cloned out of the whole application per request, as axum clones the application itself.
impl<S, R, K, C, A, T, W> axum::extract::FromRef<CloudApp<S, R, K, C, A, T, W>>
    for ReportsState<R, K, C, A, PublishedTillNames<T>>
where
    R: Clone,
    K: Clone,
    C: Clone,
    A: Clone,
    T: Clone,
{
    fn from_ref(app: &CloudApp<S, R, K, C, A, T, W>) -> Self {
        Self {
            rollups: app.rollups.clone(),
            keys: app.keys.clone(),
            clock: app.clock.clone(),
            admin: app.admin.clone(),
            audit: Arc::clone(&app.audit),
            till_names: PublishedTillNames(app.config_trees.clone()),
        }
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
            get(daily_rollups::<R, K, C, A, PublishedTillNames<T>>),
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
        .route(
            "/sync/stores/{store_id}/events",
            post(edge_publish_events::<S, R, K, C, A, T, W>).layer(
                axum::extract::DefaultBodyLimit::max(MAX_PUBLISHED_EVENTS_BYTES),
            ),
        )
        .route(
            "/sync/stores/{store_id}/events/hello",
            post(edge_events_hello::<S, R, K, C, A, T, W>),
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
            "/admin/stores/{store_id}/config/nodes",
            get(admin_config_nodes::<S, R, K, C, A, T, W>),
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
            post(admin_rollups_reset::<R, K, C, A, PublishedTillNames<T>>),
        )
        .route(
            "/admin/stores/{store_id}/rollups/daily",
            get(admin_daily_rollups::<R, K, C, A, PublishedTillNames<T>>),
        )
        // Beside the other rollup reads, and on **this** router, because the roster is folded into
        // the same materialised blob the reads above answer from — the state that answers it lives
        // here. It is under `/admin/stores/…/devices/` rather than `/admin/devices/` because it is a
        // fact about one store, and `admitted` distinguishes it from the sibling
        // `/admin/stores/{store_id}/devices`, which lists the console's **named** devices: a
        // different identity space that nothing joins to this one (ADR-0118 §4).
        .route(
            "/admin/stores/{store_id}/devices/admitted",
            get(admin_admitted_devices::<S, R, K, C, A, T, W>),
        )
        .route(
            "/admin/stores/{store_id}/revenue/daily",
            get(admin_revenue_daily::<R, K, C, A, PublishedTillNames<T>>),
        )
        .route(
            "/admin/stores/{store_id}/reports/xz",
            get(admin_xz_report::<R, K, C, A, PublishedTillNames<T>>),
        )
        .route(
            "/admin/stores/{store_id}/rollups/export",
            get(admin_export_rollups::<R, K, C, A, PublishedTillNames<T>>),
        )
        .route(
            "/admin/stores/{store_id}/revenue/export",
            get(admin_export_revenue::<R, K, C, A, PublishedTillNames<T>>),
        )
        .route(
            "/admin/stores/{store_id}/revenue/fees/export",
            get(admin_export_revenue_fees::<R, K, C, A, PublishedTillNames<T>>),
        )
        .route(
            "/admin/stores/{store_id}/revenue/tax/export",
            get(admin_export_revenue_tax::<R, K, C, A, PublishedTillNames<T>>),
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

/// The collaborators the chain-finding read shares: the anchor ledger it lists from, the admin store
/// the permission check authenticates against, and the clock that drives the session guard.
#[derive(Clone)]
struct ChainState<A, C> {
    anchors: Arc<dyn AnchorLedger>,
    admin: A,
    clock: C,
}

/// The default and maximum page size for the chain-finding read.
const CHAIN_FINDINGS_DEFAULT_LIMIT: u32 = 100;
const CHAIN_FINDINGS_MAX_LIMIT: u32 = 500;

/// Builds the chain-finding sub-router, stated independently of [`CloudApp`].
///
/// One route: `GET /admin/chain-findings`, the contradictions the cloud refused
/// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4,
/// [ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)). Until this
/// existed a finding reached a human only as a line in the server's log and a row nobody could
/// read — which for a mechanism whose entire product is *being noticed* is most of the way to not
/// having it.
///
/// Stated independently and merged in `main`, like [`reconcile_router`], rather than threading an
/// extra collaborator through every [`CloudApp`] handler. The ledger is an `Arc<dyn AnchorLedger>`
/// because that is what [`crate::cloud::Cloud`] already holds one as.
pub fn chain_router<A, C>(anchors: Arc<dyn AnchorLedger>, admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/chain-findings", get(admin_chain_findings::<A, C>))
        .with_state(ChainState {
            anchors,
            admin,
            clock,
        })
}

/// A `GET /admin/chain-findings` query: the tenant whose findings to list, an optional store filter,
/// and an optional page size.
#[derive(Debug, Clone, Deserialize)]
struct ChainFindingsQuery {
    /// The tenant whose findings to read (a 26-character ULID).
    tenant_id: String,
    /// Narrow to one store (a ULID); absent reads across the tenant's stores.
    #[serde(default)]
    store_id: Option<String>,
    /// Cap the number of findings returned; defaults to [`CHAIN_FINDINGS_DEFAULT_LIMIT`], clamped to
    /// [`CHAIN_FINDINGS_MAX_LIMIT`].
    #[serde(default)]
    limit: Option<u32>,
}

/// One refused chain claim on the wire.
///
/// Positions, hashes and identifiers — no event contents and no customer identifier. A chain hash is
/// a digest of an envelope whose every payload field `pos_proto::pii` has already proven free of
/// personal data.
#[derive(Debug, Clone, serde::Serialize)]
struct ChainFindingView {
    /// The finding's id, derived from the contradiction rather than minted, so a re-delivery of the
    /// same bad claim does not multiply into rows.
    finding_id: String,
    /// The store whose log disagrees with itself (a ULID string).
    store_id: String,
    /// The chain length both hashes claim to describe.
    chain_seq: u64,
    /// What the cloud holds there.
    held_head: String,
    /// What it was offered against it.
    offered_head: String,
    /// The event that carried the offered claim, so the finding traces back to a record.
    offered_event_id: String,
    /// Unix ms when the **cloud** noticed. The server's clock, which is the one an investigation can
    /// rely on — the store's is on the anchor, and a store that is tampering controls that one.
    noticed_at_ms: i64,
}

impl ChainFindingView {
    fn from_conflict(conflict: AnchorConflict) -> Self {
        Self {
            finding_id: conflict.conflict_id,
            store_id: conflict.report.store.to_string(),
            chain_seq: conflict.report.chain_seq,
            held_head: conflict.report.held_head.as_str().to_owned(),
            offered_head: conflict.report.offered_head.as_str().to_owned(),
            offered_event_id: conflict.report.offered_event_id.to_string(),
            noticed_at_ms: conflict.noticed_at.as_milliseconds_since_epoch(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/chain-findings",
    params(
        ("tenant_id" = String, Query, description = "The tenant whose findings to read (a 26-character ULID)"),
        ("store_id" = Option<String>, Query, description = "Narrow to one store (a ULID); absent reads across the tenant's stores"),
    ),
    responses(
        (status = 200, description = "The contradictions the cloud refused, newest first. Each is \
                                      two hashes claiming to describe one store's chain at one \
                                      length, and only one of them can be true. An empty list is \
                                      the expected answer: it means nothing was refused, not that \
                                      nothing was checked"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The anchor ledger is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// Lists a tenant's most recent refused chain claims, newest first. Tenant-scoped (a store's
/// findings are its tenant's data), behind [`ConsolePermission::Read`].
async fn admin_chain_findings<A, C>(
    State(state): State<ChainState<A, C>>,
    headers: HeaderMap,
    Query(query): Query<ChainFindingsQuery>,
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
        .unwrap_or(CHAIN_FINDINGS_DEFAULT_LIMIT)
        .min(CHAIN_FINDINGS_MAX_LIMIT);
    match state.anchors.list_conflicts(tenant_id, store, limit).await {
        Ok(conflicts) => {
            let view: Vec<ChainFindingView> = conflicts
                .into_iter()
                .map(ChainFindingView::from_conflict)
                .collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "reading the chain findings failed");
            service_unavailable("chain findings")
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
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
// Store archives ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md))
// ---------------------------------------------------------------------------------------------

/// The largest sealed archive the cloud accepts from a store.
///
/// The same number `pos_edge::backup::MAX_ARCHIVE_BYTES` refuses to *write*, so a store cannot
/// produce an archive this rejects — the two must move together, and a mismatch shows up as a
/// store whose backups have silently stopped. On the route rather than the reverse proxy, for the
/// reason the release upload gives: a fork terminating TLS elsewhere ([ADR-0090](../../../docs/adr/0090-tls-postures.md)'s
/// `external` posture) still gets the limit.
const MAX_STORE_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

/// What the collaborators the archive routes need: the key store and clock that authenticate a
/// store, the object store the sealed bytes go to, the registry that indexes them, and the
/// box-local secret that wraps a key.
#[derive(Clone)]
struct ArchiveState<K, C, B, R> {
    keys: K,
    clock: C,
    blobs: B,
    archives: R,
    secret: ArchiveSecret,
}

/// When the store took the snapshot — its clock, not the cloud's.
///
/// Required rather than defaulted to "now": a shop that was offline for a day ships yesterday's
/// archive today, and the recovery point is when the snapshot was taken. Defaulting would quietly
/// record the wrong one, and the wrong one is the number an operator reads when deciding what they
/// have lost.
#[derive(Debug, Clone, Deserialize)]
struct ArchiveUploadQuery {
    taken_at: i64,
}

/// The key a store seals with, as it comes back over `/sync`.
#[derive(Debug, Clone, Serialize)]
struct ArchiveKeyResponse {
    key: String,
}

/// What the cloud recorded, echoed so a store can log what it shipped.
#[derive(Debug, Clone, Serialize)]
struct ArchiveAcceptedResponse {
    object_key: String,
    size_bytes: i64,
    sha256: String,
}

/// Builds the store-archive sub-router
/// ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
///
/// Merged only when **both** an object store and `archive_key_secret` are configured, exactly like
/// [`ota_artifact_router`]: with no object store there is nowhere for the bytes to go, and with no
/// secret there is nowhere safe to keep the key, so a route that always answers `503` would be
/// worse than one honestly absent. `pos-cloud`'s boot says which of the two is missing.
///
/// Two routes, both authenticated by the store's own scoped key and both held to it by
/// [`require_store`] — the same gate the report route beside them uses (S1). `Scope::ReadConfig` is
/// what a store's provisioned key carries, and it is what the whole store-facing `/sync` surface is
/// gated on; a new scope would be a scope every already-issued key lacks, which would show up as
/// archives that never start.
pub fn store_archive_router<K, C, B, R>(
    keys: K,
    clock: C,
    blobs: B,
    archives: R,
    secret: ArchiveSecret,
) -> Router
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    R: ArchiveStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/archive-key",
            post(serve_store_archive_key::<K, C, B, R>),
        )
        .route(
            "/sync/stores/{store_id}/archive",
            post(receive_store_archive::<K, C, B, R>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_STORE_ARCHIVE_BYTES,
        ))
        .with_state(ArchiveState {
            keys,
            clock,
            blobs,
            archives,
            secret,
        })
}

/// `POST /sync/stores/{store_id}/archive-key` — the key this store seals its archives with, minted
/// on the first ask.
///
/// Mint-on-first-ask rather than an admin action, because a store that starts archiving must not
/// need somebody to have remembered; the console reveals and rotates it afterwards. The mint is
/// insert-if-absent in one statement ([`ArchiveStore::adopt_key`]), so two tills asking in the same
/// second get the *same* key rather than two — one of which would be the key to archives nobody
/// could open.
///
/// A `POST` for a value that reads like a `GET`, and deliberately so: the first call has a side
/// effect that outlives it. A `GET` that mints is one a caching proxy or a browser prefetch may
/// perform unasked, and the thing it would create here is the key to every archive this store ever
/// ships.
async fn serve_store_archive_key<K, C, B, R>(
    State(state): State<ArchiveState<K, C, B, R>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    R: ArchiveStore + Clone + Send + Sync + 'static,
{
    let (grant, store_id) =
        match authorize_store(&state.keys, &state.clock, &headers, &store_id).await {
            Ok(pair) => pair,
            Err(refusal) => return refusal,
        };

    match state.archives.wrapped_key(grant.tenant(), store_id).await {
        Ok(Some(wrapped)) => match StoreArchiveKey::unwrap_from(&wrapped, &state.secret) {
            Ok(key) => Json(ArchiveKeyResponse { key: key.to_text() }).into_response(),
            Err(error) => {
                // The stored key does not open under this box's secret. Almost always
                // `archive_key_secret` was changed, which orphans every key wrapped under the old
                // one (ADR-0124 Amendment 1). `Internal`, not `Unavailable`: retrying does not fix
                // it, and telling a store to back off and try again would be a lie.
                tracing::error!(
                    %error,
                    store = %store_id,
                    "a stored archive key does not open under archive_key_secret; was the secret                      changed?"
                );
                api_error(
                    ErrorStatus::Internal,
                    "this store's archive key cannot be read with the configured                      archive_key_secret",
                )
            }
        },
        Ok(None) => mint_store_archive_key(&state, grant.tenant(), store_id).await,
        Err(error) => {
            tracing::warn!(%error, "reading the archive registry failed");
            service_unavailable("archive registry")
        }
    }
}

/// Mints, wraps and adopts a key, answering with whichever key is current afterwards.
async fn mint_store_archive_key<K, C, B, R>(
    state: &ArchiveState<K, C, B, R>,
    tenant: TenantId,
    store_id: StoreId,
) -> Response
where
    R: ArchiveStore,
    C: ClockSource,
{
    let Ok(minted) = StoreArchiveKey::mint() else {
        return api_error(
            ErrorStatus::Internal,
            "the OS entropy source is unavailable, so no archive key could be minted",
        );
    };
    let wrapped = match minted.wrap(&state.secret) {
        Ok(wrapped) => wrapped,
        Err(error) => {
            tracing::error!(%error, "wrapping a new archive key failed");
            return api_error(
                ErrorStatus::Internal,
                "the new archive key could not be wrapped for storage",
            );
        }
    };
    let minted_at = state.clock.now().as_milliseconds_since_epoch();
    // The returned wrapped key is whichever one is current — this one, or the one a concurrent
    // till adopted a moment earlier. Unwrapping *that* rather than answering with `minted` is what
    // makes the race safe rather than merely rare.
    let current = match state
        .archives
        .adopt_key(tenant, store_id, &wrapped, minted_at)
        .await
    {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(%error, "recording a new archive key failed");
            return service_unavailable("archive registry");
        }
    };
    match StoreArchiveKey::unwrap_from(&current, &state.secret) {
        Ok(key) => Json(ArchiveKeyResponse { key: key.to_text() }).into_response(),
        Err(error) => {
            tracing::error!(%error, "a just-adopted archive key does not open");
            api_error(
                ErrorStatus::Internal,
                "this store's archive key cannot be read with the configured archive_key_secret",
            )
        }
    }
}

/// `POST /sync/stores/{store_id}/archive?taken_at=` — a store ships a sealed archive.
///
/// The cloud is a **sink for bytes it cannot read**. It does not open the archive, does not parse
/// it, and holds no key material for it beyond the wrapped column — the seal happened at the till
/// precisely so that a store's buyer details ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md))
/// never exist here in plaintext.
///
/// What it *does* check is the header: eight magic bytes and a format version, so an obviously
/// wrong body (a plain database, a truncated download, an HTML error page from a proxy) is refused
/// at upload instead of discovered at restore. That is a shape check and emphatically not
/// verification — only the key opens an archive, and the cloud has none.
async fn receive_store_archive<K, C, B, R>(
    State(state): State<ArchiveState<K, C, B, R>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ArchiveUploadQuery>,
    body: axum::body::Bytes,
) -> Response
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    R: ArchiveStore + Clone + Send + Sync + 'static,
{
    let (grant, store_id) =
        match authorize_store(&state.keys, &state.clock, &headers, &store_id).await {
            Ok(pair) => pair,
            Err(refusal) => return refusal,
        };
    if query.taken_at <= 0 {
        return api_error(
            ErrorStatus::InvalidArgument,
            "taken_at: milliseconds since the epoch, from the store's own clock",
        );
    }
    if !looks_like_a_store_archive(&body) {
        return api_error(
            ErrorStatus::InvalidArgument,
            "this body is not a store archive; the store seals one with `pos-edge archive`",
        );
    }

    let key_text = archive_object_key(store_id, query.taken_at);
    let Ok(object_key) = BlobKey::parse(&key_text) else {
        // Unreachable: the components are a ULID and a zero-padded integer. Reported rather than
        // unwrapped, because a panic here would take the process down for a case the types nearly
        // rule out.
        return api_error(
            ErrorStatus::Internal,
            "the archive's storage key could not be built",
        );
    };
    if let Err(error) = state.blobs.put(&object_key, &body).await {
        tracing::warn!(%error, key = object_key.as_str(), "storing a store archive failed");
        return service_unavailable("object store");
    }

    let size_bytes = i64::try_from(body.len()).unwrap_or(i64::MAX);
    let sha256 = hex_digest(&body);
    let archive = StoreArchive {
        store_id,
        taken_at: query.taken_at,
        object_key: key_text,
        size_bytes,
        sha256: sha256.clone(),
        received_at: state.clock.now().as_milliseconds_since_epoch(),
    };
    if let Err(error) = state
        .archives
        .record_archive(grant.tenant(), &archive)
        .await
    {
        // The bytes are stored and the index row is not, so the archive exists but nothing knows
        // it does. `Unavailable` rather than `Internal`, because the store retrying is exactly the
        // right response: `put` is last-write-wins and `record_archive` is an upsert, so the retry
        // converges rather than duplicating.
        tracing::warn!(%error, "recording a store archive failed after the bytes were stored");
        return service_unavailable("archive registry");
    }
    tracing::info!(
        store = %store_id,
        size_bytes,
        taken_at = query.taken_at,
        "a store archive arrived"
    );
    Json(ArchiveAcceptedResponse {
        object_key: archive.object_key,
        size_bytes,
        sha256,
    })
    .into_response()
}

/// Authenticates a store's own scoped key and holds it to the store in the path.
///
/// Extracted because both archive routes need the identical four steps, and a second copy is a
/// second place for one of them to go missing.
async fn authorize_store<K, C>(
    keys: &K,
    clock: &C,
    headers: &HeaderMap,
    store_id: &str,
) -> Result<(crate::auth::apikey::Grant, StoreId), Response>
where
    K: ApiKeyStore,
    C: ClockSource,
{
    let grant = authenticate(keys, clock, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    require_scope(&grant, Scope::ReadConfig).map_err(IntoResponse::into_response)?;
    let store_id = match parse_ulid_fields([("store_id", store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return Err(refusal),
    };
    require_store(&grant, store_id).map_err(IntoResponse::into_response)?;
    Ok((grant, store_id))
}

/// Whether these bytes begin the way `pos_edge::backup` writes an archive.
///
/// The magic and the format byte, and nothing else. It costs nine bytes to tell an operator "you
/// uploaded the wrong file" at the moment they can still fix it, rather than at a restore months
/// later; it proves nothing about the contents, which only the key can.
fn looks_like_a_store_archive(body: &[u8]) -> bool {
    const MAGIC: &[u8] = b"P4PSTORE";
    const FORMAT_VERSION: u8 = 1;
    body.get(..MAGIC.len()) == Some(MAGIC) && body.get(MAGIC.len()) == Some(&FORMAT_VERSION)
}

// --- The console's view of what a store has archived (ADR-0124 slice 3) ------------------------

/// How many archives one console read returns.
///
/// The window is measured in weeks and archives are daily, so this comfortably covers a whole
/// window and then some. It is a cap rather than a page: the question this answers is "what can I
/// restore", and an operator scrolling past sixty backups is not the case to design for.
const MAX_ARCHIVES_LISTED: i64 = 60;

/// What the console-facing archive read needs.
#[derive(Clone)]
struct ArchiveReadState<R, A, C> {
    archives: R,
    admin: A,
    clock: C,
}

/// One archive as the console sees it.
///
/// No key and nothing that could open one — this is the *index*, and the key is a separate,
/// deliberate ask. `object_key` is here because it is what an operator types into a restore.
#[derive(Debug, serde::Serialize)]
struct ArchiveView {
    /// When the store took the snapshot, Unix ms. **This is the recovery point** — not
    /// `received_at_ms`, which is when a possibly-delayed upload landed.
    taken_at_ms: i64,
    /// When the cloud received it, Unix ms. The gap between the two is how far behind a store's
    /// uplink was.
    received_at_ms: i64,
    /// How many bytes, sealed.
    size_bytes: i64,
    /// The cloud's hex SHA-256 of the sealed bytes as they arrived, so a download can be checked
    /// before anybody spends time on a key.
    sha256: String,
    /// Where the bytes are in the object store.
    object_key: String,
}

/// The answer: what this store has, newest first.
#[derive(Debug, serde::Serialize)]
struct StoreArchivesResponse {
    archives: Vec<ArchiveView>,
}

/// Builds the console-facing archive read
/// ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md) slice 3).
///
/// Mounted **unconditionally**, unlike [`store_archive_router`], and that is the decision: the
/// store-facing routes need an object store and `archive_key_secret` to do anything at all, but
/// this one needs only the registry. An operator whose `archive_key_secret` has gone missing is
/// exactly the operator who most needs to see what archives exist — hiding the list along with the
/// routes would take the evidence away at the moment it matters.
pub fn store_archive_admin_router<R, A, C>(archives: R, admin: A, clock: C) -> Router
where
    R: ArchiveStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/stores/{store_id}/archives",
            get(admin_list_store_archives::<R, A, C>),
        )
        .with_state(ArchiveReadState {
            archives,
            admin,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/archives",
    params(
        ("store_id" = String, Path, description = "The store whose archives to list (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant the store belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "This store's sealed archives, newest first — the first row's \
                                      taken_at_ms is the recovery point. An empty list is this \
                                      answer too: the store has never archived"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The archive registry is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// `GET /admin/stores/{store_id}/archives?tenant_id=` — what this store has shipped, newest first.
///
/// Behind [`ConsolePermission::Read`], the same gate the fleet view uses: "when did this store last
/// back up" is an operational fact every console role should be able to see, and a Viewer who
/// cannot see that a shop has not archived in a fortnight is a Viewer who cannot raise it.
async fn admin_list_store_archives<R, A, C>(
    State(state): State<ArchiveReadState<R, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    R: ArchiveStore + Clone + Send + Sync + 'static,
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
    match state
        .archives
        .list_archives(tenant, store, MAX_ARCHIVES_LISTED)
        .await
    {
        // An empty list is a `200`, not a `404`: a store that has never archived is a real and
        // important answer, and the one an operator most needs to see plainly.
        Ok(archives) => Json(StoreArchivesResponse {
            archives: archives
                .into_iter()
                .map(|archive| ArchiveView {
                    taken_at_ms: archive.taken_at,
                    received_at_ms: archive.received_at,
                    size_bytes: archive.size_bytes,
                    sha256: archive.sha256,
                    object_key: archive.object_key,
                })
                .collect(),
        })
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "reading a store's archives failed");
            service_unavailable("archive registry")
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The `/admin` release surface: putting an artifact in, and reading what is hosted
// ---------------------------------------------------------------------------------------------

/// The header the upload carries the signature in: the **first base64 line** of the `.minisig` file
/// minisign wrote (line 2 of the file — line 1 is the untrusted comment).
///
/// Deliberately a different name from [`ARTIFACT_SIGNATURE_HEADER`], which the *serve* route answers
/// with in lowercase hex. Same bytes, two encodings, two directions: reusing one name for both would
/// make a mis-encoded upload look like a valid download header.
const UPLOAD_SIGNATURE_HEADER: &str = "x-pos-minisig";

/// The collaborators the `/admin` release routes need: the object store the bytes go to, the registry
/// that records them, and the admin/clock/audit trio every console write carries.
#[derive(Clone)]
struct OtaReleaseAdminState<B, L, A, C> {
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

/// Builds the `/admin` release sub-router — the half that puts an artifact *into* the cloud, and
/// hands a Windows store its one-file installer back out of it.
///
/// Merged only when `[artifacts]` is configured, exactly like [`ota_artifact_router`]: without an
/// object store there is nowhere for the bytes to go, and a route that always answers `503` is worse
/// than a route that is honestly absent.
///
/// `POST /admin/ota/releases?release=&arch=` takes the **bare executable** as its body and minisign's
/// signature line in [`UPLOAD_SIGNATURE_HEADER`]. The bare executable, not the release tarball,
/// because `UpdateInstaller::apply` writes the bytes it is handed as the next binary — so the
/// signature the edge checks has to cover *these* bytes ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2). Unpacking a tarball server-side would hand `apply` bytes nobody signed.
pub fn ota_release_admin_router<B, L, A, C>(
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/ota/releases",
            post(admin_upload_ota_release::<B, L, A, C>),
        )
        .route(
            "/admin/ota/releases/{release}",
            get(admin_list_ota_release::<B, L, A, C>),
        )
        .route(
            "/admin/ota/releases/{release}/installer",
            get(admin_download_installer::<B, L, A, C>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_ARTIFACT_BYTES))
        .with_state(OtaReleaseAdminState {
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

/// Puts one artifact's bytes in the object store and its row in the registry.
///
/// Shared by both intake paths — the upload and the release fetch
/// ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 4) — so that whichever door
/// an artifact arrives through, the same immutability rule, the same keys and the same write order
/// apply. Only the audit entry differs, and that stays in the handlers where the source is known.
///
/// **Immutability is checked before anything is written.** The first version of the upload wrote both
/// blobs and *then* asked the registry, so re-sending a release tag with different bytes overwrote
/// the stored executable and its signature and only afterwards answered `409`: the registry kept the
/// old digest while the object store held the new pair, and an edge fetching that release would have
/// verified the new signature over the new bytes and installed them happily under a version string
/// that already meant something else. Reading first closes that, and the write order ADR-0088 relies
/// on — signature, binary, then the row — is unchanged for a genuinely new artifact.
///
/// Identical bytes still write their blobs before answering "already hosted": the digest says the
/// content is the same, so the write is a no-op except in the one case where it is a repair (a blob
/// lost under a row that survived).
///
/// # Errors
///
/// The [`Response`] to send: `409` for a release already hosted with different bytes, `503` for an
/// object store or registry that could not be reached, `500` if the keys could not be composed.
async fn admit_release_artifact<B, L, C>(
    blobs: &B,
    releases: &L,
    clock: &C,
    release: &str,
    target: TargetTriple,
    binary: &[u8],
    signature: &[u8],
) -> Result<(ReleaseArtifact, RecordOutcome), Response>
where
    B: BlobStore,
    L: ArtifactStore,
    C: ClockSource,
{
    let (Ok(binary_key), Ok(signature_key)) = (
        artifact_key(release, &target, ArtifactKind::Binary),
        artifact_key(release, &target, ArtifactKind::Signature),
    ) else {
        return Err(api_error(
            ErrorStatus::Internal,
            "could not compose the artifact keys",
        ));
    };
    let artifact = ReleaseArtifact {
        release: release.to_owned(),
        target,
        size_bytes: i64::try_from(binary.len()).unwrap_or(i64::MAX),
        sha256: hex_digest(binary),
        recorded_at: clock.now(),
    };
    let stored = match releases
        .find_artifact(&artifact.release, &artifact.target)
        .await
    {
        Ok(stored) => stored,
        Err(error) => {
            tracing::error!(%error, "reading the release registry before an intake failed");
            return Err(service_unavailable("the release registry"));
        }
    };
    let admitted = admit_artifact(
        stored.as_ref().map(|found| found.sha256.as_str()),
        &artifact,
    )
    .map_err(already_hosted_refusal)?;
    store_artifact_blobs(blobs, &binary_key, &signature_key, binary, signature).await?;
    if admitted == RecordOutcome::AlreadyRecorded {
        return Ok((artifact, RecordOutcome::AlreadyRecorded));
    }
    match releases.record_artifact(&artifact).await {
        Ok(outcome) => Ok((artifact, outcome)),
        // The read above closes the ordinary case; this stays the authority for two intakes racing.
        Err(error @ ArtifactStoreError::Immutable { .. }) => Err(already_hosted_refusal(error)),
        Err(error @ ArtifactStoreError::Unavailable(_)) => {
            tracing::error!(%error, "recording a release artifact failed");
            Err(service_unavailable("the release registry"))
        }
    }
}

/// The `409` a release already hosted with different bytes earns.
///
/// A version a ring may have installed has to keep meaning the same thing, or a rollback target
/// stops being a known quantity ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)).
fn already_hosted_refusal(error: ArtifactStoreError) -> Response {
    match error {
        ArtifactStoreError::Immutable { release, target } => api_error_with_details(
            ErrorStatus::AlreadyExists,
            format!("release {release} for {target} is already hosted with different bytes"),
            &[("release", "already hosted with a different digest")],
        ),
        // `admit_artifact` returns only `Immutable`, and the caller maps `Unavailable` itself; this
        // arm exists because the type says it can, not because a path reaches it.
        ArtifactStoreError::Unavailable(reason) => {
            tracing::error!(%reason, "the release registry was unavailable");
            service_unavailable("the release registry")
        }
    }
}

/// A super-admin uploads one release artifact: the bare executable, plus its minisign signature.
///
/// Idempotent by [`admit_artifact`]'s rule — re-uploading the same release/target with identical
/// bytes answers `200` rather than failing, so a re-run of a release step does not wedge; different
/// bytes for a release a ring may already have installed are refused, because a version that can
/// change under a fleet is not a version.
async fn admin_upload_ota_release<B, L, A, C>(
    State(state): State<OtaReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ReleaseUploadQuery>,
    body: axum::body::Bytes,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
    let (artifact, outcome) = match admit_release_artifact(
        &state.blobs,
        &state.releases,
        &state.clock,
        &query.release,
        target,
        &body,
        &signature,
    )
    .await
    {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
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
async fn admin_list_ota_release<B, L, A, C>(
    State(state): State<OtaReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Path(release): Path<String>,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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

/// Which store a one-file installer is for and which cloud it dials — both written into its file
/// name — and, optionally, which Windows build.
///
/// Every field is optional to serde so that a missing one is refused in the API's own envelope,
/// naming the field, rather than as axum's bare query rejection.
#[derive(Debug, Clone, Deserialize)]
struct InstallerQuery {
    #[serde(default)]
    store_id: Option<String>,
    /// The cloud as a browser shows it: `host` or `host:port`.
    #[serde(default)]
    cloud: Option<String>,
    /// A Windows target triple; absent means [`WINDOWS_TARGET`].
    #[serde(default)]
    arch: Option<String>,
}

/// Checks everything about an installer request that can be judged from the request alone, and
/// composes the file name.
///
/// # Errors
///
/// The [`Response`] to send: a `400` naming `release`, `store_id`, `cloud` or `arch`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape the other route helpers carry"
)]
fn parse_installer_request(
    release: &str,
    query: &InstallerQuery,
) -> Result<(TargetTriple, String), Response> {
    if let Err(error) = validate_release_tag(release) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        ));
    }
    let [store_id] = parse_ulid_fields([("store_id", query.store_id.as_deref().unwrap_or(""))])?;
    let file_name =
        installer_file_name(query.cloud.as_deref().unwrap_or(""), StoreId::new(store_id)).map_err(
            |error| {
                api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    format!("cloud: {error}"),
                    &[("cloud", "cannot be written into the installer's name")],
                )
            },
        )?;
    let target =
        TargetTriple::parse(query.arch.as_deref().unwrap_or(WINDOWS_TARGET)).map_err(|error| {
            api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("arch: {error}"),
                &[("arch", "not a target triple")],
            )
        })?;
    if !target.is_windows() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "arch: {target} is not a Windows build — the one-file installer is Windows-only, \
                 and a Linux store runs the install-pos-edge.sh the console generates"
            ),
            &[("arch", "not a Windows target")],
        ));
    }
    Ok((target, file_name))
}

/// `GET /admin/ota/releases/{release}/installer?store_id=&cloud=` — a hosted release's Windows
/// executable, named as that store's one-file installer
/// ([ADR-0141](../../../docs/adr/0141-the-console-hands-out-the-installer-by-name.md)).
///
/// **The bytes are the release's own.** Nothing is appended or rewritten: the store and the cloud
/// travel in the file *name*, which is what lets the minisign signature over these bytes keep
/// verifying, and what makes the file safe to hand out — the name carries a store id and a host,
/// neither of them a secret, and the credential still arrives only through activation.
///
/// **`console.data.read`**, like the list beside it: this reads what the cloud hosts. It writes
/// nothing and provisions nothing — a box it installs is unactivated until somebody with
/// `console.devices.manage` mints it a code.
///
/// **The cloud's host comes from the caller**, not from the `Host` header. The console passes the
/// address its own browser is showing, the same value it writes into every generated installer;
/// behind a reverse proxy the `Host` a handler sees can be the upstream's, and a file naming
/// `127.0.0.1` would install a store that dials itself.
///
/// Buffered, as the store-facing serve route is ([`read_artifact_blobs`]): a download is a person
/// at a console, one file at a time.
async fn admin_download_installer<B, L, A, C>(
    State(state): State<OtaReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Path(release): Path<String>,
    Query(query): Query<InstallerQuery>,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
    let (target, file_name) = match parse_installer_request(&release, &query) {
        Ok(parsed) => parsed,
        Err(refusal) => return refusal,
    };
    match state.releases.find_artifact(&release, &target).await {
        Ok(Some(_hosted)) => {}
        Ok(None) => {
            return api_error_with_details(
                ErrorStatus::NotFound,
                format!(
                    "release {release} is not hosted for {target} — put it in the cloud from the \
                     OTA screen first"
                ),
                &[("release", "not hosted for this target")],
            );
        }
        Err(error) => {
            tracing::error!(%error, "reading the release registry for an installer failed");
            return service_unavailable("the release registry");
        }
    }
    let Ok(key) = artifact_key(&release, &target, ArtifactKind::Binary) else {
        // Unreachable: the tag validated and the triple parsed, and those are the only inputs.
        return api_error(
            ErrorStatus::Internal,
            "the release's storage key could not be built",
        );
    };
    let bytes = match read_one_artifact_blob(&state.blobs, &key, "artifact").await {
        Ok(bytes) => bytes,
        Err(refusal) => return refusal,
    };
    (
        [
            (CONTENT_TYPE, "application/octet-stream".to_owned()),
            // The name is composed only from a validated host, port and ULID, so it holds nothing a
            // quoted-string would need escaped.
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
        ],
        bytes,
    )
        .into_response()
}

// ---------------------------------------------------------------------------------------------
// The other door: the cloud fetches the pair the release workflow already published
// ---------------------------------------------------------------------------------------------

/// The collaborators the release-fetch route needs: everything the upload needs, plus the forge.
#[derive(Clone)]
struct OtaReleaseFetchState<B, L, A, C, S> {
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    source: Option<S>,
}

/// A `POST /admin/ota/releases/fetch` body: which version to go and get.
///
/// `release` is spelled the way the registry keys it and a rollout's `target_version` names it —
/// bare, without a leading `v` ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2). Which tag that is on the forge is the source's business, and it tries both.
#[derive(Debug, Clone, Deserialize)]
struct FetchReleaseRequest {
    release: String,
}

/// One artifact the fetch put in the cloud.
#[derive(Debug, Clone, Serialize)]
struct FetchedArtifactResponse {
    arch: String,
    size_bytes: i64,
    sha256: String,
    /// `recorded` for a new artifact, `already_hosted` for one the cloud already held with these
    /// exact bytes — so a re-run reads as a no-op rather than as work.
    outcome: &'static str,
    /// The asset the bytes came from, so the console can say what it took.
    asset: String,
}

/// Builds the `/admin` release-fetch sub-router — the door that does not need the operator to hold
/// the bytes ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 4).
///
/// Merged wherever `[artifacts]` is, and `source` is `None` when no `[release_source]` is
/// configured. That is the opposite posture from the *store-facing* artifact route, deliberately:
/// there, absence is a meaningful answer an edge reads as "install nothing", and a route answering
/// `503` would be worse than none. Here the caller is a console with a person at it, and a bare
/// `404` from an unmerged route is indistinguishable from "that version was never released" — so
/// the route exists and says which block of `cloud.toml` is missing.
///
/// Either way the upload route is unaffected: it needs no outbound network and no credential, and
/// is what `docs/release-runbook.md` documents as the way in when this one cannot be used.
pub fn ota_release_fetch_router<B, L, A, C, S>(
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    source: Option<S>,
) -> Router
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    S: ReleaseSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/ota/releases/fetch",
            post(admin_fetch_ota_release::<B, L, A, C, S>),
        )
        .with_state(OtaReleaseFetchState {
            blobs,
            releases,
            admin,
            clock,
            audit,
            source,
        })
}

/// Turns a fetch failure into the refusal an operator can act on.
///
/// Each one names the thing to go and look at: the version they typed, the token in `cloud.toml`,
/// the asset that is wrong, or the network. A fetch failure is never fatal to a release — the upload
/// route is the way past every one of them.
fn release_fetch_refusal(error: &ReleaseSourceError) -> Response {
    match error {
        ReleaseSourceError::NoSuchRelease { candidates } => api_error_with_details(
            ErrorStatus::NotFound,
            format!(
                "the release source publishes no release as {} — check the version, or that the \
                 release workflow finished",
                candidates.join(" or ")
            ),
            &[("release", "no release is published under either spelling")],
        ),
        ReleaseSourceError::Unauthorized => api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "the release source refused the cloud's credentials — a private repository needs \
             release_source.token in cloud.toml, and an expired one reads the same way",
            &[("release_source.token", "absent, expired, or not permitted")],
        ),
        ReleaseSourceError::TooLarge { asset, size_bytes } => api_error_with_details(
            ErrorStatus::Unprocessable,
            format!("{asset} is {size_bytes} bytes, past what the cloud will host"),
            &[("release", "an asset is larger than the cloud hosts")],
        ),
        ReleaseSourceError::Malformed { asset, reason } => api_error_with_details(
            ErrorStatus::Unprocessable,
            format!("{asset}: {reason}"),
            &[("release", "an asset is not what it claims to be")],
        ),
        ReleaseSourceError::Unavailable(reason) => api_error(
            ErrorStatus::Unavailable,
            format!("the release source: {reason}"),
        ),
    }
}

/// A super-admin points the cloud at a release, and it fetches every pair the forge holds for it.
///
/// The button behind `docs/release-runbook.md` step 5: instead of downloading six files and running
/// three `curl`s with a console cookie, the operator names the version. What the cloud *holds*
/// afterwards is identical either way — the same blobs, the same rows, the same immutability rule —
/// because both doors go through [`admit_release_artifact`].
///
/// Not a transaction. Each target is admitted on its own, so a refusal on the third leaves the first
/// two hosted; that is the same grain the upload has always had (one call per target), and the
/// refusal names which target stopped it.
async fn admin_fetch_ota_release<B, L, A, C, S>(
    State(state): State<OtaReleaseFetchState<B, L, A, C, S>>,
    headers: HeaderMap,
    Json(request): Json<FetchReleaseRequest>,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    S: ReleaseSource + Clone + Send + Sync + 'static,
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
    let Some(source) = state.source.as_ref() else {
        return api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "this cloud has no release source configured, so it cannot go and get a release: set \
             [release_source] in cloud.toml, or send the signed pair to POST /admin/ota/releases \
             (docs/release-runbook.md step 5)",
            &[("release_source", "not configured in cloud.toml")],
        );
    };
    let release = request.release.trim().to_owned();
    if let Err(error) = validate_release_tag(&release) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        );
    }
    let fetched = match source.fetch(&release).await {
        Ok(fetched) => fetched,
        Err(error) => {
            tracing::warn!(%error, %release, "fetching a release from the release source failed");
            return release_fetch_refusal(&error);
        }
    };
    if fetched.artifacts.is_empty() {
        return api_error_with_details(
            ErrorStatus::Unprocessable,
            format!(
                "{} carries no OTA pair: the cloud installs the bare executable \
                 (pos-edge-{}-<target>.bin, or .exe on Windows) with its .minisig beside it, and \
                 neither the archive nor the checksums are that",
                fetched.tag, fetched.tag
            ),
            &[("release", "the release holds no signed executable")],
        );
    }

    let mut recorded = Vec::with_capacity(fetched.artifacts.len());
    for artifact in fetched.artifacts {
        let asset = artifact.asset_name.clone();
        let (admitted, outcome) = match admit_release_artifact(
            &state.blobs,
            &state.releases,
            &state.clock,
            &release,
            artifact.target,
            &artifact.binary,
            &artifact.signature,
        )
        .await
        {
            Ok(admitted) => admitted,
            Err(refusal) => return refusal,
        };
        recorded.push(FetchedArtifactResponse {
            arch: admitted.target.to_string(),
            size_bytes: admitted.size_bytes,
            sha256: admitted.sha256,
            outcome: match outcome {
                RecordOutcome::Recorded => "recorded",
                RecordOutcome::AlreadyRecorded => "already_hosted",
            },
            asset,
        });
    }
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        None,
        "release.fetch",
        "release_artifact",
        &release,
        None,
        Some(serde_json::json!({
            "tag": fetched.tag,
            "artifacts": recorded,
        })),
    )
    .await;
    Json(serde_json::json!({
        "release": release,
        "tag": fetched.tag,
        "artifacts": recorded,
    }))
    .into_response()
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
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
    /// Gives a tenant created here its six starting roles
    /// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
    starting_roles: Arc<dyn StartingRoles>,
}

/// Builds the org-registry sub-router ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
///
/// Every route is behind the super-admin session guard, and names a tenant the admin-is-global way
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): a `?tenant_id=` query for the
/// listings, the request body for a create. Device routes nest under their store, so they never
/// collide with the `/admin/devices/proposals` onboarding queue ([`device_router`]). A tenant created
/// here is given its six starting roles through `starting_roles`
/// ([`crate::starting_roles`]).
pub fn registry_router<Rg, A, C>(
    registry: Rg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    starting_roles: Arc<dyn StartingRoles>,
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
            starting_roles,
        })
}

/// Create a course — a named point in the service sequence (ADR-0130).
#[derive(Debug, Clone, Deserialize)]
struct CreateCourseRequest {
    tenant_id: String,
    name: String,
    /// Where it falls in the sequence, ascending. Absent is `0`, which puts a first course first
    /// and leaves the id to break the tie with any other unpositioned one.
    #[serde(default)]
    sort: i32,
}

/// Update a course's name, position and/or status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateCourseRequest {
    tenant_id: String,
    name: String,
    #[serde(default)]
    sort: i32,
    status: String,
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

/// A store's `node_key` node as the store runs it, without the node's settings fields, or `null`
/// when it runs none.
///
/// Read from the tree's four layers composed ([`ConfigTree::effective`]), not from the Store layer
/// alone. A rollback writes the restored version onto the Tenant layer and empties the others
/// ([`ConfigTree::restore_with`]), so a Store-layer read answered `null` for every node the store
/// still ran: the console's forms went blank, and its checks refused a store that held the node. A
/// node a tenant or brand layer sets is read with the store's own, because the store runs it.
///
/// The fields the settings register puts on the node (`pos_proto::settings::register`) are left
/// out. They are written on the Tenant layer by the settings compile, and they are the settings
/// store's to decide (ADR-0160 decision 3): a form that read one and saved the node back would pin
/// it on the Store layer, where it would shadow every later settings change for the store. A node
/// that holds nothing but settings answers `null`, as [`check_prerequisites`] counts it: a `locale`
/// holding only `tax_rounding` is not the locale a menu waits for.
async fn read_store_node_as_run<Cfg>(
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
        Ok(Some(state)) => {
            let composed = ConfigTree::from_state(store_id, CapabilityValidator, state).effective();
            Ok(node_as_run(node_key, composed.get(node_key)))
        }
        Ok(None) => Ok(serde_json::Value::Null),
        Err(error) => Err(config_store_error_response(&error)),
    }
}

/// The answer [`read_store_node_as_run`] gives for `node`, the store's `node_key` as its layers
/// compose it: the node without its settings fields, and `null` for a node the store does not run
/// or one that holds nothing but settings. A node that is not an object is answered as it is.
fn node_as_run(node_key: &str, node: Option<&serde_json::Value>) -> serde_json::Value {
    match node {
        None => serde_json::Value::Null,
        Some(value) if only_settings(node_key, value) => serde_json::Value::Null,
        Some(serde_json::Value::Object(fields)) => {
            let register = pos_proto::settings::register();
            serde_json::Value::Object(
                fields
                    .iter()
                    .filter(|(field, _)| {
                        !register.iter().any(|setting| {
                            setting.node == node_key && setting.field == field.as_str()
                        })
                    })
                    .map(|(field, value)| (field.clone(), value.clone()))
                    .collect(),
            )
        }
        Some(other) => other.clone(),
    }
}

/// The range a store's `event_log_days` may take (ADR-0145): the `retention` node's own, which the
/// edge reads the node through and ignores a value outside of ([`pos_proto::retention`]).
pub use pos_proto::retention::EVENT_LOG_DAYS;

/// A choice a request names by its wire token: `None` where it names none, which leaves the choice
/// to whatever stands for it, or a token of `E` this release knows.
///
/// # Errors
///
/// A `400 INVALID_ENUM_VALUE` naming `field` for a token this release does not know, and for `E`'s
/// `UNSPECIFIED`, which is what an older reader takes a newer token to be and never a choice.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn optional_choice<E: WireEnum>(field: &str, token: Option<&str>) -> Result<Option<E>, Response> {
    let Some(token) = token else {
        return Ok(None);
    };
    match E::from_wire(token) {
        Some(known) if known != E::UNSPECIFIED => Ok(Some(known)),
        _ => Err(enum_refusal(field, accepted_tokens::<E>())),
    }
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
struct FleetState<F, A, C, Cfg, Ack> {
    fleet: F,
    admin: A,
    clock: C,
    /// The store's own configuration, for the one thing the fleet read model cannot answer: which
    /// country the store publishes ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Used by the **single-store** read only, which does exactly one `load` for the one store it
    /// already names. The list route deliberately does not: a comparison there would be one
    /// config-tree load per row — work that grows with the fleet, which `AGENTS.md` §2's
    /// bounded-work discipline and [ADR-0098](../../../docs/adr/0098-paged-admin-reads.md) both
    /// refuse. The tempting shortcut is worse than the read it saves: copying `country_code` onto
    /// the registry row would make a second copy of a versioned node, and a config rollback can
    /// move the node backwards while the copy keeps asserting the old value — a silent disagreement
    /// about a store's country, which is the exact failure ADR-0114 exists to prevent.
    config_trees: Cfg,
    /// A store's recorded answer to its region difference
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)) — read by the
    /// single-store read beside the comparison it answers, and written by the acknowledge route.
    acknowledgements: Ack,
    /// The trail the acknowledge route writes to. The row holds *what is currently answered*; this
    /// holds every answer ever given, with the person against it, and nothing overwrites it.
    audit: Arc<dyn AuditRecorder>,
}

/// A recorded answer to a store's region difference, as the console sees it
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// The two country codes ride along rather than being left implicit: the console renders them in
/// the sentence it shows ("data in SG, store registered in VN — acknowledged"), and a reader of the
/// payload can see *what* was answered without inferring it from the fields beside it.
///
/// No `acknowledged_by`. The row does not carry one — the audit action
/// `store.edge_placement.acknowledge` does, and it is the copy that survives the next answer. The
/// console links to the store's audit tab rather than showing a name this read cannot honestly give.
#[derive(Debug, Clone, serde::Serialize)]
struct RegionAcknowledgementView {
    /// The country the store was registered in when the answer was written.
    profile_country: String,
    /// The country its data was resting in when the answer was written.
    region_country: String,
    /// Why the difference is correct, as somebody typed it. Carried verbatim, never parsed.
    reason: String,
    /// When it was written, in milliseconds since the epoch — the same representation every other
    /// instant on this read uses.
    acknowledged_at_ms: i64,
}

/// One bound print agent as the fleet console sees it
/// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
///
/// Two opaque identifiers and a duration, carried verbatim from what the store reported. No document,
/// no line, no name: nothing a ticket *says* helps diagnose a stalled agent, so nothing a ticket says
/// crosses this wire either.
#[derive(Debug, Clone, serde::Serialize)]
struct FleetPrintAgent {
    /// The terminal a console admin created and a manager bound at the till.
    agent_device_id: String,
    /// The paired device answering for it — the box an operator has to walk to.
    paired_device_id: String,
    /// How long the oldest still-unacknowledged job has waited, or `null` when nothing is waiting.
    /// `null` is the healthy answer and is deliberately not zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    oldest_unacknowledged_secs: Option<u64>,
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
    /// The print agents the store last reported, or `null` if it never has
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// `null` and `[]` are different answers and the console renders them differently: `null` is a
    /// store that has said nothing about agents — every store whose edge runs in the shop, and every
    /// edge on a binary older than this field — while `[]` is a store that looked and has none.
    print_agents: Option<Vec<FleetPrintAgent>>,
    /// Unix ms of the heartbeat that reported them, or `null`. A standing is only as current as the
    /// beat that carried it, so the instant travels with the list.
    print_agents_reported_at_ms: Option<i64>,
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
    /// Where in the world that machine is — its ISO 3166-1 alpha-2 country — or `null`
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// `null` for a store nobody has bumped, for one whose edge placement is in-store (the machine
    /// is in the shop and has no region), and for a row written before migration `0058`. All three
    /// mean the same thing to a reader — there is nothing to show — which is why this is a plain
    /// `Option` and the placement beside it is not.
    region_country: Option<String>,
    /// The place a person recognises — `ap-southeast-1`, `Ho Chi Minh City`, `the rack in Sakai` —
    /// or `null`. Present exactly when [`region_country`](Self::region_country) is.
    ///
    /// Free text an admin typed, carried verbatim and never parsed. It is on this read because the
    /// question it answers ("where has this store been running?") is asked while somebody has the
    /// fleet page open, not afterwards.
    region_label: Option<String>,
    /// The country this store publishes in its `locale` node
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// **Present only on the single-store read**, which is the one route that loads a store's
    /// configuration; the listing omits the key entirely rather than sending `null`, so a console
    /// can tell "not compared here" from "compared, and there is no country". Absence must never
    /// render as agreement.
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_country: Option<String>,
    /// Whether the region agrees with that country — `agrees`, `differs`, `country-not-recorded` —
    /// or absent when there is nothing to compare
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Derived by [`region_agreement`] rather than in the console, for the reason `handover` beside
    /// it is: the third case is the one that is easy to get wrong, and getting it wrong means a
    /// store with no recorded country renders as agreeing.
    ///
    /// Omitted on the listing, exactly as [`profile_country`](Self::profile_country) is — a
    /// comparison there would be one config-tree load per row.
    #[serde(skip_serializing_if = "Option::is_none")]
    region_agreement: Option<&'static str>,
    /// The answer somebody recorded for **this** difference, or absent
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Present only when a stored acknowledgement names the same two country codes the store has
    /// right now. An answer written about a difference the store no longer has is not returned —
    /// see [`standing_acknowledgement`] — so a console that renders "acknowledged" whenever this key
    /// is present is correct without knowing the rule.
    ///
    /// Omitted on the listing with the two fields above it, for the same reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    region_acknowledgement: Option<RegionAcknowledgementView>,
    /// The generation the last bump displaced and nothing has yet proved drained, or `null`
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// Shown beside [`handover`](Self::handover) rather than in place of it: the state says *what*
    /// is happening and this says *which machine* it is about, which is the number an operator needs
    /// to type into a settle.
    lease_superseded_generation: Option<u64>,
    /// Which state this store's handover is in — `taking-over`, `settled`, `retired` — or `null`
    /// when it has never had one
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// Derived here, not stored, exactly as `online` and `config_current` are, and by
    /// [`handover_state`] so the console never re-implements the rule — in particular the same-beat
    /// guard, which is the half that is easy to get wrong and expensive to get wrong.
    ///
    /// `null` is a real answer and the commonest one: a store on generation `0` has never been
    /// replaced, and a store with no lease row has never been issued one. Neither is a handover in
    /// any state, so the console renders no badge rather than a reassuring one.
    handover: Option<&'static str>,
    /// When a person decided this handover's outgoing machine is no longer needed, or `null`.
    retired_at_ms: Option<i64>,
    /// The deciding admin's id, or `null`.
    ///
    /// An id, and the console resolves it to a name through the admin roster if it wants one. The
    /// email lives in the audit trail, which is read under a permission this view is not behind.
    retired_by: Option<String>,
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
            print_agents: row.print_agents.clone().map(|agents| {
                agents
                    .into_iter()
                    .map(|agent| FleetPrintAgent {
                        agent_device_id: agent.agent_device_id,
                        paired_device_id: agent.paired_device_id,
                        oldest_unacknowledged_secs: agent.oldest_unacknowledged_secs,
                    })
                    .collect()
            }),
            print_agents_reported_at_ms: row
                .print_agents_reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            lease_generation_held: row.lease_generation_held,
            lease_reported_at_ms: row
                .lease_reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            lease_generation_authoritative: row.lease_generation_authoritative,
            lease_superseded,
            edge_placement: row.edge_placement.as_wire(),
            region_country: row
                .region
                .as_ref()
                .map(|region| region.country_str().to_owned()),
            region_label: row.region.as_ref().map(|region| region.label().to_owned()),
            // Filled in by the single-store route after this builds; the listing leaves all three
            // absent, because it does not perform the comparison and so has nothing an
            // acknowledgement could be measured against.
            profile_country: None,
            region_agreement: None,
            region_acknowledgement: None,
            lease_superseded_generation: row.superseded_generation,
            handover: handover_state(HandoverFacts {
                authoritative: row.lease_generation_authoritative.map(LeaseGeneration::new),
                superseded: row.superseded_generation.map(LeaseGeneration::new),
                retired_at: row.retired_at,
                held: row.lease_generation_held.map(LeaseGeneration::new),
                outbox_depth: row.outbox_depth,
                outbox_reported_at: row.outbox_reported_at,
                lease_reported_at: row.lease_reported_at,
            })
            .map(HandoverState::as_wire),
            retired_at_ms: row
                .retired_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            retired_by: row.retired_by,
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
pub fn fleet_router<F, A, C, Cfg, Ack>(
    fleet: F,
    admin: A,
    clock: C,
    config_trees: Cfg,
    acknowledgements: Ack,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/fleet", get(admin_list_fleet::<F, A, C, Cfg, Ack>))
        .route(
            "/admin/fleet/{store_id}",
            get(admin_fleet_store::<F, A, C, Cfg, Ack>),
        )
        // Under `/admin/stores` because it is a fact about a store, and served from *this* router
        // because answering it needs both halves of the comparison: the region, which lives on the
        // lease behind `FleetStore`, and the published country, which lives in the config tree.
        // `registry_router` holds neither, and giving it both to keep the paths together would put
        // two store handles in a router whose whole payload is the registry record.
        .route(
            "/admin/stores/{store_id}/region-acknowledgement",
            post(admin_acknowledge_region::<F, A, C, Cfg, Ack>),
        )
        .with_state(FleetState {
            fleet,
            admin,
            clock,
            config_trees,
            acknowledgements,
            audit,
        })
}

/// Maps a fleet read failure to a retryable `503`, logging the detail rather than leaking it.
fn fleet_error_response(error: &FleetStoreError) -> Response {
    tracing::error!(%error, "a fleet read failed");
    api_error(ErrorStatus::Unavailable, "the fleet service is unavailable")
}

/// A super-admin (any console role) lists a tenant's whole fleet, online/offline derived at read.
async fn admin_list_fleet<F, A, C, Cfg, Ack>(
    State(state): State<FleetState<F, A, C, Cfg, Ack>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
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
async fn admin_fleet_store<F, A, C, Cfg, Ack>(
    State(state): State<FleetState<F, A, C, Cfg, Ack>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
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
    let row = match state.fleet.store_detail(tenant, store).await {
        Ok(Some(row)) => row,
        Ok(None) => return not_found("store"),
        Err(error) => return fleet_error_response(&error),
    };
    // The country this store publishes, for the one comparison ADR-0114 asks for. One `load` for
    // the one store already named — the read this route performs anyway is a single row, so this
    // is bounded work, unlike the same comparison on the listing.
    //
    // A config-tree failure is **not** an error here. This read's job is to report a store's
    // health, and failing the whole page because a second store's configuration could not be read
    // would take the console away from an operator mid-incident. It degrades to "no country", which
    // renders as `country-not-recorded` — visibly not an answer, never as agreement.
    let profile_country = match state.config_trees.load(tenant, store).await {
        Ok(tree) => tree.and_then(|tree| published_country(&tree.record)),
        Err(error) => {
            tracing::warn!(%error, "a store's country could not be read for its region comparison");
            None
        }
    };
    let agreement = region_agreement(row.region.as_ref(), profile_country);
    // The answer, if somebody has given one for *this* difference. A read failure degrades the same
    // way the tree's does and for the same reason — a store that has been answered showing as
    // unanswered is a warning too many, which is the safe direction to fail in.
    let recorded = match state.acknowledgements.current(tenant, store).await {
        Ok(recorded) => recorded,
        Err(error) => {
            tracing::warn!(%error, "a store's region acknowledgement could not be read");
            None
        }
    };
    let standing =
        standing_acknowledgement(recorded.as_ref(), row.region.as_ref(), profile_country).map(
            |acknowledgement| RegionAcknowledgementView {
                profile_country: acknowledgement.profile_country().as_str().to_owned(),
                region_country: acknowledgement.region_country().as_str().to_owned(),
                reason: acknowledgement.reason().to_owned(),
                acknowledged_at_ms: acknowledgement
                    .acknowledged_at()
                    .as_milliseconds_since_epoch(),
            },
        );
    let mut view = FleetStoreView::from_row(row, now_ms);
    view.profile_country = profile_country.map(|country| country.as_str().to_owned());
    view.region_agreement = agreement.map(RegionAgreement::as_wire);
    view.region_acknowledgement = standing;
    (StatusCode::OK, Json(view)).into_response()
}

/// What a console sends to answer a store's region warning
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// **The two country codes are the precondition, and they are in the body rather than in an
/// `If-Match` header.** ADR-0114's Consequences said `If-Match`; the code does not, and the reason
/// is the one [`LeaseStore::settle_handover`] already gives one table over: *the named value is the
/// precondition*, and here it is strictly stronger than any version could be. There is no single
/// resource whose `ETag` covers both halves of this comparison — the region lives on `store_lease`
/// and the country lives in the config tree, two rows with two independent writers — so an
/// `If-Match` would have to name one of them and silently miss changes to the other. Naming both
/// codes catches every move of either, and says in the request exactly what the admin was looking at
/// when they decided. The departure is written up in the record's delivery note.
#[derive(Debug, Clone, Deserialize)]
struct AcknowledgeRegionRequest {
    /// The tenant the store belongs to.
    tenant_id: String,
    /// The country the console showed as the store's own when the admin decided.
    profile_country: String,
    /// The country the console showed the store's data resting in when the admin decided.
    region_country: String,
    /// Why the difference is correct. Required, non-blank, at most [`REGION_REASON_LIMIT`]
    /// characters. `#[serde(default)]` so an omitted field is refused through the AIP-193 envelope
    /// naming it, rather than by axum's extractor as a bare `422`.
    #[serde(default)]
    reason: String,
}

/// The difference a store has **right now**, confirmed to be the one the caller was looking at.
///
/// Both halves are read fresh, and a failure to read either is a `503` rather than a degraded
/// answer. That is the difference between this and the single-store read: that read may show
/// "country not recorded" when a config load fails, because a partial page beats no page — but
/// *recording a decision* against a country this cloud could not actually read would put a fact in
/// the audit trail that nothing checked.
///
/// # Errors
///
/// The finished refusal: `404` for an unknown store, `503` when either half is unreadable, and
/// `FailedPrecondition` for a store with nothing to answer, a store whose codes have moved, or a
/// store whose region agrees.
async fn live_difference<F, Cfg>(
    fleet: &F,
    config_trees: &Cfg,
    tenant: TenantId,
    store: StoreId,
    claimed: (CountryCode, CountryCode),
) -> Result<RegionDifference, Response>
where
    F: FleetStore + Send + Sync,
    Cfg: ConfigTreeStore + Send + Sync,
{
    let (claimed_profile, claimed_region) = claimed;
    let row = match fleet.store_detail(tenant, store).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(not_found("store")),
        Err(error) => return Err(fleet_error_response(&error)),
    };
    let published = match config_trees.load(tenant, store).await {
        Ok(tree) => tree.and_then(|tree| published_country(&tree.record)),
        Err(error) => {
            tracing::error!(%error, "a store's country could not be read to record an acknowledgement");
            return Err(api_error(
                ErrorStatus::Unavailable,
                "the store's configuration could not be read; the acknowledgement was not recorded",
            ));
        }
    };
    let (Some(region), Some(published)) = (row.region.as_ref(), published) else {
        return Err(api_error(
            ErrorStatus::FailedPrecondition,
            "this store has no region difference to acknowledge — it has no region, or has not \
             published a country. Reload the store to see its current standing",
        ));
    };
    if region.country() != claimed_region || published != claimed_profile {
        return Err(api_error(
            ErrorStatus::FailedPrecondition,
            "the store has moved since this screen was drawn: the countries you named are not the \
             ones it has now. Reload it and look at the difference as it stands",
        ));
    }
    if region.country() == published {
        return Err(api_error(
            ErrorStatus::FailedPrecondition,
            "this store's region agrees with the country it is registered in, so there is nothing \
             to acknowledge",
        ));
    }
    Ok(RegionDifference {
        profile_country: published,
        region_country: region.country(),
    })
}

/// The two countries a store's live difference is between, confirmed against what the caller named.
struct RegionDifference {
    /// The country the store publishes in its `locale` node.
    profile_country: CountryCode,
    /// The country its data is resting in.
    region_country: CountryCode,
}

#[utoipa::path(
    post,
    path = "/admin/stores/{store_id}/region-acknowledgement",
    params(("store_id" = String, Path, description = "The store's 26-character ULID")),
    request_body(
        description = "The tenant, the two country codes the console showed, and one sentence \
                       saying why the difference between them is correct",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Recorded. The body is the answer as stored"),
        (status = 400, description = "A country code is not two letters, or the reason is blank \
                                      or over 280 characters", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "Nothing to answer, or the store has moved since this screen \
                                      was drawn — the codes you named are not the ones it has now", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The fleet, the config tree, or the acknowledgement store is \
                                      unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// An admin records why a store's region difference is correct
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Behind [`ConsolePermission::ManageStores`] rather than `PublishConfig`: this is a judgement about
/// where a store's data may rest, which is the same judgement the bump that put it there was, and
/// the person who makes it is the person who moves stores between machines.
///
/// # It answers one difference, not the store
///
/// The request names both country codes and the write refuses `FailedPrecondition` when either has
/// moved since the console drew the screen. Without that check an admin could acknowledge a
/// difference that no longer exists — or worse, one that changed to something they never saw — and
/// the record would say a person approved a transfer they were never shown. The row then keeps the
/// pair, so the *read* applies the same rule for free: an answer stops standing when the store
/// moves, and the warning comes back unanswered.
///
/// Refused for a store with no difference at all, because there is nothing to answer: a store whose
/// region agrees, or which has no region, or which has published no country, would otherwise
/// accumulate reasons for questions nobody asked.
async fn admin_acknowledge_region<F, A, C, Cfg, Ack>(
    State(state): State<FleetState<F, A, C, Cfg, Ack>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<AcknowledgeRegionRequest>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
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
    let (tenant, store) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant, store]) => (TenantId::new(tenant), StoreId::new(store)),
            Err(refusal) => return refusal,
        };
    let (Ok(claimed_profile), Ok(claimed_region)) = (
        CountryCode::parse(&request.profile_country),
        CountryCode::parse(&request.region_country),
    ) else {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "profile_country and region_country must both be ISO 3166-1 alpha-2 codes — the two \
             the console showed you. They say which difference you are answering",
            &[
                ("profile_country", "INVALID_FORMAT"),
                ("region_country", "INVALID_FORMAT"),
            ],
        );
    };

    let difference = match live_difference(
        &state.fleet,
        &state.config_trees,
        tenant,
        store,
        (claimed_profile, claimed_region),
    )
    .await
    {
        Ok(difference) => difference,
        Err(refusal) => return refusal,
    };

    let now = state.clock.now();
    let acknowledgement = match RegionAcknowledgement::new(
        difference.profile_country,
        difference.region_country,
        &request.reason,
        now,
    ) {
        Ok(acknowledgement) => acknowledgement,
        Err(refusal) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "reason is required and must be at most 280 characters — one sentence saying why \
                 this store's data resting outside its own country is correct",
                &[("reason", refusal.code())],
            );
        }
    };
    if let Err(error) = state
        .acknowledgements
        .record(tenant, store, &acknowledgement)
        .await
    {
        tracing::error!(%error, "a region acknowledgement could not be recorded");
        return api_error(
            ErrorStatus::Unavailable,
            "the acknowledgement could not be recorded",
        );
    }
    // Unredacted, and deliberately: the two country codes are business metadata, and the reason is
    // operator-supplied free text carried whole because a trail that summarises the reason answers
    // nothing. It is never logged and never in an event payload.
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant),
        "store.edge_placement.acknowledge",
        "store",
        &store.to_string(),
        None,
        Some(serde_json::json!({
            "profile_country": acknowledgement.profile_country().as_str(),
            "region_country": acknowledgement.region_country().as_str(),
            "reason": acknowledgement.reason(),
        })),
    )
    .await;
    (
        StatusCode::OK,
        Json(RegionAcknowledgementView {
            profile_country: acknowledgement.profile_country().as_str().to_owned(),
            region_country: acknowledgement.region_country().as_str().to_owned(),
            reason: acknowledgement.reason().to_owned(),
            acknowledged_at_ms: acknowledgement
                .acknowledged_at()
                .as_milliseconds_since_epoch(),
        }),
    )
        .into_response()
}

/// The country a store's published `locale` node names, or `None` when it has never published one
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Reads the store's layers composed, as it runs them ([`ConfigTreeState::effective`]):
/// `admin_publish_locale` writes the Store layer, and a rollback restores a version onto the Tenant
/// layer. A value that is not two ASCII letters answers `None` rather than an error: a node written
/// by a fork, or by a build older than the required-`country_code` change, is a store this console
/// still has to be able to draw.
fn published_country(tree: &ConfigTreeState) -> Option<CountryCode> {
    tree.effective()
        .get("locale")?
        .get("country_code")?
        .as_str()
        .and_then(|code| CountryCode::parse(code).ok())
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
    /// The language the tenant's starting roles are named in, a language tag such as `vi`. The
    /// console sends the one it is shown in. Absent, or one the console's language packs do not
    /// carry, names them in English. Nothing about the tenant records it.
    #[serde(default)]
    language: Option<String>,
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
///
/// The tenant then gets its six starting roles, one for each of `pos-core`'s built-in roles,
/// granting what the catalogue's defaults say ([`crate::starting_roles`],
/// [ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)). They are
/// audited as one `role.seed` entry carrying their ids and counts. The tenant exists whatever the
/// seeding does, so a seeding failure is logged rather than refused: the tenant then starts with
/// fewer roles, and its owner creates the rest in People.
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
            seed_tenant_roles(&state, &context, tenant_id, request.language.as_deref()).await;
            versioned_created(record, &version)
        }
        Err(error) => registry_error_response(&error),
    }
}

/// Gives a tenant just created its starting roles, and audits what was written as `role.seed`: the
/// ids and the counts, nothing else. A failure is logged with the tenant's id; the tenant stands.
async fn seed_tenant_roles<Rg, A, C>(
    state: &RegistryState<Rg, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    language: Option<&str>,
) where
    C: ClockSource,
{
    match state.starting_roles.seed(tenant_id, language).await {
        Ok(seeded) if seeded.created.is_empty() => {}
        Ok(seeded) => {
            let after = serde_json::json!({
                "role_template_ids": seeded
                    .created
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                "created": seeded.created.len(),
                "kept": seeded.kept.len(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                context,
                Some(tenant_id),
                "role.seed",
                "tenant",
                &tenant_id.to_string(),
                None,
                Some(after),
            )
            .await;
        }
        Err(error) => {
            tracing::error!(%error, %tenant_id, "a new tenant's starting roles could not be written");
        }
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
    /// Where in the world the machine taking this generation is — its ISO 3166-1 alpha-2 country
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Required together with `region_label` when `edge_placement` names a hosted mode, and refused
    /// on any other bump: an in-store machine is in the shop, and a bump that names no placement is
    /// a swap in place, so a region on either would be a region edited without moving the store.
    #[serde(default)]
    region_country: Option<String>,
    /// The place a person recognises — `ap-southeast-1`, `Ho Chi Minh City`, `the rack in Sakai`.
    ///
    /// Stored verbatim and never parsed by anything. It is free text an admin typed, so the
    /// console's helper text asks for a place and not a person; it is never logged and never enters
    /// an event payload.
    #[serde(default)]
    region_label: Option<String>,
    /// The undrained generation this bump is knowingly abandoning, or absent.
    ///
    /// It names the value already sitting in `superseded_generation` — *not* the generation this
    /// bump is about to displace. So an acknowledged bump does not clear the column; it rolls it
    /// forward, and the fact that the older generation was abandoned survives in the audit trail,
    /// which is why the entry carries both numbers.
    #[serde(default)]
    acknowledge_undrained: Option<u64>,
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
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
        // The two ways a handover ends by hand. Neither publishes a config node — nothing about
        // them changes what a till is told — so they sit beside the bump rather than going through
        // `publish_ota_node`, and audit themselves directly.
        .route(
            "/admin/config/lease/settle",
            post(admin_settle_handover::<Cfg, A, C, L>),
        )
        .route(
            "/admin/config/lease/retire",
            post(admin_retire_handover::<Cfg, A, C, L>),
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
async fn require_hosted_release<L: ArtifactStore>(
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
                 would fetch nothing — put one there first (POST /admin/ota/releases)"
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
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
            node: as_requested(node),
            action: "config.ota.publish",
            detail: audit_detail,
            companions: Vec::new(),
        },
    )
    .await
}

/// Flips the kill switch on a store's published rollout: loads the `fleet_update` the store runs, sets
/// `halted`, and re-publishes it on the Store layer — preserving the rest of the rollout, so an
/// operator halts a bad rollout (or resumes a paused one) without re-typing the target, ring, and
/// key. `400` if the store has no rollout to halt.
///
/// The rollout is read from the tree's layers composed ([`ConfigTreeState::effective`]): a rollback
/// restores a version onto the Tenant layer and empties the Store layer, and a halt that read the
/// Store layer alone refused a store running the restored rollout.
///
/// It is read from the tree the halt writes over ([`halted_rollout`], composed inside the
/// conditional write's retry), so a rollout published while the halt is in flight is the one it
/// halts. The halt used to read the tree once, before the publish read it again to write, and wrote
/// the rollout it had read first, halted, over the newer one: the kill switch's own write was a lost
/// update.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every node \
              publish's closure carries — and it is the halt's own refusal, which abandons the write"
)]
async fn admin_halt_rollout<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<HaltRolloutRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
    let halted = request.halted;
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "fleet_update",
            node: |tree: Option<&ConfigTreeState>| halted_rollout(tree, halted),
            action: "config.ota.halt",
            detail: serde_json::json!({ "halted": halted }),
            companions: Vec::new(),
        },
    )
    .await
}

/// The rollout `tree` runs, its layers composed, with the kill switch set to `halted` and the rest
/// of it kept; or the `400` a store with no published rollout earns.
///
/// Called with the tree as each attempt of the write reads it, so the refusal is decided on the tree
/// the halt would write over, and a refusal abandons the write before anything is audited.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
fn halted_rollout(
    tree: Option<&ConfigTreeState>,
    halted: bool,
) -> Result<serde_json::Value, Response> {
    let Some(mut node) = tree
        .map(ConfigTreeState::effective)
        .and_then(|document| document.get("fleet_update").cloned())
    else {
        return Err(api_error(
            ErrorStatus::InvalidArgument,
            "the store has no published rollout to halt",
        ));
    };
    if let serde_json::Value::Object(map) = &mut node {
        map.insert("halted".to_owned(), serde_json::Value::Bool(halted));
    }
    Ok(node)
}

/// A node the request names in full — a rollout, a placement, a lease — for [`OtaNodeWrite::node`].
/// Published as it is on every attempt, because nothing in it depends on what the store holds.
#[expect(
    clippy::result_large_err,
    reason = "the closure's Err is an axum Response by design — the shape every node publish's \
              closure carries — and this one never takes the Err branch at all"
)]
fn as_requested(
    node: serde_json::Value,
) -> impl FnMut(Option<&ConfigTreeState>) -> Result<serde_json::Value, Response> {
    move |_| Ok(node.clone())
}

/// One OTA node write: where it lands and how the trail names it. Grouped because the five travel
/// together and mean nothing apart — a `key` on the wrong `level` is exactly the mistake
/// [`ConfigTreeState::layer`] exists to prevent, so they are chosen at one place.
struct OtaNodeWrite<'a, F> {
    /// The config layer the node belongs on: Store for a rollout, Device for a placement.
    level: ConfigLevel,
    /// The node's key in that layer — `fleet_update` or `device_ota`.
    key: &'a str,
    /// The value to set, composed on the tree as each attempt of the write reads it: a node the
    /// request names in full ignores the tree ([`as_requested`]), and one that changes what the store
    /// already holds reads it there ([`halted_rollout`]). `Err` refuses the write.
    node: F,
    /// The audit action id.
    action: &'a str,
    /// The audit detail, which carries no personal data — a rollout is fleet configuration.
    detail: serde_json::Value,
    /// Nodes published in the **same** version beside `node`, so no pull can see one without the
    /// other. Only the lease bump has one: the receipt floor a replacement box must number above
    /// (ADR-0149).
    companions: Vec<(&'a str, serde_json::Value)>,
}

/// The shared load→set-node→publish→save→audit tail behind every OTA lever.
///
/// The node is composed inside [`publish_config_nodes_with`]'s retry, on the tree each attempt reads,
/// so a lever that changes what the store holds changes what it holds when the write lands. A
/// refusal from the node abandons the write, and nothing is audited.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every node \
              publish's closure carries — and it passes on the node's own refusal unchanged"
)]
async fn publish_ota_node<Cfg, A, C, L, F>(
    state: &OtaConfigState<Cfg, A, C, L>,
    context: &AdminContext,
    tenant_id: TenantId,
    store_id: StoreId,
    write: OtaNodeWrite<'_, F>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    F: FnMut(Option<&ConfigTreeState>) -> Result<serde_json::Value, Response>,
{
    let OtaNodeWrite {
        level,
        key,
        mut node,
        action,
        detail,
        companions,
    } = write;
    let id = match publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        level,
        |tree| {
            let mut nodes = vec![(key.to_owned(), node(tree)?)];
            nodes.extend(
                companions
                    .iter()
                    .map(|(companion, value)| ((*companion).to_owned(), value.clone())),
            );
            Ok(nodes)
        },
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

/// Reads a store's currently-published rollout — the `fleet_update` node the store runs, its layers
/// composed so a rollout a rollback restored reads as it runs — or `null` if none is published.
/// Behind [`ConsolePermission::Read`].
async fn admin_get_rollout<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Query(query): Query<OtaRolloutQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
                .map(ConfigTreeState::effective)
                .and_then(|document| document.get("fleet_update").cloned())
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
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
            node: as_requested(node.clone()),
            action: "config.ota.placement",
            detail: node,
            companions: Vec::new(),
        },
    )
    .await
}

/// Reads a store's published placement — the `device_ota` node the store runs, its layers composed
/// so a placement a rollback restored reads as it runs — or `null` if the store has never been
/// placed. Behind [`ConsolePermission::Read`].
async fn admin_get_placement<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Query(query): Query<OtaRolloutQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
                .map(ConfigTreeState::effective)
                .and_then(|document| document.get("device_ota").cloned())
                .unwrap_or(serde_json::Value::Null);
            (StatusCode::OK, Json(placement)).into_response()
        }
        Err(error) => config_store_error_response(&error),
    }
}

/// `412` for a bump whose `If-Match` does not match the row
/// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
///
/// `current` is read *after* the refusal, so it can already be stale by the time it is rendered.
/// That is fine and deliberate: it is for the message, never for a decision. The decision was the
/// write's own `WHERE`, which cannot race.
fn lease_moved_response(current: Option<LeaseGeneration>) -> Response {
    api_error_with_details(
        ErrorStatus::FailedPrecondition,
        "the store's lease has moved since you read it; re-read and retry",
        &[(
            "if-match",
            &current.map_or_else(
                || "NO_LEASE_ISSUED".to_owned(),
                |generation| generation.value().to_string(),
            ),
        )],
    )
}

/// `422` for a bump that would abandon a machine still holding events
/// ([ADR-0096](../../../docs/adr/0096-unprocessable-status.md)).
///
/// You do not move a store off a machine whose events are still on it — not without a person
/// saying, by name and in the trail, that those events are being abandoned. The detail names the
/// generation the caller has to acknowledge to proceed.
fn lease_undrained_response(superseded: LeaseGeneration) -> Response {
    api_error_with_details(
        ErrorStatus::Unprocessable,
        "this store's previous machine still holds events this cloud has never seen; acknowledge \
         that generation to move the store anyway, which abandons them",
        &[("acknowledge_undrained", &superseded.value().to_string())],
    )
}

/// Turns a region refusal into its [AIP-193](../../../docs/naming-and-api.md) envelope
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Extracted from [`admin_bump_lease`] rather than inlined: five messages is most of a screen, and
/// the handler it sits in already carries the generation, the placement, the acknowledgement and
/// the publish.
///
/// Each message names what the field accepts, not only what it rejected — the same rule the
/// placement's own refusal one screen up follows, and the difference between an admin who fixes the
/// form and one who guesses at it.
fn region_refusal_response(refusal: RegionRefusal) -> Response {
    let message = match refusal {
        RegionRefusal::Incomplete => {
            "a hosted edge placement needs both region_country and region_label — where a store's \
             data rests is recorded, not inferred"
        }
        RegionRefusal::NotForPlacement => {
            "a region belongs only to a hosted edge placement: an in-store machine is in the shop, \
             and a bump that names no edge_placement is a swap in place. To move a hosted store's \
             region, name its edge_placement again alongside the new one"
        }
        RegionRefusal::CountryShape => {
            "region_country must be an ISO 3166-1 alpha-2 code — two letters, such as SG or JP"
        }
        RegionRefusal::LabelEmpty => "region_label must name a place",
        RegionRefusal::LabelTooLong => {
            "region_label must be at most 64 characters — it names a place for a person to \
             recognise, not an address"
        }
    };
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        message,
        &[(refusal.field(), "INVALID_VALUE")],
    )
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
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    let context = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await?;
    let [tenant_id, store_id] = parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ])?;
    let (tenant_id, store_id) = (TenantId::new(tenant_id), StoreId::new(store_id));
    let requested_placement = lease_bump::requested_placement(request.edge_placement.as_deref())?;
    // Where the store's data will rest, decided by the same request that decides which machine
    // holds it (ADR-0114). The rule is a pure function so it can be tested without an HTTP stack;
    // here it is only turned into an envelope.
    let region = resolve_region(
        requested_placement,
        request.region_country.as_deref(),
        request.region_label.as_deref(),
    )
    .map_err(region_refusal_response)?;
    // The counter moves first. If the publish then fails the store is on a generation no till has
    // been told about, which is the safe half of the split: every box keeps the generation it holds
    // and keeps updating, and the next bump (or a re-publish) closes it. The opposite order would
    // publish a generation the authority never issued.
    //
    // The placement rides inside that same write rather than following it, so there is no interval
    // in which the store record and the lease disagree (ADR-0110).
    let expected_generation = if_match_lease(&headers)?;
    let acknowledged = request.acknowledge_undrained.map(LeaseGeneration::new);
    let bump = match state
        .config_trees
        .bump(
            tenant_id,
            store_id,
            state.clock.now(),
            requested_placement,
            region,
            acknowledged,
            expected_generation,
        )
        .await
    {
        Ok(LeaseBumpOutcome::Issued(bump)) => bump,
        // Both refusals return *before* `lease_node` and `publish_ota_node` below. That ordering is
        // the point: the comment above explains why the counter moves before the publish, and a
        // request that issued no generation must not reach a publish that would have to name one.
        Ok(LeaseBumpOutcome::VersionMismatch { current }) => {
            return Err(lease_moved_response(current));
        }
        Ok(LeaseBumpOutcome::Undrained { superseded }) => {
            return Err(lease_undrained_response(superseded));
        }
        Err(error) => return Err(lease_store_error_response(&error)),
    };
    let node = lease_node(bump.generation);
    let receipt_floor = receipt_floor(&state.config_trees, tenant_id, store_id).await;
    let companions = receipt_floor
        .map(|number| ("receipt_floor", serde_json::json!({ "number": number })))
        .into_iter()
        .collect();
    let response = publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "lease",
            node: as_requested(node),
            action: "config.lease.bump",
            // `edge_placement` is what the store now is; `edge_placement_moved` says whether this
            // bump chose it. Without the second flag the trail cannot tell a store that was moved
            // to a hosted machine from one whose already-hosted box was swapped — the same row
            // either way, and only one of them is a change of what the store can promise.
            detail: lease_bump::bump_audit_detail(
                &bump,
                requested_placement,
                acknowledged,
                receipt_floor,
            ),
            companions,
        },
    )
    .await;
    Ok(response)
}

/// The receipt floor a lease bump publishes beside the lease (ADR-0149): the highest receipt number
/// the cloud has ingested from the store, so the box that learns it is the store has already learned
/// where its numbers must start. A log that cannot be read does not hold the takeover back: the lease
/// still publishes, without a floor, and the warning says so.
async fn receipt_floor<Cfg: LeaseStore>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Option<u64> {
    match config_trees
        .highest_receipt_number(tenant_id, store_id)
        .await
    {
        Ok(floor) => floor,
        Err(error) => {
            tracing::warn!(
                %error,
                store = %store_id,
                "could not read the highest receipt number; the lease is published without a receipt floor"
            );
            None
        }
    }
}

/// A `POST /admin/config/lease/settle` body: the store, and the generation the caller is attesting
/// has nothing left on it.
///
/// `superseded_generation` is required and is the whole precondition. It is not "which handover do
/// you mean" — one store has at most one in flight — it is the caller stating *which machine* they
/// checked. A settle that took no number would let a stale console close a handover the operator
/// never looked at.
#[derive(Debug, Clone, Deserialize)]
struct SettleHandoverRequest {
    tenant_id: String,
    store_id: String,
    superseded_generation: u64,
}

/// A `POST /admin/config/lease/retire` body: which store's settled handover is being retired.
///
/// No generation, and that asymmetry with settle is deliberate. Settle is an attestation about one
/// named machine's disk; retire is a decision about the handover the row currently describes, and
/// the row's own preconditions — settled, and not already retired — are what make it unambiguous.
#[derive(Debug, Clone, Deserialize)]
struct RetireHandoverRequest {
    tenant_id: String,
    store_id: String,
}

/// `POST /admin/config/lease/settle` — a person attests that a superseded machine holds no events,
/// closing a handover that will never close itself. Behind [`ConsolePermission::ManageStores`].
///
/// The automatic path is a heartbeat from the outgoing machine reporting that generation with an
/// empty outbox ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)). This
/// route is for the machine that will never send one: a box already powered off, or one whose disk
/// an operator has read directly. It is an attestation, so it is audited with the attester's name.
///
/// Publishes nothing. A settled handover changes what the *console* knows, not what any till is
/// told — the lease generation the store runs on is untouched.
async fn admin_settle_handover<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<SettleHandoverRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
    let superseded = LeaseGeneration::new(request.superseded_generation);
    match state
        .config_trees
        .settle_handover(tenant_id, store_id, superseded)
        .await
    {
        Ok(SettleOutcome::Settled) => {}
        Ok(SettleOutcome::NotSuperseded { current }) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store's handover is not on the generation you named; re-read and check which \
                 machine you are attesting to",
                &[(
                    "superseded_generation",
                    &current.map_or_else(
                        || "NO_HANDOVER_IN_FLIGHT".to_owned(),
                        |generation| generation.value().to_string(),
                    ),
                )],
            );
        }
        Ok(SettleOutcome::NoLease) => {
            return not_found("store");
        }
        Err(error) => return lease_store_error_response(&error),
    }
    // After the write, and best-effort: a settle that succeeded must not be reported as a failure
    // because its trail entry could not be written (ADR-0069). `before` is the handover that was in
    // flight; `after` is `null`, because settling is the removal of it.
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.lease.settle",
        "store",
        &request.store_id,
        Some(serde_json::json!({ "superseded_generation": request.superseded_generation })),
        None,
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /admin/config/lease/retire` — a person decides a settled handover's outgoing machine, its
/// database and its hosting are no longer needed. Behind [`ConsolePermission::ManageStores`].
///
/// Nothing transitions into this on its own and nothing ever will: it is a judgement about money and
/// risk that no fact in this system implies
/// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)). It refuses while a
/// handover is in flight, because a machine that may hold the only copy of a night's sales is not
/// one anybody can decide is unnecessary — settle it first, by draining it or by attesting it empty.
///
/// Publishes nothing, for the same reason settle does not.
async fn admin_retire_handover<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<RetireHandoverRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
    let at = state.clock.now();
    match state
        .config_trees
        .retire(tenant_id, store_id, at, &context.admin.id)
        .await
    {
        Ok(RetireOutcome::Retired) => {}
        Ok(RetireOutcome::Undrained { superseded }) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store's handover is still in flight, so its previous machine may still hold \
                 events; settle it before deciding it is no longer needed",
                &[("superseded_generation", &superseded.value().to_string())],
            );
        }
        // The decider's id, never their email: the trail carries the address, and a refusal body is
        // read by whoever asked, who may not be the person entitled to see it.
        Ok(RetireOutcome::AlreadyRetired { at, by }) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store's handover has already been retired; the trail records who decided and \
                 when",
                &[
                    ("retired_at", &at.as_milliseconds_since_epoch().to_string()),
                    ("retired_by", &by),
                ],
            );
        }
        // Not a race and not a missing store: the store exists, holds its first-ever lease, and has
        // never had a second machine. `422` rather than `404` says exactly that — the resource is
        // there, the act does not apply to it.
        Ok(RetireOutcome::NeverSuperseded) => {
            return api_error_with_details(
                ErrorStatus::Unprocessable,
                "this store is on its first lease and has never handed over to another machine, so                  there is no previous machine to retire",
                &[("generation", "0")],
            );
        }
        Ok(RetireOutcome::Raced) => {
            return api_error_with_details(
                ErrorStatus::FailedPrecondition,
                "this store's handover moved while you were retiring it; re-read and retry",
                &[("retired_at", "CHANGED")],
            );
        }
        Ok(RetireOutcome::NoLease) => {
            return not_found("store");
        }
        Err(error) => return lease_store_error_response(&error),
    }
    // `retired_by` is the admin's id here too, matching the row. The entry's own `actor` already
    // carries the email and the role as they stood, which is where a human-readable actor belongs.
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.lease.retire",
        "store",
        &request.store_id,
        None,
        Some(serde_json::json!({
            "retired_at": at.as_milliseconds_since_epoch(),
            "retired_by": context.admin.id,
        })),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
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
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
                "generation": current.map(LeaseGeneration::value),
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
                    ("print_agents", "INVALID_VALUE"),
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
            report.print_agents,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => config_store_error_response(&error),
    }
}

/// The most envelopes one `POST /sync/stores/{store_id}/events` may carry
/// ([ADR-0143](../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)).
pub const MAX_PUBLISHED_EVENTS: usize = 256;

/// The largest body that route reads, 4 MiB (ADR-0143). A bigger one is answered `413` before it is
/// parsed.
pub const MAX_PUBLISHED_EVENTS_BYTES: usize = 4 * 1024 * 1024;

/// `POST /sync/stores/{store_id}/events`'s body: the store's committed events, oldest first.
#[derive(Debug, Deserialize)]
struct PublishedEvents {
    events: Vec<EventEnvelope<RawPayload>>,
}

/// What a publish came to: how many envelopes, counted from the first, the cloud holds. Always the
/// whole batch on success, since ingest commits all of it or none (ADR-0143).
#[derive(Debug, Serialize)]
struct PublishedEventsAccepted {
    accepted: usize,
}

/// `POST /sync/stores/{store_id}/events` — a store publishes its own events over HTTPS
/// ([ADR-0143](../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)).
///
/// Scope `publish_events`, which a box's device credential always carries, and the store in the
/// path must be the credential's. **Every envelope must name that store**: a batch with any other
/// store in it is refused whole, so no credential can write another shop's history. Ingest is the
/// same idempotent, all-or-nothing [`Cloud::ingest`] the NATS cursor uses, so a batch published
/// twice is stored once and the answer is the whole batch either way.
async fn edge_publish_events<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    body: axum::body::Bytes,
) -> Response
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let store_id = match publishing_store(&app, &headers, &store_id).await {
        Ok(store_id) => store_id,
        Err(refusal) => return refusal,
    };
    let batch = match serde_json::from_slice::<PublishedEvents>(&body) {
        Ok(batch) => batch,
        Err(error) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("the body is not a batch of events: {error}"),
                &[("events", "INVALID_VALUE")],
            );
        }
    };
    if batch.events.len() > MAX_PUBLISHED_EVENTS {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("a batch carries at most {MAX_PUBLISHED_EVENTS} events"),
            &[("events", "TOO_MANY")],
        );
    }
    if batch.events.iter().any(|event| event.store_id != store_id) {
        return api_error(
            ErrorStatus::PermissionDenied,
            "every event in a batch must belong to the store it is published for",
        );
    }
    match app.cloud.ingest(&batch.events).await {
        Ok(_outcome) => (
            StatusCode::OK,
            Json(PublishedEventsAccepted {
                accepted: batch.events.len(),
            }),
        )
            .into_response(),
        Err(error) => error_response(&error),
    }
}

/// `POST /sync/stores/{store_id}/events/hello` — the protocol negotiation for the HTTPS event link
/// ([ADR-0024](../../../docs/adr/0024-protocol-version-negotiation.md), ADR-0143), run on the cloud's side with
/// the cloud's own window. Answers `200` with the [`HelloOutcome`](pos_proto::protocol::HelloOutcome)
/// either way: a refusal is an answer, not an error, and the store keeps trading on it.
async fn edge_events_hello<S, R, K, C, A, T, W>(
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
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let store_id = match publishing_store(&app, &headers, &store_id).await {
        Ok(store_id) => store_id,
        Err(refusal) => return refusal,
    };
    let hello = match serde_json::from_slice::<pos_proto::protocol::Hello>(&body) {
        Ok(hello) => hello,
        Err(error) => {
            return api_error(
                ErrorStatus::InvalidArgument,
                format!("the body is not a hello: {error}"),
            );
        }
    };
    if hello.store_id != store_id {
        return api_error(
            ErrorStatus::PermissionDenied,
            "the hello names a store other than the one it is sent for",
        );
    }
    let outcome = pos_proto::protocol::negotiate(
        &hello,
        pos_proto::MIN_SUPPORTED_PROTOCOL_VERSION,
        pos_proto::PROTOCOL_VERSION,
    );
    (StatusCode::OK, Json(outcome)).into_response()
}

/// The checks both event-link routes share: an authenticated caller holding `publish_events`, for
/// the store named in the path, which must be the caller's own.
async fn publishing_store<S, R, K, C, A, T, W>(
    app: &CloudApp<S, R, K, C, A, T, W>,
    headers: &HeaderMap,
    store_id: &str,
) -> Result<StoreId, Response>
where
    K: ApiKeyStore + Send + Sync,
    C: ClockSource + Send + Sync,
{
    let grant = authenticate(&app.keys, &app.clock, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    require_scope(&grant, Scope::PublishEvents).map_err(IntoResponse::into_response)?;
    let store_id = match parse_ulid_fields([("store_id", store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return Err(refusal),
    };
    require_store(&grant, store_id).map_err(IntoResponse::into_response)?;
    Ok(store_id)
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
    /// One standing per bound print agent ([ADR-0112](../../../docs/adr/0112-print-agents.md)), or
    /// `None` when the store said nothing about them — an older edge, or one whose record could not
    /// be read.
    ///
    /// The distinction between absent and empty is load-bearing and is why this is not a bare
    /// `Vec`. Absent leaves whatever the cloud last knew alone. Empty says *this store has no bound
    /// agent*, which a manager releasing the last terminal produces and which must replace the
    /// stored list — a console still showing an agent nobody is bound to is the stale answer this
    /// field exists to prevent.
    #[serde(default)]
    print_agents: Option<Vec<PrintAgentStanding>>,
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

/// The one route [`throttle_sync`] guards on its own, much smaller budget: the unauthenticated code
/// exchange (`docs/pos-spec.md` §16, "device activation codes … are rate-limited"). It had no limit
/// at all, so nothing but the code space stood between a script and a guessing run, and every guess
/// cost a database lookup.
const ACTIVATE_PATH: &str = "/activate";

/// Code exchanges allowed per client within [`ACTIVATE_WINDOW_SECS`]. A technician mistypes a code
/// a few times; a box activates once. Ten leaves room for the first and none for a guessing run.
const ACTIVATE_MAX_ATTEMPTS: usize = 10;

/// The sliding window [`ACTIVATE_MAX_ATTEMPTS`] counts over, in seconds.
const ACTIVATE_WINDOW_SECS: u64 = 600;

/// Opening a claim ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md))
/// shares [`ACTIVATE_PATH`]'s budget: both are how an unauthenticated box first asks for anything.
const CLAIM_PATH: &str = "/claim";

/// Collection polls sit under this prefix, and get [`CLAIM_COLLECT_MAX_POLLS`] of their own.
const CLAIM_COLLECT_PREFIX: &str = "/claim/";

/// Collection polls allowed per client within [`CLAIM_COLLECT_WINDOW_SECS`]: one every five seconds,
/// with room for a second box claimed from the same shop.
const CLAIM_COLLECT_MAX_POLLS: usize = 120;

/// The sliding window [`CLAIM_COLLECT_MAX_POLLS`] counts over, in seconds.
const CLAIM_COLLECT_WINDOW_SECS: u64 = 300;

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
    let path = request.uri().path();
    let (limiter, refusal) = if path.starts_with(SYNC_PREFIX) {
        (
            &throttle.limiter,
            "too many requests to the store sync surface; try again later",
        )
    } else if path == ACTIVATE_PATH || path == CLAIM_PATH {
        (
            &throttle.activate_limiter,
            "too many activation attempts; try again later",
        )
    } else if path.starts_with(CLAIM_COLLECT_PREFIX) {
        (
            &throttle.collect_limiter,
            "too many claim polls; try again later",
        )
    } else {
        return next.run(request).await;
    };
    let ip = client_ip(request.headers(), throttle.trusted_proxy_hops);
    let keys = [format!("ip:{}", ip.unwrap_or("unknown"))];
    if let Err(retry_after_secs) = limiter.check_and_record(&keys, throttle.clock.now()) {
        return too_many_requests(refusal, retry_after_secs);
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
    /// `/activate`'s own budget, independent of `/sync`'s so a busy store's polling cannot lock a
    /// technician out of activating the next box.
    activate_limiter: SlidingRateLimiter,
    /// The claim-collection polls' budget, apart from the rest so a box waiting to be claimed cannot
    /// spend a technician's activation attempts.
    collect_limiter: SlidingRateLimiter,
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
    headers.insert(
        STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
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
                // Never the staff PIN hashes (S7), never a staff name or code to a role without
                // `ReadPeople` (ADR-0158), and never a price to a role without `ReadRevenue` (S8)
                // — see `staff_redacted_for` and `without_prices`.
                Some(effective) => {
                    let document = staff_redacted_for(effective, context.admin.role);
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

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/devices/admitted",
    params(
        ("store_id" = String, Path, description = "The store whose roster to read (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that store belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's admitted devices, still-admitted first and \
                                      newest admission first within each group. Each entry carries \
                                      the edge-minted local device id, when it was admitted, the \
                                      paired device whose signed-in manager minted the code (absent \
                                      for the code a box announces at boot), and when it was \
                                      retired if it has been. A retired device keeps its row. \
                                      Empty for a store that has admitted nothing, and for one \
                                      whose rollup has not been projected yet"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The rollup store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "devices",
)]
/// Which devices a store has admitted, which it has retired, and when
/// ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
///
/// The read that makes `device.admission.granted` and `device.admission.revoked` a feature rather
/// than two events with no reader — the state the tree already had one of, and which that record
/// named as the thing not to repeat. Before this the console's picture of a store's tills was zero
/// rows, however many tablets the store had actually paired.
///
/// **T3**, so `console.data.read` — the same standing the fleet detail beside it takes. The roster
/// carries device identifiers and instants: no money, and **no employee**, because who admitted a
/// device is recorded on the store's own disk and reaches no cloud (ADR-0118 §5). The tenant is
/// named explicitly, as on every `/admin` read, because the console admin is global.
///
/// Unwindowed and unpaged: a store's roster is a handful of rows, at the rate it commissions tills.
async fn admin_admitted_devices<S, R, K, C, A, T, W>(
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    match admitted_devices(&app.rollups, tenant_id, store_id).await {
        Ok(devices) => (StatusCode::OK, Json(devices)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
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
    let phc = hash_password(password, &salt_bytes).ok()?;
    Some((secret, phc))
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
mod devices_compile_tests {
    //! What a till's receipts compile to ([`super::compile_devices`], ADR-0160 decision 4), and the
    //! rule for a till whose receipt printer leaves the store's approved devices: it is published
    //! without the field, and prints at the store's receipt printer. And what a till's float compiles
    //! to (ADR-0167 decision 4): the float where one is set, and not a byte where none is.
    use super::devices::publish::compile_devices;
    use crate::devices::DeviceProposalSummary;
    use pos_proto::devices::PublishedDevice;
    use pos_proto::ids::DeviceId;
    use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};
    use pos_proto::ulid::Ulid;

    /// An approved row of the one store, as the device store lists it.
    fn row(
        id: u128,
        kind: &str,
        connection: Option<&str>,
        station: Option<u128>,
    ) -> DeviceProposalSummary {
        DeviceProposalSummary {
            id: Ulid::from_u128(id).to_string(),
            store_id: Ulid::from_u128(0x5).to_string(),
            kind: kind.to_owned(),
            name: format!("Device {id}"),
            address: String::new(),
            connection: connection.map(str::to_owned),
            station_id: station.map(|station| Ulid::from_u128(station).to_string()),
            agent_device_id: None,
            drawer_attached: false,
            paper_width: None,
            cuts_paper: None,
            receipt_printer_id: None,
            receipt_language: None,
            receipt_second_language: None,
            opening_float_minor: None,
            status: "approved".to_owned(),
            version: "1".to_owned(),
        }
    }

    /// The bar till, naming `printer` and printing in English then Vietnamese.
    fn bar_till(printer: u128) -> DeviceProposalSummary {
        DeviceProposalSummary {
            receipt_printer_id: Some(Ulid::from_u128(printer).to_string()),
            receipt_language: Some("RECEIPT_LANGUAGE_EN".to_owned()),
            receipt_second_language: Some("RECEIPT_SECOND_LANGUAGE_VI".to_owned()),
            ..row(7, "terminal", None, None)
        }
    }

    /// The compiled entry for `id`.
    fn compiled(rows: &[DeviceProposalSummary], id: u128) -> PublishedDevice {
        compile_devices(rows)
            .devices()
            .iter()
            .find(|device| device.device_id == DeviceId::new(Ulid::from_u128(id)))
            .cloned()
            .expect("the device is published")
    }

    #[test]
    fn a_till_carries_its_receipt_printer_and_languages_and_one_nobody_set_carries_none() {
        let rows = [
            row(1, "printer", Some("network"), None),
            row(2, "printer", Some("network"), None),
            bar_till(2),
            row(8, "terminal", None, None),
        ];
        let bar = compiled(&rows, 7);
        assert_eq!(
            bar.receipt_printer_id,
            Some(DeviceId::new(Ulid::from_u128(2)))
        );
        assert_eq!(bar.receipt_language(), Some(ReceiptLanguage::English));
        assert_eq!(
            bar.receipt_second_language(),
            Some(ReceiptSecondLanguage::Vietnamese)
        );
        let door = serde_json::to_value(compiled(&rows, 8)).expect("json");
        for field in [
            "receipt_printer_id",
            "receipt_language",
            "receipt_second_language",
        ] {
            assert!(door.get(field).is_none(), "{field} is not written: {door}");
        }
    }

    #[test]
    fn a_till_whose_receipt_printer_goes_is_published_without_it_and_keeps_its_languages() {
        // Archived: the printer is no longer among the approved rows. A station's printer, and a
        // printer approved before a connection was recorded, which the publish holds back, are not
        // a counter's receipt printer either. Each till prints at the store's instead.
        for rows in [
            vec![row(1, "printer", Some("network"), None), bar_till(2)],
            vec![row(2, "printer", Some("network"), Some(0x5A)), bar_till(2)],
            vec![row(2, "printer", None, None), bar_till(2)],
            vec![row(2, "kds", Some("network"), None), bar_till(2)],
        ] {
            let bar = compiled(&rows, 7);
            assert_eq!(bar.receipt_printer_id, None, "{rows:?}");
            assert_eq!(bar.receipt_language(), Some(ReceiptLanguage::English));
            assert_eq!(
                bar.receipt_second_language(),
                Some(ReceiptSecondLanguage::Vietnamese)
            );
        }
    }

    #[test]
    fn a_till_carries_its_own_float_and_one_nobody_set_carries_no_key() {
        // `0` is a float, a drawer started empty, and not the store's.
        let float = |id, minor| DeviceProposalSummary {
            opening_float_minor: Some(minor),
            ..row(id, "terminal", None, None)
        };
        let rows = [
            float(7, 250_000),
            float(8, 0),
            row(9, "terminal", None, None),
        ];
        assert_eq!(compiled(&rows, 7).opening_float_minor(), Some(250_000));
        assert_eq!(compiled(&rows, 8).opening_float_minor(), Some(0));
        let door = serde_json::to_value(compiled(&rows, 9)).expect("json");
        assert!(door.get("opening_float_minor").is_none(), "{door}");
    }

    #[test]
    fn a_node_whose_tills_set_no_float_is_the_bytes_it_was_before_the_column() {
        // Migration 0085 leaves every row's float null, so a store nobody has set a float for
        // publishes the devices node it published before, byte for byte.
        let rows = [
            row(1, "printer", Some("network"), None),
            row(7, "terminal", None, None),
        ];
        let json = serde_json::to_string(&compile_devices(&rows)).expect("json");
        assert_eq!(
            json,
            concat!(
                r#"{"devices":[{"device_id":"00000000000000000000000001","#,
                r#""kind":"DEVICE_KIND_PRINTER","connection":"DEVICE_CONNECTION_NETWORK","#,
                r#""address":"","name":"Device 1"},"#,
                r#"{"device_id":"00000000000000000000000007","kind":"DEVICE_KIND_TERMINAL","#,
                r#""connection":"DEVICE_CONNECTION_UNSPECIFIED","address":"","name":"Device 7"}]}"#,
            )
        );
    }
}

#[cfg(test)]
mod locale_node_tests {
    //! What a store's `locale` node actually carries
    //! ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)).
    //!
    //! Covered here rather than through the route because `checked_locale_node` *is* what the route
    //! validates: the handler around it holds a permission check, two ULID parses and a config-tree
    //! write, none of which can tell you whether the node an edge receives is complete.
    use super::config_nodes::locale::{PublishLocaleRequest, checked_locale_node, packed_facts};
    use crate::countries;
    use pos_proto::locale::{PublishedLocale, PublishedNumberFormat};
    use serde_json::Value;

    /// A publish as the console sends one, for a store trading in `currency`.
    fn request(country: &str, currency: &str) -> PublishLocaleRequest {
        PublishLocaleRequest {
            tenant_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            store_id: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned(),
            country_code: country.to_owned(),
            currency_code: currency.to_owned(),
            timezone: "Asia/Kolkata".to_owned(),
            cutoff_hour: 4,
            display_language: None,
            prices_include_tax: true,
            cash_rounding_increment: Some(100),
            cash_denominations: vec![10_000, 20_000],
        }
    }

    #[test]
    fn the_node_tells_the_store_how_many_decimals_its_currency_has() {
        // ADR-0134: "It rides the `locale` config node to the store and is served on
        // `GET /api/locale`." Without it the edge keeps its bootstrap zero, `GET /api/locale`
        // serves that zero as though it were published, and the till's `?? fallbackExponent(...)`
        // never fires because `0` is not nullish. An Indian store then draws 26145 paise as
        // `INR 26,145` and prints `INR 26145` on a tax invoice — the two defects #416 and #417
        // were merged to fix.
        let node =
            checked_locale_node(&request("IN", "INR"), &packed_facts(&countries::registry()))
                .expect("a valid publish");
        assert_eq!(
            node.get("currency_exponent").and_then(Value::as_u64),
            Some(2),
            "the rupee has two decimal places and the node has to say so: {node}"
        );
    }

    #[test]
    fn the_node_tells_the_store_how_long_to_keep_a_personal_record() {
        // ADR-0107 gave the edge its own retention sweep and had it read this key. The node never
        // carried it, so every store ran on the bootstrap 365 instead of its country's figure.
        //
        // Nothing was wrong *yet* — all four packs say 365 — which is precisely why it needed a
        // test rather than a reader noticing. The figure each pack carries is marked "a default,
        // not a determination"; the first time one is set for real, this is what makes the publish
        // mean something.
        let node =
            checked_locale_node(&request("IN", "INR"), &packed_facts(&countries::registry()))
                .expect("a valid publish");
        assert_eq!(
            node.get("default_retention_days").and_then(Value::as_u64),
            Some(365),
            "the store's retention period has to reach the store: {node}"
        );
    }

    #[test]
    fn the_node_tells_the_store_how_its_country_writes_a_number() {
        // ADR-0136. Vietnam is the case worth asserting: it is the only shipped pack whose format
        // differs from the compiled default, so it is the only one where a store receiving nothing
        // and a store receiving the right thing look different.
        let node =
            checked_locale_node(&request("VN", "VND"), &packed_facts(&countries::registry()))
                .expect("a valid publish");
        let format = node.get("number_format").expect("the node names a format");
        assert_eq!(
            format.get("decimal_separator").and_then(Value::as_str),
            Some(",")
        );
        assert_eq!(
            format.get("group_separator").and_then(Value::as_str),
            Some(".")
        );
        assert_eq!(
            format.get("digits_per_group").and_then(Value::as_u64),
            Some(3)
        );
    }

    #[test]
    fn the_node_tells_the_store_the_language_of_its_country() {
        // ADR-0160: a new store's receipts print in its country's language, which the edge has no
        // pack to look up.
        let node =
            checked_locale_node(&request("VN", "VND"), &packed_facts(&countries::registry()))
                .expect("a valid publish");
        assert_eq!(
            node.get("country_language").and_then(Value::as_str),
            Some("vi"),
            "{node}"
        );
    }

    /// The node carries every field the edge reads, and nothing else but the two the cloud keeps for
    /// itself.
    ///
    /// The fields are the type's, `pos_proto::locale::PublishedLocale`, the one the edge applies the
    /// node through, rather than a list written out here: the node parses as it, sends each field it
    /// reads and reads back as stated, and sends no other key but the cloud's own. A field added to
    /// the type does not compile below until it is stated.
    ///
    /// Both directions are checked, because the gap has appeared both ways round: a key the edge
    /// reads and the cloud never sends is a feature that does nothing, which is where
    /// `currency_exponent` and `default_retention_days` both sat through the releases that depended
    /// on them, and a key the cloud sends and the edge never reads is a payload nobody consumes.
    /// Neither shows up as a failure anywhere else.
    ///
    /// Its sibling closes the other half: `every_field_a_locale_node_carries_reaches_the_session`
    /// in `crates/pos-edge/src/config_client.rs` asserts each of these fields actually moves a
    /// session field.
    #[test]
    fn the_node_carries_what_the_edge_reads_and_nothing_unclaimed() {
        // Not read through `PublishedLocale`. `country_code` is the fleet console's own comparison
        // against a store's region (ADR-0114). `display_language` is the store's choice rather than
        // its country's, and the edge reads it apart from the type, field by field, to name the
        // menu's items in it and keep it on the session (ADR-0074).
        const THE_CLOUD_S_OWN: &[&str] = &["country_code", "display_language"];

        let mut published = request("VN", "VND");
        published.display_language = Some("vi".to_owned());
        let node = checked_locale_node(&published, &packed_facts(&countries::registry()))
            .expect("a valid publish");
        let sent = node.as_object().expect("the node is an object");

        // The node as the edge reads it, through its text as `session_from_config` parses it, and
        // written back: an optional field the node sends as `null` reads as absent.
        let read: PublishedLocale =
            serde_json::from_str(&node.to_string()).expect("the edge applies the node it is sent");
        let read = serde_json::to_value(read).expect("serialise");
        let read = read.as_object().expect("an object");

        // Every key the type reads, from the type: a value stating each field, written.
        let every_field = serde_json::to_value(PublishedLocale {
            currency_code: String::new(),
            currency_exponent: Some(0),
            timezone: String::new(),
            cutoff_hour: 0,
            prices_include_tax: true,
            cash_rounding_increment: Some(0),
            cash_denominations: vec![0],
            default_retention_days: Some(0),
            number_format: Some(PublishedNumberFormat {
                decimal_separator: String::new(),
                group_separator: String::new(),
                digits_per_group: 0,
            }),
            country_language: Some(String::new()),
        })
        .expect("serialise");
        let every_field = every_field.as_object().expect("an object");

        let missing: Vec<&String> = every_field
            .keys()
            .filter(|key| !sent.contains_key(*key) || !read.contains_key(*key))
            .collect();
        assert!(
            missing.is_empty(),
            "the edge parses these and the node does not send them, so they do nothing: {missing:?}"
        );

        let unclaimed: Vec<&String> = sent
            .keys()
            .filter(|key| {
                !every_field.contains_key(*key) && !THE_CLOUD_S_OWN.contains(&key.as_str())
            })
            .collect();
        assert!(
            unclaimed.is_empty(),
            "the node sends these and nothing reads them — add them to `PublishedLocale` or to the \
             list above, whichever is true: {unclaimed:?}"
        );
    }

    #[test]
    fn a_zero_decimal_currency_says_zero_rather_than_saying_nothing() {
        // The đồng's exponent really is zero, and that has to be *published* rather than left to
        // the edge's bootstrap to coincide with. The two are indistinguishable on a Vietnamese
        // store, which is exactly why the rupee case above is the one that catches this.
        let node =
            checked_locale_node(&request("VN", "VND"), &packed_facts(&countries::registry()))
                .expect("a valid publish");
        assert_eq!(
            node.get("currency_exponent").and_then(Value::as_u64),
            Some(0),
            "the node must state the đồng's zero, not omit it: {node}"
        );
    }
}

#[cfg(test)]
mod revoked_devices_tests {
    //! The deny-list a revocation writes ([`super::append_revoked_device`]): the list the store runs,
    //! read from the tree's layers composed, with the new id appended.
    use super::devices::revoke::append_revoked_device;
    use crate::config_tree::ConfigTreeState;
    use pos_core::device_revocation::MAX_REVOKED_DEVICES;
    use pos_proto::ulid::Ulid;
    use serde_json::{Value, json};

    /// A tree whose Tenant layer holds `tenant` and whose Store layer holds `store`.
    fn tree(tenant: Value, store: Value) -> ConfigTreeState {
        ConfigTreeState {
            layers: [tenant, json!({}), store, json!({})],
            history: Vec::new(),
            k: 8,
        }
    }

    fn id(n: u128) -> String {
        Ulid::from_u128(n).to_string()
    }

    fn deny_list(ids: &[String]) -> Value {
        json!({ "revoked_devices": { "device_ids": ids } })
    }

    /// The ids the append writes onto the Store layer.
    fn written(before: &ConfigTreeState, listed: &str) -> Vec<String> {
        let (key, node) = append_revoked_device(Some(before), listed)
            .expect("appended")
            .into_iter()
            .next()
            .expect("one node");
        assert_eq!(key, "revoked_devices");
        serde_json::from_value(node["device_ids"].clone()).expect("a list of ids")
    }

    #[test]
    fn a_restored_list_is_kept_and_the_new_id_appended_to_it() {
        // A rollback leaves the restored list on the Tenant layer and the Store layer empty.
        let restored = tree(deny_list(&[id(1), id(2)]), json!({}));
        assert_eq!(written(&restored, &id(3)), vec![id(1), id(2), id(3)]);
        assert_eq!(
            written(&restored, &id(1)),
            vec![id(1), id(2)],
            "an id already listed adds nothing"
        );
    }

    #[test]
    fn the_list_appended_to_is_the_one_the_store_runs() {
        // A list on the Store layer replaces one below it when the layers merge, so it is the list
        // the store runs, and the one a revocation adds to.
        let both = tree(deny_list(&[id(1)]), deny_list(&[id(2)]));
        assert_eq!(written(&both, &id(3)), vec![id(2), id(3)]);
    }

    #[test]
    fn the_cap_counts_the_list_the_store_runs() {
        let full: Vec<String> = (1_u128..).take(MAX_REVOKED_DEVICES).map(id).collect();
        let restored = tree(deny_list(&full), json!({}));
        let refused =
            append_revoked_device(Some(&restored), &id(0xFFFF)).expect_err("past the cap");
        assert_eq!(refused.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            written(&restored, &id(1)).len(),
            MAX_REVOKED_DEVICES,
            "an id already listed re-publishes the full list"
        );
    }
}

#[cfg(test)]
mod node_as_run_tests {
    //! What a store's node reads as once its layers are composed ([`super::node_as_run`]).
    use super::node_as_run;
    use serde_json::{Value, json};

    #[test]
    fn a_node_reads_without_its_settings_and_one_of_settings_alone_reads_as_none() {
        let locale = json!({
            "currency_code": "VND",
            "timezone": "Asia/Ho_Chi_Minh",
            "tax_rounding": "TAX_ROUNDING_DOWN",
        });
        assert_eq!(
            node_as_run("locale", Some(&locale)),
            json!({ "currency_code": "VND", "timezone": "Asia/Ho_Chi_Minh" })
        );
        for (node, settings) in [
            ("locale", json!({ "tax_rounding": "TAX_ROUNDING_DOWN" })),
            ("qr", json!({ "table_order": "TABLE_ORDER_JOIN" })),
        ] {
            assert_eq!(node_as_run(node, Some(&settings)), Value::Null, "{node}");
        }
        assert_eq!(node_as_run("locale", None), Value::Null);
    }

    #[test]
    fn a_node_with_no_settings_fields_reads_as_it_is_composed() {
        // `table_order` is a setting of the `qr` node, not of `channels`.
        let channels = json!({ "enabled": ["SALES_CHANNEL_DINE_IN"], "table_order": 1 });
        assert_eq!(node_as_run("channels", Some(&channels)), channels);
        // An empty node is a node, as the publish check counts it, and a node that is not an
        // object, `null` included, is answered as it is.
        for node in [json!({}), Value::Null, json!([1, 2]), json!("text")] {
            assert_eq!(node_as_run("qr", Some(&node)), node, "{node}");
        }
    }
}

#[cfg(test)]
mod qr_node_tests {
    //! The `qr` node the console's QR form publishes reads back through the type the edge and the
    //! guest intake read it with (`pos_proto::qr::PublishedQr`), every field as it was written.
    use super::config_nodes::compilers::{QrGuardrailFields, qr_guardrail_node};
    use super::config_nodes::qr::QrBusinessHoursRequest;
    use pos_proto::qr::{PublishedQr, QrBusinessHours, TableOrder};

    #[test]
    fn the_node_the_qr_form_writes_reads_back_through_its_type() {
        for (enabled, hours) in [
            (
                Some(false),
                Some(QrBusinessHoursRequest {
                    open_hour: 22,
                    close_hour: 2,
                    tz_offset_minutes: 420,
                }),
            ),
            (None, None),
        ] {
            let fields = QrGuardrailFields {
                enabled,
                staff_confirmation_required: false,
                per_table_limit: 5,
                rate_window_secs: 300,
                business_hours: hours.clone(),
            };
            let node = qr_guardrail_node(&fields).expect("a valid publish");
            let read: PublishedQr =
                serde_json::from_str(&node.to_string()).expect("the node parses");
            assert_eq!(read.table_order(), TableOrder::Separate);
            assert_eq!(read.enabled, enabled);
            assert_eq!(read.staff_confirmation_required, Some(false));
            assert_eq!(read.per_table_limit, Some(5));
            assert_eq!(read.rate_window_secs, Some(300));
            assert_eq!(
                read.business_hours,
                hours.map(|hours| QrBusinessHours {
                    open_hour: hours.open_hour,
                    close_hour: hours.close_hour,
                    tz_offset_minutes: Some(hours.tz_offset_minutes),
                })
            );
            assert_eq!(PublishedQr::guardrails(&node), Some(read.clone()));

            // Nothing the form writes is lost or renamed on the way through the type.
            let mut written_back = serde_json::to_value(&read).expect("serialise");
            if let serde_json::Value::Object(fields) = &mut written_back {
                fields.remove("table_order");
            }
            assert_eq!(written_back, node);
        }
    }
}

// The preview tests below reach the prerequisite check through `super`, as they did when it lived in
// this file; the import sits below the file's first `#[cfg(test)]`, with the other test code.
#[cfg(test)]
use config_publish::check_prerequisites;

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
                nodes: store_layer
                    .as_object()
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default(),
                effective: store_layer,
            }],
            k: 8,
        }
    }

    /// The node keys a publish names, as `publish_config_nodes_with` computes them.
    fn publishing(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_menu_is_refused_to_a_store_with_no_tax_table_and_the_refusal_says_which() {
        // Finding F7: the batch path skipped this store; the single-store path published happily
        // and produced a shop that raises TaxRateNotConfigured at the payment screen.
        let before = json!({"locale": {"currency": "VND"}});
        let refusal = super::check_prerequisites(&before, &publishing(&["menu"]))
            .expect_err("a menu with no tax table is refused");
        assert_eq!(
            refusal.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn a_menu_publishes_when_the_prerequisites_are_inherited_rather_than_store_level() {
        // The check reads the effective document, so a tax node from the tenant layer counts. It
        // has to: refusing it would be refusing a store that is correctly configured.
        let before = json!({"locale": {"currency": "VND"}, "tax": {"rates": []}});
        super::check_prerequisites(&before, &publishing(&["menu"]))
            .expect("the inherited nodes satisfy the rule");
    }

    #[test]
    fn a_publish_that_writes_its_own_prerequisite_in_the_same_call_is_not_refused() {
        super::check_prerequisites(&json!({}), &publishing(&["menu", "tax", "locale"]))
            .expect("one call writing all three is one call, not three out of order");
    }

    #[test]
    fn a_node_with_no_prerequisites_is_never_refused_even_on_an_empty_store() {
        super::check_prerequisites(&json!({}), &publishing(&["locale"]))
            .expect("locale needs nothing");
        super::check_prerequisites(&json!({}), &publishing(&["tax"])).expect("nor does tax");
    }

    #[test]
    fn a_null_prerequisite_counts_as_absent() {
        // A node explicitly set to null is a node the store does not have; treating the key's
        // presence as enough would pass a menu whose tax table is the word "null".
        let before = json!({"locale": {"currency": "VND"}, "tax": Value::Null});
        super::check_prerequisites(&before, &publishing(&["menu"]))
            .expect_err("a null tax node is no tax node");
    }

    #[test]
    fn a_locale_that_carries_only_its_settings_is_not_the_locale_a_menu_needs() {
        // ADR-0160: a tenant's tax rounding reaches the Tenant layer of every store it covers,
        // including one whose locale was never published. That store still has no locale.
        let setting = json!({"tax_rounding": "TAX_ROUNDING_DOWN"});
        let before = json!({"locale": setting, "tax": {"rates": []}});
        super::check_prerequisites(&before, &publishing(&["menu"]))
            .expect_err("a setting alone is no locale");
        let before = json!({
            "locale": {"currency": "VND", "tax_rounding": "TAX_ROUNDING_DOWN"},
            "tax": {"rates": []},
        });
        super::check_prerequisites(&before, &publishing(&["menu"]))
            .expect("a published locale with the setting beside it");
    }

    #[test]
    fn a_backup_node_of_settings_alone_is_a_settings_only_node() {
        // ADR-0160 decision 6: the settings compile writes `backup.interval_hours` on the Tenant
        // layer of every store it reaches, and the node holds nothing else, so the publish check
        // treats it as it treats a `locale` holding only its tax rounding.
        assert!(super::only_settings(
            "backup",
            &json!({"interval_hours": 6})
        ));
        assert!(!super::only_settings(
            "backup",
            &json!({"interval_hours": 6, "destination": "elsewhere"})
        ));
        assert!(
            !crate::config_tree::NODE_PREREQUISITES
                .iter()
                .any(|(_, needs)| needs.contains(&"backup")),
            "and no node waits on it"
        );
    }

    #[test]
    fn nothing_waits_on_the_tender_keys_node() {
        // ADR-0160: the node holds settings and nothing else, which the rule above counts as no node.
        assert!(
            !crate::config_tree::NODE_PREREQUISITES
                .iter()
                .any(|(_, needs)| needs.contains(&"tender_keys"))
        );
    }

    #[test]
    fn nothing_waits_on_the_counter_node() {
        // ADR-0160: the walk-in channel's node holds settings and nothing else, as `tender_keys` does.
        assert!(
            !crate::config_tree::NODE_PREREQUISITES
                .iter()
                .any(|(_, needs)| needs.contains(&"counter"))
        );
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
        ConfigLevel, EntityStatus, accepted_list, accepted_tokens, entity_status_refusal,
        parse_config_level, parse_entity_status,
    };
    use crate::auth::admin::{AdminRole, AdminStatus};
    use axum::http::StatusCode;
    use pos_proto::enums::{PaymentMethod, SalesChannel};
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

    use super::campaigns::{CampaignRequest, build_campaign};
    use super::config_nodes::parse_known_tokens;
    use super::inventory::{
        IngredientRequest, RecipeRequest, SupplierRequest, build_ingredient, build_recipe,
        build_supplier,
    };
    use super::{FieldRefusal, api_error, error_response};
    use axum::http::StatusCode;
    use axum::response::Response;
    use pos_cloud_api::response::http_status;
    use pos_cloud_reports::dashboard::rollup::WindowError;
    use pos_cloud_reports::refusal::window_refusal;
    use pos_ports::{PortError, PortName};
    use pos_proto::enums::SalesChannel;
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
    //! Three independent redactions run on the same document and the tests keep them independent:
    //! the staff credential goes unconditionally (**S7**), the staff names and codes go by role
    //! (ADR-0158), and the priced nodes go by role (**S8**). A change that merged any two would be a
    //! change in behaviour for one of them. `staff_redacted_for` applies the two staff rules
    //! together, as the config reads and the publish previews must, and still as two rules.

    use super::config_publish::redaction::{
        PRICED_CONFIG_NODES, without_staff_credentials, without_staff_identities,
    };
    use super::{staff_redacted_for, without_prices};
    use crate::auth::admin::AdminRole;
    use crate::config_tree::merge::diff;
    use serde_json::json;

    /// A document shaped like a real effective config: a priced node, a free one, and the staff node.
    fn document() -> serde_json::Value {
        json!({
            "menu": { "channels": { "dine_in": { "items": [{ "price_amount_minor": 95_000 }] } } },
            "campaigns": { "rules": [{ "amount": { "currency": "VND", "minor": 20_000 } }] },
            "fees": { "fees": [{ "code": "SERVICE", "rate": { "numerator": 5, "denominator": 100 } }] },
            "layout": { "buttons": ["margherita"] },
            "capabilities": { "tips_enabled": true },
            "permissions": {
                "store_id": "01S",
                "staff": [{
                    "id": "01J",
                    "code": "C01",
                    "name": "Alice",
                    "permissions": ["billing.discount.apply"],
                    "pin_phc": "$argon2id$v=19$...",
                }],
            },
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
    fn a_role_without_read_people_gets_no_staff_name_or_code() {
        let redacted = without_staff_identities(&document());
        let member = redacted["permissions"]["staff"][0]
            .as_object()
            .expect("the staff member is still listed");
        assert!(
            !member.contains_key("name") && !member.contains_key("code"),
            "a name and a staff code are T1 and need ReadPeople (ADR-0158): {member:?}"
        );
        // Only those two go: the config screen still shows what each person at the store may do.
        assert_eq!(member["id"], "01J");
        assert_eq!(member["permissions"], json!(["billing.discount.apply"]));
        assert_eq!(redacted["permissions"]["store_id"], "01S");
        assert_eq!(redacted["capabilities"], document()["capabilities"]);
    }

    #[test]
    fn the_staff_redactions_are_independent() {
        // A caller with ReadPeople still loses the PIN hash, and losing the PIN hash does not take
        // the name with it — the credential goes for everyone, the person only for some roles.
        let credential_gone = without_staff_credentials(&document());
        let member = &credential_gone["permissions"]["staff"][0];
        assert_eq!(
            member["name"], "Alice",
            "S7 removes the credential, not the person"
        );
        assert_eq!(member["code"], "C01");

        let identity_gone = without_staff_identities(&document());
        assert!(
            identity_gone["permissions"]["staff"][0]
                .get("pin_phc")
                .is_some(),
            "the credential is S7's to remove, and both config reads apply S7 on every read"
        );
        assert!(
            identity_gone.get("menu").is_some(),
            "and the prices are S8's: two permissions, however the four roles line up today"
        );
    }

    #[test]
    fn redacting_a_document_that_has_no_priced_node_changes_nothing() {
        // A store that has published capabilities and nothing else is the common case on day one.
        let plain = json!({ "capabilities": { "tips_enabled": false } });
        assert_eq!(without_prices(&plain), plain);
    }

    #[test]
    fn every_role_reads_the_staff_its_permissions_allow_and_none_reads_a_pin_hash() {
        for (role, names_staff) in [
            (AdminRole::Owner, true),
            (AdminRole::Admin, true),
            (AdminRole::Ops, false),
            (AdminRole::Viewer, false),
        ] {
            let redacted = staff_redacted_for(&document(), role);
            let member = redacted["permissions"]["staff"][0]
                .as_object()
                .expect("the member is still listed");
            assert!(
                !member.contains_key("pin_phc"),
                "{role:?}: never the credential (S7)"
            );
            assert_eq!(
                member.contains_key("name"),
                names_staff,
                "{role:?}: a name only with ReadPeople (ADR-0158): {member:?}"
            );
            assert_eq!(
                member.contains_key("code"),
                names_staff,
                "{role:?}: a staff code only with ReadPeople (ADR-0158): {member:?}"
            );
            assert_eq!(member["id"], "01J", "{role:?}: the member stays");
            assert!(
                redacted.get("menu").is_some(),
                "{role:?}: the prices are S8's to remove, not this function's"
            );
        }
    }

    #[test]
    fn a_preview_patch_is_redacted_at_the_documents_own_path() {
        // A preview answers with the merge patch from the store's document to the candidate.
        // RFC 7386 carries a changed array whole, so renaming one member and resetting their PIN
        // puts the whole staff array in the patch: complete members, where the document keeps them.
        let before = document();
        let mut after = document();
        after["permissions"]["staff"][0]["name"] = json!("Alicia");
        after["permissions"]["staff"][0]["pin_phc"] = json!("$argon2id$v=19$new...");
        let patch = diff(&before, &after);
        assert!(
            patch["permissions"]["staff"][0]["pin_phc"].is_string(),
            "unredacted, the patch carries the new hash; that is what the redaction is for: {patch}"
        );

        let ops = staff_redacted_for(&patch, AdminRole::Ops);
        let member = ops["permissions"]["staff"][0]
            .as_object()
            .expect("the member is still in the patch");
        for field in ["pin_phc", "name", "code"] {
            assert!(
                !member.contains_key(field),
                "Ops reads the patch without `{field}`: {member:?}"
            );
        }
        assert_eq!(member["id"], "01J", "and still sees which member changed");

        let owner = staff_redacted_for(&patch, AdminRole::Owner);
        let member = &owner["permissions"]["staff"][0];
        assert_eq!(member["name"], "Alicia", "an Owner reads the rename");
        assert!(member.get("pin_phc").is_none(), "and nobody reads the hash");

        // A patch that does not reach the staff comes back as it went in. `{}` is the preview of a
        // publish that would change nothing, and must stay `{}`.
        for untouched in [
            json!({}),
            json!({ "permissions": null }),
            json!({ "permissions": { "store_id": "01S" } }),
            json!({ "capabilities": { "tips_enabled": true } }),
        ] {
            assert_eq!(staff_redacted_for(&untouched, AdminRole::Viewer), untouched);
        }
    }
}

#[cfg(test)]
mod audit_read_redaction_tests {
    //! What a role without `console.people.read` reads of the audit trail (ADR-0158 decision 9): an
    //! entry about an employee without the person's staff code, and everything else as written.

    use super::admin::audit::{audit_views, without_audited_staff_identity};
    use crate::audit::AuditEntry;
    use crate::auth::admin::AdminRole;
    use pos_cloud_api::audit::{AuditActor, AuditId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use serde_json::json;

    /// An entry as a console write records it, about one entity of `entity_type`.
    fn entry(
        entity_type: &str,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) -> AuditEntry {
        AuditEntry {
            id: AuditId::new(Ulid::from_u128(1)),
            tenant_id: None,
            actor: AuditActor {
                admin_id: "01ADMIN".to_owned(),
                email: "owner@example.test".to_owned(),
                role: AdminRole::Owner,
            },
            action: format!("{entity_type}.update"),
            entity_type: entity_type.to_owned(),
            entity_id: "01J".to_owned(),
            before,
            after,
            request_id: None,
            at: Timestamp::from_milliseconds_since_epoch(0).expect("the epoch is an instant"),
        }
    }

    #[test]
    fn an_employee_entry_loses_the_staff_code_and_keeps_the_rest() {
        let read = without_audited_staff_identity(entry(
            "employee",
            Some(json!({ "id": "01J", "code": "C01", "status": "active" })),
            // No route writes a name (ADR-0070); if one ever did, it goes the same way.
            Some(json!({ "id": "01J", "code": "C01", "status": "archived", "name": "Alice" })),
        ));
        for (side, recorded) in [("before", &read.before), ("after", &read.after)] {
            let fields = recorded
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .expect("both sides are still there");
            assert!(
                !fields.contains_key("code") && !fields.contains_key("name"),
                "{side}: {fields:?}"
            );
            assert_eq!(fields["id"], "01J", "{side}: which employee stays");
        }
        assert_eq!(read.entity_id, "01J");
        assert_eq!(read.before.expect("before")["status"], "active");
        assert_eq!(read.after.expect("after")["status"], "archived");

        // A role's name is the role's and a store's is the shop's: only an employee is a person.
        for (entity_type, recorded) in [
            (
                "role",
                json!({ "id": "01R", "name": "Cashier", "permissions": [] }),
            ),
            ("store", json!({ "name": "Bến Thành" })),
            (
                "assignment",
                json!({ "id": "01A", "employee_id": "01J", "store_id": "01S" }),
            ),
        ] {
            let kept =
                without_audited_staff_identity(entry(entity_type, None, Some(recorded.clone())));
            assert_eq!(kept.after, Some(recorded), "{entity_type}");
        }
    }

    #[test]
    fn only_a_role_holding_read_people_reads_the_staff_code() {
        for (role, names_staff) in [
            (AdminRole::Owner, true),
            (AdminRole::Admin, true),
            (AdminRole::Ops, false),
            (AdminRole::Viewer, false),
        ] {
            let written = entry(
                "employee",
                None,
                Some(json!({ "id": "01J", "code": "C01", "status": "active" })),
            );
            let views = audit_views(vec![written], role);
            let after = views
                .first()
                .and_then(|view| view.after.as_ref())
                .expect("the entry and its after are read");
            assert_eq!(
                after.get("code").is_some(),
                names_staff,
                "{role:?}: {after}"
            );
            assert_eq!(after["status"], "active", "{role:?}: the rest stays");
        }
    }
}

#[cfg(test)]
mod settings_catalogue_tests {
    //! What the settings catalogue tells the console about each kind, and the reason a write of a
    //! value the kind does not take is refused with (ADR-0160 decision 9).
    //!
    //! The console draws its form from these fields and nothing else, so each kind is pinned: a
    //! choice carries its values; a whole number its bounds and unit and no values; a switch
    //! neither. A whole number and a switch are built here, as the register's own tests build
    //! them, so these tests do not move when a register entry does.

    use pos_proto::settings::{Setting, SettingScope, SettingShape, SettingUnit, register};
    use serde_json::json;

    use super::setting_refusal::setting_refusal_response;
    use super::settings::catalogue::SettingCatalogueEntry;
    use crate::settings::SettingRefusal;

    fn entry(setting: &Setting) -> serde_json::Value {
        serde_json::to_value(SettingCatalogueEntry::of(setting)).expect("an entry serialises")
    }

    fn wait_seconds() -> Setting {
        Setting {
            node: "example",
            field: "wait_seconds",
            shape: SettingShape::Int {
                min: 0,
                max: 3600,
                unit: SettingUnit::Seconds,
                default: 0,
                preset: Some(120),
            },
            scopes: &[SettingScope::Store],
            since: "0.15.0",
            confirm: None,
            summary: "How long to wait.",
        }
    }

    fn print_on_settle() -> Setting {
        Setting {
            node: "example",
            field: "print_on_settle",
            shape: SettingShape::Bool {
                default: true,
                preset: None,
            },
            scopes: &[SettingScope::Tenant, SettingScope::Store],
            since: "0.15.0",
            confirm: None,
            summary: "Whether to print.",
        }
    }

    /// The status and the `details` the console reads when `setting` refuses `value`.
    async fn refused(setting: &Setting, value: &serde_json::Value) -> (u16, serde_json::Value) {
        let refusal = setting
            .check(value)
            .expect_err("the setting refuses the value");
        let response = setting_refusal_response(&SettingRefusal::ValueNotAllowed {
            setting_key: setting.key(),
            allowed: setting.takes(),
            refusal,
        });
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("collect the refusal");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("a JSON envelope");
        (status, body["error"]["details"].clone())
    }

    #[tokio::test]
    async fn each_wrong_value_is_refused_with_the_reason_the_console_explains() {
        let at_fault = |reason: &str| json!([{ "field": "value", "reason": reason }]);
        let seconds = wait_seconds();
        assert_eq!(
            refused(&seconds, &json!(3601)).await,
            (400, at_fault("OUT_OF_RANGE"))
        );
        assert_eq!(
            refused(&seconds, &json!(-1)).await,
            (400, at_fault("OUT_OF_RANGE"))
        );
        for not_whole in [json!(120.5), json!("120"), json!(true)] {
            assert_eq!(
                refused(&seconds, &not_whole).await,
                (400, at_fault("INVALID_VALUE")),
                "{not_whole} is not a whole number"
            );
        }
        for not_a_switch in [json!("true"), json!(1)] {
            assert_eq!(
                refused(&print_on_settle(), &not_a_switch).await,
                (400, at_fault("INVALID_VALUE")),
                "{not_a_switch} is not true or false"
            );
        }
    }

    #[test]
    fn a_choice_lists_its_values_and_no_bounds() {
        let shift = register()
            .into_iter()
            .find(|setting| setting.key() == "shift.no_shift_selling")
            .expect("the register has the shift setting");
        let shown = entry(&shift);
        assert_eq!(shown["kind"], "SETTING_KIND_CHOICE");
        assert_eq!(
            shown["values"],
            json!(["NO_SHIFT_SELLING_ALLOW", "NO_SHIFT_SELLING_REFUSE"])
        );
        assert_eq!(shown["default"], "NO_SHIFT_SELLING_ALLOW");
        assert_eq!(shown["preset"], "NO_SHIFT_SELLING_REFUSE");
        for absent in ["min", "max", "unit"] {
            assert!(
                shown.get(absent).is_none(),
                "a choice carries no `{absent}`"
            );
        }
    }

    #[test]
    fn a_whole_number_carries_its_bounds_and_unit_and_numbers_for_its_default_and_preset() {
        assert_eq!(
            entry(&wait_seconds()),
            json!({
                "setting_key": "example.wait_seconds",
                "node": "example",
                "field": "wait_seconds",
                "kind": "SETTING_KIND_INT",
                "min": 0,
                "max": 3600,
                "unit": "SETTING_UNIT_SECONDS",
                "default": 0,
                "preset": 120,
                "scopes": ["SETTING_SCOPE_STORE"],
                "since": "0.15.0",
            })
        );
    }

    #[test]
    fn a_switch_carries_booleans_and_nothing_to_choose_from() {
        assert_eq!(
            entry(&print_on_settle()),
            json!({
                "setting_key": "example.print_on_settle",
                "node": "example",
                "field": "print_on_settle",
                "kind": "SETTING_KIND_BOOL",
                "default": true,
                "scopes": ["SETTING_SCOPE_TENANT", "SETTING_SCOPE_STORE"],
                "since": "0.15.0",
            })
        );
    }
}
