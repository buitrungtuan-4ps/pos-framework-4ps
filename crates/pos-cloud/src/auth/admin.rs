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
use serde::Deserialize;
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

/// The super-admin store: the one credential, and the server-side session table
/// ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)). A table in `store-postgres`; a fake in
/// tests.
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

    /// Persists a session by `token_hash`, valid until `expires_at`.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn create_session(
        &self,
        token_hash: [u8; 32],
        expires_at: Timestamp,
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

    /// Revokes the session with `token_hash`. Idempotent: revoking an absent session is `Ok(())`.
    ///
    /// # Errors
    ///
    /// [`AdminStoreError`] if the store could not be written.
    fn revoke_session(
        &self,
        token_hash: [u8; 32],
    ) -> impl Future<Output = Result<(), AdminStoreError>> + Send;
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
    store
        .create_session(hash_token(session_token), expiry(now, ttl_secs))
        .await
        .map_err(|_| LoginDenied::StoreUnavailable)?;
    Ok(())
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
        AdminCredential, AdminStore, AdminStoreError, LoginDenied, LoginRequest, SessionDenied,
        authenticate_session, hash_token, login, logout,
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

    /// An in-memory admin store: at most one credential, a session table, and a down switch.
    #[derive(Default)]
    struct FakeAdmin {
        credential: Mutex<Option<SuperAdminCredential>>,
        last_used_totp_step: Mutex<Option<u64>>,
        sessions: Mutex<HashMap<[u8; 32], Timestamp>>,
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
        ) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            self.sessions
                .lock()
                .expect("lock")
                .insert(token_hash, expires_at);
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
                .is_some_and(|expires_at| *expires_at > now))
        }

        async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
            if self.down {
                return Err(AdminStoreError::new("down"));
            }
            self.sessions.lock().expect("lock").remove(&token_hash);
            Ok(())
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
}
