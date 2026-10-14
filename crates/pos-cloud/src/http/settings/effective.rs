// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/settings/effective`: the value each setting takes at a store, and where it comes from.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::WireEnum;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::publish::TenantLayout;
use super::{SettingsState, settings_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{Json, not_found};
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;

/// The store whose effective settings to read.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct EffectiveSettingsQuery {
    tenant_id: String,
    store_id: String,
}

/// One setting as one store runs it, and where its value comes from.
#[derive(Debug, Clone, serde::Serialize)]
struct EffectiveSetting {
    setting_key: String,
    value: serde_json::Value,
    /// The scope that set it, or absent when the store runs the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'static str>,
    /// The tenant, brand, store group or store that set it.
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_id: Option<String>,
}

#[utoipa::path(
    get,
    path = "/admin/settings/effective",
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "Every setting as the store runs it: the value, and the scope \
                                      and id it comes from, which are absent for a default"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry or the store groups is \
                                      unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `GET /admin/settings/effective` — what one store runs, and why.
pub(super) async fn admin_effective_settings<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<EffectiveSettingsQuery>,
) -> Response
where
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
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
    let effective: Vec<EffectiveSetting> = pos_proto::settings::register()
        .iter()
        .map(|setting| {
            let key = setting.key();
            match resolved
                .iter()
                .find(|resolved| resolved.node == setting.node && resolved.field == setting.field)
            {
                Some(resolved) => EffectiveSetting {
                    setting_key: key,
                    value: resolved.value.clone(),
                    scope: Some(resolved.scope.as_wire()),
                    scope_id: Some(resolved.scope_id.clone()),
                },
                None => EffectiveSetting {
                    setting_key: key,
                    value: setting.default_value(),
                    scope: None,
                    scope_id: None,
                },
            }
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "settings": effective })),
    )
        .into_response()
}
