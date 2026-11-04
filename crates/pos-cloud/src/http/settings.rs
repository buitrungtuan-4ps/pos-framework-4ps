// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Settings (`/admin/settings`, ADR-0160 decision 3): the catalogue, a tenant's values at each
//! scope, the effective values at a store, presets, and their publish.
//!
//! The state, the router and the scope, audit and error helpers are here.

use core::future::Future;
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use catalogue::{admin_list_settings, admin_settings_catalogue};
use effective::admin_effective_settings;
use pos_country::CountryRegistry;
use pos_proto::WireEnum;
use pos_proto::determinism::ClockSource;
use pos_proto::settings::SettingScope;
use presets::{CountrySettingValues, admin_apply_setting_presets, country_setting_values};
use publish::{SettingPublishResult, admin_publish_settings};
use values::{admin_clear_setting, admin_put_setting};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::config_tree::ConfigTreeStore;
use crate::http::Json;
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;
use pos_cloud_api::response::StoresPublished;
use pos_cloud_config::package_pin::SettingsPackages;

// `pub(super)`: the unit tests in `http.rs` and `settings_confirm.rs` reach the catalogue entry.
pub(super) mod catalogue;
mod catch_up;
mod effective;
mod package_pin;
mod package_status;
mod presets;
mod publish;
mod values;

// The handlers the console's OpenAPI document names.
pub(crate) use catalogue::{__path_admin_list_settings, __path_admin_settings_catalogue};
pub use catch_up::{CATCH_UP_QUEUE, PackageCatchUp};
pub(crate) use effective::__path_admin_effective_settings;
pub(crate) use package_pin::__path_admin_choose_package;
pub(crate) use package_status::__path_admin_store_package;
pub(crate) use presets::__path_admin_apply_setting_presets;
pub(crate) use presets::as_node_value;
pub(crate) use publish::__path_admin_publish_settings;
// Moved to pos-cloud-config with the tenant layout; imported here, so the settings routes and
// their callers keep reaching them through `super` and `crate::http`.
use pos_cloud_config::route::setting_scope_refusal;
pub(in crate::http) use pos_cloud_config::route::settings_store_error_response;
pub(crate) use values::{__path_admin_clear_setting, __path_admin_put_setting};

// --- Settings (`/admin/settings`, ADR-0160 decision 3) -----------------------------------------

/// The collaborators the settings routes need: the values; the registry and the store groups that
/// say which stores a value reaches; the config trees the resolved values are written onto; each
/// compiled-in country's values for a new store; and the admin, clock and audit every route carries.
#[derive(Clone)]
struct SettingsState<St, Rg, Grp, Cfg, A, C> {
    settings: St,
    registry: Rg,
    groups: Grp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    /// What each country pack gives a new store, computed once from the compiled registry at
    /// start-up (ADR-0183): the registry does not change while the process runs.
    country_values: Arc<CountrySettingValues>,
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
///
/// `countries` is the compiled registry: a new store's values take its country pack's setting
/// defaults before the register's presets (ADR-0183).
pub fn settings_router<St, Rg, Grp, Cfg, A, C>(
    countries: &CountryRegistry,
    settings: St,
    registry: Rg,
    groups: Grp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    St: SettingsStore + SettingsPackages + Clone + Send + Sync + 'static,
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
        // The store's choice among the package versions it is allowed (ADR-0189 decision 2): the
        // one way `package.chosen_version_id` is written.
        .route(
            "/admin/stores/{store_id}/package",
            get(package_status::admin_store_package::<St, Rg, Grp, Cfg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}/package/choice",
            axum::routing::put(package_pin::admin_choose_package::<St, Rg, Grp, Cfg, A, C>),
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
            country_values: Arc::new(country_setting_values(countries)),
        })
}

/// The hand-off the OTA report route gives each reporting store, and the worker that catches a
/// waiting store's package up once its release meets the pinned version
/// ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 5).
/// The caller spawns the worker; it ends when every hand-off is dropped.
pub fn package_catch_up<St, Rg, Grp, Cfg, A, C>(
    countries: &CountryRegistry,
    settings: St,
    registry: Rg,
    groups: Grp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> (
    PackageCatchUp,
    impl Future<Output = ()> + Send + 'static + use<St, Rg, Grp, Cfg, A, C>,
)
where
    St: SettingsStore + SettingsPackages + Send + Sync + 'static,
    Rg: RegistryStore + Send + Sync + 'static,
    Grp: StoreGroupStore + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Send + Sync + 'static,
    A: Send + Sync + 'static,
    C: ClockSource + Send + Sync + 'static,
{
    let (hand_off, receiver) = PackageCatchUp::channel();
    let state = SettingsState {
        settings,
        registry,
        groups,
        config_trees,
        admin,
        clock,
        audit,
        country_values: Arc::new(country_setting_values(countries)),
    };
    (hand_off, catch_up::run(state, receiver))
}

/// The `200` a write answers: every store it reached and how its publish went.
fn publish_results_response(results: &[SettingPublishResult]) -> Response {
    (
        StatusCode::OK,
        Json(StoresPublished {
            stores: results.to_vec(),
        }),
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
