// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/stores/{store_id}/permissions/readiness`: what each role at a store lacks before the
//! store enforces it.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::WireEnum;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::settings::SettingScope;
use serde::{Deserialize, Serialize};

use super::requests::people_error_response;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::people::{AssignmentStore, Employee, EmployeeStore, RoleTemplate, RoleTemplateStore};
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;
use pos_cloud_api::refusal::not_found;
use pos_cloud_config::route::{TenantLayout, settings_store_error_response};

// --- Permissions readiness (`/admin/stores/{store_id}/permissions/readiness`, ADR-0158) ---------

/// The collaborators the readiness read needs: the people store, for the roles, the people and the
/// assignments that reach a store; the settings, the registry and the store groups that resolve the
/// store's `permissions.enforced`; and the admin and clock the session guard uses. It writes
/// nothing, so it carries no audit recorder.
#[derive(Clone)]
struct ReadinessState<P, St, Rg, Grp, A, C> {
    people: P,
    settings: St,
    registry: Rg,
    groups: Grp,
    admin: A,
    clock: C,
}

/// Builds the permissions-readiness sub-router
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md), Rollout).
///
/// One read: what each role held at a store does not grant ([`crate::people_readiness`]), beside
/// whether the store enforces each person's own permissions and which level says so. Behind
/// [`ConsolePermission::ReadPeople`], because it reads the store's roles and assignments. The answer
/// names roles and counts people, and names no person.
pub fn permissions_readiness_router<P, St, Rg, Grp, A, C>(
    people: P,
    settings: St,
    registry: Rg,
    groups: Grp,
    admin: A,
    clock: C,
) -> Router
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/stores/{store_id}/permissions/readiness",
            get(admin_permissions_readiness::<P, St, Rg, Grp, A, C>),
        )
        .with_state(ReadinessState {
            people,
            settings,
            registry,
            groups,
            admin,
            clock,
        })
}

/// The tenant a readiness read is scoped to.
#[derive(Debug, Clone, Deserialize)]
struct ReadinessQuery {
    tenant_id: String,
}

/// What `GET /admin/stores/{store_id}/permissions/readiness` answers.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
struct PermissionsReadiness {
    store_id: String,
    /// What the store runs for `permissions.enforced`.
    enforced: bool,
    /// The scope of the value the store runs, a `SETTING_SCOPE_*` token, absent when it runs the
    /// default.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    enforced_scope: Option<&'static str>,
    /// The tenant, brand, store group or store that value was written for, beside `enforced_scope`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    enforced_scope_id: Option<String>,
    roles: Vec<crate::people_readiness::RoleReadiness>,
    people_without_role: usize,
}

/// What a store runs for `permissions.enforced`, and the scope and id of the value it runs, or
/// `None` for the register's default: the same resolve `GET /admin/settings/effective` answers.
fn enforced_at(
    values: &[crate::settings::SettingValue],
    placement: &crate::settings::StorePlacement,
) -> (bool, Option<(SettingScope, String)>) {
    crate::settings::resolve_for_store(values, placement)
        .into_iter()
        .find(|resolved| {
            resolved.node == pos_proto::people::PublishedPermissions::NODE
                && resolved.field == "enforced"
        })
        .map_or(
            (
                pos_proto::people::PublishedPermissions::default().enforced,
                None,
            ),
            |resolved| {
                (
                    resolved.value.as_bool().unwrap_or_default(),
                    Some((resolved.scope, resolved.scope_id)),
                )
            },
        )
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/permissions/readiness",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.people.read"))),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = PermissionsReadiness, description = "`store_id`; `enforced`, what the store runs for \
                                      `permissions.enforced`, with `enforced_scope` and \
                                      `enforced_scope_id` naming the value's level, both absent \
                                      for the default; `roles`, one per active role an active \
                                      person holds at the store through any assignment that reaches \
                                      it, by name, each with `role_template_id`, `name`, `people` \
                                      (how many hold it, each once) and `missing` — every \
                                      permission a store decides that the role grants neither \
                                      directly nor with approval, as `id`, `group` and `risk`, low \
                                      risk first and catalogue order within a level; and \
                                      `people_without_role`, how many people at the store hold no \
                                      active role there. No person is named"),
        (status = 400, description = "store_id or tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store, the settings store, the registry or the \
                                      store groups is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
/// `GET /admin/stores/{store_id}/permissions/readiness` — what each role held at a store does not
/// grant, read before the store enforces each person's own permissions.
///
/// The roles are those the store's `permissions` node is compiled from: every assignment that
/// reaches the store, whatever its scope, held by an active person, with an active role. Each is
/// compared with the permission catalogue and nothing else; what anybody did at the till is not
/// read (ADR-0070).
async fn admin_permissions_readiness<P, St, Rg, Grp, A, C>(
    State(state): State<ReadinessState<P, St, Rg, Grp, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ReadinessQuery>,
) -> Response
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ReadPeople,
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
    let (enforced, source) = enforced_at(&values, &placement);
    let assignments =
        match AssignmentStore::list_for_store(&state.people, tenant_id, store_id).await {
            Ok(assignments) => assignments,
            Err(error) => return people_error_response(&error),
        };
    let employees: Vec<Employee> = match EmployeeStore::list(&state.people, tenant_id).await {
        Ok(employees) => employees.into_iter().map(|row| row.record).collect(),
        Err(error) => return people_error_response(&error),
    };
    let roles: Vec<RoleTemplate> = match RoleTemplateStore::list(&state.people, tenant_id).await {
        Ok(roles) => roles.into_iter().map(|row| row.record).collect(),
        Err(error) => return people_error_response(&error),
    };
    let readiness = crate::people_readiness::store_readiness(&employees, &roles, &assignments);
    let (enforced_scope, enforced_scope_id) = match source {
        Some((scope, scope_id)) => (Some(scope.as_wire()), Some(scope_id)),
        None => (None, None),
    };
    (
        StatusCode::OK,
        Json(PermissionsReadiness {
            store_id: store_id.to_string(),
            enforced,
            enforced_scope,
            enforced_scope_id,
            roles: readiness.roles,
            people_without_role: readiness.people_without_role,
        }),
    )
        .into_response()
}
