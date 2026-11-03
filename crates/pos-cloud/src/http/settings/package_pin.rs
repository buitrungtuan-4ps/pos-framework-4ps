// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `package` settings' own rules on a write
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 2),
//! and `PUT /admin/stores/{store_id}/package/choice`, the one way a store's choice is written.
//!
//! Who chooses: the ADR names `admin.package.choose`, a store permission. Until a store's own
//! people can sign in to the console, which needs its own ADR, the route is gated by
//! `console.config.publish`, so HQ chooses on a store's behalf.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_cloud_config::package_pin::{self, ChoiceRefusal, SettingsPackages, may_choose};
use pos_cloud_config::packages::PackageStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{PackageVersionId, StoreId, TenantId};
use pos_proto::settings::SettingScope;
use pos_proto::wire_enum::Open;
use serde::Deserialize;

use super::publish::{TenantLayout, publish_settings_to};
use super::{SettingsState, publish_results_response, settings_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::setting_refusal::setting_refusal_response;
use crate::http::{Json, not_found};
use crate::registry::RegistryStore;
use crate::settings::{SettingRefusal, SettingValue, SettingsStore, StorePlacement};
use crate::store_groups::StoreGroupStore;

/// The refusal a write of a `package` setting earns beyond the register, or `None` to write it:
///
/// - `package.chosen_version_id` is written only through the choose route;
/// - every identifier names a published version of the tenant's packages;
/// - no store the write reaches is locked at a broader scope.
///
/// `values` are the tenant's values before the write.
pub(super) async fn package_write_refusal<P: PackageStore>(
    packages: &P,
    tenant_id: TenantId,
    layout: &TenantLayout,
    values: &[SettingValue],
    (setting_key, scope, scope_id): (&str, SettingScope, &str),
    value: &serde_json::Value,
) -> Option<Response> {
    if !package_pin::is_package_key(setting_key) {
        return None;
    }
    if setting_key == package_pin::CHOSEN_KEY {
        return Some(setting_refusal_response(
            &SettingRefusal::WrittenThroughItsRoute(setting_key.to_owned()),
        ));
    }
    let ids: Vec<&str> = match value {
        serde_json::Value::String(id) if setting_key != package_pin::MODE_KEY => vec![id],
        serde_json::Value::Array(ids) => ids.iter().filter_map(serde_json::Value::as_str).collect(),
        _ => Vec::new(),
    };
    for id in ids {
        let Ok(version_id) = id.parse::<PackageVersionId>() else {
            continue;
        };
        match packages.get_version(tenant_id, version_id).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Some(setting_refusal_response(&SettingRefusal::UnknownVersion {
                    setting_key: setting_key.to_owned(),
                    package_version_id: id.to_owned(),
                }));
            }
            Err(error) => {
                tracing::error!(%error, "the package store could not be read for a settings write");
                return Some(api_error_with_details(
                    ErrorStatus::Unavailable,
                    "the package service is unavailable",
                    &[],
                ));
            }
        }
    }
    let locked = layout
        .reached_by(scope, scope_id)
        .into_iter()
        .find(|store_id| {
            layout
                .placement(*store_id)
                .is_some_and(|placement| package_pin::locked_above(values, &placement, scope))
        })?;
    Some(setting_refusal_response(&SettingRefusal::LockedAbove {
        setting_key: setting_key.to_owned(),
        store_id: locked.to_string(),
    }))
}

/// A `PUT /admin/stores/{store_id}/package/choice` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PackageChoiceRequest {
    tenant_id: String,
    /// The version the store runs from now, one of its allowed versions; `null` clears the choice,
    /// so the store runs its default version.
    #[serde(default)]
    package_version_id: Option<String>,
}

#[utoipa::path(
    put,
    path = "/admin/stores/{store_id}/package/choice",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    params(("store_id" = String, Path, description = "The store's 26-character ULID")),
    request_body(
        content = PackageChoiceRequest, description = "The tenant, and the version the store chooses, or null to clear its choice",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = pos_cloud_api::response::StoresPublished<super::SettingPublishResult>, description = "Written, and the store republished"),
        (status = 400, description = "An id is not a ULID, or the version is not one the store is allowed (NOT_ALLOWED)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The store's package is not in PACKAGE_MODE_CHOOSE (PACKAGE_NOT_CHOOSING)", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry, the store groups or the config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `PUT /admin/stores/{store_id}/package/choice`: the store's choice among the package versions it
/// is allowed (ADR-0189 decision 2), written at the store and republished.
pub(super) async fn admin_choose_package<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<PackageChoiceRequest>,
) -> Response
where
    St: SettingsStore + SettingsPackages + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let actor = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(actor) => actor,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let chosen = match request.package_version_id.as_deref() {
        Some(id) => match parse_ulid_fields([("package_version_id", id)]) {
            Ok([id]) => Some(id.to_string()),
            Err(refusal) => return refusal,
        },
        None => None,
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
    let store_scope_id = store_id.to_string();
    let before = values
        .iter()
        .find(|held| {
            held.setting_key == package_pin::CHOSEN_KEY
                && held.scope.known() == SettingScope::Store
                && held.scope_id == store_scope_id
        })
        .map(|held| held.value.clone());
    if let Err(refusal) =
        write_choice(&state, tenant_id, &values, &placement, chosen.as_deref()).await
    {
        return refusal;
    }
    let results = publish_settings_to(&state, tenant_id, &layout, &[store_id]).await;
    audit_action(
        &state.audit,
        &state.clock,
        &actor,
        Some(tenant_id),
        "package.choose",
        "store",
        &store_scope_id,
        before.map(|value| serde_json::json!({ "package_version_id": value })),
        Some(serde_json::json!({ "package_version_id": chosen })),
    )
    .await;
    publish_results_response(&results)
}

/// Writes the store's choice, or clears it for `None`, once the store's resolved settings allow it.
async fn write_choice<St, Rg, Grp, Cfg, A, C>(
    state: &SettingsState<St, Rg, Grp, Cfg, A, C>,
    tenant_id: TenantId,
    values: &[SettingValue],
    placement: &StorePlacement,
    chosen: Option<&str>,
) -> Result<(), Response>
where
    St: SettingsStore,
    C: ClockSource,
{
    let Some(id) = chosen else {
        return state
            .settings
            .delete(
                tenant_id,
                package_pin::CHOSEN_KEY,
                SettingScope::Store,
                &placement.store_id,
            )
            .await
            .map(|_| ())
            .map_err(|error| settings_store_error_response(&error));
    };
    let resolved = crate::settings::resolve_for_store(values, placement);
    match may_choose(&resolved, id) {
        None => {}
        Some(ChoiceRefusal::NotChoosing) => {
            return Err(api_error_with_details(
                ErrorStatus::FailedPrecondition,
                "the store's package is not in PACKAGE_MODE_CHOOSE",
                &[("package_version_id", "PACKAGE_NOT_CHOOSING")],
            ));
        }
        Some(ChoiceRefusal::NotAllowed) => {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "the version is not one the store is allowed",
                &[("package_version_id", "NOT_ALLOWED")],
            ));
        }
    }
    let value = SettingValue {
        setting_key: package_pin::CHOSEN_KEY.to_owned(),
        scope: Open::from_known(SettingScope::Store),
        scope_id: placement.store_id.clone(),
        value: serde_json::json!(id),
        update_time: state.clock.now(),
        pack_country_code: None,
    };
    state
        .settings
        .put(tenant_id, &value)
        .await
        .map_err(|error| settings_store_error_response(&error))
}
