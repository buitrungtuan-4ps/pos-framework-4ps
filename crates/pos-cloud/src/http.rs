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
//! `activation_refused`, [`too_many_login_attempts`], `admin_service_unavailable`) and
//! [`error_response`]. Remaining: the inline validation refusals written at each handler — also the
//! ones that will carry field-level `details`, which is why they are their own slice.
//!
//! The router is generic over its collaborators — the [`EventStore`], the [`RollupStore`], the
//! [`ApiKeyStore`], the [`AdminStore`], the [`ConfigTreeStore`], the [`WebhookEndpointStore`], and the
//! [`ClockSource`] — bundled in [`CloudApp`]. Tests drive it against `pos-fakes` and the binary serves
//! it over `store-postgres` with the identical handler code (ADR-0026).

use core::fmt;
use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::HeaderName;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, REFERRER_POLICY, RETRY_AFTER, SET_COOKIE, STRICT_TRANSPORT_SECURITY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::envelope::{EventEnvelope, RawPayload};

use crate::audit::{AuditRecorder, NoopAuditRecorder};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::apikey::{ApiKeyAdminStore, ApiKeyId, ApiKeyStore, Scope};
use crate::auth::enrol::{TOTP_SECRET_BYTES, constant_time_eq};
use crate::auth::password::hash_password;
use crate::auth::rate_limit::SlidingRateLimiter;
use crate::cloud::Cloud;
use crate::config::InternalSecret;
use crate::config_tree::ConfigTreeStore;
use crate::config_tree::prerequisites_for;
use crate::dashboard::RollupStore;
use crate::openapi::ApiDoc;

pub(crate) mod admin;
mod admitted_devices;
pub(crate) mod alerts;
mod archives;
mod authored_layer;
pub(crate) mod campaigns;
mod chain;
mod cloud_routes;
pub(crate) mod config_nodes;
pub(crate) mod config_publish;
pub(crate) mod countries;
pub(crate) mod fleet;
pub(crate) mod health;
pub(crate) mod integrations;
mod lease_bump;
mod locale_node;
mod ota;
pub(crate) mod ota_rollout;
mod reason_codes;
pub(crate) mod reconcile;
pub(crate) mod registry;
mod release_schedule;
mod settings;
mod settings_confirm;
mod store_groups;
pub(crate) mod subjects;
mod sync;

// The console's OpenAPI document (`crate::openapi_admin`) names each documented handler by its
// path here: the handler itself where this module calls it, and the `__path_*` item utoipa derives
// beside it, re-exported `pub(crate)` from the area that now holds the handler.

pub use admin::audit_router;
pub(crate) use admin::{
    __path_admin_generate_recovery_codes, __path_admin_login, __path_admin_logout,
    __path_admin_recovery_codes_status, __path_admin_reenrol_totp, __path_admin_session,
    __path_admin_setup, __path_admin_whoami, admin_generate_recovery_codes, admin_login,
    admin_logout, admin_recovery_codes_status, admin_reenrol_totp, admin_session, admin_setup,
    admin_whoami,
};
use admin::{
    admin_accept_invite, admin_create_api_key, admin_invite_admin, admin_list_admins,
    admin_list_api_keys, admin_list_invites, admin_list_sessions, admin_revoke_api_key,
    admin_revoke_invite, admin_revoke_other_sessions, admin_revoke_session, admin_set_admin_role,
    admin_set_admin_status,
};
pub(crate) use admitted_devices::__path_admin_admitted_devices;
use admitted_devices::admin_admitted_devices;
pub use alerts::alerts_router;
pub(crate) use archives::__path_admin_list_store_archives;
pub use archives::{store_archive_admin_router, store_archive_router};
use campaigns::scheduled_error_response;
pub use campaigns::{
    campaign_router, config_campaigns_router, scheduled_publish_router, voucher_router,
};
pub use catalog::{catalog_publish_router, catalog_router};
pub(crate) use chain::__path_admin_chain_findings;
pub use chain::chain_router;
pub use config_nodes::{
    config_channels_router, config_locale_router, config_store_profile_router, config_tax_router,
};
pub(crate) use config_publish::{
    __path_admin_cancel_release, __path_admin_config_nodes, __path_admin_create_release,
    __path_admin_list_releases, __path_admin_read_release, __path_admin_schedule_release,
};
use config_publish::{
    LoadedTree, ReleaseNodeRequest, ReleaseState, ScheduleReleaseRequest, admin_config_nodes,
    admin_config_rollback, admin_config_version_effective, admin_config_versions, if_match_config,
    load_tree_for_write, preview_config_node, publish_config_nodes, publish_nodes,
    release_error_response, release_report_response, release_target_stores,
    release_target_timezones, staff_redacted_for, unresolvable_moment,
};
use config_publish::{admin_config_effective, admin_config_publish};
pub use config_publish::{config_preview_router, config_release_router};
pub(crate) use config_publish::{publish_config_nodes_with, strip_tree_version};
pub use countries::country_router;
pub(crate) use devices::{
    __path_admin_revoke_device, __path_admin_set_printer_paper, __path_admin_set_terminal_float,
    __path_admin_set_terminal_receipt,
};
pub use devices::{
    activation_router, claim_router, device_publish_router, device_revoke_router, device_router,
};
pub use fees::fees_router;
pub(crate) use fees::{
    __path_admin_create_fee, __path_admin_delete_fee, __path_admin_effective_fees,
    __path_admin_list_fees, __path_admin_preview_fees, __path_admin_publish_fees,
    __path_admin_put_fee,
};
pub(crate) use fleet::__path_admin_acknowledge_region;
pub use fleet::fleet_router;
pub use floor::{floor_publish_router, floor_router, table_qr_router};
pub use health::health_router;
pub(crate) use integrations::{
    __path_admin_create_connection, __path_admin_delete_connection, __path_admin_get_connection,
    __path_admin_list_connections, __path_admin_list_providers, __path_admin_publish_integrations,
    __path_admin_update_connection,
};
use integrations::{
    admin_delete_webhook, admin_enable_webhook, admin_list_webhooks, admin_register_webhook,
};
pub use integrations::{config_integrations_router, connection_router, provider_catalogue_router};
pub use inventory::{config_inventory_router, inventory_router};
pub use media::media_router;
pub use ota::{
    ota_artifact_router, ota_release_admin_router, ota_release_fetch_router, ota_report_router,
};
pub use ota_rollout::ota_config_router;
pub use people::{
    capabilities_router, config_capabilities_router, people_publish_router, people_router,
    permissions_readiness_router,
};
pub(crate) use reason_codes::{
    __path_admin_create_reason_code, __path_admin_delete_reason_code, __path_admin_get_reason_code,
    __path_admin_list_reason_codes, __path_admin_publish_reason_codes,
    __path_admin_update_reason_code,
};
pub use reason_codes::{config_reason_codes_router, reason_code_router};
pub use reconcile::reconcile_router;
pub use registry::registry_router;
use setting_refusal::save_if_confirmed;
pub use sync::{MAX_PUBLISHED_EVENTS, MAX_PUBLISHED_EVENTS_BYTES};
use sync::{edge_config_sync, edge_events_hello, edge_heartbeat, edge_publish_events};
// What the people, fee, settings, store-group and release routes refuse a registry failure with.
use pos_cloud_config::route::registry_error_response;
pub use settings::settings_router;
pub(crate) use settings::{
    __path_admin_apply_setting_presets, __path_admin_clear_setting,
    __path_admin_effective_settings, __path_admin_list_settings, __path_admin_publish_settings,
    __path_admin_put_setting, __path_admin_settings_catalogue,
};
use store_groups::unknown_batch_node;
pub(crate) use store_groups::{
    __path_admin_create_store_group, __path_admin_list_store_group_batches,
    __path_admin_list_store_groups, __path_admin_publish_to_store_group,
    __path_admin_read_store_group_batch, __path_admin_set_store_group_members,
    __path_admin_update_store_group,
};
pub use store_groups::{BatchNode, BatchNodePlan, batch_nodes, store_group_router};
pub use subjects::subjects_router;
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
pub(crate) use pos_cloud_api::console::{audit_action, mint_ulid};
pub(crate) use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
// The refusals, paging and conditional-write helpers every area's routes share live in
// pos-cloud-api too; imported here, so `crate::http::not_found` and the rest still name them.
use pos_cloud_api::conditional::{
    EntityTagRefusal, already_exists, if_match, parse_entity_tag, version_mismatch,
    versioned_created, versioned_ok,
};
use pos_cloud_api::console::random_hex_32;
// What the configuration routes share, and the settings refusals, live in pos-cloud-config;
// imported here, so `crate::http::read_store_node_as_run` and the rest still name them.
use pos_cloud_api::paged::{
    offset_without_limit_refusal, page_bound_refusal, paged_ok, parse_page,
};
use pos_cloud_api::refusal::{
    FieldRefusal, accepted_tokens, enum_refusal, not_found, parse_optional_ulid, present_param,
    unprocessable_violations,
};
use pos_cloud_config::route::{
    PublishedConfig, RegistryTenantQuery, config_store_error_response, entity_status_refusal,
    mint_version_id, no_published_configuration, parse_config_level, parse_entity_status,
    read_store_node_as_run,
};
use pos_cloud_config::setting_refusal;
// The inventory routes live in pos-cloud-inventory; imported under the module's old name, so
// `crate::http::inventory::…` still names them.
use pos_cloud_inventory::routes as inventory;
// The device routes live in pos-cloud-devices; imported under the module's old name, so
// `crate::http::devices::…` still names them. The admitted-device roster stays here.
use pos_cloud_devices::routes as devices;
// Moved to pos-cloud-catalog (ADR-0170 decision 4, ADR-0175); imported under their old names, so
// `crate::http::catalog::…` and the routers keep their paths.
use pos_cloud_catalog::routes::{catalog, fees, media, tax_rates, translations};
// Moved to pos-cloud-floor; imported under its old name, so `crate::http::floor::…` and the
// routers keep their paths.
use pos_cloud_devices::routes::CLAIM_PATH;
use pos_cloud_floor::routes as floor;
// Moved to pos-cloud-people; imported under its old name, so `crate::http::people::…` and the
// routers keep their paths.
use pos_cloud_people::routes as people;
use pos_cloud_reports::xz::admin_xz_report;

use crate::registry::RegistryStore;
// These are ADR-0125 config releases — a named set of config publishes going out to a set of shops.
// They import under their own plain spelling, which they could not do while `crate::ota` also
// exported a `ReleaseStore`; that one is `ArtifactStore` now, so the alias this import used to carry
// is gone and the reader no longer has to hold two meanings of one word at once.
use crate::store_groups::StoreGroupStore;
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

/// Builds the cloud router over `app`: every area's routes ([`cloud_routes`]), then the state, then
/// the security headers.
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
    cloud_routes::all_routes::<S, R, K, C, A, T, W>()
        .with_state(app)
        // The admin-console security headers ([ADR-0067] slice 5) on every response this router
        // serves — which is *only* this router. `main.rs` applies the same layer to the
        // fully-composed service, which is what actually reaches the SPA fallback and the merged
        // `/admin` sub-routers; this line covered neither, and until production-readiness **S3** the
        // comment here claimed otherwise while `main.rs` carried no such layer. Both are kept: the
        // headers are `insert`ed, so applying the middleware twice sets the same values once.
        .layer(axum::middleware::from_fn(security_headers))
}

/// Liveness: answers as soon as the process is serving.
async fn health() -> &'static str {
    "ok"
}

/// The range a store's `event_log_days` may take (ADR-0145): the `retention` node's own, which the
/// edge reads the node through and ignores a value outside of ([`pos_proto::retention`]).
pub use pos_proto::retention::EVENT_LOG_DAYS;

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
use pos_cloud_config::publish::{check_prerequisites, only_settings};

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

    use super::{accepted_tokens, entity_status_refusal, parse_config_level, parse_entity_status};
    use crate::auth::admin::{AdminRole, AdminStatus};
    use crate::config_tree::ConfigLevel;
    use crate::registry::EntityStatus;
    use axum::http::StatusCode;
    use pos_cloud_api::refusal::accepted_list;
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
        PRICED_CONFIG_NODES, without_prices, without_staff_credentials, without_staff_identities,
    };
    use super::staff_redacted_for;
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
