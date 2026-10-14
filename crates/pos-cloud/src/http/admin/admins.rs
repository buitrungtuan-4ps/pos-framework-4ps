// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/admins`: list the console admins and change one's role or status.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_proto::determinism::ClockSource;
use serde::Deserialize;

use super::{admin_service_unavailable, guard_last_owner_change};
use crate::auth::admin::{AdminRole, AdminStatus, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{CloudApp, Json, enum_refusal, not_found};

/// Lists the console admins — identity and role only, never a credential. Needs
/// `console.admins.invite` (owner or admin may view the roster).
pub(in crate::http) async fn admin_list_admins<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::InviteAdmins,
    )
    .await
    {
        return denied;
    }
    match app.admin.list_admin_users().await {
        Ok(admins) => (StatusCode::OK, Json(admins)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing admins failed");
            admin_service_unavailable()
        }
    }
}

/// A request to change an admin's role.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct UpdateAdminRoleRequest {
    /// The new role: `owner`/`admin`/`ops`/`viewer`.
    role: String,
}

/// Changes an admin's role. Needs `console.admins.manage` (owner). Refuses demoting the last active
/// owner. `404` if there is no admin with that id.
pub(in crate::http) async fn admin_set_admin_role<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateAdminRoleRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageAdmins,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Some(role) = AdminRole::from_token(&request.role) else {
        return enum_refusal("role", AdminRole::ALL.iter().map(|role| role.as_token()));
    };
    if let Err(response) = guard_last_owner_change(&app.admin, &id, role != AdminRole::Owner).await
    {
        return response;
    }
    match app.admin.set_admin_user_role(&id, role).await {
        Ok(true) => {
            audit_action(
                &app.audit,
                &app.clock,
                &context,
                None,
                "admin.role_set",
                "admin",
                &id,
                None,
                Some(serde_json::json!({ "role": role.as_token() })),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("admin"),
        Err(error) => {
            tracing::error!(%error, "setting an admin role failed");
            admin_service_unavailable()
        }
    }
}

/// A request to change an admin's status.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct UpdateAdminStatusRequest {
    /// The new status: `active` or `suspended`.
    status: String,
}

/// Suspends or reactivates an admin. Needs `console.admins.manage` (owner). Refuses suspending the
/// last active owner. `404` if there is no admin with that id.
pub(in crate::http) async fn admin_set_admin_status<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateAdminStatusRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageAdmins,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let Some(status) = AdminStatus::from_token(&request.status) else {
        return enum_refusal(
            "status",
            AdminStatus::ALL.iter().map(|status| status.as_token()),
        );
    };
    if let Err(response) =
        guard_last_owner_change(&app.admin, &id, status == AdminStatus::Suspended).await
    {
        return response;
    }
    match app.admin.set_admin_user_status(&id, status).await {
        Ok(true) => {
            audit_action(
                &app.audit,
                &app.clock,
                &context,
                None,
                "admin.status_set",
                "admin",
                &id,
                None,
                Some(serde_json::json!({ "status": status.as_token() })),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("admin"),
        Err(error) => {
            tracing::error!(%error, "setting an admin status failed");
            admin_service_unavailable()
        }
    }
}
