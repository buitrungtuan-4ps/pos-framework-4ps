// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's own route table, one sub-router per area, which [`super::router`] merges and serves.
//!
//! Each area's routes are registered exactly as they were in one flat table; axum matches a request
//! by its path and method whichever sub-router registered it, so splitting the table changes no
//! route, no precedence and no layer.

use super::{
    AdminStore, ApiKeyAdminStore, ApiKeyStore, ClockSource, CloudApp, ConfigTreeStore, EventStore,
    MAX_PUBLISHED_EVENTS_BYTES, PublishedTillNames, RollupStore, Router, WebhookEndpointStore,
    admin_accept_invite, admin_admitted_devices, admin_config_effective, admin_config_nodes,
    admin_config_publish, admin_config_rollback, admin_config_version_effective,
    admin_config_versions, admin_create_api_key, admin_daily_rollups, admin_delete_webhook,
    admin_enable_webhook, admin_export_revenue, admin_export_revenue_fees,
    admin_export_revenue_tax, admin_export_rollups, admin_generate_recovery_codes,
    admin_invite_admin, admin_list_admins, admin_list_api_keys, admin_list_invites,
    admin_list_sessions, admin_list_webhooks, admin_login, admin_logout,
    admin_recovery_codes_status, admin_reenrol_totp, admin_register_webhook, admin_revenue_daily,
    admin_revoke_api_key, admin_revoke_invite, admin_revoke_other_sessions, admin_revoke_session,
    admin_rollups_reset, admin_session, admin_set_admin_role, admin_set_admin_status, admin_setup,
    admin_whoami, admin_xz_report, daily_rollups, delete, edge_config_sync, edge_events_hello,
    edge_heartbeat, edge_publish_events, get, health, ingest, openapi, post,
};
use crate::openapi_admin::admin_openapi;

/// Every route [`super::router`] serves, before its state and its security-headers layer.
pub(super) fn all_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
        .merge(public_routes())
        .merge(sync_routes())
        .merge(session_routes())
        .merge(access_routes())
        .merge(store_config_routes())
        .merge(store_report_routes())
        .merge(webhook_routes())
}

/// The public and machine-to-machine surface: liveness, ingest, the keyed `/v1` rollups, and the two OpenAPI documents.
fn public_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
}

/// The `/sync` surface an edge dials with its store's key: config pull, heartbeat, and the event rail.
fn sync_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
}

/// An administrator's own session: signing in and out, their sessions, their second factor, and first-run setup.
fn session_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
}

/// Who can reach the cloud: API keys, administrators and their invitations.
fn access_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
}

/// A store's configuration: the effective document, its nodes and versions, rollback, and publishing a level.
fn store_config_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
}

/// A store's figures, read from its materialised rollups: the daily rollups, the admitted
/// devices, revenue, the X/Z report, and the exports.
fn store_report_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
}

/// The webhook endpoints a tenant registers, lists, deletes and re-enables.
fn webhook_routes<S, R, K, C, A, T, W>() -> Router<CloudApp<S, R, K, C, A, T, W>>
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
}
