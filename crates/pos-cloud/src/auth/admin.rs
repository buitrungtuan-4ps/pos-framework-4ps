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

use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use pos_proto::determinism::ClockSource;
use pos_proto::error::ErrorStatus;
use pos_proto::time::Timestamp;

use crate::http::{api_error, service_unavailable};

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

/// One admin's identity *and* their own credential — what a sign-in authenticates against
/// ([ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md)).
///
/// [`AdminCredential`] is the single `super_admin` row; this is one `admin_users` row. The columns
/// have existed since migration 0018 and were written by the invite-acceptance route from the day it
/// shipped — they were simply never read, so an invited admin could never sign in and every session
/// was bound to whichever owner row came back first. This type is the read that closes that.
///
/// [`AdminUser`] is deliberately kept as a separate field rather than flattened: it is the safe half,
/// and it is what a caller hands on to [`AdminContext`]. `credential` redacts its own secrets, so a
/// derived [`fmt::Debug`] cannot leak the hash or the TOTP seed.
#[derive(Debug, Clone)]
pub struct AdminLogin {
    /// The admin's identity, role and status — safe to serialise.
    pub user: AdminUser,
    /// Their own password hash, TOTP secret, and newest step spent.
    pub credential: AdminCredential,
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
    /// Both statuses. [`Self::from_token`] and the refusal that lists what `status` accepts are both
    /// derived from this, so adding a variant updates them instead of leaving them stale.
    pub const ALL: &'static [Self] = &[Self::Active, Self::Suspended];

    /// The token stored in PostgreSQL and carried on the wire.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
        }
    }

    /// Parses a stored token, or `None` if it names no known status. Derived from [`Self::ALL`] and
    /// [`Self::as_token`] rather than a second hand-written match, as [`AdminRole::from_token`] is.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.as_token() == token)
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

    // ---- Credential recovery + rotation ([ADR-0067] slice 6) ----

    /// Replaces the super-admin's TOTP secret with `secret` and resets the last-used step, so the
    /// freshly-enrolled authenticator's codes verify from step zero. The credential login checks is
    /// the super-admin one, so this is the secret a signed-in admin rotates when re-enrolling.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn rotate_totp_secret(
        &self,
        secret: Vec<u8>,
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

    /// Replaces `admin_id`'s recovery codes with `codes` — regenerating the set invalidates whatever
    /// was there. Only the `SHA-256` of each code is stored.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn store_recovery_codes(
        &self,
        admin_id: &str,
        codes: Vec<NewRecoveryCode>,
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

    /// Consumes an unused recovery code for `admin_id` matching `code_hash`, single-use: the first
    /// caller to match an unused code claims it (stamping `used_at = now`); a replay, or a code that
    /// was never issued, matches nothing. Returns whether this call claimed a code.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn consume_recovery_code(
        &self,
        admin_id: &str,
        code_hash: [u8; 32],
        now: Timestamp,
    ) -> impl Future<Output = Result<bool, AdminStoreError>> + Send;

    /// How many of `admin_id`'s recovery codes are still unused — for the "N codes left" display,
    /// never the codes themselves.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be read.
    fn count_recovery_codes(
        &self,
        admin_id: &str,
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

    // ---- Per-admin sign-in ([ADR-0119]) ----

    /// The admin whose email matches `email` case-insensitively, **with their credential**, or
    /// `None` — the read [`login`] authenticates against
    /// ([ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md)).
    ///
    /// Separate from [`find_admin_user_by_email`](Self::find_admin_user_by_email) rather than a
    /// widening of it: only the sign-in path has any business holding a password hash and a TOTP
    /// secret, and a listing route that reached for the wrong method would serialise both.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] only if the store itself could not be read — never for an absent admin,
    /// which is `Ok(None)`.
    fn find_admin_login_by_email(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<Option<AdminLogin>, AdminStoreError>> + Send;

    /// Records `step` as the newest TOTP step **`admin_id`** has spent, advancing only forward — the
    /// per-admin form of [`record_totp_step`](Self::record_totp_step)
    /// ([ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md)).
    ///
    /// Per-admin is not a tidiness point. The single-row version burns a step for *everybody*, so
    /// admin A signing in at step *N* would make admin B's perfectly valid code in the same
    /// 30-second window look like a replay — an availability bug that only appears once a second
    /// admin exists, which is exactly when per-admin login starts being used. Monotone and
    /// idempotent, as the global one is.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn record_admin_totp_step(
        &self,
        admin_id: &str,
        step: u64,
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

    /// Replaces `admin_id`'s TOTP secret with `secret` and resets *their* last-used step, so the
    /// freshly-enrolled authenticator's codes verify from step zero — the per-admin form of
    /// [`rotate_totp_secret`](Self::rotate_totp_secret)
    /// ([ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md)).
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn rotate_admin_totp_secret(
        &self,
        admin_id: &str,
        secret: Vec<u8>,
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;

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

/// An admin sign-in request: who is signing in, their password, and the current TOTP code.
///
/// `email` is **optional**, and that is the one load-bearing choice in
/// [ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md). Migration 0018 gave the
/// only admin every existing installation has a synthetic, non-routable address
/// ([`IMPLICIT_OWNER_EMAIL`]) that nobody has seen and no route can change, so requiring the field
/// would lock out every installation on the day this deployed. Absent, it resolves to the single
/// admin when there is exactly one — which is every installation today — and is refused once there
/// are two, which is exactly when guessing would start being wrong.
///
/// [`fmt::Debug`] redacts the password and any recovery code, so a logged request cannot leak either.
/// The email is not redacted: it is the identity, and it is what the log needs to attribute a failed
/// attempt.
#[derive(Clone, Deserialize)]
pub struct LoginRequest {
    /// Who is signing in. Absent resolves to the single admin; see the type's own note.
    #[serde(default)]
    pub email: Option<String>,
    /// The admin's password.
    pub password: String,
    /// The current 6-digit TOTP code. Ignored when a `recovery_code` is present.
    #[serde(default)]
    pub totp_code: String,
    /// A one-time recovery code, used in place of the TOTP second factor when the authenticator is
    /// lost ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6). Absent for an
    /// ordinary password + TOTP sign-in.
    #[serde(default)]
    pub recovery_code: Option<String>,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .field("totp_code", &self.totp_code)
            .field(
                "recovery_code",
                &self.recovery_code.as_ref().map(|_| "<redacted>"),
            )
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
            // No `details`: naming what was wrong about a sign-in attempt tells a guesser which
            // half to vary. The message is deliberately the same whichever half failed.
            Self::Invalid => api_error(ErrorStatus::Unauthenticated, "sign-in failed"),
            Self::StoreUnavailable => service_unavailable("sign-in"),
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
            Self::Unauthorized => api_error(ErrorStatus::Unauthenticated, "unauthorized"),
            Self::StoreUnavailable => service_unavailable("sign-in"),
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

/// Authenticates an admin login and, on success, persists a session for `mint.token` bound to the
/// admin who actually signed in.
///
/// Each admin authenticates against **their own** row
/// ([ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md)): the credential
/// migration 0018 has stored per admin since it shipped, and which the invite-acceptance route has
/// written for every invited admin, is now the credential this reads. Before that it read the single
/// `super_admin` row, so an invited admin could never sign in at all and every session was attributed
/// to whichever active owner the store returned first.
///
/// `request.email` is optional and resolves per [`resolve_login`]. Everything else is unchanged from
/// ADR-0034: both factors are checked before any verdict
/// ([`SuperAdminCredential::authenticate`]), the matched TOTP step is burned *before* the session is
/// written, and the session is stored as `SHA-256(mint.token)` — never the token itself.
/// `mint.token` is minted from a CSPRNG by the caller ([`crate::http`]); the same token goes into the
/// [`session`](super::session) cookie so the browser can present it, and its hash is what this
/// stores. The session carries a sliding idle TTL (`mint.idle_ttl_secs`) bounded by an absolute cap
/// (`mint.absolute_ttl_secs`).
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
    let Some(admin) = resolve_login(store, request.email.as_deref()).await? else {
        // No such admin, no admin at all, or an ambiguous omitted email. All the same generic
        // refusal: whether an address exists is not a distinction worth handing out, and neither is
        // how many admins the installation has.
        return Err(LoginDenied::Invalid);
    };
    if admin.user.status != AdminStatus::Active {
        // Suspended keeps the row and its history but refuses new sessions. Same generic refusal as a
        // wrong password — "this address is suspended" would confirm the address.
        return Err(LoginDenied::Invalid);
    }
    // The second factor: a one-time recovery code stands in for TOTP when the authenticator is lost
    // ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6). Either way both factors
    // are weighed before a single generic Invalid — the no-oracle rule (ADR-0034).
    let presented_recovery = request
        .recovery_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty());
    if let Some(code) = presented_recovery {
        // The password must verify before a code is consumed, so a wrong-password attempt can never
        // burn a valid code (a denial of service on recovery); the consume is atomic and single-use.
        // The codes consumed are this admin's own — before ADR-0119 they were the first owner's,
        // whoever presented them.
        let password_ok = admin
            .credential
            .credential
            .password_matches(&request.password);
        let recovered = if password_ok {
            store
                .consume_recovery_code(&admin.user.id, hash_recovery_code(code), now)
                .await
                .map_err(|_| LoginDenied::StoreUnavailable)?
        } else {
            false
        };
        if !recovered {
            return Err(LoginDenied::Invalid);
        }
    } else {
        // Password + TOTP, both evaluated inside `authenticate`. Burn the matched step before the
        // session is written, so the same code — and any earlier one — can never mint a second. The
        // step is recorded against *this* admin: burning it globally would refuse a second admin's
        // valid code in the same 30-second window as a replay.
        let authenticated = admin
            .credential
            .credential
            .authenticate(
                &request.password,
                &request.totp_code,
                unix_seconds(now),
                admin.credential.last_used_totp_step,
            )
            .map_err(|_| LoginDenied::Invalid)?;
        store
            .record_admin_totp_step(&admin.user.id, authenticated.totp_step)
            .await
            .map_err(|_| LoginDenied::StoreUnavailable)?;
    }
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
            admin_id: Some(admin.user.id),
            ip: mint.ip.map(str::to_owned),
            user_agent: mint.user_agent.map(str::to_owned),
        })
        .await
        .map_err(|_| LoginDenied::StoreUnavailable)?;
    Ok(())
}

/// The admin a sign-in request names, or `None` if it names nobody resolvable
/// ([ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md) §3–4).
///
/// A present email is looked up directly. An **absent** one resolves to the single admin when the
/// store holds exactly one, and to `None` when it holds none or more than one. That is the whole of
/// the compatibility decision: every installation today has one admin whose address is the synthetic
/// [`IMPLICIT_OWNER_EMAIL`] nobody has seen, so omitting the field must keep working for them — and
/// must stop the moment a second admin makes "the one admin" the wrong answer.
///
/// It opens no enumeration surface. With one admin, omitting the email is equivalent to naming it,
/// so there is nothing to learn; with two or more, an absent email is the same generic refusal as a
/// wrong one. `None` here is never distinguishable from a wrong password at the wire.
///
/// # Errors
///
/// [`LoginDenied::StoreUnavailable`] if the store could not be read.
async fn resolve_login<A>(store: &A, email: Option<&str>) -> Result<Option<AdminLogin>, LoginDenied>
where
    A: AdminStore,
{
    // An empty or whitespace-only field is the same as an absent one: a form that submits `""` for
    // an untouched input must not be told it named an admin called "".
    let named = email.map(str::trim).filter(|email| !email.is_empty());
    let email = if let Some(email) = named {
        email.to_owned()
    } else {
        let admins = store
            .list_admin_users()
            .await
            .map_err(|_| LoginDenied::StoreUnavailable)?;
        // The `else` covers both zero (nobody enrolled) and two-or-more (ambiguous). Matching on the
        // slice rather than testing `len() == 1` is what makes that exhaustive at compile time.
        let [only] = admins.as_slice() else {
            return Ok(None);
        };
        only.email.clone()
    };
    store
        .find_admin_login_by_email(&email)
        .await
        .map_err(|_| LoginDenied::StoreUnavailable)
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

/// The stored form of a recovery code — `SHA-256` of its *normalised* text
/// ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6). Only the hash is ever
/// stored; the code itself reaches the admin once, at generation. Normalisation
/// ([`normalize_recovery_code`]) means the admin may type the code with or without its display
/// dashes and in any case and still match.
#[must_use]
pub fn hash_recovery_code(code: &str) -> [u8; 32] {
    hash_token(&normalize_recovery_code(code))
}

/// Canonicalises a recovery code for hashing: keep only ASCII alphanumerics, lower-cased. So the
/// display form `"a1b2-c3d4-e5f6-a7b8"` and a typed `"A1B2 C3D4 E5F6 A7B8"` hash identically.
fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// A recovery code to store: its row id (a ULID minted at the edge) and `SHA-256` hash — never the
/// code itself ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 6).
#[derive(Debug, Clone)]
pub struct NewRecoveryCode {
    /// The row's id (a ULID string).
    pub id: String,
    /// `SHA-256` of the normalised code.
    pub code_hash: [u8; 32],
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
        AdminCredential, AdminInvite, AdminLogin, AdminRole, AdminStatus, AdminStore,
        AdminStoreError, AdminUser, IMPLICIT_OWNER_EMAIL, IMPLICIT_OWNER_ID, LoginDenied,
        LoginRequest, NewAdminInvite, NewAdminSession, NewAdminUser, NewRecoveryCode,
        SessionDenied, SessionMint, authenticate_session, authenticated_admin, hash_recovery_code,
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

    /// A sign-in request that omits the email — the shape a one-admin installation sends, and the
    /// one ADR-0119 keeps working so an upgrade locks nobody out.
    fn request(password: &str, totp_code: &str) -> LoginRequest {
        LoginRequest {
            email: None,
            password: password.to_owned(),
            totp_code: totp_code.to_owned(),
            recovery_code: None,
        }
    }

    /// A sign-in request naming who is signing in — the shape a multi-admin installation sends.
    fn request_as(email: &str, password: &str, totp_code: &str) -> LoginRequest {
        LoginRequest {
            email: Some(email.to_owned()),
            ..request(password, totp_code)
        }
    }

    /// A login request presenting a recovery code in place of a TOTP code.
    fn recovery_request(password: &str, recovery_code: &str) -> LoginRequest {
        LoginRequest {
            email: None,
            password: password.to_owned(),
            totp_code: String::new(),
            recovery_code: Some(recovery_code.to_owned()),
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

    /// A stored recovery code in the fake: its hash and whether it has been spent.
    #[derive(Clone)]
    struct StoredRecoveryCode {
        code_hash: [u8; 32],
        used: bool,
    }

    /// An in-memory admin store: the legacy single credential, the multi-admin `admin_users` table
    /// **with each admin's own credential** (the columns migration 0018 has always carried, which
    /// [ADR-0119](../../../docs/adr/0119-each-admin-signs-in-as-themselves.md) makes login read), an
    /// invitations table, a session table, per-admin recovery codes, and a down switch.
    ///
    /// `credentials` is keyed by admin id and is what sign-in authenticates against;
    /// `credential`/`last_used_totp_step` are the legacy `super_admin` row, still on the seam and no
    /// longer read by login.
    #[derive(Default)]
    struct FakeAdmin {
        credential: Mutex<Option<SuperAdminCredential>>,
        last_used_totp_step: Mutex<Option<u64>>,
        sessions: Mutex<SessionRows>,
        admin_users: Mutex<Vec<AdminUser>>,
        credentials: Mutex<HashMap<String, AdminCredential>>,
        invites: Mutex<Vec<StoredInvite>>,
        recovery_codes: Mutex<HashMap<String, Vec<StoredRecoveryCode>>>,
        down: bool,
    }

    impl FakeAdmin {
        /// An installation with one enrolled admin — the shape every real installation has today:
        /// the migrated/implicit owner, holding the shared test credential, plus the legacy
        /// `super_admin` row the migration carried it over from.
        fn provisioned() -> Self {
            Self::with_admins(&[(IMPLICIT_OWNER_ID, IMPLICIT_OWNER_EMAIL, AdminRole::Owner)])
        }

        /// An installation with the named admins, each holding the shared test credential — so a
        /// two-admin store can be built without every test restating the seeding.
        fn with_admins(admins: &[(&str, &str, AdminRole)]) -> Self {
            let users = admins
                .iter()
                .map(|&(id, email, role)| AdminUser {
                    id: id.to_owned(),
                    email: email.to_owned(),
                    name: "Owner".to_owned(),
                    role,
                    status: AdminStatus::Active,
                })
                .collect();
            let credentials = admins
                .iter()
                .map(|&(id, _, _)| {
                    (
                        id.to_owned(),
                        AdminCredential {
                            credential: provisioned_credential(),
                            last_used_totp_step: None,
                        },
                    )
                })
                .collect();
            Self {
                credential: Mutex::new(Some(provisioned_credential())),
                admin_users: Mutex::new(users),
                credentials: Mutex::new(credentials),
                ..Self::default()
            }
        }

        fn unavailable() -> Self {
            Self {
                down: true,
                ..Self::default()
            }
        }

        /// The step the implicit owner's sign-in burned — the per-admin column login writes now.
        fn recorded_step(&self) -> Option<u64> {
            self.recorded_step_for(IMPLICIT_OWNER_ID)
        }

        /// The step `admin_id`'s sign-in burned, or `None` if they have never signed in (or do not
        /// exist). Two admins have separate values, which is the point of the per-admin column.
        fn recorded_step_for(&self, admin_id: &str) -> Option<u64> {
            self.credentials
                .lock()
                .expect("lock")
                .get(admin_id)
                .and_then(|credential| credential.last_used_totp_step)
        }

        /// The step the *legacy* single-row column holds — still on the seam, and no longer written
        /// by sign-in.
        fn recorded_legacy_step(&self) -> Option<u64> {
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

        async fn rotate_totp_secret(&self, secret: Vec<u8>) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut slot = self.credential.lock().expect("lock");
            if let Some(credential) = slot.take() {
                *slot = Some(credential.with_totp(TotpSecret::new(secret)));
            }
            // A fresh secret starts unused, exactly as the SQL resets `last_used_totp_step = NULL`.
            *self.last_used_totp_step.lock().expect("lock") = None;
            Ok(())
        }

        async fn store_recovery_codes(
            &self,
            admin_id: &str,
            codes: Vec<NewRecoveryCode>,
        ) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let rows = codes
                .into_iter()
                .map(|code| StoredRecoveryCode {
                    code_hash: code.code_hash,
                    used: false,
                })
                .collect();
            // Regenerating replaces the whole set, as the SQL delete-then-insert does.
            self.recovery_codes
                .lock()
                .expect("lock")
                .insert(admin_id.to_owned(), rows);
            Ok(())
        }

        async fn consume_recovery_code(
            &self,
            admin_id: &str,
            code_hash: [u8; 32],
            _now: Timestamp,
        ) -> Result<bool, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let mut map = self.recovery_codes.lock().expect("lock");
            let Some(rows) = map.get_mut(admin_id) else {
                return Ok(false);
            };
            // Single-use: the first unused code that matches is spent, as the SQL `WHERE used_at IS
            // NULL` guard is.
            match rows
                .iter_mut()
                .find(|row| !row.used && row.code_hash == code_hash)
            {
                Some(row) => {
                    row.used = true;
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn count_recovery_codes(&self, admin_id: &str) -> Result<u64, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let map = self.recovery_codes.lock().expect("lock");
            let unused = map
                .get(admin_id)
                .map_or(0, |rows| rows.iter().filter(|row| !row.used).count());
            Ok(u64::try_from(unused).unwrap_or(u64::MAX))
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
            // The credential is stored alongside the identity, exactly as the single `admin_users`
            // INSERT does — which is what makes the invited admin able to sign in (ADR-0119).
            self.credentials.lock().expect("lock").insert(
                user.id.clone(),
                AdminCredential {
                    credential: SuperAdminCredential::new(
                        user.password_phc,
                        TotpSecret::new(user.totp_secret),
                    ),
                    last_used_totp_step: None,
                },
            );
            users.push(AdminUser {
                id: user.id,
                email: user.email,
                name: user.name,
                role: user.role,
                status: AdminStatus::Active,
            });
            Ok(true)
        }

        async fn find_admin_login_by_email(
            &self,
            email: &str,
        ) -> Result<Option<AdminLogin>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            let users = self.admin_users.lock().expect("lock");
            let Some(user) = users
                .iter()
                .find(|user| user.email.eq_ignore_ascii_case(email))
            else {
                return Ok(None);
            };
            let credentials = self.credentials.lock().expect("lock");
            Ok(credentials
                .get(&user.id)
                .cloned()
                .map(|credential| AdminLogin {
                    user: user.clone(),
                    credential,
                }))
        }

        async fn record_admin_totp_step(
            &self,
            admin_id: &str,
            step: u64,
        ) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            if let Some(credential) = self.credentials.lock().expect("lock").get_mut(admin_id) {
                // Forward only, as the `IS NULL OR < $2` guard makes the real UPDATE.
                let newest = credential
                    .last_used_totp_step
                    .map_or(step, |seen| seen.max(step));
                credential.last_used_totp_step = Some(newest);
            }
            Ok(())
        }

        async fn rotate_admin_totp_secret(
            &self,
            admin_id: &str,
            secret: Vec<u8>,
        ) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            if let Some(stored) = self.credentials.lock().expect("lock").get_mut(admin_id) {
                stored.credential = stored.credential.clone().with_totp(TotpSecret::new(secret));
                // A fresh secret starts unused, exactly as the SQL resets `last_used_totp_step`.
                stored.last_used_totp_step = None;
            }
            Ok(())
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

    // ---- Per-admin sign-in ([ADR-0119]) ----

    /// Two admins, each holding the shared test credential: the owner every installation already has
    /// and an invited admin. The shape the whole record is about.
    fn two_admins() -> FakeAdmin {
        FakeAdmin::with_admins(&[
            (IMPLICIT_OWNER_ID, IMPLICIT_OWNER_EMAIL, AdminRole::Owner),
            ("id-invited", "invited@example.test", AdminRole::Ops),
        ])
    }

    #[tokio::test]
    async fn an_invited_admin_can_sign_in_at_all() {
        // The defect ADR-0119 exists for. `admin_users.password_phc` has been written for every
        // invited admin since the invitation route shipped, and login read the single `super_admin`
        // row instead — so every screen in `/admins` described a capability that did not exist.
        let store = two_admins();
        let clock = clock();
        login(
            &store,
            &clock,
            &request_as(
                "invited@example.test",
                "a-strong-passphrase",
                &current_code(),
            ),
            &mint("invited-token"),
        )
        .await
        .expect("an invited admin signs in with their own credential");
        let context = authenticated_admin(&store, &clock, &cookie_header("invited-token"))
            .await
            .expect("the issued session authorises");
        assert_eq!(context.admin.id, "id-invited");
        assert_eq!(context.admin.role, AdminRole::Ops);
    }

    #[tokio::test]
    async fn the_session_belongs_to_whoever_authenticated_not_to_the_first_owner() {
        // The second, worse consequence. `acting_owner_id` returned the *first active owner*
        // whoever signed in, so on a two-admin installation the audit trail (ADR-0069) attributed
        // every action to a coin flip. Nothing warned, because the session was perfectly valid.
        let store = two_admins();
        let clock = clock();
        login(
            &store,
            &clock,
            &request_as(
                "invited@example.test",
                "a-strong-passphrase",
                &current_code(),
            ),
            &mint("ops-token"),
        )
        .await
        .expect("login");
        let context = authenticated_admin(&store, &clock, &cookie_header("ops-token"))
            .await
            .expect("live session");
        assert_ne!(
            context.admin.id, IMPLICIT_OWNER_ID,
            "the session must not be attributed to the owner who did not sign in"
        );
        assert_eq!(context.admin.id, "id-invited");
    }

    #[tokio::test]
    async fn one_admins_sign_in_does_not_burn_the_others_totp_step() {
        // Why the step is per-admin (ADR-0119 §5). Burning it globally would refuse a second
        // admin's perfectly valid code in the same 30-second window as a replay — an availability
        // bug that appears exactly when per-admin login starts being used.
        let store = two_admins();
        let clock = clock();
        login(
            &store,
            &clock,
            &request_as(
                "invited@example.test",
                "a-strong-passphrase",
                &current_code(),
            ),
            &mint("ops-token"),
        )
        .await
        .expect("the first admin signs in");
        assert!(store.recorded_step_for("id-invited").is_some());
        assert_eq!(
            store.recorded_step_for(IMPLICIT_OWNER_ID),
            None,
            "the other admin's step must be untouched"
        );
        // The same instant, the same code, the other admin: valid, not a replay.
        login(
            &store,
            &clock,
            &request_as(IMPLICIT_OWNER_EMAIL, "a-strong-passphrase", &current_code()),
            &mint("owner-token"),
        )
        .await
        .expect("the second admin signs in in the same window");
        assert_eq!(
            store.recorded_legacy_step(),
            None,
            "the legacy single-row column is no longer what sign-in writes"
        );
    }

    #[tokio::test]
    async fn an_omitted_email_signs_the_single_admin_in() {
        // The compatibility half of the decision. Every installation today has one admin whose
        // address is the synthetic placeholder nobody has seen and no route can change, so the
        // request shape they send today must keep working on the day this deploys.
        let store = FakeAdmin::provisioned();
        login(
            &store,
            &clock(),
            &request("a-strong-passphrase", &current_code()),
            &mint("no-email"),
        )
        .await
        .expect("an email-less sign-in resolves to the single admin");
    }

    #[tokio::test]
    async fn an_omitted_email_is_refused_once_a_second_admin_exists() {
        // And the half that makes it retire itself: the fallback stops the moment "the one admin"
        // becomes the wrong answer. A generic refusal, so it reveals no admin count.
        let store = two_admins();
        assert_eq!(
            login(
                &store,
                &clock(),
                &request("a-strong-passphrase", &current_code()),
                &mint("ambiguous"),
            )
            .await,
            Err(LoginDenied::Invalid)
        );
    }

    #[tokio::test]
    async fn a_blank_email_is_the_same_as_an_omitted_one() {
        // A form that submits `""` for an untouched input must not be told it named an admin
        // called "" — it must take the single-admin path.
        let store = FakeAdmin::provisioned();
        login(
            &store,
            &clock(),
            &request_as("   ", "a-strong-passphrase", &current_code()),
            &mint("blank-email"),
        )
        .await
        .expect("a blank email takes the omitted-email path");
    }

    #[tokio::test]
    async fn an_unknown_email_is_the_same_generic_refusal_as_a_wrong_password() {
        let store = two_admins();
        assert_eq!(
            login(
                &store,
                &clock(),
                &request_as(
                    "nobody@example.test",
                    "a-strong-passphrase",
                    &current_code()
                ),
                &mint("unknown"),
            )
            .await,
            Err(LoginDenied::Invalid),
            "whether an address exists is not a distinction worth handing out"
        );
    }

    #[tokio::test]
    async fn a_suspended_admin_cannot_sign_in() {
        // Suspended keeps the row and its history but refuses new sessions — the off-boarding path
        // that does not destroy the audit trail. Same generic refusal, so it does not confirm the
        // address either.
        let store = two_admins();
        store
            .set_admin_user_status("id-invited", AdminStatus::Suspended)
            .await
            .expect("suspend");
        assert_eq!(
            login(
                &store,
                &clock(),
                &request_as(
                    "invited@example.test",
                    "a-strong-passphrase",
                    &current_code()
                ),
                &mint("suspended"),
            )
            .await,
            Err(LoginDenied::Invalid)
        );
    }

    #[tokio::test]
    async fn an_email_matches_case_insensitively() {
        // One account per address regardless of case, exactly as the `lower(email)` unique index
        // enforces — so an operator typing their address capitalised is not locked out.
        let store = two_admins();
        login(
            &store,
            &clock(),
            &request_as(
                "Invited@Example.TEST",
                "a-strong-passphrase",
                &current_code(),
            ),
            &mint("cased"),
        )
        .await
        .expect("a capitalised address is the same identity");
    }

    #[tokio::test]
    async fn a_store_with_no_admin_at_all_refuses_rather_than_admitting_anyone() {
        let store = FakeAdmin::default();
        assert_eq!(
            login(
                &store,
                &clock(),
                &request("a-strong-passphrase", &current_code()),
                &mint("nobody"),
            )
            .await,
            Err(LoginDenied::Invalid)
        );
    }

    #[tokio::test]
    async fn a_recovery_code_consumes_the_signing_in_admins_own_code() {
        // Before ADR-0119 the code consumed was the first owner's, whoever presented it — so an
        // invited admin's recovery codes were unreachable and the owner's were spendable by them.
        let store = two_admins();
        store
            .store_recovery_codes("id-invited", vec![recovery("r1", "let-me-in")])
            .await
            .expect("store");
        let clock = clock();
        login(
            &store,
            &clock,
            &LoginRequest {
                email: Some("invited@example.test".to_owned()),
                ..recovery_request("a-strong-passphrase", "let-me-in")
            },
            &mint("recovered"),
        )
        .await
        .expect("recovery sign-in");
        let context = authenticated_admin(&store, &clock, &cookie_header("recovered"))
            .await
            .expect("live session");
        assert_eq!(context.admin.id, "id-invited");
        assert_eq!(
            store
                .count_recovery_codes("id-invited")
                .await
                .expect("count"),
            0,
            "the code is burned"
        );
        assert_eq!(
            store
                .count_recovery_codes(IMPLICIT_OWNER_ID)
                .await
                .expect("count"),
            0,
            "and the other admin never had one to lose"
        );
    }

    #[tokio::test]
    async fn login_request_debug_still_redacts_the_password_while_naming_the_admin() {
        let rendered = format!(
            "{:?}",
            request_as("someone@example.test", "a-strong-passphrase", "123456")
        );
        assert!(
            !rendered.contains("a-strong-passphrase"),
            "the password leaked into Debug: {rendered}"
        );
        // The email is the identity, and it is what a log needs to attribute a failed attempt.
        assert!(rendered.contains("someone@example.test"));
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
        // One enrolled admin, holding the shared test credential — so an email-less request
        // resolves to them (ADR-0119) and the minted session binds to `id-owner`.
        let store = FakeAdmin::with_admins(&[("id-owner", "owner@example.test", AdminRole::Owner)]);
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
        // One enrolled admin, holding the shared test credential — so an email-less request
        // resolves to them (ADR-0119) and the minted session binds to `id-owner`.
        let store = FakeAdmin::with_admins(&[("id-owner", "owner@example.test", AdminRole::Owner)]);
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
        // One enrolled admin, holding the shared test credential — so an email-less request
        // resolves to them (ADR-0119) and the minted session binds to `id-owner`.
        let store = FakeAdmin::with_admins(&[("id-owner", "owner@example.test", AdminRole::Owner)]);
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
        // One enrolled admin, holding the shared test credential — so an email-less request
        // resolves to them (ADR-0119) and the minted session binds to `id-owner`.
        let store = FakeAdmin::with_admins(&[("id-owner", "owner@example.test", AdminRole::Owner)]);
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

    // ---- TOTP re-enrolment + recovery codes ([ADR-0067] slice 6) ----

    /// A recovery code to store: a row id and the hash of `code`.
    fn recovery(id: &str, code: &str) -> NewRecoveryCode {
        NewRecoveryCode {
            id: id.to_owned(),
            code_hash: hash_recovery_code(code),
        }
    }

    #[tokio::test]
    async fn recovery_codes_store_count_and_burn_single_use() {
        let store = FakeAdmin::default();
        store
            .store_recovery_codes(
                "id-1",
                vec![
                    recovery("r1", "aaaa-bbbb"),
                    recovery("r2", "cccc-dddd"),
                    recovery("r3", "eeee-ffff"),
                ],
            )
            .await
            .expect("store");
        assert_eq!(store.count_recovery_codes("id-1").await.expect("count"), 3);

        // Consuming normalises the input, so typing it without dashes and upper-cased still matches.
        assert!(
            store
                .consume_recovery_code("id-1", hash_recovery_code("AAAABBBB"), clock().now())
                .await
                .expect("consume")
        );
        assert_eq!(store.count_recovery_codes("id-1").await.expect("count"), 2);
        // Single-use: the same code cannot be spent twice.
        assert!(
            !store
                .consume_recovery_code("id-1", hash_recovery_code("aaaa-bbbb"), clock().now())
                .await
                .expect("consume"),
            "a spent code cannot be reused"
        );
        // A code that was never issued matches nothing.
        assert!(
            !store
                .consume_recovery_code("id-1", hash_recovery_code("0000-0000"), clock().now())
                .await
                .expect("consume")
        );
    }

    #[tokio::test]
    async fn regenerating_recovery_codes_replaces_the_set() {
        let store = FakeAdmin::default();
        store
            .store_recovery_codes(
                "id-1",
                vec![recovery("r1", "old1-old1"), recovery("r2", "old2-old2")],
            )
            .await
            .expect("store");
        store
            .store_recovery_codes(
                "id-1",
                vec![
                    recovery("n1", "new1-new1"),
                    recovery("n2", "new2-new2"),
                    recovery("n3", "new3-new3"),
                ],
            )
            .await
            .expect("regenerate");
        assert_eq!(store.count_recovery_codes("id-1").await.expect("count"), 3);
        assert!(
            !store
                .consume_recovery_code("id-1", hash_recovery_code("old1-old1"), clock().now())
                .await
                .expect("consume"),
            "regenerating invalidates the previous set"
        );
        assert!(
            store
                .consume_recovery_code("id-1", hash_recovery_code("new1-new1"), clock().now())
                .await
                .expect("consume")
        );
    }

    #[tokio::test]
    async fn a_recovery_code_signs_in_in_place_of_totp_and_is_burned() {
        // One enrolled admin, holding the shared test credential — so an email-less request
        // resolves to them (ADR-0119) and the minted session binds to `id-owner`.
        let store = FakeAdmin::with_admins(&[("id-owner", "owner@example.test", AdminRole::Owner)]);
        store
            .store_recovery_codes("id-owner", vec![recovery("r1", "help-me-in")])
            .await
            .expect("store");
        let clock = clock();
        login(
            &store,
            &clock,
            &recovery_request("a-strong-passphrase", "help-me-in"),
            &mint("recovered"),
        )
        .await
        .expect("recovery sign-in");
        // The session is live and bound to the owner, exactly as a TOTP sign-in would be.
        let context = authenticated_admin(&store, &clock, &cookie_header("recovered"))
            .await
            .expect("the recovered session authorises");
        assert_eq!(context.admin.id, "id-owner");
        // Single-use: the same code cannot sign in a second time.
        assert_eq!(
            login(
                &store,
                &clock,
                &recovery_request("a-strong-passphrase", "help-me-in"),
                &mint("again"),
            )
            .await,
            Err(LoginDenied::Invalid),
            "a recovery code is spent after one use"
        );
    }

    #[tokio::test]
    async fn a_wrong_password_never_burns_a_recovery_code() {
        // One enrolled admin, holding the shared test credential — so an email-less request
        // resolves to them (ADR-0119) and the minted session binds to `id-owner`.
        let store = FakeAdmin::with_admins(&[("id-owner", "owner@example.test", AdminRole::Owner)]);
        store
            .store_recovery_codes("id-owner", vec![recovery("r1", "keep-me-safe")])
            .await
            .expect("store");
        let clock = clock();
        assert_eq!(
            login(
                &store,
                &clock,
                &recovery_request("wrong-passphrase", "keep-me-safe"),
                &mint("nope"),
            )
            .await,
            Err(LoginDenied::Invalid)
        );
        // The code survived, so a correct sign-in still spends it — a wrong password did not.
        assert_eq!(
            store.count_recovery_codes("id-owner").await.expect("count"),
            1
        );
        login(
            &store,
            &clock,
            &recovery_request("a-strong-passphrase", "keep-me-safe"),
            &mint("yes"),
        )
        .await
        .expect("a correct recovery sign-in still works");
    }

    #[tokio::test]
    async fn rotating_the_totp_secret_resets_the_step_and_kills_old_codes() {
        let store = FakeAdmin::provisioned();
        let clock = clock();
        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            &mint("first"),
        )
        .await
        .expect("login");
        assert!(store.recorded_step().is_some());

        // The *admin's own* secret — the one sign-in reads (ADR-0119 §5). Rotating the legacy
        // single-row `super_admin` secret no longer touches anybody's ability to sign in.
        store
            .rotate_admin_totp_secret(IMPLICIT_OWNER_ID, b"a-brand-new-totp-secret-value".to_vec())
            .await
            .expect("rotate");
        assert_eq!(
            store.recorded_step(),
            None,
            "a freshly enrolled secret starts from an unused step"
        );
        // A code from the replaced authenticator no longer verifies against the new secret.
        assert_eq!(
            login(
                &store,
                &clock,
                &request("a-strong-passphrase", &current_code()),
                &mint("second"),
            )
            .await,
            Err(LoginDenied::Invalid),
            "codes from the old authenticator are dead after re-enrolment"
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
