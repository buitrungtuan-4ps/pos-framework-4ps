// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console's own surface (`/admin`): signing in and out, first-boot setup, the audit read, API
//! keys, self-service sessions and security, and admin management and invitations (ADR-0067).
//!
//! The refusal and the last-owner guard the admin-management handlers share are here.

use axum::response::Response;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;

use crate::auth::admin::{AdminRole, AdminStatus, AdminStore};

mod admins;
mod api_keys;
// `pub(super)`: the audit-view unit tests in `http.rs` reach the views and the staff redaction.
pub(super) mod audit;
mod auth;
mod invites;
mod security;
mod sessions;

pub use audit::audit_router;
// The handlers the main router mounts.
pub(super) use admins::{admin_list_admins, admin_set_admin_role, admin_set_admin_status};
pub(super) use api_keys::{admin_create_api_key, admin_list_api_keys, admin_revoke_api_key};
pub(crate) use auth::{admin_login, admin_logout, admin_session, admin_setup, admin_whoami};
pub(super) use invites::{
    admin_accept_invite, admin_invite_admin, admin_list_invites, admin_revoke_invite,
};
pub(crate) use security::{
    admin_generate_recovery_codes, admin_recovery_codes_status, admin_reenrol_totp,
};
pub(super) use sessions::{admin_list_sessions, admin_revoke_other_sessions, admin_revoke_session};
// The handlers the console's OpenAPI document names.
pub(crate) use admins::{
    __path_admin_list_admins, __path_admin_set_admin_role, __path_admin_set_admin_status,
};
pub(crate) use auth::{
    __path_admin_login, __path_admin_logout, __path_admin_session, __path_admin_setup,
    __path_admin_whoami,
};
pub(crate) use invites::{
    __path_admin_accept_invite, __path_admin_invite_admin, __path_admin_list_invites,
    __path_admin_revoke_invite,
};
pub(crate) use security::{
    __path_admin_generate_recovery_codes, __path_admin_recovery_codes_status,
    __path_admin_reenrol_totp,
};
pub(crate) use sessions::{
    __path_admin_list_sessions, __path_admin_revoke_other_sessions, __path_admin_revoke_session,
};

// --- Console admin management and invitations ([ADR-0067]) --------------------------------------

/// A `503` when the admin store is unreachable — the transient, retryable failure for the admin
/// surface, with the detail kept to the server's log rather than the client.
fn admin_service_unavailable() -> Response {
    api_error(ErrorStatus::Unavailable, "the admin service is unavailable")
}

/// Refuses a change that would remove the **last active owner** — a demotion or a suspension of the
/// sole remaining `owner` ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)). `removes_owner`
/// is true when the pending change would stop the target being an active owner; otherwise the guard
/// is a no-op. An absent target passes through, so the caller's own update returns the `404`.
async fn guard_last_owner_change<A>(
    admin: &A,
    id: &str,
    removes_owner: bool,
) -> Result<(), Response>
where
    A: AdminStore,
{
    if !removes_owner {
        return Ok(());
    }
    let target = match admin.get_admin_user(id).await {
        Ok(Some(target)) => target,
        Ok(None) => return Ok(()),
        Err(error) => {
            tracing::error!(%error, "reading an admin failed");
            return Err(admin_service_unavailable());
        }
    };
    if target.role == AdminRole::Owner && target.status == AdminStatus::Active {
        let owners = admin.count_active_owners().await.map_err(|error| {
            tracing::error!(%error, "counting owners failed");
            admin_service_unavailable()
        })?;
        if owners <= 1 {
            return Err(api_error(
                ErrorStatus::FailedPrecondition,
                "cannot remove the last active owner",
            ));
        }
    }
    Ok(())
}
