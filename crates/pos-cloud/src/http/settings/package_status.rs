// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/stores/{store_id}/package`: which package version a store is pinned to, which one
//! its document carries, and, when they differ, why it waits
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 5).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_cloud_config::package_pin::{
    ContentChange, Pin, SettingsPackages, StoreReleases as _, WaitReason, pinned_version,
};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::package::PublishedPackage;

use super::publish::{TenantLayout, content_change};
use super::{SettingsState, settings_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{Json, RegistryTenantQuery, not_found};
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;

/// A store's package, as the console shows it.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(super) struct StorePackage {
    /// The version the store's pin names, or absent when it is not pinned or names nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pinned_version_id: Option<String>,
    /// The version the store's published document carries, or absent when it runs the default
    /// package.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    running_version_id: Option<String>,
    /// Why the store does not run the version it is pinned to, or absent when it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    waiting: Option<Waiting>,
}

/// Why a store keeps the version it runs.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(super) struct Waiting {
    /// `EDGE_TOO_OLD`, `RELEASE_UNKNOWN`, `VERSION_MISSING`, `PIN_UNRESOLVED` or `UNREADABLE`.
    reason: WaitReason,
    /// The release the store last reported, or absent if it never has.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    installed_release: Option<String>,
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/package",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("store_id" = String, Path, description = "The store's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = StorePackage, description = "The version the store is pinned to, the version it runs, and why it waits when they differ: \
                                      its release is older than the version needs or unknown, the version is missing, or the pin names none"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry, the store groups or the config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `GET /admin/stores/{store_id}/package` — a store's pinned, running and waiting package version.
pub(super) async fn admin_store_package<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    St: SettingsStore + SettingsPackages + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let layout = match TenantLayout::load(&state.registry, &state.groups, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let Some(placement) = layout.placement(store_id) else {
        return not_found("store");
    };
    let values = match state.settings.list(tenant_id).await {
        Ok(values) => values,
        Err(error) => return settings_store_error_response(&error),
    };
    let resolved = crate::settings::resolve_for_store(&values, &placement);
    let pinned_version_id = match pinned_version(&resolved) {
        Pin::Version(id) => Some(id.to_string()),
        Pin::Unpinned | Pin::Unresolved => None,
    };
    let waiting = match content_change(&state.settings, tenant_id, store_id, &resolved).await {
        ContentChange::Keep(reason) => Some(Waiting {
            reason,
            installed_release: state
                .settings
                .releases()
                .installed_release(tenant_id, store_id)
                .await
                .ok()
                .flatten(),
        }),
        ContentChange::Set(_) | ContentChange::Remove => None,
    };
    let running_version_id = running_version(&state.config_trees, tenant_id, store_id).await;
    (
        StatusCode::OK,
        Json(StorePackage {
            pinned_version_id,
            running_version_id,
            waiting,
        }),
    )
        .into_response()
}

/// The version the store's published document carries on its Tenant layer, if any.
pub(super) async fn running_version<Cfg: ConfigTreeStore>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Option<String> {
    let tree = config_trees.load(tenant_id, store_id).await.ok()??;
    let node = tree
        .record
        .layer(ConfigLevel::Tenant)
        .get(PublishedPackage::NODE)?;
    serde_json::from_value::<PublishedPackage>(node.clone())
        .ok()?
        .content
        .map(|content| content.package_version_id.to_string())
}
