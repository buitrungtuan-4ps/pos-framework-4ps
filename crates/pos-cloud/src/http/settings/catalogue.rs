// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/settings/catalogue` and `GET /admin/settings`: every setting, and a tenant's values.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::WireEnum;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::values::SettingsTenantQuery;
use super::{SettingsState, settings_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{Json, settings_confirm};
use crate::settings::SettingValue;
use crate::settings::SettingsStore;

/// One setting as the console's settings screen draws it: the register entry, without its
/// summary. The screen labels a setting and each of its values from its own translations, keyed by
/// the setting's key, so no English sentence reaches it from here.
///
/// What else it carries depends on the kind: a choice's `values`; a whole number's `min`, `max` and
/// `unit`, which the screen bounds its input by and names in the operator's language; nothing more
/// for a switch. `default` and `preset` are the value as the node carries it — a token, a number or
/// a boolean. `confirm` is a value written only with a recorded confirmation (ADR-0169).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(in crate::http) struct SettingCatalogueEntry {
    setting_key: String,
    node: &'static str,
    field: &'static str,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    values: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    unit: Option<&'static str>,
    default: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    preset: Option<serde_json::Value>,
    scopes: Vec<&'static str>,
    since: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    confirm: Option<settings_confirm::CatalogueConfirm>,
}

impl SettingCatalogueEntry {
    pub(in crate::http) fn of(setting: &pos_proto::settings::Setting) -> Self {
        use pos_proto::settings::SettingShape;

        let (values, bounds) = match &setting.shape {
            SettingShape::Choice { values, .. } => (Some(values.clone()), None),
            SettingShape::Int { min, max, unit, .. } => (None, Some((*min, *max, unit.as_wire()))),
            SettingShape::Bool { .. } => (None, None),
        };
        Self {
            setting_key: setting.key(),
            node: setting.node,
            field: setting.field,
            kind: setting.kind().as_wire(),
            values,
            min: bounds.map(|(min, _, _)| min),
            max: bounds.map(|(_, max, _)| max),
            unit: bounds.map(|(_, _, unit)| unit),
            default: setting.default_value(),
            preset: setting.preset_value(),
            scopes: setting.scopes.iter().map(|scope| scope.as_wire()).collect(),
            since: setting.since,
            confirm: setting
                .confirm
                .map(settings_confirm::CatalogueConfirm::from),
        }
    }
}

/// What the settings catalogue answers: every setting the register declares.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SettingsCatalogue {
    /// One entry per setting, in register order.
    settings: Vec<SettingCatalogueEntry>,
}

/// What the settings list answers: every value the tenant has written, at every scope.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SettingValues {
    /// The stored values.
    values: Vec<SettingValue>,
}

#[utoipa::path(
    get,
    path = "/admin/settings/catalogue",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    responses(
        (status = 200, body = SettingsCatalogue, description = "Every setting in the register: its key, node and field, its \
                                      kind (SETTING_KIND_CHOICE, _INT or _BOOL) and what it \
                                      takes — a choice's values, or a whole number's min, max \
                                      and unit — its default and the value a new store is given \
                                      (each as the node carries it), the scopes it may be written \
                                      at, the first release that honours it, and any value it \
                                      writes only with a recorded confirmation (confirm)"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `GET /admin/settings/catalogue` — the settings register, for the console's settings screen.
pub(super) async fn admin_settings_catalogue<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
) -> Response
where
    St: Clone + Send + Sync + 'static,
    Rg: Clone + Send + Sync + 'static,
    Grp: Clone + Send + Sync + 'static,
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
    let entries: Vec<SettingCatalogueEntry> = pos_proto::settings::register()
        .iter()
        .map(SettingCatalogueEntry::of)
        .collect();
    (
        StatusCode::OK,
        Json(SettingsCatalogue { settings: entries }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/admin/settings",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose values to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = SettingValues, description = "Every value the tenant has written: the setting, the scope \
                                      and the id it was written for, the value and when"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `GET /admin/settings` — the values a tenant has written, at every scope.
pub(super) async fn admin_list_settings<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<SettingsTenantQuery>,
) -> Response
where
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: Clone + Send + Sync + 'static,
    Grp: Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.settings.list(tenant_id).await {
        Ok(values) => (StatusCode::OK, Json(SettingValues { values })).into_response(),
        Err(error) => settings_store_error_response(&error),
    }
}

#[cfg(test)]
mod answer_tests {
    //! Each answer serialises to the bytes the `json!` it replaced did.

    use super::{SettingValues, SettingsCatalogue};

    #[test]
    fn the_catalogue_and_the_values_are_their_lists() {
        let catalogue = serde_json::to_value(SettingsCatalogue {
            settings: Vec::new(),
        });
        assert_eq!(
            catalogue.expect("json"),
            serde_json::json!({ "settings": [] })
        );
        let values = serde_json::to_value(SettingValues { values: Vec::new() });
        assert_eq!(values.expect("json"), serde_json::json!({ "values": [] }));
    }
}
