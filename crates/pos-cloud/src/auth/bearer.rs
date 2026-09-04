// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Bearer authentication for the public `/v1` surface: turn an `Authorization: Bearer pos_…` header
//! into a verified [`Grant`], or a refusal that leaks nothing ([ADR-0037](../../../docs/adr/0037-api-keys.md)).
//!
//! This is the seam between the HTTP edge and the pure [`apikey`](super::apikey) engine. It reads the
//! header, looks the key up by its public id, and [`verify`]s it against the clock's `now`. Two
//! properties matter here and both are deliberate:
//!
//!  * **No oracle.** A missing header, a token that will not parse, an unknown id, a wrong secret, a
//!    revoked key and an expired key all render the *same* `401`. Only the server's log records which
//!    it was ([`AuthDenied`]). A prober holding a random `pos_…` string learns nothing about whether
//!    an id exists, so it cannot enumerate keys.
//!  * **A store outage is not a denial.** If the key store itself cannot be read the answer is a
//!    retryable `503`, not a `401` — the caller's credential may be perfectly good, so telling it
//!    "unauthorized" would be a lie that makes it throw the key away.
//!
//! Authentication (who are you) and authorisation (may you do this) are separate steps:
//! [`authenticate`] yields the [`Grant`], then [`require_scope`] gates the specific action with a
//! `403`. A `403` is safe to distinguish from a `401` because by then the caller has already proven
//! its identity — it is not a probe.

use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};

use pos_proto::determinism::ClockSource;
use pos_proto::error::ErrorStatus;

use super::apikey::{ApiKeyStore, Grant, Scope, parse, verify};
use crate::http::api_error;

/// The authentication scheme this surface accepts.
const SCHEME: &str = "Bearer";

/// Authenticates a `/v1` request from its `Authorization` header, as of the clock's current instant.
///
/// Returns the [`Grant`] the presented key carries. The caller then [`require_scope`]s the specific
/// action before acting, and must scope every data access to [`Grant::tenant`].
///
/// # Errors
///
/// [`AuthDenied`] — a single generic `401` for every credential problem (no oracle), or a retryable
/// `503` if the key store itself was unreachable.
pub async fn authenticate<K, C>(
    keys: &K,
    clock: &C,
    headers: &HeaderMap,
) -> Result<Grant, AuthDenied>
where
    K: ApiKeyStore,
    C: ClockSource,
{
    let header = headers.get(AUTHORIZATION).ok_or(AuthDenied::Missing)?;
    let value = header.to_str().map_err(|_| AuthDenied::Malformed)?;
    // RFC 7235: the scheme token is case-insensitive; the credential is the rest, after one space.
    let (scheme, token) = value.split_once(' ').ok_or(AuthDenied::Malformed)?;
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return Err(AuthDenied::Malformed);
    }
    let presented = parse(token.trim()).map_err(|_| AuthDenied::Invalid)?;
    let stored = keys
        .lookup(presented.id)
        .await
        .map_err(|_| AuthDenied::StoreUnavailable)?;
    // An unknown id is indistinguishable from a bad secret to the client — both are `Invalid`.
    let Some(stored) = stored else {
        return Err(AuthDenied::Invalid);
    };
    verify(&stored, &presented, clock.now()).map_err(|_| AuthDenied::Invalid)
}

/// Enforces that a verified `grant` was issued the `scope` this action needs.
///
/// # Errors
///
/// [`ScopeDenied`] — a `403` — if the scope was not granted. Deny by default: an ungranted scope is
/// refused even though the key is otherwise valid.
pub fn require_scope(grant: &Grant, scope: Scope) -> Result<(), ScopeDenied> {
    if grant.authorizes(scope) {
        Ok(())
    } else {
        Err(ScopeDenied)
    }
}

/// The `403` a missing scope produces. A zero-size marker, so the `Result` it rides in stays small;
/// it becomes the forbidden response only when turned into one at the HTTP edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeDenied;

impl IntoResponse for ScopeDenied {
    fn into_response(self) -> Response {
        // An authorisation refusal carries no `details`: which scope was missing is exactly what a
        // caller probing the key's reach would want to learn.
        api_error(
            ErrorStatus::PermissionDenied,
            "the API key is not authorised for this action",
        )
    }
}

/// Why a request failed authentication. Every variant but [`AuthDenied::StoreUnavailable`] renders
/// the identical `401`, so the reason stays in the server's log and never reaches the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthDenied {
    /// No `Authorization` header at all.
    Missing,
    /// The header is present but not a `Bearer <token>` (bad scheme, non-ASCII, no space).
    Malformed,
    /// The token does not parse, names no known key, or fails [`verify`] (bad secret, revoked,
    /// expired). All collapse to one outcome so nothing can be probed.
    Invalid,
    /// The key store itself could not be read. A retryable `503`, not a denial.
    StoreUnavailable,
}

impl AuthDenied {
    /// Whether this denial is the store's fault (retryable) rather than the credential's.
    #[must_use]
    pub const fn is_store_unavailable(self) -> bool {
        matches!(self, Self::StoreUnavailable)
    }
}

impl IntoResponse for AuthDenied {
    fn into_response(self) -> Response {
        if self.is_store_unavailable() {
            return api_error(
                ErrorStatus::Unavailable,
                "the authentication service is unavailable",
            );
        }
        // One generic 401 for every credential problem, and one generic *body*: the envelope carries
        // no `details`, because a field-level reason here would be the oracle this arm exists to
        // avoid. `WWW-Authenticate` names the scheme so a well-behaved client knows how to present a
        // key, without revealing anything about this one.
        let mut response = api_error(ErrorStatus::Unauthenticated, "unauthorized");
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static(SCHEME));
        response
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::IntoResponse as _;

    use pos_fakes::FakeClock;
    use pos_proto::ids::TenantId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use super::{AuthDenied, authenticate, require_scope};
    use crate::auth::apikey::{
        ApiKeyId, ApiKeyStore, ApiKeyStoreError, Scope, StoredApiKey, issue,
    };

    /// A low-entropy, obviously-fake secret so no real key material is committed.
    const FAKE_SECRET: &str = "fakesecretfortestsonly";

    fn key_id() -> ApiKeyId {
        ApiKeyId::new(Ulid::from_u128(0xA11CE))
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(0x7E11A))
    }

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(secs.saturating_mul(1000)).expect("valid")
    }

    /// An in-memory key store keyed by id, plus a switch to simulate the database being down.
    #[derive(Default)]
    struct FakeKeys {
        rows: Mutex<HashMap<ApiKeyId, StoredApiKey>>,
        down: bool,
    }

    impl FakeKeys {
        fn with(stored: StoredApiKey) -> Self {
            let mut rows = HashMap::new();
            rows.insert(stored.id, stored);
            Self {
                rows: Mutex::new(rows),
                down: false,
            }
        }

        fn unavailable() -> Self {
            Self {
                rows: Mutex::new(HashMap::new()),
                down: true,
            }
        }
    }

    impl ApiKeyStore for FakeKeys {
        async fn lookup(&self, id: ApiKeyId) -> Result<Option<StoredApiKey>, ApiKeyStoreError> {
            if self.down {
                return Err(ApiKeyStoreError::new("the store is down"));
            }
            Ok(self.rows.lock().expect("lock").get(&id).cloned())
        }
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid header"),
        );
        headers
    }

    fn issued(scopes: &[Scope]) -> (StoredApiKey, String) {
        issue(
            key_id(),
            tenant(),
            scopes.iter().copied().collect(),
            FAKE_SECRET,
            None,
        )
    }

    #[tokio::test]
    async fn a_valid_key_authenticates_to_its_grant() {
        let (stored, token) = issued(&[Scope::ReadRollups]);
        let keys = FakeKeys::with(stored);
        let clock = FakeClock::new(at(100));

        let grant = authenticate(&keys, &clock, &bearer(&token))
            .await
            .expect("a valid key authenticates");
        assert_eq!(
            grant.tenant(),
            tenant(),
            "the grant carries the key's tenant"
        );
        require_scope(&grant, Scope::ReadRollups).expect("the scope was granted");
    }

    #[tokio::test]
    async fn an_unknown_id_and_a_bad_secret_are_the_same_refusal() {
        let (stored, token) = issued(&[Scope::ReadRollups]);
        let clock = FakeClock::new(at(100));

        // Unknown id: the store has no such key.
        let empty = FakeKeys::default();
        assert_eq!(
            authenticate(&empty, &clock, &bearer(&token)).await,
            Err(AuthDenied::Invalid),
            "an unknown key is Invalid, never a distinct 'not found'"
        );

        // Wrong secret against a known id.
        let keys = FakeKeys::with(stored);
        let forged = format!("pos_{}_wrongsecret", key_id());
        assert_eq!(
            authenticate(&keys, &clock, &bearer(&forged)).await,
            Err(AuthDenied::Invalid),
            "a bad secret is the same Invalid, so the two cannot be told apart"
        );
    }

    #[tokio::test]
    async fn a_missing_or_malformed_header_is_refused() {
        let clock = FakeClock::new(at(100));
        let keys = FakeKeys::default();

        assert_eq!(
            authenticate(&keys, &clock, &HeaderMap::new()).await,
            Err(AuthDenied::Missing)
        );

        let mut wrong_scheme = HeaderMap::new();
        wrong_scheme.insert(AUTHORIZATION, HeaderValue::from_static("Basic abc123"));
        assert_eq!(
            authenticate(&keys, &clock, &wrong_scheme).await,
            Err(AuthDenied::Malformed),
            "only the Bearer scheme is accepted"
        );
    }

    #[tokio::test]
    async fn a_store_outage_is_retryable_not_a_denial() {
        let (_stored, token) = issued(&[Scope::ReadRollups]);
        let clock = FakeClock::new(at(100));
        let keys = FakeKeys::unavailable();

        let denied = authenticate(&keys, &clock, &bearer(&token))
            .await
            .expect_err("the store is down");
        assert_eq!(denied, AuthDenied::StoreUnavailable);
        assert_eq!(
            denied.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a store outage must not masquerade as a bad credential"
        );
    }

    #[tokio::test]
    async fn an_ungranted_scope_is_forbidden_even_for_a_valid_key() {
        let (stored, token) = issued(&[Scope::ReadRollups]);
        let keys = FakeKeys::with(stored);
        let clock = FakeClock::new(at(100));

        let grant = authenticate(&keys, &clock, &bearer(&token))
            .await
            .expect("valid");
        let refused = require_scope(&grant, Scope::PlaceOrders).expect_err("not granted");
        assert_eq!(refused.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn every_credential_problem_renders_the_identical_401() {
        // The three client-facing variants must be byte-for-byte indistinguishable in the response,
        // or the difference is itself an oracle.
        let mut statuses = Vec::new();
        for denied in [
            AuthDenied::Missing,
            AuthDenied::Malformed,
            AuthDenied::Invalid,
        ] {
            let response = denied.into_response();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response
                    .headers()
                    .get(WWW_AUTHENTICATE)
                    .expect("the scheme is advertised"),
                "Bearer"
            );
            statuses.push(response.status());
        }
        assert!(
            statuses
                .iter()
                .all(|status| *status == StatusCode::UNAUTHORIZED)
        );
    }
}
