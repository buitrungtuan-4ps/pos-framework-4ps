// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/settings/presets`: a preset's values applied to a store.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::settings::SettingScope;
use pos_proto::wire_enum::Open;
use serde::Deserialize;

use super::publish::SettingPublishResult;
use super::publish::{TenantLayout, publish_settings_to};
use super::{SettingsState, settings_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{Json, not_found};
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;

/// A store to publish settings to, or every store of the tenant.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct SettingsStoreRequest {
    pub(super) tenant_id: String,
    #[serde(default)]
    pub(super) store_id: Option<String>,
}

/// What applying a preset answers: the settings it wrote, and how publishing to the store went.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct PresetsApplied {
    /// The setting keys the preset wrote.
    applied: Vec<String>,
    /// One result per store the write reached.
    stores: Vec<SettingPublishResult>,
}

#[utoipa::path(
    post,
    path = "/admin/settings/presets",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    request_body(
        content = SettingsStoreRequest, description = "The tenant and the store to give the new-store values",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = PresetsApplied, description = "Each setting with a new-store value that the store did not \
                                      already set is written at the store's own scope, and the \
                                      store republished. The body lists the settings written and \
                                      the store's publish outcome"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry, the store groups or the \
                                      config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `POST /admin/settings/presets` — gives a store the values the owner chose for a new store
/// (ADR-0160 decision 1), which the console calls when it creates one.
///
/// Only settings the store does not set itself are written, so calling it twice, or on a store an
/// operator has already configured, changes nothing the operator chose.
pub(super) async fn admin_apply_setting_presets<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<SettingsStoreRequest>,
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
    let store_field = request.store_id.clone().unwrap_or_default();
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &store_field),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let layout = match TenantLayout::load(&state.registry, &state.groups, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    if layout.placement(store_id).is_none() {
        return not_found("store");
    }
    let held = match state.settings.list(tenant_id).await {
        Ok(values) => values,
        Err(error) => return settings_store_error_response(&error),
    };
    let store_scope_id = store_id.to_string();
    let mut applied = Vec::new();
    for setting in pos_proto::settings::register() {
        let Some(preset) = setting.preset_value() else {
            continue;
        };
        let key = setting.key();
        let already = held.iter().any(|value| {
            value.setting_key == key
                && value.scope.known() == SettingScope::Store
                && value.scope_id == store_scope_id
        });
        if already {
            continue;
        }
        let value = crate::settings::SettingValue {
            setting_key: key.clone(),
            scope: Open::from_known(SettingScope::Store),
            scope_id: store_scope_id.clone(),
            value: preset,
            update_time: state.clock.now(),
        };
        if let Err(error) = state.settings.put(tenant_id, &value).await {
            return settings_store_error_response(&error);
        }
        applied.push(key);
    }
    let results = publish_settings_to(&state, tenant_id, &layout, &[store_id]).await;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "setting.presets.apply",
        "store",
        &store_scope_id,
        None,
        Some(serde_json::json!({ "settings": applied })),
    )
    .await;
    (
        StatusCode::OK,
        Json(PresetsApplied {
            applied,
            stores: results,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod answer_tests {
    //! Each answer serialises to the bytes the `json!` it replaced did.

    use super::PresetsApplied;

    #[test]
    fn a_preset_answers_what_it_applied_and_its_stores() {
        let answer = PresetsApplied {
            applied: vec!["session.idle_lock_seconds".to_owned()],
            stores: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(answer).expect("json"),
            serde_json::json!({ "applied": ["session.idle_lock_seconds"], "stores": [] })
        );
    }
}
