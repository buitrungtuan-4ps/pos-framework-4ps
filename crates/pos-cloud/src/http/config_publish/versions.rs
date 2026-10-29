// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store's config versions: the nodes and versions it has, one version's effective document,
//! and rollback (ADR-0069).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{ConfigTenantQuery, audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use serde::Deserialize;

use super::if_match::if_match_config;
use super::redaction::config_redacted_for;
use super::{load_tree_for_write, strip_tree_version};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::http::setting_refusal::save_if_confirmed;
use crate::http::{
    CloudApp, Json, PublishedConfig, config_store_error_response, mint_version_id,
    no_published_configuration, not_found,
};

// --- Config version history (`/admin/stores/{id}/config/versions` + `/config/rollback`, ADR-0069) --

/// One published config version as the console lists it: the version id, when it was published (from
/// the ULID's own timestamp, Unix ms), and whether it is the store's current version.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct ConfigVersionView {
    version_id: String,
    at_ms: i64,
    current: bool,
}

/// One node of a store's published configuration, as the console lists it (finding **F6**).
///
/// The console's question has always been per node — "is the menu this store is running the menu I
/// last edited?" — and until now the only answer available was per tree: one version id covering
/// thirteen nodes, so publishing a tax change made the menu look freshly published too. Every
/// version now records which nodes it wrote ([`PublishedVersion::nodes`]), and this is that read.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct ConfigNodeView {
    /// The node key — `locale`, `tax`, `menu`, `permissions`, and so on.
    node: String,
    /// The version that last published this node, or `null` when no recorded version names it.
    ///
    /// `null` is *not* "never published": the node is in the effective document, so it was. It means
    /// the version that put it there predates this field, or was a whole-layer replace that named
    /// nothing. The console says "published, when is not recorded" rather than inventing a date.
    #[schema(required = true)]
    version_id: Option<String>,
    /// When that version was published (from its ULID's own timestamp, Unix ms), or `null` with it.
    #[schema(required = true)]
    at_ms: Option<i64>,
}

/// The per-node publish state of one store's configuration.
///
/// `current_version_id` is the tree's newest version, which is what a store *should* be holding. The
/// version it *is* holding is on the fleet read (`GET /admin/fleet/stores/{id}`), which every screen
/// that asks this question has already loaded — so it is not repeated here, and the comparison
/// happens where both numbers already are.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct ConfigNodesView {
    #[schema(required = true)]
    current_version_id: Option<String>,
    nodes: Vec<ConfigNodeView>,
}

/// A super-admin rolls a store's config back to a past version.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(in crate::http) struct RollbackConfigRequest {
    /// The version to restore (a ULID); its effective document becomes the new current version.
    version_id: String,
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/config/nodes",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("store_id" = String, Path, description = "The store whose published nodes to list (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant the store belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = ConfigNodesView, description = "The tree's current version id and, per node of the effective \
                                      document, the version that last published it and when. A \
                                      node whose version is null is published but by a version \
                                      that does not record which nodes it wrote"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "This store has never published a configuration", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config",
)]
/// `GET /admin/stores/{store_id}/config/nodes` — which node was published when (finding **F6**).
///
/// Read-only and `console.data.read`, the same as the version history beside it. The effective
/// document is not returned, only its node **keys**, so this route carries no configuration values
/// at all and needs none of [`strip_tree_version`]'s redaction: a caller learns that a store has a
/// `permissions` node and when it was published, never what is in it.
pub(in crate::http) async fn admin_config_nodes<S, R, K, C, A, T, W>(
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
            let nodes = tree
                .published_nodes()
                .into_iter()
                .map(|(node, version)| ConfigNodeView {
                    node,
                    version_id: version.map(|id| id.to_string()),
                    at_ms: version
                        .map(|id| i64::try_from(id.as_ulid().timestamp_ms()).unwrap_or(i64::MAX)),
                })
                .collect();
            (
                StatusCode::OK,
                Json(ConfigNodesView {
                    current_version_id: tree.current_version().map(|id| id.to_string()),
                    nodes,
                }),
            )
                .into_response()
        }
        Ok(None) => no_published_configuration(),
        Err(error) => config_store_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/config/versions",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = [ConfigVersionView], description = "Every configuration version the store has had, newest first, \
                                      as an array: `version_id`, `at_ms` (read from the id) and \
                                      `current`. The history only grows: a rollback adds a version"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "This store has never published a configuration", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config",
)]
/// Lists a store's published config versions newest-first (super-admin only). The version history is
/// the config tree's own append-only log; the `at` of each is read from its ULID id, so no separate
/// timestamp column is needed. `404` if the store has no tree yet.
pub(in crate::http) async fn admin_config_versions<S, R, K, C, A, T, W>(
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

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/config/versions/{version_id}",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("version_id" = String, Path, description = "The version (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = pos_cloud_api::body_schema::FreeFormJson, description = "That version's effective configuration, for comparing with \
                                      another. Redacted for the reader: never a staff PIN hash, a person's name or staff code only for a role \
                                      holding console.people.read, and prices only for a role \
                                      holding console.reports.revenue: a past version is no less sensitive"),
        (status = 400, description = "tenant_id, store_id or version_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The store has never published a configuration, or has no \
                                      such version", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config",
)]
/// The effective (composed, validated) document of one past config version (super-admin only), for
/// the console's diff view. `404` if the store or the named version is unknown.
pub(in crate::http) async fn admin_config_version_effective<S, R, K, C, A, T, W>(
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
                // The diff view reads a past version, and a past version holds past PIN hashes (S7),
                // past staff names (ADR-0158) and past prices (S8) — none is less sensitive for
                // being last month's.
                Some(effective) => {
                    let document = config_redacted_for(effective, context.admin.role);
                    (StatusCode::OK, Json(document)).into_response()
                }
                None => not_found("config version"),
            }
        }
        Ok(None) => no_published_configuration(),
        Err(error) => config_store_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/stores/{store_id}/config/rollback",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The configuration version the rollback was \
                                                     decided against (ADR-0095). Required"),
    ),
    request_body(
        content = RollbackConfigRequest, description = "`version_id`, the past version to restore",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = PublishedConfig, description = "Restored as a new current version: the history is never \
                                      rewritten. The body carries `config_version_id`, which the \
                                      store pulls on its next sync. Audited with both versions"),
        (status = 400, description = "An id is absent or not a ULID; If-Match is absent or \
                                      malformed; or the restored version runs a value that needs a \
                                      recorded confirmation first (ADR-0169)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The store has never published a configuration, or has no \
                                      such version", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The store's configuration changed since you read it, or \
                                      If-Match is `*` but the store has been published to: re-read \
                                      it and try again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store or the entropy source is unavailable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config",
)]
/// Rolls a store's config back to a past version (super-admin only). Append-only: the chosen version's
/// effective document is re-published as a *new* current version, so nothing in the history is altered
/// or removed and the store pulls the restored config on its next sync. The action is audited as
/// `config.rollback` ([ADR-0069](../../../docs/adr/0069-audit-trail.md)). `404` if the store or the
/// named version is unknown.
pub(in crate::http) async fn admin_config_rollback<S, R, K, C, A, T, W>(
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
    // A version from before QR ordering had one switch comes back with the switch it ran, and
    // `qr.enabled` and the channel list agreeing with it (ADR-0160 decision 5).
    let from_before_the_switch = crate::qr_ordering::predates(version_id, app.qr_switch_since);
    let before = tree.effective();
    let Some(new_id) = tree.restore_with(version_id, new_version_id, |document| {
        if from_before_the_switch {
            crate::qr_ordering::settle(document);
        }
    }) else {
        return not_found("config version");
    };
    // A version that ran a value needing a recorded confirmation, which the store no longer runs,
    // comes back only once it is confirmed on Settings (ADR-0169 decision 3): `save_if_confirmed`.
    let saved = (tenant_id, store_id, loaded.version.as_ref());
    if let Err(refusal) = save_if_confirmed(&app.config_trees, saved, &before, &tree).await {
        return refusal;
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
