// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT` and `DELETE /admin/settings`: a setting's value written or cleared at one scope.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use pos_proto::wire_enum::Open;
use serde::Deserialize;

use super::publish::{TenantLayout, publish_settings_to};
use super::{
    SettingsState, parse_setting_scope, publish_results_response, setting_audit_id,
    settings_store_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::setting_refusal::setting_refusal_response;
use crate::http::{not_found, settings_confirm};
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;

/// The tenant a settings read is for.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SettingsTenantQuery {
    pub(super) tenant_id: String,
}

/// A `PUT /admin/settings` body: one value, at one scope.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PutSettingRequest {
    tenant_id: String,
    setting_key: String,
    /// `SETTING_SCOPE_TENANT`, `_BRAND`, `_STORE_GROUP` or `_STORE`.
    scope: String,
    /// The tenant, brand, store group or store the value is for.
    scope_id: String,
    value: serde_json::Value,
    /// Whether the person confirmed the notice of a value that needs it (ADR-0169 decision 2).
    #[serde(default)]
    notice_confirmed: bool,
}

#[utoipa::path(
    put,
    path = "/admin/settings",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    request_body(
        content = PutSettingRequest, description = "The tenant, the setting's key, the scope (SETTING_SCOPE_TENANT, _BRAND, \
                       _STORE_GROUP or _STORE), the id it is for, the value, and \
                       notice_confirmed (default false), true when the person confirmed the \
                       notice of a value its catalogue entry's confirm names",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Written, and every store it reaches republished. The body \
                                      lists each store with SETTING_PUBLISH_APPLIED and the new \
                                      config version, SETTING_PUBLISH_UNCHANGED, or \
                                      SETTING_PUBLISH_FAILED"),
        (status = 400, description = "An id is not a ULID, the scope is not a scope, the setting \
                                      is not in the register, the setting cannot be set at that \
                                      scope, the value is not one it takes, the value needs its \
                                      notice confirmed and notice_confirmed is not true, or a \
                                      tenant-scope value names another tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such brand, store group or store", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The store group is archived, so its values reach no store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry, the store groups or the \
                                      config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `PUT /admin/settings` — writes one value and republishes every store it reaches.
pub(super) async fn admin_put_setting<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PutSettingRequest>,
) -> Response
where
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let scope = match parse_setting_scope(&request.scope) {
        Ok(scope) => scope,
        Err(refusal) => return refusal,
    };
    let notice = match crate::settings::validate(&request.setting_key, scope, &request.value)
        .and_then(|setting| {
            crate::settings::confirmation(&setting, &request.value, request.notice_confirmed)
        }) {
        Ok(notice) => notice,
        Err(refusal) => return setting_refusal_response(&refusal),
    };
    let layout = match TenantLayout::load(&state.registry, &state.groups, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let scope_id = match layout.scope_target(scope, &request.scope_id) {
        Ok(scope_id) => scope_id,
        Err(refusal) => return refusal,
    };
    let before = match state.settings.list(tenant_id).await {
        Ok(values) => values
            .into_iter()
            .find(|held| {
                held.setting_key == request.setting_key
                    && held.scope.known() == scope
                    && held.scope_id == scope_id
            })
            .map(|held| held.value),
        Err(error) => return settings_store_error_response(&error),
    };
    let value = crate::settings::SettingValue {
        setting_key: request.setting_key.clone(),
        scope: Open::from_known(scope),
        scope_id: scope_id.clone(),
        value: request.value.clone(),
        update_time: state.clock.now(),
    };
    if let Err(error) = state.settings.put(tenant_id, &value).await {
        return settings_store_error_response(&error);
    }
    let results = publish_settings_to(
        &state,
        tenant_id,
        &layout,
        &layout.reached_by(scope, &scope_id),
    )
    .await;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "setting.set",
        "setting",
        &setting_audit_id(&request.setting_key, scope, &scope_id),
        before.map(|value| serde_json::json!({ "value": value })),
        Some(settings_confirm::audit_after(&request.value, notice)),
    )
    .await;
    publish_results_response(&results)
}

/// The value a `DELETE /admin/settings` clears.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ClearSettingQuery {
    tenant_id: String,
    setting_key: String,
    scope: String,
    scope_id: String,
}

#[utoipa::path(
    delete,
    path = "/admin/settings",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
        ("setting_key" = String, Query, description = "The setting's key, node.field"),
        ("scope" = String, Query, description = "SETTING_SCOPE_TENANT, _BRAND, _STORE_GROUP or _STORE"),
        ("scope_id" = String, Query, description = "The tenant, brand, store group or store the value was written for"),
    ),
    responses(
        (status = 200, description = "Cleared, and every store it reached republished, with the \
                                      same per-store outcomes as a write"),
        (status = 400, description = "An id is not a ULID or the scope is not a scope", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No value is written there", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry, the store groups or the \
                                      config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `DELETE /admin/settings` — clears one value and republishes every store it reached, which then
/// run the next most specific value, or the default.
pub(super) async fn admin_clear_setting<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ClearSettingQuery>,
) -> Response
where
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let scope = match parse_setting_scope(&query.scope) {
        Ok(scope) => scope,
        Err(refusal) => return refusal,
    };
    let scope_id = match parse_ulid_fields([("scope_id", &query.scope_id)]) {
        Ok([scope_id]) => scope_id.to_string(),
        Err(refusal) => return refusal,
    };
    let held = match state.settings.list(tenant_id).await {
        Ok(values) => values.into_iter().find(|held| {
            held.setting_key == query.setting_key
                && held.scope.known() == scope
                && held.scope_id == scope_id
        }),
        Err(error) => return settings_store_error_response(&error),
    };
    let Some(held) = held else {
        return not_found("setting value");
    };
    let layout = match TenantLayout::load(&state.registry, &state.groups, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    match state
        .settings
        .delete(tenant_id, &query.setting_key, scope, &scope_id)
        .await
    {
        Ok(_) => {}
        Err(error) => return settings_store_error_response(&error),
    }
    let results = publish_settings_to(
        &state,
        tenant_id,
        &layout,
        &layout.reached_by(scope, &scope_id),
    )
    .await;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "setting.clear",
        "setting",
        &setting_audit_id(&query.setting_key, scope, &scope_id),
        Some(serde_json::json!({ "value": held.value })),
        None,
    )
    .await;
    publish_results_response(&results)
}
