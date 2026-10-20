// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/stores/{store_id}/config/{level}` and `/admin/stores/{store_id}/config`: author one
//! level of a store's config tree and publish it, and read the effective document.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{ConfigTenantQuery, audit_action, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::{load_tree_for_write, strip_tree_version};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{CapabilityValidator, ConfigLevel, ConfigTree, ConfigTreeStore};
use crate::http::authored_layer::publish_authored_layer;
use crate::http::config_publish::config_redacted_for;
use crate::http::{
    CloudApp, Json, PublishedConfig, config_store_error_response, enum_refusal,
    no_published_configuration, parse_config_level,
};

#[utoipa::path(
    put,
    path = "/admin/stores/{store_id}/config/{level}",
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("level" = String, Path, description = "The layer to replace: `tenant`, `brand`, `store` or \
                                              `device`"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The configuration version the write was \
                                                     made against, or `*` for a store never \
                                                     published to (ADR-0095). Required"),
    ),
    request_body(
        description = "The whole document for that layer, as JSON. It replaces the layer, every key \
                       of it, and the four layers are composed and validated into the store's new \
                       configuration (ADR-0033)",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published. The body carries `config_version_id`, the new \
                                      version, which the store pulls on its next sync. Audited with \
                                      the level and the version, never the document"),
        (status = 400, description = "tenant_id or store_id is not a ULID; level is not one of the \
                                      four; If-Match is absent or malformed; or a value needs a \
                                      recorded confirmation first (ADR-0169)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The store's configuration changed since you read it, or \
                                      If-Match is `*` but the store has been published to: re-read \
                                      it and try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The composed configuration does not validate, listing each \
                                      violation. Nothing is stored and the last good version stays \
                                      current", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store or the entropy source is unavailable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config",
)]
/// Authors one level of a store's config tree and publishes the composed version (super-admin only).
///
/// Loads the store's tree for the query's tenant, replaces the named `level`'s document with the
/// request body, and publishes — composing, validating, and (only if valid) appending a new version,
/// which is then persisted. A rejected version is a `422` carrying the violations; nothing is stored
/// and the last good version stays current ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
pub(in crate::http) async fn admin_config_publish<S, R, K, C, A, T, W>(
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

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/config",
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's current effective configuration: its four layers \
                                      composed. Redacted for the reader: never a staff PIN hash, a person's name or staff code only for a role \
                                      holding console.people.read, and prices only for a role \
                                      holding console.reports.revenue"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "This store has never published a configuration", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config",
)]
/// The current effective (composed, validated) config document for a store (super-admin only), or
/// `404` if nothing has been published to it yet.
pub(in crate::http) async fn admin_config_effective<S, R, K, C, A, T, W>(
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
                // — see `config_redacted_for`.
                Some(effective) => {
                    let document = config_redacted_for(effective, context.admin.role);
                    (StatusCode::OK, Json(document)).into_response()
                }
                None => no_published_configuration(),
            }
        }
        Ok(None) => no_published_configuration(),
        Err(error) => config_store_error_response(&error),
    }
}
