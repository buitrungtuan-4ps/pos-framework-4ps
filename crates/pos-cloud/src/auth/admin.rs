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

    /// Persists a session by `token_hash`, valid until `expires_at`, owned by `admin_id`.
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
        token_hash: [u8; 32],
        expires_at: Timestamp,
        admin_id: Option<&str>,
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

/// Authenticates a super-admin login and, on success, persists a session for `session_token`.
///
/// Both factors are checked before any verdict ([`SuperAdminCredential::authenticate`]), the matched
/// TOTP step is burned *before* the session is written, and the session is stored as
/// `SHA-256(session_token)` — never the token itself. `session_token` is minted from a CSPRNG by the
/// caller ([`crate::http`]); the same token goes into the [`session`](super::session) cookie so the
/// browser can present it, and its hash is what this stores.
///
/// # Errors
///
/// [`LoginDenied::Invalid`] for any credential problem (a single generic refusal), or
/// [`LoginDenied::StoreUnavailable`] if the store could not be read or written.
pub async fn login<A, C>(
    store: &A,
    clock: &C,
    request: &LoginRequest,
    session_token: &str,
    ttl_secs: u64,
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
    store
        .create_session(
            hash_token(session_token),
            expiry(now, ttl_secs),
            owner_id.as_deref(),
        )
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

/// The Unix-seconds value TOTP verification consumes, from a millisecond [`Timestamp`]. Clamped at the
/// epoch — a pre-epoch clock is nonsensical here and would only ever fail to verify.
fn unix_seconds(now: Timestamp) -> u64 {
    u64::try_from(now.as_milliseconds_since_epoch().max(0)).unwrap_or(0) / 1000
}

/// `now + ttl_secs`, saturating. On overflow past the representable range the expiry falls back to
/// `now` — an already-expired session, which fails safe (the login just has to be retried) rather
/// than minting one that never dies.
fn expiry(now: Timestamp, ttl_secs: u64) -> Timestamp {
    let ttl_ms = i64::try_from(ttl_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
    let at = now.as_milliseconds_since_epoch().saturating_add(ttl_ms);
    Timestamp::from_milliseconds_since_epoch(at).unwrap_or(now)
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
    use pos_proto::time::Timestamp;

    use super::{
        AdminCredential, AdminRole, AdminStatus, AdminStore, AdminStoreError, AdminUser,
        IMPLICIT_OWNER_ID, LoginDenied, LoginRequest, NewAdminUser, SessionDenied,
        authenticate_session, authenticated_admin, hash_token, login, logout,
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

    /// A stored session row: its expiry and the id of the admin it belongs to (`None` for a legacy
    /// session), keyed in the table by `SHA-256(token)`.
    type SessionRows = HashMap<[u8; 32], (Timestamp, Option<String>)>;

    /// An in-memory admin store: at most one legacy credential, the multi-admin `admin_users`
    /// table, a session table, and a down switch.
    #[derive(Default)]
    struct FakeAdmin {
        credential: Mutex<Option<SuperAdminCredential>>,
        last_used_totp_step: Mutex<Option<u64>>,
        sessions: Mutex<SessionRows>,
        admin_users: Mutex<Vec<AdminUser>>,
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

        async fn create_session(
            &self,
            token_hash: [u8; 32],
            expires_at: Timestamp,
            admin_id: Option<&str>,
        ) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            self.sessions
                .lock()
                .expect("lock")
                .insert(token_hash, (expires_at, admin_id.map(str::to_owned)));
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
            Ok(self
                .sessions
                .lock()
                .expect("lock")
                .get(&token_hash)
                .is_some_and(|(expires_at, _)| *expires_at > now))
        }

        async fn session_admin(
            &self,
            token_hash: [u8; 32],
            now: Timestamp,
        ) -> Result<Option<super::LiveSession>, AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            Ok(self
                .sessions
                .lock()
                .expect("lock")
                .get(&token_hash)
                .filter(|(expires_at, _)| *expires_at > now)
                .map(|(_, admin_id)| super::LiveSession {
                    admin_id: admin_id.clone(),
                }))
        }

        async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            self.sessions.lock().expect("lock").remove(&token_hash);
            Ok(())
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
    }

    #[tokio::test]
    async fn a_correct_login_issues_a_session_the_guard_then_accepts() {
        let store = FakeAdmin::provisioned();
        let clock = clock();

        login(
            &store,
            &clock,
            &request("a-strong-passphrase", &current_code()),
            "session-token-abc",
            TTL,
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
                "t1",
                TTL
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
                "t2",
                TTL
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
            "first",
            TTL,
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
                "second",
                TTL
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
                "t",
                TTL
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
                "t",
                TTL
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
            "expiring",
            TTL,
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
            "live",
            TTL,
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
            .create_session(hash_token("tok-viewer"), live_expiry(), Some("id-viewer"))
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
            .create_session(hash_token("tok"), live_expiry(), Some("id-1"))
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
            .create_session(hash_token("tok"), live_expiry(), Some("ghost"))
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
            .create_session(hash_token("legacy"), live_expiry(), None)
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
            .create_session(hash_token("legacy"), live_expiry(), None)
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
            "fresh-token",
            TTL,
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
}
