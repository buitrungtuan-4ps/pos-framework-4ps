// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Settings (`/admin/settings`, ADR-0160 decision 3): the catalogue, a tenant's values at each
//! scope, the effective values at a store, presets, and their publish.
//!
//! The state, the router and the scope, audit and error helpers are here.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use catalogue::{admin_list_settings, admin_settings_catalogue};
use effective::admin_effective_settings;
use pos_proto::WireEnum;
use pos_proto::determinism::ClockSource;
use pos_proto::settings::SettingScope;
use presets::admin_apply_setting_presets;
use publish::{SettingPublishResult, admin_publish_settings};
use values::{admin_clear_setting, admin_put_setting};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::config_tree::ConfigTreeStore;
use crate::http::Json;
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;

// `pub(super)`: the unit tests in `http.rs` and `settings_confirm.rs` reach the catalogue entry.
pub(super) mod catalogue;
mod effective;
mod presets;
mod publish;
mod values;

// The handlers the console's OpenAPI document names.
pub(crate) use catalogue::{__path_admin_list_settings, __path_admin_settings_catalogue};
pub(crate) use effective::__path_admin_effective_settings;
pub(crate) use presets::__path_admin_apply_setting_presets;
// What the permissions readiness read uses.
pub(crate) use publish::__path_admin_publish_settings;
pub(super) use publish::TenantLayout;
// Moved to pos-cloud-config with the tenant layout; imported here, so the settings routes and
// their callers keep reaching them through `super` and `crate::http`.
use pos_cloud_config::route::setting_scope_refusal;
pub(in crate::http) use pos_cloud_config::route::settings_store_error_response;
pub(crate) use values::{__path_admin_clear_setting, __path_admin_put_setting};

// --- Settings (`/admin/settings`, ADR-0160 decision 3) -----------------------------------------

/// The collaborators the settings routes need: the values; the registry and the store groups that
/// say which stores a value reaches; the config trees the resolved values are written onto; and the
/// admin, clock and audit every route carries.
#[derive(Clone)]
struct SettingsState<St, Rg, Grp, Cfg, A, C> {
    settings: St,
    registry: Rg,
    groups: Grp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the settings sub-router
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 3).
///
/// A value is written once, at the tenant, a brand, a store group or one store, and every store it
/// reaches is republished in the same request: each one's resolved values are composed onto its
/// Tenant layer ([`crate::settings::tenant_layer_nodes`]), so they merge with the nodes the console
/// publishes on the Store layer. Reads are behind [`ConsolePermission::Read`](crate::auth::console_rbac::ConsolePermission::Read); a write publishes, so
/// it is behind [`ConsolePermission::PublishConfig`](crate::auth::console_rbac::ConsolePermission::PublishConfig), like every node publish.
pub fn settings_router<St, Rg, Grp, Cfg, A, C>(
    settings: St,
    registry: Rg,
    groups: Grp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/settings/catalogue",
            get(admin_settings_catalogue::<St, Rg, Grp, Cfg, A, C>),
        )
        .route(
            "/admin/settings",
            get(admin_list_settings::<St, Rg, Grp, Cfg, A, C>)
                .put(admin_put_setting::<St, Rg, Grp, Cfg, A, C>)
                .delete(admin_clear_setting::<St, Rg, Grp, Cfg, A, C>),
        )
        .route(
            "/admin/settings/effective",
            get(admin_effective_settings::<St, Rg, Grp, Cfg, A, C>),
        )
        .route(
            "/admin/settings/presets",
            post(admin_apply_setting_presets::<St, Rg, Grp, Cfg, A, C>),
        )
        .route(
            "/admin/settings/publish",
            post(admin_publish_settings::<St, Rg, Grp, Cfg, A, C>),
        )
        .with_state(SettingsState {
            settings,
            registry,
            groups,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// The `200` a write answers: every store it reached and how its publish went.
fn publish_results_response(results: &[SettingPublishResult]) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({ "stores": results })),
    )
        .into_response()
}

/// What the audit trail names a value by: its setting, its scope and the id it was written for.
fn setting_audit_id(setting_key: &str, scope: SettingScope, scope_id: &str) -> String {
    format!("{setting_key} {scope} {scope_id}")
}

/// Reads a scope token, refusing anything but the four scopes.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn parse_setting_scope(token: &str) -> Result<SettingScope, Response> {
    match SettingScope::from_wire(token.trim()) {
        Some(SettingScope::Unspecified) | None => Err(setting_scope_refusal()),
        Some(scope) => Ok(scope),
    }
}
