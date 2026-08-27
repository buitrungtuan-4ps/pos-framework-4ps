// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The super-admin login flow and its server-side session store
//! ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
//!
//! [`super`] holds the pure two-factor check ([`SuperAdminCredential::authenticate`](super::SuperAdminCredential::authenticate))
//! and the cookie policy ([`session`](super::session)). This module is the seam that turns them into
//! a login: it loads the stored credential, runs the check, and — on success — mints a session the
//! [`session`](super::session) cookie carries. Three properties carry over from [ADR-0034](../../../docs/adr/0034-super-admin-auth.md)
//! and are enforced here:
//!
//!  * **No oracle.** A wrong password, a wrong code, a replayed code, and a not-yet-provisioned admin
//!    all collapse to the same [`LoginDenied::Invalid`] — a single generic `401`. The specific reason
//!    stays in the server's log. A store outage is [`LoginDenied::StoreUnavailable`] instead — a
//!    retryable `503`, because the caller's credentials may be perfectly good.
//!  * **Single-use codes survive a crash.** The matched TOTP step is recorded *before* the session is
//!    written, so even a retried login cannot mint two sessions from one code.
//!  * **Only a hash is stored.** The cookie carries a 256-bit random token; the store keeps only its
//!    `SHA-256`, so a database read yields no usable session — the same posture as the API-key secret.
//!
//! Like the rest of [`crate::auth`], the module is **pure and deterministic**: the clock is a
//! [`ClockSource`] parameter and the session token is minted from a CSPRNG at the binary edge
//! ([`crate::http`]) and passed in, so every rule here is unit-tested with no clock and no entropy.

use core::fmt;
use core::future::Future;

use axum::http::header::COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use pos_proto::determinism::ClockSource;
use pos_proto::time::Timestamp;

use super::SuperAdminCredential;
use super::session::COOKIE_NAME;

/// The stored super-admin credential and the last TOTP step it has used.
///
/// There is exactly one super-admin ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)); this is
/// its persisted form. `last_used_totp_step` is `None` until the first successful login, and only ever
/// moves forward, which is what makes a code single-use across process restarts.
#[derive(Clone)]
pub struct AdminCredential {
    /// The password hash and TOTP secret, in the pure form [`SuperAdminCredential::authenticate`]
    /// consumes.
    pub credential: SuperAdminCredential,
    /// The newest TOTP step already spent, or `None` if the admin has never logged in.
    pub last_used_totp_step: Option<u64>,
}

impl fmt::Debug for AdminCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `credential` redacts its own secrets; the step is not sensitive.
        formatter
            .debug_struct("AdminCredential")
            .field("credential", &self.credential)
            .field("last_used_totp_step", &self.last_used_totp_step)
            .finish()
    }
}

/// A console admin's role — the least-privilege tier its session is granted
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)). This is the stable vocabulary the
/// schema and seam store; the role→permission templates (the compile-forced §9-style registry) land
/// in a later G1 slice and are built on top of these variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminRole {
    /// Everything, including managing other admins. There is always at least one active owner.
    Owner,
    /// All tenant data; cannot manage admins.
    Admin,
    /// Day-to-day operations: devices, activation, webhooks, config publish.
    Ops,
    /// Read-only.
    Viewer,
}

impl AdminRole {
    /// Every role, in privilege order — for enumeration and the console picker.
    pub const ALL: &'static [Self] = &[Self::Owner, Self::Admin, Self::Ops, Self::Viewer];

    /// The token stored in PostgreSQL and carried on the wire.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Ops => "ops",
            Self::Viewer => "viewer",
        }
    }

    /// Parses a stored token, or `None` if it names no known role. An unrecognised value fails closed
    /// (the caller treats it as no role) rather than being coerced to a privileged default.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|role| role.as_token() == token)
    }
}

/// Whether an admin can sign in. `Suspended` keeps the row and its history but refuses new sessions —
/// the off-boarding path that does not destroy the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminStatus {
    /// In use — can sign in.
    Active,
    /// Retired — cannot sign in; kept for history.
    Suspended,
}

impl AdminStatus {
    /// The token stored in PostgreSQL and carried on the wire.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
        }
    }

    /// Parses a stored token, or `None` if it names no known status.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "active" => Some(Self::Active),
            "suspended" => Some(Self::Suspended),
            _ => None,
        }
    }
}

/// A console admin as listed — identity and role, never the credential. Safe to serialise to the
/// console: it carries no password hash and no TOTP secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdminUser {
    /// The admin's ULID id (a string; minted at the HTTP edge).
    pub id: String,
    /// The login identity. Unique case-insensitively across admins.
    pub email: String,
    /// The display name.
    pub name: String,
    /// The role that decides the session's permissions.
    pub role: AdminRole,
    /// Whether the admin can sign in.
    pub status: AdminStatus,
}

/// The input to provisioning a new console admin: identity, role, and the freshly-hashed credential
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)). `password_phc` is the Argon2id PHC
/// string and `totp_secret` the raw RFC 6238 secret, both minted at the HTTP edge exactly as
/// first-boot enrolment does; a new admin starts `active`.
///
/// [`fmt::Debug`] redacts the password hash and the TOTP secret, so neither can reach a log through a
/// derived `Debug`.
#[derive(Clone)]
pub struct NewAdminUser {
    /// The admin's ULID id.
    pub id: String,
    /// The login identity — the caller passes it already normalised (trimmed, lower-case); uniqueness
    /// is enforced case-insensitively regardless.
    pub email: String,
    /// The display name.
    pub name: String,
    /// The role to grant.
    pub role: AdminRole,
    /// The Argon2id PHC string — the hash, never the password.
    pub password_phc: String,
    /// The raw RFC 6238 TOTP shared secret.
    pub totp_secret: Vec<u8>,
}

impl fmt::Debug for NewAdminUser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewAdminUser")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("name", &self.name)
            .field("role", &self.role)
            .field("password_phc", &"<redacted>")
            .field("totp_secret", &"<redacted>")
            .finish()
    }
}

/// A pending or accepted invitation, as listed ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
/// Carries no token — only its `SHA-256` is ever stored — so this is safe to serialise to the console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdminInvite {
    /// The invite's ULID id.
    pub id: String,
    /// The address the invitee will sign in with once they accept.
    pub email: String,
    /// The display name the accepted admin will carry.
    pub name: String,
    /// The role the accepted admin will be granted.
    pub role: AdminRole,
    /// The id of the admin who issued the invite.
    pub invited_by: String,
    /// Whether the invite has been accepted (its self-enrolment completed).
    pub accepted: bool,
}

/// The input to minting an invitation: identity, role, the inviter, the single-use token's hash, and
/// the expiry ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)). Only
/// `SHA-256(token)` is stored; the raw token reaches the invitee once (the copy-invite-link the
/// inviter hands over out-of-band) and is never persisted — the same posture as the session token.
#[derive(Debug, Clone)]
pub struct NewAdminInvite {
    /// The invite's ULID id.
    pub id: String,
    /// The invitee's email (normalised — trimmed, lower-case; unique case-insensitively enforced).
    pub email: String,
    /// The display name.
    pub name: String,
    /// The role to grant on acceptance.
    pub role: AdminRole,
    /// `SHA-256` of the single-use invite token.
    pub token_hash: [u8; 32],
    /// The id of the admin issuing the invite.
    pub invited_by: String,
    /// When the invite stops being acceptable (Unix milliseconds).
    pub expires_at: Timestamp,
}

/// A live admin session, as the role-aware guard reads it: the id of the admin it belongs to, or
/// `None` for a legacy session minted before multi-admin
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    /// The [`AdminUser`] id the session belongs to, or `None` for a pre-multi-admin session.
    pub admin_id: Option<String>,
}

/// The acting admin behind an authenticated `/admin` request — the identity a role-gated route
/// checks its required [`ConsolePermission`](super::console_rbac::ConsolePermission) against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminContext {
    /// The signed-in admin.
    pub admin: AdminUser,
}

/// The columns of a session to mint, computed at the HTTP edge and handed to
/// [`AdminStore::create_session`] ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)
/// slice 4). `expires_at` is the first idle boundary (`now + idle_ttl`), `absolute_expires_at` the
/// hard cap (`now + absolute_ttl`) the sliding TTL can never pass, and `idle_ttl_ms` the window a
/// real request slides the session forward by. `ip`/`user_agent` are captured so the admin can
/// recognise the session in their own session list; both are optional (a request may carry neither).
#[derive(Debug, Clone)]
pub struct NewAdminSession {
    /// `SHA-256(token)` — what the store keys the row by; the token itself is never stored.
    pub token_hash: [u8; 32],
    /// When the session was minted, from the clock (Unix-ms `Timestamp`).
    pub created_at: Timestamp,
    /// When the session next expires if it is not slid before then.
    pub expires_at: Timestamp,
    /// The absolute ceiling the sliding TTL can never pass.
    pub absolute_expires_at: Timestamp,
    /// The idle window a real guarded request slides the session forward by, in milliseconds.
    pub idle_ttl_ms: i64,
    /// The `admin_users` id the session belongs to, or `None` for a legacy session.
    pub admin_id: Option<String>,
    /// The client IP the session was minted for, if known.
    pub ip: Option<String>,
    /// The client user-agent the session was minted for, if known.
    pub user_agent: Option<String>,
}

/// One of an admin's own live sessions, as listed for the self-service "my sessions" view
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4). Carries the session's
/// opaque revocation handle (`token_hash`, the `SHA-256` of the token — never reversible to it) and
/// the accountability details, never the token itself, so it is safe to serialise to the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// `SHA-256(token)` — the opaque handle the console revokes this session by.
    pub token_hash: [u8; 32],
    /// The client IP the session was minted for, if it was known.
    pub ip: Option<String>,
    /// The client user-agent the session was minted for, if it was known.
    pub user_agent: Option<String>,
    /// When the session was minted.
    pub created_at: Timestamp,
    /// When the session currently expires, after any sliding.
    pub expires_at: Timestamp,
}

/// The super-admin store: the one credential, and the server-side session table
/// ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)). A table in `store-postgres`; a fake in
/// tests.
///
/// [ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) extends this seam with the
/// multi-admin surface — provisioning, listing and role/status management of named
/// [`AdminUser`]s over the `admin_users` table. The single-super-admin methods stay for now: the
/// login flow and session guard migrate onto `admin_users` in a later slice, so through the
/// transition both the legacy credential and the new user table are readable.
///
/// Sessions are keyed by `SHA-256(token)`, never the token, so nothing this store holds can be
/// replayed as a live session if the table leaks.
pub trait AdminStore {
    /// Loads the single super-admin credential, or `None` if one has not been provisioned yet.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] only if the store itself could not be read — never for an absent admin,
    /// which is `Ok(None)`.
    fn load_credential(
        &self,
    ) -> impl Future<Output = Result<Option<AdminCredential>, AdminStoreError>> + Send;

    /// Provisions the single super-admin credential *if none exists yet* — the first-boot enrolment
    /// ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md)). `password_phc` is the
    /// Argon2id PHC string and `totp_secret` the raw shared secret. Returns whether it created the
    /// credential: `Ok(false)` means one was already provisioned and nothing was written, so the
    /// caller refuses the enrolment rather than replacing a live admin.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn provision_credential(
        &self,
        password_phc: String,
        totp_secret: Vec<u8>,
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// Records `step` as the newest TOTP step spent, advancing the stored value only forward so a
    /// concurrent or replayed login cannot lower it. Idempotent for a step already recorded.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn record_totp_step(
        &self,
        step: u64,
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

    /// Persists a session ([`NewAdminSession`]) — its token hash, its sliding/absolute expiries and
    /// idle window, the owning admin, and the client IP/user-agent it was minted for.
    ///
    /// `admin_id` is the [`AdminUser`] the session belongs to, or `None` for a legacy session minted
    /// before multi-admin ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)); a
    /// `None`-owned session is still valid but resolves to no specific admin.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn create_session(
        &self,
        session: NewAdminSession,
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

    /// Whether a session with `token_hash` exists and has not expired as of `now`.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn session_is_valid(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// The live session for `token_hash` as of `now`, with the id of the admin it belongs to — or
    /// `None` if there is no live session. The role-aware guard's lookup
    /// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)): a `Some(LiveSession)` whose
    /// `admin_id` is `None` is a legacy session (minted before multi-admin).
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn session_admin(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> impl Future<Output = Result<Option<LiveSession>, AdminStoreError>> + Send;

    /// Revokes the session with `token_hash`. Idempotent: revoking an absent session is `Ok(())`.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn revoke_session(
        &self,
        token_hash: [u8; 32],
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

    /// Lists `admin_id`'s own live sessions (not expired as of `now`), newest first — the self-service
    /// "my sessions" view ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4).
    /// Scoped to the one admin, so no admin sees another's sessions.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn list_admin_sessions(
        &self,
        admin_id: &str,
        now: Timestamp,
    ) -> impl Future<Output = Result<Vec<SessionSummary>, AdminStoreError>> + Send;

    /// Revokes one of `admin_id`'s own sessions by `token_hash`, scoped so an admin can only revoke a
    /// session that is theirs. Returns `Ok(false)` if none matched (absent, or not owned by them).
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn revoke_admin_session(
        &self,
        admin_id: &str,
        token_hash: [u8; 32],
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// Revokes all of `admin_id`'s sessions except `except_token_hash` (their current one) — "sign out
    /// everywhere else". Returns how many were revoked.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn revoke_other_admin_sessions(
        &self,
        admin_id: &str,
        except_token_hash: [u8; 32],
    ) -> impl Future<Output = Result<u64, AdminStoreError>> + Send;

    // ---- Multi-admin surface ([ADR-0067]) ----

    /// Provisions a new console admin. Returns `Ok(false)` without writing when an admin with the
    /// same email (compared case-insensitively) already exists, so a caller never silently replaces
    /// one.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn create_admin_user(
        &self,
        user: NewAdminUser,
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// Lists every console admin — identity and role only, never a credential.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn list_admin_users(
        &self,
    ) -> impl Future<Output = Result<Vec<AdminUser>, AdminStoreError>> + Send;

    /// The admin with `id`, or `None` if there is none.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn get_admin_user(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<AdminUser>, AdminStoreError>> + Send;

    /// The admin whose email matches `email` case-insensitively, or `None` — the login-identity
    /// lookup.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn find_admin_user_by_email(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<Option<AdminUser>, AdminStoreError>> + Send;

    /// Sets an admin's role. Returns `Ok(false)` if no admin has `id`.
    ///
    /// The last-owner invariant is the caller's to uphold (via [`count_active_owners`](Self::count_active_owners));
    /// this method is the mechanism, not the policy.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn set_admin_user_role(
        &self,
        id: &str,
        role: AdminRole,
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// Sets an admin's status (active/suspended). Returns `Ok(false)` if no admin has `id`.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn set_admin_user_status(
        &self,
        id: &str,
        status: AdminStatus,
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// How many admins are both `owner` and `active`. Callers check this before a demotion or a
    /// suspension to keep the "always at least one active owner" invariant
    /// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn count_active_owners(&self) -> impl Future<Output = Result<u64, AdminStoreError>> + Send;

    // ---- Invitations ([ADR-0067]) ----

    /// Records a pending invitation, keyed for acceptance by its token hash.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn create_invite(
        &self,
        invite: NewAdminInvite,
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

    /// The still-acceptable invitation whose token hashes to `token_hash` as of `now` — pending
    /// (not yet accepted) and not expired — or `None`. The self-enrolment lookup.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn find_pending_invite_by_token(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> impl Future<Output = Result<Option<AdminInvite>, AdminStoreError>> + Send;

    /// Marks the invite `id` accepted at `accepted_at`, atomically and single-use: returns `Ok(true)`
    /// only if this call is the one that claimed a still-pending invite, `Ok(false)` if it was
    /// already accepted (or absent), so a replayed acceptance cannot enrol twice.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn mark_invite_accepted(
        &self,
        id: &str,
        accepted_at: Timestamp,
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// Lists the invitations still pending (not accepted and not expired as of `now`), for the
    /// console's pending-invites view.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn list_pending_invites(
        &self,
        now: Timestamp,
    ) -> impl Future<Output = Result<Vec<AdminInvite>, AdminStoreError>> + Send;

    /// Revokes (deletes) a pending invitation by id. Returns `Ok(false)` if none matched. Idempotent.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn revoke_invite(&self, id: &str)
    -> impl Future<Output = Result<bool, AdminStoreError>> + Send;
}

/// A failure of the admin store itself — the database is unreachable — as distinct from a wrong
/// credential (which is a verdict, not an error) or an absent admin (which is `Ok(None)`).
#[derive(Debug, thiserror::Error)]
#[error("the admin store failed: {0}")]
pub struct AdminStoreError(String);

impl AdminStoreError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// A super-admin sign-in request: the password and the current TOTP code.
///
/// [`fmt::Debug`] redacts the password, so a logged request cannot leak it.
#[derive(Clone, Deserialize)]
pub struct LoginRequest {
    /// The super-admin password.
    pub password: String,
    /// The current 6-digit TOTP code.
    pub totp_code: String,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("password", &"<redacted>")
            .field("totp_code", &self.totp_code)
            .finish()
    }
}

/// Why a sign-in was refused.
///
/// Every credential problem — wrong password, wrong or replayed code, no admin provisioned — is one
/// [`Invalid`](Self::Invalid), so the client cannot tell them apart. Only the store being down is
/// distinguished, as a retryable `503`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginDenied {
    /// The credentials did not authenticate. A single generic `401`.
    Invalid,
    /// The admin store could not be reached. A retryable `503`.
    StoreUnavailable,
}

impl IntoResponse for LoginDenied {
    fn into_response(self) -> Response {
        match self {
            Self::Invalid => (StatusCode::UNAUTHORIZED, "sign-in failed").into_response(),
            Self::StoreUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "the sign-in service is unavailable",
            )
                .into_response(),
        }
    }
}

/// Why a session check failed. Mirrors [`LoginDenied`]: an absent or invalid session is `401`, a
/// store outage is a retryable `503`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDenied {
    /// No session cookie, or one that names no live session.
    Unauthorized,
    /// The admin store could not be reached.
    StoreUnavailable,
}

impl IntoResponse for SessionDenied {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
            Self::StoreUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "the sign-in service is unavailable",
            )
                .into_response(),
        }
    }
}

/// Everything about the session a successful [`login`] mints, computed at the HTTP edge: the CSPRNG
/// token, how long it may idle, its absolute ceiling, and the client details captured for the admin's
/// own session list ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4).
///
/// `idle_ttl_secs` is the sliding idle window and `absolute_ttl_secs` the hard cap it can never pass;
/// a real guarded request slides the session forward by the idle window up to the cap
/// ([`authenticated_admin`]).
#[derive(Debug, Clone)]
pub struct SessionMint<'a> {
    /// The 256-bit CSPRNG session token, minted by the caller ([`crate::http`]); only its hash is
    /// stored, and the same token goes into the [`session`](super::session) cookie.
    pub token: &'a str,
    /// The idle window, in seconds: a session left idle longer than this expires.
    pub idle_ttl_secs: u64,
    /// The absolute ceiling, in seconds: a session can never live past `now + absolute_ttl_secs`,
    /// however active it is.
    pub absolute_ttl_secs: u64,
    /// The client IP the session is minted for, if known.
    pub ip: Option<&'a str>,
    /// The client user-agent the session is minted for, if known.
    pub user_agent: Option<&'a str>,
}

/// Authenticates a super-admin login and, on success, persists a session for `mint.token`.
///
/// Both factors are checked before any verdict ([`SuperAdminCredential::authenticate`]), the matched
/// TOTP step is burned *before* the session is written, and the session is stored as
/// `SHA-256(mint.token)` — never the token itself. `mint.token` is minted from a CSPRNG by the
/// caller ([`crate::http`]); the same token goes into the [`session`](super::session) cookie so the
/// browser can present it, and its hash is what this stores. The session carries a sliding idle TTL
/// (`mint.idle_ttl_secs`) bounded by an absolute cap (`mint.absolute_ttl_secs`).
///
/// # Errors
///
/// [`LoginDenied::Invalid`] for any credential problem (a single generic refusal), or
/// [`LoginDenied::StoreUnavailable`] if the store could not be read or written.
pub async fn login<A, C>(
    store: &A,
    clock: &C,
    request: &LoginRequest,
    mint: &SessionMint<'_>,
) -> Result<(), LoginDenied>
where
    A: AdminStore,
    C: ClockSource,
{
    let now = clock.now();
    let loaded = store
        .load_credential()
        .await
        .map_err(|_| LoginDenied::StoreUnavailable)?;
    let Some(admin) = loaded else {
        // No admin provisioned. There is exactly one super-admin and its existence is not a secret
        // (a real deployment always has one), so the generic refusal here reveals nothing — there is
        // nothing to enumerate.
        return Err(LoginDenied::Invalid);
    };
    // Both factors evaluated regardless of which is wrong, and every failure collapses to one
    // Invalid — the no-oracle rule (ADR-0034).
    let authenticated = admin
        .credential
        .authenticate(
            &request.password,
            &request.totp_code,
            unix_seconds(now),
            admin.last_used_totp_step,
        )
        .map_err(|_| LoginDenied::Invalid)?;
    // Burn the step first: even if the session write below is retried after a crash, this code — and
    // any earlier one — can never mint a second session.
    store
        .record_totp_step(authenticated.totp_step)
        .await
        .map_err(|_| LoginDenied::StoreUnavailable)?;
    // Bind the session to the acting admin. The single-super-admin credential maps to the one `owner`
    // ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)), so during the transition the
    // session belongs to the first active owner; a store with no owner row yet (the pure-credential
    // fakes, or a not-yet-seeded install) binds `None`, still a valid session. Email-based per-admin
    // login replaces this owner lookup in a later slice.
    let owner_id = acting_owner_id(store).await?;
    let absolute_expires_at = expiry(now, mint.absolute_ttl_secs);
    // The first idle boundary, never past the cap (which only matters under a misconfigured idle ≥
    // absolute — normal config has idle well below the cap, so this is just `now + idle`).
    let expires_at = min_timestamp(expiry(now, mint.idle_ttl_secs), absolute_expires_at);
    store
        .create_session(NewAdminSession {
            token_hash: hash_token(mint.token),
            created_at: now,
            expires_at,
            absolute_expires_at,
            idle_ttl_ms: millis_from_secs(mint.idle_ttl_secs),
            admin_id: owner_id,
            ip: mint.ip.map(str::to_owned),
            user_agent: mint.user_agent.map(str::to_owned),
        })
        .await
        .map_err(|_| LoginDenied::StoreUnavailable)?;
    Ok(())
}

/// The id of the admin a freshly-authenticated super-admin login belongs to: the first active
/// `owner`, or `None` if the store holds no admin rows yet. Transitional — a later slice authenticates
/// each admin by email and no longer infers the owner.
async fn acting_owner_id<A>(store: &A) -> Result<Option<String>, LoginDenied>
where
    A: AdminStore,
{
    let admins = store
        .list_admin_users()
        .await
        .map_err(|_| LoginDenied::StoreUnavailable)?;
    Ok(admins
        .into_iter()
        .find(|admin| admin.role == AdminRole::Owner && admin.status == AdminStatus::Active)
        .map(|admin| admin.id))
}

/// Verifies the session cookie on an incoming admin request, as of the clock's current instant.
///
/// The guard every `/admin` route past login uses: it reads the [`session`](super::session) cookie,
/// hashes it, and asks the store whether that session is live.
///
/// # Errors
///
/// [`SessionDenied::Unauthorized`] if the cookie is absent or names no live session;
/// [`SessionDenied::StoreUnavailable`] if the store could not be read.
pub async fn authenticate_session<A, C>(
    store: &A,
    clock: &C,
    headers: &HeaderMap,
) -> Result<(), SessionDenied>
where
    A: AdminStore,
    C: ClockSource,
{
    let token = session_token_from_cookies(headers).ok_or(SessionDenied::Unauthorized)?;
    let valid = store
        .session_is_valid(hash_token(token), clock.now())
        .await
        .map_err(|_| SessionDenied::StoreUnavailable)?;
    if valid {
        Ok(())
    } else {
        Err(SessionDenied::Unauthorized)
    }
}

/// Resolves the acting admin behind an incoming `/admin` request — the role-aware guard the
/// permission-gated routes stand behind ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
///
/// It reads the session cookie, confirms the session is live, and resolves the [`AdminUser`] it
/// belongs to. A session owned by a specific admin resolves to that admin — refused if the admin is
/// suspended or gone, so revoking access takes effect at once. A live legacy session (one minted
/// before multi-admin, with no `admin_id`) resolves to the first active `owner`, so an operator's
/// existing session keeps working across the upgrade. The returned [`AdminContext`] is what a route
/// checks its required [`ConsolePermission`](super::console_rbac::ConsolePermission) against.
///
/// # Errors
///
/// [`SessionDenied::Unauthorized`] if the cookie is absent, names no live session, or the session's
/// admin is suspended or gone; [`SessionDenied::StoreUnavailable`] if the store could not be read.
pub async fn authenticated_admin<A, C>(
    store: &A,
    clock: &C,
    headers: &HeaderMap,
) -> Result<AdminContext, SessionDenied>
where
    A: AdminStore,
    C: ClockSource,
{
    let token = session_token_from_cookies(headers).ok_or(SessionDenied::Unauthorized)?;
    let session = store
        .session_admin(hash_token(token), clock.now())
        .await
        .map_err(|_| SessionDenied::StoreUnavailable)?
        .ok_or(SessionDenied::Unauthorized)?;
    let admin = match session.admin_id {
        // A session bound to a specific admin resolves to that admin — and only while they are still
        // active, so a suspended or deleted admin's live sessions stop authorising at once.
        Some(id) => {
            let admin = store
                .get_admin_user(&id)
                .await
                .map_err(|_| SessionDenied::StoreUnavailable)?
                .ok_or(SessionDenied::Unauthorized)?;
            if admin.status != AdminStatus::Active {
                return Err(SessionDenied::Unauthorized);
            }
            admin
        }
        // A legacy session (minted before multi-admin) belongs to the sole owner.
        None => legacy_session_owner(store).await?,
    };
    Ok(AdminContext { admin })
}

/// Resolves the owner a legacy (pre-multi-admin) session belongs to. The first active `owner` if the
/// table has one; otherwise, only when there are **no admin rows at all** (a pristine install whose
/// `super_admin` was enrolled but not yet mirrored into `admin_users`), a synthetic implicit owner —
/// which is exactly who the single super-admin was, so a valid session is never locked out during the
/// upgrade. A populated table with no active owner is anomalous and refused rather than escalated.
async fn legacy_session_owner<A>(store: &A) -> Result<AdminUser, SessionDenied>
where
    A: AdminStore,
{
    let admins = store
        .list_admin_users()
        .await
        .map_err(|_| SessionDenied::StoreUnavailable)?;
    if let Some(owner) = admins
        .iter()
        .find(|admin| admin.role == AdminRole::Owner && admin.status == AdminStatus::Active)
    {
        Ok(owner.clone())
    } else if admins.is_empty() {
        Ok(implicit_owner())
    } else {
        Err(SessionDenied::Unauthorized)
    }
}

/// The synthetic owner a pristine install falls back to before its `super_admin` is mirrored into
/// `admin_users` — the same identity (id, placeholder email) the migration seeds, so the two agree.
fn implicit_owner() -> AdminUser {
    AdminUser {
        id: IMPLICIT_OWNER_ID.to_owned(),
        email: IMPLICIT_OWNER_EMAIL.to_owned(),
        name: "Owner".to_owned(),
        role: AdminRole::Owner,
        status: AdminStatus::Active,
    }
}

/// The stable sentinel id the migration gives the migrated `owner`, reused by [`implicit_owner`] so a
/// pristine install and a migrated one name the owner identically.
pub const IMPLICIT_OWNER_ID: &str = "00000000000000000000000000";
/// The synthetic, non-routable placeholder email the migrated/implicit owner carries until it is
/// replaced from the console.
pub const IMPLICIT_OWNER_EMAIL: &str = "owner@super-admin.invalid";

/// Revokes the session named by the request's cookie, for logout.
///
/// Idempotent: a request with no session cookie revokes nothing and still succeeds, so the caller
/// can always clear the client cookie afterwards.
///
/// # Errors
///
/// [`AdminStoreError`] if the store could not be written.
pub async fn logout<A>(store: &A, headers: &HeaderMap) -> Result<(), AdminStoreError>
where
    A: AdminStore,
{
    if let Some(token) = session_token_from_cookies(headers) {
        store.revoke_session(hash_token(token)).await?;
    }
    Ok(())
}

/// Reads the super-admin session token from the request's `Cookie` header(s), if present.
fn session_token_from_cookies(headers: &HeaderMap) -> Option<&str> {
    // A request may carry more than one `Cookie` header, and each may pack several `name=value` pairs
    // separated by `; ` — scan them all for ours.
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|header| header.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(name, value)| (name == COOKIE_NAME).then_some(value))
}

/// `SHA-256` of a session token — what the store keeps and looks up, never the token itself.
fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The stored form of a session token — `SHA-256(token)` — for callers that seed a session directly
/// (tests, or a future admin-tooling path) with the same transform the guard applies to the cookie.
#[must_use]
pub fn hash_session_token(token: &str) -> [u8; 32] {
    hash_token(token)
}

/// The token hash of the session the request's own cookie names, or `None` if it carries none — so a
/// handler can tell which of an admin's listed sessions is the current one, and exclude it from a
/// "sign out everywhere else" ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4).
#[must_use]
pub fn current_session_token_hash(headers: &HeaderMap) -> Option<[u8; 32]> {
    session_token_from_cookies(headers).map(hash_token)
}

/// The Unix-seconds value TOTP verification consumes, from a millisecond [`Timestamp`]. Clamped at the
/// epoch — a pre-epoch clock is nonsensical here and would only ever fail to verify.
fn unix_seconds(now: Timestamp) -> u64 {
    u64::try_from(now.as_milliseconds_since_epoch().max(0)).unwrap_or(0) / 1000
}

/// `now + ttl_secs`, saturating. On overflow past the representable range the expiry falls back to
/// `now` — an already-expired session, which fails safe (the login just has to be retried) rather
/// than minting one that never dies.
fn expiry(now: Timestamp, ttl_secs: u64) -> Timestamp {
    let at = now
        .as_milliseconds_since_epoch()
        .saturating_add(millis_from_secs(ttl_secs));
    Timestamp::from_milliseconds_since_epoch(at).unwrap_or(now)
}

/// `ttl_secs` as milliseconds, saturating at [`i64::MAX`] — the wire form the session's idle window
/// and expiries are computed in.
fn millis_from_secs(ttl_secs: u64) -> i64 {
    i64::try_from(ttl_secs.saturating_mul(1000)).unwrap_or(i64::MAX)
}

/// The earlier of two instants — so the first idle boundary never lands past the absolute cap.
fn min_timestamp(a: Timestamp, b: Timestamp) -> Timestamp {
    if a <= b { a } else { b }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use argon2::password_hash::SaltString;
    use axum::http::header::COOKIE;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse as _;

    use pos_fakes::FakeClock;
    use pos_proto::determinism::ClockSource as _;
    use pos_proto::time::Timestamp;

    use super::{
        AdminCredential, AdminInvite, AdminRole, AdminStatus, AdminStore, AdminStoreError,
        AdminUser, IMPLICIT_OWNER_ID, LoginDenied, LoginRequest, NewAdminInvite, NewAdminSession,
        NewAdminUser, SessionDenied, SessionMint, authenticate_session, authenticated_admin,
        hash_token, login, logout,
    };
    use crate::auth::SuperAdminCredential;
    use crate::auth::password::hash_password;
    use crate::auth::session::COOKIE_NAME;
    use crate::auth::totp::{DIGITS, TotpSecret, code_at};

    /// A fixed instant well past the epoch, so an issued session (with a positive TTL) is live.
    const NOW_MS: i64 = 1_700_000_000_000;
    /// The obviously-fake TOTP seed shared by the tests; never real key material.
    const TOTP_SEED: &[u8] = b"12345678901234567890123456789012";
    /// The one-hour session TTL the tests issue against.
    const TTL: u64 = 3600;

    fn clock() -> FakeClock {
        FakeClock::new(Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"))
    }

    fn provisioned_credential() -> SuperAdminCredential {
        let salt = SaltString::encode_b64(b"a-fixed-test-salt").expect("salt");
        let phc = hash_password("a-strong-passphrase", &salt).expect("hash");
        SuperAdminCredential::new(phc, TotpSecret::new(TOTP_SEED.to_vec()))
    }

    /// The current valid code for the shared seed at `NOW_MS`.
    fn current_code() -> String {
        code_at(
            &TotpSecret::new(TOTP_SEED.to_vec()),
            u64::try_from(NOW_MS).expect("positive") / 1000,
            DIGITS,
        )
    }

    fn request(password: &str, totp_code: &str) -> LoginRequest {
        LoginRequest {
            password: password.to_owned(),
            totp_code: totp_code.to_owned(),
        }
    }

    fn cookie_header(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("{COOKIE_NAME}={token}")).expect("valid header"),
        );
        headers
    }

    /// A session-mint with the idle window equal to the absolute cap (both `TTL`) and no client
    /// details — the fixed-TTL shape the pre-slice-4 tests expect, so a session neither slides beyond
    /// nor short of `TTL`.
    fn mint(token: &str) -> SessionMint<'_> {
        SessionMint {
            token,
            idle_ttl_secs: TTL,
            absolute_ttl_secs: TTL,
            ip: None,
            user_agent: None,
        }
    }

    /// Seeds a session row directly (bypassing login), for the guard tests: created at `NOW_MS`, with
    /// an idle window of `TTL` and its absolute cap at `expires_at`, owned by `admin_id`.
    fn seed_session(token: &str, expires_at: Timestamp, admin_id: Option<&str>) -> NewAdminSession {
        NewAdminSession {
            token_hash: hash_token(token),
            created_at: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"),
            expires_at,
            absolute_expires_at: expires_at,
            idle_ttl_ms: i64::try_from(TTL * 1000).expect("fits an i64"),
            admin_id: admin_id.map(str::to_owned),
            ip: None,
            user_agent: None,
        }
    }

    /// A stored session row, keyed in the table by `SHA-256(token)`: its sliding expiry, the absolute
    /// cap and idle window that drive the slide, the id of the admin it belongs to (`None` for a
    /// legacy session), and the client details captured for the admin's own session list.
    #[derive(Clone)]
    struct SessionRow {
        created_at: Timestamp,
        expires_at: Timestamp,
        absolute_expires_at: Option<Timestamp>,
        idle_ttl_ms: Option<i64>,
        admin_id: Option<String>,
        ip: Option<String>,
        user_agent: Option<String>,
    }

    type SessionRows = HashMap<[u8; 32], SessionRow>;

    /// A stored invitation row in the fake, keyed for acceptance by its token hash.
    #[derive(Clone)]
    struct StoredInvite {
        id: String,
        email: String,
        name: String,
        role: AdminRole,
        invited_by: String,
        token_hash: [u8; 32],
        expires_at: Timestamp,
        accepted: bool,
    }

    /// An in-memory admin store: at most one legacy credential, the multi-admin `admin_users`
    /// table, an invitations table, a session table, and a down switch.
    #[derive(Default)]
    struct FakeAdmin {
        credential: Mutex<Option<SuperAdminCredential>>,
        last_used_totp_step: Mutex<Option<u64>>,
        sessions: Mutex<SessionRows>,
        admin_users: Mutex<Vec<AdminUser>>,
        invites: Mutex<Vec<StoredInvite>>,
        down: bool,
    }

    impl FakeAdmin {
        fn provisioned() -> Self {
            Self {
                credential: Mutex::new(Some(provisioned_credential())),
                ..Self::default()
            }
        }

        fn unavailable() -> Self {
            Self {
                down: true,
                ..Self::default()
            }
        }

        fn recorded_step(&self) -> Option<u64> {
            *self.last_used_totp_step.lock().expect("lock")
        }
    }

    impl AdminStore for FakeAdmin {
        async fn load_credential(&self) -> Result<Option<AdminCredential>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            Ok(self
                .credential
                .lock()
                .expect("lock")
                .clone()
                .map(|credential| AdminCredential {
                    credential,
                    last_used_totp_step: *self.last_used_totp_step.lock().expect("lock"),
                }))
        }

        async fn provision_credential(
            &self,
            password_phc: String,
            totp_secret: Vec<u8>,
        ) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut slot = self.credential.lock().expect("lock");
            if slot.is_some() {
                return Ok(false);
            }
            *slot = Some(SuperAdminCredential::new(
                password_phc,
                TotpSecret::new(totp_secret),
            ));
            Ok(true)
        }

        async fn record_totp_step(&self, step: u64) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut last = self.last_used_totp_step.lock().expect("lock");
            // Monotonic, exactly as the SQL `UPDATE ... WHERE step < $1` is.
            if last.is_none_or(|current| step > current) {
                *last = Some(step);
            }
            Ok(())
        }

        async fn create_session(&self, session: NewAdminSession) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            self.sessions.lock().expect("lock").insert(
                session.token_hash,
                SessionRow {
                    created_at: session.created_at,
                    expires_at: session.expires_at,
                    absolute_expires_at: Some(session.absolute_expires_at),
                    idle_ttl_ms: Some(session.idle_ttl_ms),
                    admin_id: session.admin_id,
                    ip: session.ip,
                    user_agent: session.user_agent,
                },
            );
            Ok(())
        }

        async fn session_is_valid(
            &self,
            token_hash: [u8; 32],
            now: Timestamp,
        ) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            // A pure read — no sliding, exactly as the SQL `SELECT EXISTS(...)` is.
            Ok(self
                .sessions
                .lock()
                .expect("lock")
                .get(&token_hash)
                .is_some_and(|row| row.expires_at > now))
        }

        async fn session_admin(
            &self,
            token_hash: [u8; 32],
            now: Timestamp,
        ) -> Result<Option<super::LiveSession>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut sessions = self.sessions.lock().expect("lock");
            let Some(row) = sessions
                .get_mut(&token_hash)
                .filter(|row| row.expires_at > now)
            else {
                return Ok(None);
            };
            // Slide the idle TTL up to the absolute cap, exactly as the SQL `UPDATE ... SET expires_at
            // = LEAST(now + idle_ttl_ms, absolute_expires_at)` does; a legacy row (either column
            // `None`) is left untouched.
            if let (Some(cap), Some(idle_ms)) = (row.absolute_expires_at, row.idle_ttl_ms) {
                let slid = Timestamp::from_milliseconds_since_epoch(
                    now.as_milliseconds_since_epoch().saturating_add(idle_ms),
                )
                .unwrap_or(now);
                row.expires_at = super::min_timestamp(slid, cap);
            }
            Ok(Some(super::LiveSession {
                admin_id: row.admin_id.clone(),
            }))
        }

        async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            self.sessions.lock().expect("lock").remove(&token_hash);
            Ok(())
        }

        async fn list_admin_sessions(
            &self,
            admin_id: &str,
            now: Timestamp,
        ) -> Result<Vec<super::SessionSummary>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut summaries: Vec<super::SessionSummary> = self
                .sessions
                .lock()
                .expect("lock")
                .iter()
                .filter(|(_, row)| {
                    row.admin_id.as_deref() == Some(admin_id) && row.expires_at > now
                })
                .map(|(token_hash, row)| super::SessionSummary {
                    token_hash: *token_hash,
                    ip: row.ip.clone(),
                    user_agent: row.user_agent.clone(),
                    created_at: row.created_at,
                    expires_at: row.expires_at,
                })
                .collect();
            // Newest first, then by handle — the same total order the SQL `ORDER BY created_at DESC,
            // token_hash` gives.
            summaries.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.token_hash.cmp(&b.token_hash))
            });
            Ok(summaries)
        }

        async fn revoke_admin_session(
            &self,
            admin_id: &str,
            token_hash: [u8; 32],
        ) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut sessions = self.sessions.lock().expect("lock");
            // Scoped: only a session that both matches and is owned by this admin is removed.
            if sessions
                .get(&token_hash)
                .is_some_and(|row| row.admin_id.as_deref() == Some(admin_id))
            {
                sessions.remove(&token_hash);
                Ok(true)
            } else {
                Ok(false)
            }
        }

        async fn revoke_other_admin_sessions(
            &self,
            admin_id: &str,
            except_token_hash: [u8; 32],
        ) -> Result<u64, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut sessions = self.sessions.lock().expect("lock");
            let before = sessions.len();
            sessions.retain(|token_hash, row| {
                row.admin_id.as_deref() != Some(admin_id) || *token_hash == except_token_hash
            });
            Ok(u64::try_from(before - sessions.len()).unwrap_or(u64::MAX))
        }

        async fn create_admin_user(&self, user: NewAdminUser) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut users = self.admin_users.lock().expect("lock");
            // Case-insensitive uniqueness, exactly as the `lower(email)` unique index enforces.
            if users
                .iter()
                .any(|existing| existing.email.eq_ignore_ascii_case(&user.email))
            {
                return Ok(false);
            }
            users.push(AdminUser {
                id: user.id,
                email: user.email,
                name: user.name,
                role: user.role,
                status: AdminStatus::Active,
            });
            Ok(true)
        }

        async fn list_admin_users(&self) -> Result<Vec<AdminUser>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            Ok(self.admin_users.lock().expect("lock").clone())
        }

        async fn get_admin_user(&self, id: &str) -> Result<Option<AdminUser>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            Ok(self
                .admin_users
                .lock()
                .expect("lock")
                .iter()
                .find(|user| user.id == id)
                .cloned())
        }

        async fn find_admin_user_by_email(
            &self,
            email: &str,
        ) -> Result<Option<AdminUser>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            Ok(self
                .admin_users
                .lock()
                .expect("lock")
                .iter()
                .find(|user| user.email.eq_ignore_ascii_case(email))
                .cloned())
        }

        async fn set_admin_user_role(
            &self,
            id: &str,
            role: AdminRole,
        ) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut users = self.admin_users.lock().expect("lock");
            match users.iter_mut().find(|user| user.id == id) {
                Some(user) => {
                    user.role = role;
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn set_admin_user_status(
            &self,
            id: &str,
            status: AdminStatus,
        ) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut users = self.admin_users.lock().expect("lock");
            match users.iter_mut().find(|user| user.id == id) {
                Some(user) => {
                    user.status = status;
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn count_active_owners(&self) -> Result<u64, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let count = self
                .admin_users
                .lock()
                .expect("lock")
                .iter()
                .filter(|user| user.role == AdminRole::Owner && user.status == AdminStatus::Active)
                .count();
            Ok(u64::try_from(count).unwrap_or(u64::MAX))
        }

        async fn create_invite(&self, invite: NewAdminInvite) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            self.invites.lock().expect("lock").push(StoredInvite {
                id: invite.id,
                email: invite.email,
                name: invite.name,
                role: invite.role,
                invited_by: invite.invited_by,
                token_hash: invite.token_hash,
                expires_at: invite.expires_at,
                accepted: false,
            });
            Ok(())
        }

        async fn find_pending_invite_by_token(
            &self,
            token_hash: [u8; 32],
            now: Timestamp,
        ) -> Result<Option<AdminInvite>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            Ok(self
                .invites
                .lock()
                .expect("lock")
                .iter()
                .find(|invite| {
                    invite.token_hash == token_hash && !invite.accepted && invite.expires_at > now
                })
                .map(stored_invite_to_domain))
        }

        async fn mark_invite_accepted(
            &self,
            id: &str,
            _accepted_at: Timestamp,
        ) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut invites = self.invites.lock().expect("lock");
            match invites
                .iter_mut()
                .find(|invite| invite.id == id && !invite.accepted)
            {
                Some(invite) => {
                    invite.accepted = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn list_pending_invites(
            &self,
            now: Timestamp,
        ) -> Result<Vec<AdminInvite>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            Ok(self
                .invites
                .lock()
                .expect("lock")
                .iter()
                .filter(|invite| !invite.accepted && invite.expires_at > now)
                .map(stored_invite_to_domain)
                .collect())
        }

        async fn revoke_invite(&self, id: &str) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut invites = self.invites.lock().expect("lock");
            let before = invites.len();
            invites.retain(|invite| invite.id != id || invite.accepted);
            Ok(invites.len() != before)
        }
    }

    /// Projects a stored fake invite into the domain [`AdminInvite`] (no token crosses the boundary).
    fn stored_invite_to_domain(invite: &StoredInvite) -> AdminInvite {
        AdminInvite {
            id: invite.id.clone(),
            email: invite.email.clone(),
            name: invite.name.clone(),
            role: invite.role,
            invited_by: invite.invited_by.clone(),
            accepted: invite.accepted,
        }
    }

    #[tokio::test]
    async fn a_correct_login_issues_a_session_the_guard_then_accepts() {
        let store = FakeAdmin::provisioned();
        let clock = clock();

        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            &mint("session-token-abc"),
        )
        .await
        .expect("both factors are correct");

        // The guard accepts the issued token, and refuses a token it never minted.
        authenticate_session(&store, &clock, &cookie_header("session-token-abc"))
            .await
            .expect("the issued session is live");
        assert_eq!(
            authenticate_session(&store, &clock, &cookie_header("some-other-token")).await,
            Err(SessionDenied::Unauthorized),
            "a token the store never issued names no session"
        );
    }

    #[tokio::test]
    async fn a_wrong_password_and_a_wrong_code_are_the_same_refusal() {
        let store = FakeAdmin::provisioned();
        let clock = clock();

        assert_eq!(
            login(
                &store,
                &clock,
                &request("wrong-passphrase", &current_code()),
                &mint("t1"),
            )
            .await,
            Err(LoginDenied::Invalid),
            "a wrong password is a generic refusal"
        );
        assert_eq!(
            login(
                &store,
                &clock,
                &request("a-strong-passphrase", "000000"),
                &mint("t2"),
            )
            .await,
            Err(LoginDenied::Invalid),
            "a wrong code is the same generic refusal — the two cannot be told apart"
        );
    }

    #[tokio::test]
    async fn a_totp_code_cannot_be_replayed_to_mint_a_second_session() {
        let store = FakeAdmin::provisioned();
        let clock = clock();
        let code = current_code();

        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &code),
            &mint("first"),
        )
        .await
        .expect("first login");
        let burned = store.recorded_step().expect("a step was recorded");

        // The same code again — the step is now spent, so it is a replay and refused generically.
        assert_eq!(
            login(
                &store,
                &clock,
                &request("a-strong-passphrase", &code),
                &mint("second"),
            )
            .await,
            Err(LoginDenied::Invalid),
            "a replayed code cannot mint a second session"
        );
        assert_eq!(
            store.recorded_step(),
            Some(burned),
            "the recorded step did not move on a refused login"
        );
    }

    #[tokio::test]
    async fn a_store_outage_is_retryable_not_a_denial() {
        let store = FakeAdmin::unavailable();
        let clock = clock();
        assert_eq!(
            login(
                &store,
                &clock,
                &request("a-strong-passphrase", &current_code()),
                &mint("t"),
            )
            .await,
            Err(LoginDenied::StoreUnavailable),
            "a store outage must not masquerade as a bad credential"
        );
    }

    #[tokio::test]
    async fn a_login_against_an_unprovisioned_admin_is_a_generic_refusal() {
        let store = FakeAdmin::default();
        let clock = clock();
        assert_eq!(
            login(
                &store,
                &clock,
                &request("a-strong-passphrase", &current_code()),
                &mint("t"),
            )
            .await,
            Err(LoginDenied::Invalid),
            "an absent admin is the same generic 401, never a distinct 'not provisioned'"
        );
    }

    #[tokio::test]
    async fn an_expired_session_is_unauthorised() {
        let store = FakeAdmin::provisioned();
        let clock = clock();
        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            &mint("expiring"),
        )
        .await
        .expect("login");

        // Move the clock past the one-hour TTL: the same cookie now names an expired session.
        let past_ttl_ms = NOW_MS + i64::try_from((TTL + 1) * 1000).expect("fits an i64");
        clock.set(Timestamp::from_milliseconds_since_epoch(past_ttl_ms).expect("valid"));
        assert_eq!(
            authenticate_session(&store, &clock, &cookie_header("expiring")).await,
            Err(SessionDenied::Unauthorized),
            "a session past its TTL no longer authenticates"
        );
    }

    #[tokio::test]
    async fn logout_revokes_the_session_and_is_idempotent() {
        let store = FakeAdmin::provisioned();
        let clock = clock();
        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            &mint("live"),
        )
        .await
        .expect("login");

        logout(&store, &cookie_header("live"))
            .await
            .expect("revoke");
        assert_eq!(
            authenticate_session(&store, &clock, &cookie_header("live")).await,
            Err(SessionDenied::Unauthorized),
            "a revoked session no longer authenticates"
        );
        // No cookie at all still succeeds, so the logout route can always clear the client cookie.
        logout(&store, &HeaderMap::new())
            .await
            .expect("logout with no cookie is a no-op");
    }

    #[test]
    fn a_missing_session_cookie_is_unauthorised_and_the_request_debug_hides_the_password() {
        assert_eq!(
            LoginDenied::Invalid.into_response().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            LoginDenied::StoreUnavailable.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            SessionDenied::Unauthorized.into_response().status(),
            StatusCode::UNAUTHORIZED
        );

        let rendered = format!("{:?}", request("hunter2", "123456"));
        assert!(
            !rendered.contains("hunter2"),
            "the password leaked into Debug: {rendered}"
        );
    }

    #[test]
    fn the_token_hash_is_stable_and_not_the_token() {
        let hash = hash_token("session-token-abc");
        assert_eq!(hash, hash_token("session-token-abc"), "hashing is stable");
        assert_ne!(
            hash,
            hash_token("session-token-abd"),
            "a different token hashes differently"
        );
    }

    // ---- Multi-admin surface ([ADR-0067]) ----

    /// A new-admin input with obviously-fake credential material — never real key bytes.
    fn new_admin(id: &str, email: &str, name: &str, role: AdminRole) -> NewAdminUser {
        NewAdminUser {
            id: id.to_owned(),
            email: email.to_owned(),
            name: name.to_owned(),
            role,
            password_phc: "$argon2id$not-a-real-hash".to_owned(),
            totp_secret: b"not-a-real-totp-secret".to_vec(),
        }
    }

    #[tokio::test]
    async fn admin_users_are_created_listed_and_fetched() {
        let store = FakeAdmin::default();
        assert!(
            store
                .create_admin_user(new_admin(
                    "id-owner",
                    "owner@example.test",
                    "Owner",
                    AdminRole::Owner
                ))
                .await
                .expect("store up")
        );
        assert!(
            store
                .create_admin_user(new_admin(
                    "id-ops",
                    "ops@example.test",
                    "Ops",
                    AdminRole::Ops
                ))
                .await
                .expect("store up")
        );

        assert_eq!(store.list_admin_users().await.expect("list").len(), 2);

        let fetched = store
            .get_admin_user("id-ops")
            .await
            .expect("get")
            .expect("present");
        assert_eq!(fetched.email, "ops@example.test");
        assert_eq!(fetched.role, AdminRole::Ops);
        assert_eq!(
            fetched.status,
            AdminStatus::Active,
            "a freshly created admin starts active"
        );
        assert!(
            store
                .get_admin_user("id-nobody")
                .await
                .expect("get")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_duplicate_email_is_refused_case_insensitively() {
        let store = FakeAdmin::default();
        assert!(
            store
                .create_admin_user(new_admin(
                    "id-1",
                    "Person@Example.test",
                    "P",
                    AdminRole::Admin
                ))
                .await
                .expect("store up")
        );
        assert!(
            !store
                .create_admin_user(new_admin(
                    "id-2",
                    "person@example.test",
                    "P2",
                    AdminRole::Viewer
                ))
                .await
                .expect("store up"),
            "the same address in a different case is the same identity"
        );
        assert_eq!(store.list_admin_users().await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn find_by_email_is_case_insensitive() {
        let store = FakeAdmin::default();
        store
            .create_admin_user(new_admin(
                "id-1",
                "boss@example.test",
                "Boss",
                AdminRole::Owner,
            ))
            .await
            .expect("store up");
        let found = store
            .find_admin_user_by_email("BOSS@EXAMPLE.TEST")
            .await
            .expect("find")
            .expect("present");
        assert_eq!(found.id, "id-1");
        assert!(
            store
                .find_admin_user_by_email("nobody@example.test")
                .await
                .expect("find")
                .is_none()
        );
    }

    #[tokio::test]
    async fn role_and_status_updates_apply_and_a_missing_id_is_false() {
        let store = FakeAdmin::default();
        store
            .create_admin_user(new_admin("id-1", "a@example.test", "A", AdminRole::Viewer))
            .await
            .expect("store up");
        assert!(
            store
                .set_admin_user_role("id-1", AdminRole::Admin)
                .await
                .expect("store up")
        );
        assert!(
            store
                .set_admin_user_status("id-1", AdminStatus::Suspended)
                .await
                .expect("store up")
        );
        let user = store
            .get_admin_user("id-1")
            .await
            .expect("get")
            .expect("present");
        assert_eq!(user.role, AdminRole::Admin);
        assert_eq!(user.status, AdminStatus::Suspended);

        assert!(
            !store
                .set_admin_user_role("id-nobody", AdminRole::Ops)
                .await
                .expect("store up"),
            "updating a role for a missing id changes nothing"
        );
        assert!(
            !store
                .set_admin_user_status("id-nobody", AdminStatus::Active)
                .await
                .expect("store up")
        );
    }

    #[tokio::test]
    async fn count_active_owners_tracks_role_and_status() {
        let store = FakeAdmin::default();
        assert_eq!(store.count_active_owners().await.expect("count"), 0);
        for (id, email, role) in [
            ("id-1", "o1@example.test", AdminRole::Owner),
            ("id-2", "o2@example.test", AdminRole::Owner),
            ("id-3", "a@example.test", AdminRole::Admin),
        ] {
            store
                .create_admin_user(new_admin(id, email, "N", role))
                .await
                .expect("store up");
        }
        assert_eq!(store.count_active_owners().await.expect("count"), 2);

        // Suspending one owner and demoting the other would leave zero active owners — the count is
        // what a caller consults to refuse that last step.
        store
            .set_admin_user_status("id-1", AdminStatus::Suspended)
            .await
            .expect("store up");
        assert_eq!(store.count_active_owners().await.expect("count"), 1);
        store
            .set_admin_user_role("id-2", AdminRole::Admin)
            .await
            .expect("store up");
        assert_eq!(store.count_active_owners().await.expect("count"), 0);
    }

    #[tokio::test]
    async fn a_store_outage_surfaces_on_the_multi_admin_methods() {
        let store = FakeAdmin::unavailable();
        assert!(store.list_admin_users().await.is_err());
        assert!(
            store
                .create_admin_user(new_admin("id-1", "a@example.test", "A", AdminRole::Ops))
                .await
                .is_err()
        );
        assert!(store.count_active_owners().await.is_err());
    }

    #[test]
    fn role_and_status_tokens_round_trip() {
        for role in AdminRole::ALL {
            assert_eq!(AdminRole::from_token(role.as_token()), Some(*role));
        }
        assert_eq!(
            AdminRole::from_token("root"),
            None,
            "an unknown role fails closed"
        );

        assert_eq!(
            AdminStatus::from_token(AdminStatus::Active.as_token()),
            Some(AdminStatus::Active)
        );
        assert_eq!(
            AdminStatus::from_token(AdminStatus::Suspended.as_token()),
            Some(AdminStatus::Suspended)
        );
        assert_eq!(AdminStatus::from_token("nope"), None);
    }

    #[test]
    fn new_admin_user_debug_redacts_the_credential() {
        let rendered = format!(
            "{:?}",
            new_admin("id-1", "a@example.test", "A", AdminRole::Owner)
        );
        assert!(
            !rendered.contains("argon2id") && !rendered.contains("not-a-real-totp-secret"),
            "a secret leaked into Debug: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
    }

    // ---- The role-aware guard ([ADR-0067]) ----

    /// A live expiry an hour past `NOW_MS`, so a seeded session is valid at `clock()`.
    fn live_expiry() -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(NOW_MS + 3_600_000).expect("valid")
    }

    async fn seed_admin(store: &FakeAdmin, id: &str, email: &str, role: AdminRole) {
        store
            .create_admin_user(new_admin(id, email, "N", role))
            .await
            .expect("seed admin");
    }

    #[tokio::test]
    async fn a_session_resolves_to_its_own_admins_role() {
        let store = FakeAdmin::default();
        seed_admin(&store, "id-owner", "owner@example.test", AdminRole::Owner).await;
        seed_admin(
            &store,
            "id-viewer",
            "viewer@example.test",
            AdminRole::Viewer,
        )
        .await;
        store
            .create_session(seed_session("tok-viewer", live_expiry(), Some("id-viewer")))
            .await
            .expect("seed session");

        let context = authenticated_admin(&store, &clock(), &cookie_header("tok-viewer"))
            .await
            .expect("a live session for an active admin resolves");
        assert_eq!(context.admin.id, "id-viewer");
        assert_eq!(context.admin.role, AdminRole::Viewer);
    }

    #[tokio::test]
    async fn a_session_for_a_suspended_admin_is_refused() {
        let store = FakeAdmin::default();
        seed_admin(&store, "id-1", "a@example.test", AdminRole::Admin).await;
        store
            .set_admin_user_status("id-1", AdminStatus::Suspended)
            .await
            .expect("suspend");
        store
            .create_session(seed_session("tok", live_expiry(), Some("id-1")))
            .await
            .expect("seed session");
        assert_eq!(
            authenticated_admin(&store, &clock(), &cookie_header("tok")).await,
            Err(SessionDenied::Unauthorized),
            "a suspended admin's live session no longer authorises"
        );
    }

    #[tokio::test]
    async fn a_session_for_a_missing_admin_is_refused() {
        let store = FakeAdmin::default();
        store
            .create_session(seed_session("tok", live_expiry(), Some("ghost")))
            .await
            .expect("seed session");
        assert_eq!(
            authenticated_admin(&store, &clock(), &cookie_header("tok")).await,
            Err(SessionDenied::Unauthorized)
        );
    }

    #[tokio::test]
    async fn a_legacy_session_resolves_to_the_active_owner() {
        let store = FakeAdmin::default();
        seed_admin(&store, "id-owner", "owner@example.test", AdminRole::Owner).await;
        // A legacy session carries no admin_id.
        store
            .create_session(seed_session("legacy", live_expiry(), None))
            .await
            .expect("seed session");
        let context = authenticated_admin(&store, &clock(), &cookie_header("legacy"))
            .await
            .expect("a legacy session resolves to the owner");
        assert_eq!(context.admin.id, "id-owner");
        assert_eq!(context.admin.role, AdminRole::Owner);
    }

    #[tokio::test]
    async fn a_legacy_session_on_a_pristine_store_resolves_to_the_implicit_owner() {
        let store = FakeAdmin::default(); // no admin_users rows at all
        store
            .create_session(seed_session("legacy", live_expiry(), None))
            .await
            .expect("seed session");
        let context = authenticated_admin(&store, &clock(), &cookie_header("legacy"))
            .await
            .expect("a pristine store falls back to the implicit owner");
        assert_eq!(context.admin.id, IMPLICIT_OWNER_ID);
        assert_eq!(context.admin.role, AdminRole::Owner);
    }

    #[tokio::test]
    async fn an_absent_session_is_unauthorised() {
        let store = FakeAdmin::default();
        seed_admin(&store, "id-owner", "owner@example.test", AdminRole::Owner).await;
        assert_eq!(
            authenticated_admin(&store, &clock(), &cookie_header("never-issued")).await,
            Err(SessionDenied::Unauthorized)
        );
    }

    #[tokio::test]
    async fn login_binds_the_new_session_to_the_owner() {
        let store = FakeAdmin::provisioned();
        seed_admin(&store, "id-owner", "owner@example.test", AdminRole::Owner).await;
        login(
            &store,
            &clock(),
            &request("a-strong-passphrase", &current_code()),
            &mint("fresh-token"),
        )
        .await
        .expect("login");
        // The guard resolves the freshly-minted session straight to the owner it was bound to.
        let context = authenticated_admin(&store, &clock(), &cookie_header("fresh-token"))
            .await
            .expect("the issued session authorises");
        assert_eq!(context.admin.id, "id-owner");
        assert_eq!(context.admin.role, AdminRole::Owner);
    }

    // ---- Sessions: sliding idle TTL, listing, revocation ([ADR-0067] slice 4) ----

    /// A mint with a short idle window inside a longer absolute cap, and no client details.
    fn sliding_mint(token: &str, idle_secs: u64, absolute_secs: u64) -> SessionMint<'_> {
        SessionMint {
            token,
            idle_ttl_secs: idle_secs,
            absolute_ttl_secs: absolute_secs,
            ip: None,
            user_agent: None,
        }
    }

    /// An instant `ms` milliseconds past `NOW_MS`.
    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(NOW_MS + ms).expect("valid")
    }

    #[tokio::test]
    async fn a_real_request_slides_the_idle_ttl_up_to_the_absolute_cap() {
        let store = FakeAdmin::provisioned();
        seed_admin(&store, "id-owner", "owner@example.test", AdminRole::Owner).await;
        let clock = clock();
        // Idle window 60s, absolute cap 150s — close enough that continuous activity reaches the cap.
        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            &sliding_mint("tok", 60, 150),
        )
        .await
        .expect("login");

        // A real guarded request 50s in (within the idle window) slides the expiry to now + 60s = 110s.
        clock.set(at(50_000));
        authenticated_admin(&store, &clock, &cookie_header("tok"))
            .await
            .expect("a live session resolves and slides");
        // 100s in: past the original 60s boundary, but the slide moved it to 110s, so it is still live.
        clock.set(at(100_000));
        assert!(
            store
                .session_is_valid(hash_token("tok"), clock.now())
                .await
                .expect("read"),
            "a slid session outlives its original idle boundary"
        );

        // Keep acting at 100s: a slide would reach 160s, but the absolute cap clamps it to 150s.
        authenticated_admin(&store, &clock, &cookie_header("tok"))
            .await
            .expect("still live, slide clamped to the cap");
        clock.set(at(149_000));
        assert!(
            store
                .session_is_valid(hash_token("tok"), clock.now())
                .await
                .expect("read"),
            "live just under the absolute cap"
        );
        clock.set(at(150_001));
        assert!(
            !store
                .session_is_valid(hash_token("tok"), clock.now())
                .await
                .expect("read"),
            "no amount of sliding lets a session outlive its absolute cap"
        );
    }

    #[tokio::test]
    async fn an_idle_session_times_out_and_the_poll_does_not_keep_it_alive() {
        let store = FakeAdmin::provisioned();
        seed_admin(&store, "id-owner", "owner@example.test", AdminRole::Owner).await;
        let clock = clock();
        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            &sliding_mint("tok", 60, 3600),
        )
        .await
        .expect("login");

        // The liveness poll at 50s sees a live session but must not slide it.
        clock.set(at(50_000));
        authenticate_session(&store, &clock, &cookie_header("tok"))
            .await
            .expect("the poll sees a live session");

        // 61s in — past the idle window, with no *real* request to slide it — the session is gone,
        // proving the poll did not extend it.
        clock.set(at(61_000));
        assert_eq!(
            authenticate_session(&store, &clock, &cookie_header("tok")).await,
            Err(SessionDenied::Unauthorized),
            "an idle session times out; the liveness poll does not keep it alive"
        );
    }

    #[tokio::test]
    async fn an_admin_lists_and_revokes_only_their_own_sessions() {
        let store = FakeAdmin::default();
        // Two sessions for one admin, one for another — seeded directly (login is single-use per code).
        store
            .create_session(seed_session("mine-a", live_expiry(), Some("id-1")))
            .await
            .expect("seed");
        store
            .create_session(seed_session("mine-b", live_expiry(), Some("id-1")))
            .await
            .expect("seed");
        store
            .create_session(seed_session("theirs", live_expiry(), Some("id-2")))
            .await
            .expect("seed");

        let now = clock().now();
        let mine = store.list_admin_sessions("id-1", now).await.expect("list");
        assert_eq!(mine.len(), 2, "an admin sees only their own sessions");

        // Revoking one of mine is scoped and removes exactly that one.
        assert!(
            store
                .revoke_admin_session("id-1", hash_token("mine-a"))
                .await
                .expect("revoke")
        );
        assert_eq!(
            store
                .list_admin_sessions("id-1", now)
                .await
                .expect("list")
                .len(),
            1
        );

        // I cannot revoke another admin's session: the handle is theirs, so it is a no-op for me.
        assert!(
            !store
                .revoke_admin_session("id-1", hash_token("theirs"))
                .await
                .expect("revoke"),
            "revocation is scoped to the caller's own sessions"
        );
        assert_eq!(
            store
                .list_admin_sessions("id-2", now)
                .await
                .expect("list")
                .len(),
            1,
            "the other admin's session is untouched"
        );
    }

    #[tokio::test]
    async fn revoke_others_keeps_the_current_session_only() {
        let store = FakeAdmin::default();
        for token in ["current", "phone", "laptop"] {
            store
                .create_session(seed_session(token, live_expiry(), Some("id-1")))
                .await
                .expect("seed");
        }
        // "Sign out everywhere else" leaves exactly the current session.
        let revoked = store
            .revoke_other_admin_sessions("id-1", hash_token("current"))
            .await
            .expect("revoke others");
        assert_eq!(revoked, 2, "the two other sessions were revoked");

        let now = clock().now();
        let remaining = store.list_admin_sessions("id-1", now).await.expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].token_hash,
            hash_token("current"),
            "the current session survives sign-out-everywhere-else"
        );
    }

    #[tokio::test]
    async fn login_records_the_client_ip_and_user_agent_for_the_session_list() {
        let store = FakeAdmin::provisioned();
        seed_admin(&store, "id-owner", "owner@example.test", AdminRole::Owner).await;
        let clock = clock();
        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            &SessionMint {
                token: "tok",
                idle_ttl_secs: 60,
                absolute_ttl_secs: 3600,
                ip: Some("203.0.113.7"),
                user_agent: Some("Mozilla/5.0 (console)"),
            },
        )
        .await
        .expect("login");

        let sessions = store
            .list_admin_sessions("id-owner", clock.now())
            .await
            .expect("list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(
            sessions[0].user_agent.as_deref(),
            Some("Mozilla/5.0 (console)")
        );
    }

    // ---- Invitations ([ADR-0067]) ----

    fn new_invite(
        id: &str,
        email: &str,
        role: AdminRole,
        token: &str,
        expires_at: Timestamp,
    ) -> NewAdminInvite {
        NewAdminInvite {
            id: id.to_owned(),
            email: email.to_owned(),
            name: "N".to_owned(),
            role,
            token_hash: hash_token(token),
            invited_by: "id-owner".to_owned(),
            expires_at,
        }
    }

    #[tokio::test]
    async fn a_pending_invite_is_found_by_its_token_and_accepted_once() {
        let store = FakeAdmin::default();
        store
            .create_invite(new_invite(
                "inv-1",
                "new@example.test",
                AdminRole::Ops,
                "tok-a",
                live_expiry(),
            ))
            .await
            .expect("create invite");

        let found = store
            .find_pending_invite_by_token(hash_token("tok-a"), clock().now())
            .await
            .expect("find")
            .expect("present");
        assert_eq!(found.id, "inv-1");
        assert_eq!(found.email, "new@example.test");
        assert_eq!(found.role, AdminRole::Ops);
        assert!(!found.accepted);

        // Claiming the invite is single-use: the first call wins, the second refuses, and it is no
        // longer pending.
        assert!(
            store
                .mark_invite_accepted("inv-1", clock().now())
                .await
                .expect("accept")
        );
        assert!(
            !store
                .mark_invite_accepted("inv-1", clock().now())
                .await
                .expect("second accept"),
            "an invite cannot be accepted twice"
        );
        assert!(
            store
                .find_pending_invite_by_token(hash_token("tok-a"), clock().now())
                .await
                .expect("find")
                .is_none(),
            "an accepted invite is no longer pending"
        );
    }

    #[tokio::test]
    async fn an_expired_invite_is_neither_found_nor_listed() {
        let store = FakeAdmin::default();
        let past = Timestamp::from_milliseconds_since_epoch(NOW_MS - 1000).expect("valid");
        store
            .create_invite(new_invite(
                "inv-1",
                "x@example.test",
                AdminRole::Viewer,
                "tok",
                past,
            ))
            .await
            .expect("create invite");
        assert!(
            store
                .find_pending_invite_by_token(hash_token("tok"), clock().now())
                .await
                .expect("find")
                .is_none()
        );
        assert!(
            store
                .list_pending_invites(clock().now())
                .await
                .expect("list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_wrong_token_finds_no_invite() {
        let store = FakeAdmin::default();
        store
            .create_invite(new_invite(
                "inv-1",
                "a@example.test",
                AdminRole::Ops,
                "right",
                live_expiry(),
            ))
            .await
            .expect("create invite");
        assert!(
            store
                .find_pending_invite_by_token(hash_token("wrong"), clock().now())
                .await
                .expect("find")
                .is_none()
        );
    }

    #[tokio::test]
    async fn pending_invites_list_and_revoke() {
        let store = FakeAdmin::default();
        store
            .create_invite(new_invite(
                "inv-1",
                "a@example.test",
                AdminRole::Admin,
                "t1",
                live_expiry(),
            ))
            .await
            .expect("create invite");
        store
            .create_invite(new_invite(
                "inv-2",
                "b@example.test",
                AdminRole::Ops,
                "t2",
                live_expiry(),
            ))
            .await
            .expect("create invite");
        assert_eq!(
            store
                .list_pending_invites(clock().now())
                .await
                .expect("list")
                .len(),
            2
        );

        assert!(store.revoke_invite("inv-1").await.expect("revoke"));
        let pending = store
            .list_pending_invites(clock().now())
            .await
            .expect("list");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "inv-2");
        assert!(
            !store.revoke_invite("inv-nope").await.expect("revoke"),
            "revoking an absent invite is a no-op"
        );
    }
}
