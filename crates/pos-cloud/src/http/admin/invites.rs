// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/invites`: invite a console admin, list and revoke invites, and accept one.

use core::fmt;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use serde::Deserialize;

use super::admin_service_unavailable;
use crate::auth::admin::{
    AdminContext, AdminRole, AdminStore, NewAdminInvite, NewAdminUser, hash_session_token,
};
use crate::auth::console_rbac::ConsolePermission;
use crate::auth::enrol::{MIN_PASSWORD_LEN, build_enrolment};
use crate::http::{CloudApp, Json, enum_refusal, mint_credential, random_hex_32};

/// A request to invite a new console admin.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct InviteAdminRequest {
    /// The invitee's email — the address they will sign in with (normalised server-side).
    email: String,
    /// The invitee's display name.
    name: String,
    /// The role to grant on acceptance: `owner`/`admin`/`ops`/`viewer`.
    role: String,
}

/// The one-time response to an invitation: the id, the single-use token (shown once) the inviter
/// copies into the invite link, and the expiry. Only the token's hash is stored.
#[derive(Debug, Clone, serde::Serialize)]
struct InviteAdminResponse {
    /// The invite's id (a ULID).
    invite_id: String,
    /// The single-use invite token. **Shown once** — only its hash is stored, so it cannot be
    /// recovered later; the inviter hands the link carrying it to the invitee out-of-band.
    token: String,
    /// When the invite stops being acceptable (Unix milliseconds).
    expires_at_ms: i64,
}

/// Invites a new console admin ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)). Needs
/// `console.admins.invite` (owner or admin); only an owner may invite another owner, so an admin
/// cannot escalate. Refuses an address that is already an admin. Returns the single-use token once.
pub(in crate::http) async fn admin_invite_admin<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<InviteAdminRequest>,
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
        ConsolePermission::InviteAdmins,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let role = match invited_role(&context, &request.role) {
        Ok(role) => role,
        Err(refusal) => return refusal,
    };
    let email = match invite_email(&request.email) {
        Ok(email) => email,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = refuse_an_existing_admin(&app.admin, &email).await {
        return refusal;
    }
    let (invite, token) = match new_invite(&app, &context, email, request.name, role) {
        Ok(minted) => minted,
        Err(refusal) => return refusal,
    };
    store_invite(&app, &context, invite, token).await
}

/// The role the invite grants. No privilege escalation: only an owner may mint another owner.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn invited_role(context: &AdminContext, token: &str) -> Result<AdminRole, Response> {
    let Some(role) = AdminRole::from_token(token) else {
        return Err(enum_refusal(
            "role",
            AdminRole::ALL.iter().map(|role| role.as_token()),
        ));
    };
    if role == AdminRole::Owner && context.admin.role != AdminRole::Owner {
        return Err(api_error(
            ErrorStatus::PermissionDenied,
            "only an owner may invite an owner",
        ));
    }
    Ok(role)
}

/// The invitee's address, trimmed and lower-cased, once it looks like one.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn invite_email(raw: &str) -> Result<String, Response> {
    let email = raw.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a valid email is required",
            &[("email", "INVALID_FORMAT")],
        ));
    }
    Ok(email)
}

/// Refuses an address that is already an admin's.
async fn refuse_an_existing_admin<A: AdminStore>(admin: &A, email: &str) -> Result<(), Response> {
    match admin.find_admin_user_by_email(email).await {
        Ok(Some(_)) => Err(api_error_with_details(
            ErrorStatus::AlreadyExists,
            "an admin with that email already exists",
            &[("email", "ALREADY_EXISTS")],
        )),
        Ok(None) => Ok(()),
        Err(error) => {
            tracing::error!(%error, "checking for an existing admin failed");
            Err(admin_service_unavailable())
        }
    }
}

/// The invite to store and the single-use token it is for: an id and a token from OS entropy, an
/// expiry one TTL from now, and only the token's hash on the record.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn new_invite<S, R, K, C, A, T, W>(
    app: &CloudApp<S, R, K, C, A, T, W>,
    context: &AdminContext,
    email: String,
    name: String,
    role: AdminRole,
) -> Result<(NewAdminInvite, String), Response>
where
    C: ClockSource,
{
    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(token)) = (mint_ulid(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint an invite");
        return Err(admin_service_unavailable());
    };
    let ttl_ms = i64::try_from(app.admin_invite_ttl_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
    let Ok(expires_at) =
        pos_proto::time::Timestamp::from_milliseconds_since_epoch(now_ms.saturating_add(ttl_ms))
    else {
        return Err(admin_service_unavailable());
    };
    let invite = NewAdminInvite {
        id: id.to_string(),
        email,
        name,
        role,
        token_hash: hash_session_token(&token),
        invited_by: context.admin.id.clone(),
        expires_at,
    };
    Ok((invite, token))
}

/// Stores the invite, records the role it grants, and answers the id, the token (once) and the
/// expiry.
async fn store_invite<S, R, K, C, A, T, W>(
    app: &CloudApp<S, R, K, C, A, T, W>,
    context: &AdminContext,
    invite: NewAdminInvite,
    token: String,
) -> Response
where
    C: ClockSource,
    A: AdminStore,
{
    let (id, role, expires_at) = (invite.id.clone(), invite.role, invite.expires_at);
    match app.admin.create_invite(invite).await {
        Ok(()) => {
            // The role granted is the auditable fact; the invite email and single-use token are not
            // written to the trail. Admin management is tenant-global, so the entry carries no tenant.
            audit_action(
                &app.audit,
                &app.clock,
                context,
                None,
                "admin.invite",
                "admin_invite",
                &id,
                None,
                Some(serde_json::json!({ "role": role.as_token() })),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(InviteAdminResponse {
                    invite_id: id,
                    token,
                    expires_at_ms: expires_at.as_milliseconds_since_epoch(),
                }),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "creating an invite failed");
            admin_service_unavailable()
        }
    }
}

/// Lists the pending (not accepted, not expired) invitations. Needs `console.admins.invite`.
pub(in crate::http) async fn admin_list_invites<S, R, K, C, A, T, W>(
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
    match app.admin.list_pending_invites(app.clock.now()).await {
        Ok(invites) => (StatusCode::OK, Json(invites)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing invites failed");
            admin_service_unavailable()
        }
    }
}

/// Revokes a pending invitation by id. `204` whether or not one was pending — revoking is idempotent.
/// Needs `console.admins.invite`.
pub(in crate::http) async fn admin_revoke_invite<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
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
        ConsolePermission::InviteAdmins,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    match app.admin.revoke_invite(&id).await {
        Ok(removed) => {
            if removed {
                audit_action(
                    &app.audit,
                    &app.clock,
                    &context,
                    None,
                    "admin.invite_revoke",
                    "admin_invite",
                    &id,
                    None,
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::error!(%error, "revoking an invite failed");
            admin_service_unavailable()
        }
    }
}

/// A self-enrolment request: the single-use invite token and the password the invitee chooses.
///
/// [`fmt::Debug`] redacts both, so a logged request cannot leak the token or the password.
#[derive(Clone, Deserialize)]
pub(in crate::http) struct AcceptInviteRequest {
    /// The single-use invite token from the invite link.
    token: String,
    /// The password the invitee is choosing.
    password: String,
}

impl fmt::Debug for AcceptInviteRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptInviteRequest")
            .field("token", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Accepts an invitation and self-enrols the new admin — **pre-auth**: the invite token is the
/// authorisation, so there is no session guard ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
/// Mints the admin's own Argon2id password hash and TOTP secret exactly as first-boot enrolment does,
/// claims the invite single-use, creates the admin, and returns the one-time TOTP enrolment. A bad or
/// expired token is a generic `401`.
pub(in crate::http) async fn admin_accept_invite<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    Json(request): Json<AcceptInviteRequest>,
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
    if request.password.len() < MIN_PASSWORD_LEN {
        // One field, out of range — a `400`, not the `422` this answered before ADR-0096. The
        // difference is what it tells the reader to do: there *is* a field to go and fix, which is
        // exactly what separates `INVALID_ARGUMENT` from `UNPROCESSABLE`.
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("the password must be at least {MIN_PASSWORD_LEN} characters"),
            &[("password", "OUT_OF_RANGE")],
        );
    }
    let now = app.clock.now();
    let invite = match app
        .admin
        .find_pending_invite_by_token(hash_session_token(&request.token), now)
        .await
    {
        Ok(Some(invite)) => invite,
        Ok(None) => {
            return api_error(
                ErrorStatus::Unauthenticated,
                "the invite is invalid or has expired",
            );
        }
        Err(error) => {
            tracing::error!(%error, "looking up an invite failed");
            return admin_service_unavailable();
        }
    };
    let Some((secret, phc)) = mint_credential(&request.password) else {
        tracing::error!("could not mint a credential for invite acceptance");
        return admin_service_unavailable();
    };
    // Claim the invite before creating the admin, so a replayed acceptance cannot enrol twice.
    match app.admin.mark_invite_accepted(&invite.id, now).await {
        Ok(true) => {}
        Ok(false) => {
            return api_error(
                ErrorStatus::Unauthenticated,
                "the invite is invalid or has expired",
            );
        }
        Err(error) => {
            tracing::error!(%error, "claiming an invite failed");
            return admin_service_unavailable();
        }
    }
    let Some(admin_id) = mint_ulid(now.as_milliseconds_since_epoch()) else {
        tracing::error!("could not read OS entropy to mint an admin id");
        return admin_service_unavailable();
    };
    let user = NewAdminUser {
        id: admin_id.to_string(),
        email: invite.email,
        name: invite.name,
        role: invite.role,
        password_phc: phc,
        totp_secret: secret.to_vec(),
    };
    match app.admin.create_admin_user(user).await {
        Ok(true) => (StatusCode::CREATED, Json(build_enrolment(&secret))).into_response(),
        Ok(false) => api_error_with_details(
            ErrorStatus::AlreadyExists,
            "an admin with that email already exists",
            &[("email", "ALREADY_EXISTS")],
        ),
        Err(error) => {
            tracing::error!(%error, "creating the admin failed");
            admin_service_unavailable()
        }
    }
}
