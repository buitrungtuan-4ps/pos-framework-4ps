// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Scoped per-tenant API keys — programmatic access to the public `/v1` surface
//! ([ADR-0037](../../../docs/adr/0037-api-keys.md)).
//!
//! The super-admin ([`super`]) signs in interactively; a machine integrator presents an **API key**
//! instead. A key is `pos_<id>_<secret>`: the `id` is public and looks the key up, the `secret` is
//! the bearer proof. Three properties make it safe:
//!
//!  * **Only a hash is stored.** The cloud keeps `SHA-256(secret)`, never the secret, so a database
//!    leak yields no usable key. SHA-256 (not Argon2) is right here: the secret is a long random
//!    token, not a low-entropy human password, so there is nothing to slow a dictionary attack
//!    against, and a fast hash keeps per-request verification cheap. The compare is constant-time.
//!  * **Every key is bound to one tenant.** [`Grant::tenant`] is the tenant the key may act for, and a
//!    handler must check the resource's tenant against it — this is the isolation that stops one
//!    tenant's key reaching another's data.
//!  * **Deny by default, by scope.** A key authorises only the [`Scope`]s it was granted
//!    ([`Grant::authorizes`]); anything else is refused.
//!
//! The randomness — the id and the secret — is generated at the binary edge (a CSPRNG) and passed to
//! [`issue`], which returns the record to store and the one-time token to hand back; the token is
//! never recoverable afterwards. This module is otherwise pure and deterministic.

use core::fmt;
use std::collections::BTreeSet;

use sha2::{Digest as _, Sha256};

use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

/// The token prefix. Fixed and recognisable so a leaked key is detectable by secret scanners (and by
/// this repo's own `secrets` CI job).
pub const TOKEN_PREFIX: &str = "pos_";

/// A capability an API key may be granted. Deny-by-default: a key holds a set of these and authorises
/// nothing outside it. The set grows with the public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Read per-store activity rollups (`GET /v1/stores/{id}/rollups/daily`).
    ReadRollups,
    /// Read the raw event stream.
    ReadEvents,
    /// Create, list, and delete webhook endpoints.
    ManageWebhooks,
}

/// A key's public identifier — travels in the token in the clear and looks the key up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiKeyId(Ulid);

impl ApiKeyId {
    /// Wraps a ULID as a key id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for ApiKeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A token as presented by a client, split into its id and secret.
///
/// The secret is redacted from [`fmt::Debug`], so a presented key cannot be logged.
#[derive(Clone)]
pub struct PresentedKey {
    /// The public id, used to look the stored key up.
    pub id: ApiKeyId,
    secret: String,
}

impl fmt::Debug for PresentedKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PresentedKey")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Parses a `pos_<id>_<secret>` token.
///
/// # Errors
///
/// [`ApiKeyError::Malformed`] if the prefix, the id, or the secret is missing or the id is not a
/// ULID.
pub fn parse(token: &str) -> Result<PresentedKey, ApiKeyError> {
    let body = token
        .strip_prefix(TOKEN_PREFIX)
        .ok_or(ApiKeyError::Malformed)?;
    // A ULID is Crockford base32 and a secret carries no underscore, so the first `_` splits them.
    let (id, secret) = body.split_once('_').ok_or(ApiKeyError::Malformed)?;
    if secret.is_empty() {
        return Err(ApiKeyError::Malformed);
    }
    let ulid = id.parse::<Ulid>().map_err(|_| ApiKeyError::Malformed)?;
    Ok(PresentedKey {
        id: ApiKeyId::new(ulid),
        secret: secret.to_owned(),
    })
}

/// The stored record for an API key: everything but the secret itself.
///
/// `secret_hash` is `SHA-256(secret)`; the secret is never stored. [`fmt::Debug`] redacts the hash
/// too, so nothing key-shaped reaches a log.
#[derive(Clone)]
pub struct StoredApiKey {
    /// The public id.
    pub id: ApiKeyId,
    /// The tenant this key acts for. The one field isolation rests on.
    pub tenant_id: TenantId,
    secret_hash: [u8; 32],
    /// The scopes granted. Deny-by-default: anything absent is refused.
    pub scopes: BTreeSet<Scope>,
    /// Whether the key has been revoked.
    pub revoked: bool,
    /// When the key expires, if ever.
    pub expires_at: Option<Timestamp>,
}

impl fmt::Debug for StoredApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredApiKey")
            .field("id", &self.id)
            .field("tenant_id", &self.tenant_id)
            .field("secret_hash", &"<redacted>")
            .field("scopes", &self.scopes)
            .field("revoked", &self.revoked)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Issues a new key: builds the record to store and the one-time token to return.
///
/// `id` and `secret` are generated by the caller from a CSPRNG. The returned token
/// (`pos_<id>_<secret>`) is the only time the secret is visible; only its hash is kept.
#[must_use]
pub fn issue(
    id: ApiKeyId,
    tenant_id: TenantId,
    scopes: BTreeSet<Scope>,
    secret: &str,
    expires_at: Option<Timestamp>,
) -> (StoredApiKey, String) {
    let token = format!("{TOKEN_PREFIX}{id}_{secret}");
    let stored = StoredApiKey {
        id,
        tenant_id,
        secret_hash: hash_secret(secret),
        scopes,
        revoked: false,
        expires_at,
    };
    (stored, token)
}

/// What a verified key may do: act for one tenant, within a set of scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    tenant_id: TenantId,
    scopes: BTreeSet<Scope>,
}

impl Grant {
    /// The tenant the key may act for. A handler must check the resource's tenant against this.
    #[must_use]
    pub fn tenant(&self) -> TenantId {
        self.tenant_id
    }

    /// Whether the key was granted `scope`. Deny by default.
    #[must_use]
    pub fn authorizes(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// Why an API key was rejected. For the server's log; the client is told only that it was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyError {
    /// The token is not a well-formed `pos_<id>_<secret>`.
    #[error("the API key is malformed")]
    Malformed,
    /// The presented id does not match the looked-up record.
    #[error("the API key id does not match")]
    IdMismatch,
    /// The secret does not match the stored hash.
    #[error("the API key secret is incorrect")]
    BadSecret,
    /// The key has been revoked.
    #[error("the API key has been revoked")]
    Revoked,
    /// The key has expired.
    #[error("the API key has expired")]
    Expired,
}

/// Verifies a presented key against its stored record as of `now`.
///
/// The caller looks `stored` up by [`PresentedKey::id`]; this confirms the secret, that the ids agree,
/// and that the key is neither revoked nor expired, then returns the [`Grant`]. The secret comparison
/// is constant-time.
///
/// # Errors
///
/// [`ApiKeyError`] naming the reason (for logging; the client sees a single generic refusal).
pub fn verify(
    stored: &StoredApiKey,
    presented: &PresentedKey,
    now: Timestamp,
) -> Result<Grant, ApiKeyError> {
    if stored.id != presented.id {
        return Err(ApiKeyError::IdMismatch);
    }
    if !constant_time_eq(&hash_secret(&presented.secret), &stored.secret_hash) {
        return Err(ApiKeyError::BadSecret);
    }
    if stored.revoked {
        return Err(ApiKeyError::Revoked);
    }
    if stored.expires_at.is_some_and(|expiry| now >= expiry) {
        return Err(ApiKeyError::Expired);
    }
    Ok(Grant {
        tenant_id: stored.tenant_id,
        scopes: stored.scopes.clone(),
    })
}

/// `SHA-256` of a secret's bytes — what is stored and compared, never the secret itself.
fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// Constant-time equality over two 32-byte hashes: no early return on the first differing byte.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0_u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{ApiKeyError, ApiKeyId, Scope, StoredApiKey, issue, parse, verify};

    use std::collections::BTreeSet;

    use pos_proto::ids::TenantId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    fn key_id() -> ApiKeyId {
        ApiKeyId::new(Ulid::from_u128(0xA11CE))
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(0x7E11A))
    }

    fn scopes(items: &[Scope]) -> BTreeSet<Scope> {
        items.iter().copied().collect()
    }

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(secs.saturating_mul(1000)).expect("valid")
    }

    /// A low-entropy, obviously-fake secret so no real key material is committed.
    const FAKE_SECRET: &str = "fakesecretfortestsonly";

    fn issued(scopes_granted: &[Scope], expires_at: Option<Timestamp>) -> (StoredApiKey, String) {
        issue(
            key_id(),
            tenant(),
            scopes(scopes_granted),
            FAKE_SECRET,
            expires_at,
        )
    }

    #[test]
    fn issue_then_present_then_verify_round_trips() {
        let (stored, token) = issued(&[Scope::ReadRollups], None);
        assert!(
            token.starts_with("pos_"),
            "the token carries the scanner-visible prefix"
        );
        assert!(
            !token.contains('<'),
            "the token is the real value, shown once"
        );

        let presented = parse(&token).expect("the issued token parses");
        assert_eq!(presented.id, key_id());
        let grant = verify(&stored, &presented, at(100)).expect("a fresh key verifies");
        assert_eq!(
            grant.tenant(),
            tenant(),
            "the grant is scoped to the key's tenant"
        );
        assert!(grant.authorizes(Scope::ReadRollups));
        assert!(
            !grant.authorizes(Scope::ManageWebhooks),
            "deny by default: an ungranted scope is refused"
        );
    }

    #[test]
    fn a_wrong_secret_is_refused() {
        let (stored, _token) = issued(&[Scope::ReadRollups], None);
        let forged = parse(&format!("pos_{}_wrongsecret", key_id())).expect("parses");
        assert_eq!(
            verify(&stored, &forged, at(100)),
            Err(ApiKeyError::BadSecret)
        );
    }

    #[test]
    fn a_key_for_another_id_does_not_verify() {
        let (stored, _token) = issued(&[Scope::ReadRollups], None);
        let other = parse(&format!(
            "pos_{}_{}",
            ApiKeyId::new(Ulid::from_u128(0xBEEF)),
            FAKE_SECRET
        ))
        .expect("parses");
        assert_eq!(
            verify(&stored, &other, at(100)),
            Err(ApiKeyError::IdMismatch)
        );
    }

    #[test]
    fn a_revoked_key_is_refused() {
        let (mut stored, token) = issued(&[Scope::ReadRollups], None);
        stored.revoked = true;
        let presented = parse(&token).expect("parses");
        assert_eq!(
            verify(&stored, &presented, at(100)),
            Err(ApiKeyError::Revoked)
        );
    }

    #[test]
    fn an_expired_key_is_refused_but_a_live_one_is_not() {
        let (stored, token) = issued(&[Scope::ReadRollups], Some(at(1_000)));
        let presented = parse(&token).expect("parses");
        assert!(verify(&stored, &presented, at(999)).is_ok(), "still live");
        assert_eq!(
            verify(&stored, &presented, at(1_000)),
            Err(ApiKeyError::Expired),
            "expiry is inclusive"
        );
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert_eq!(
            parse("no-prefix_abc_def").unwrap_err(),
            ApiKeyError::Malformed
        );
        assert_eq!(parse("pos_").unwrap_err(), ApiKeyError::Malformed);
        assert_eq!(
            parse("pos_onlyid").unwrap_err(),
            ApiKeyError::Malformed,
            "no secret"
        );
        assert_eq!(
            parse("pos_not-a-ulid_secret").unwrap_err(),
            ApiKeyError::Malformed,
            "the id must be a ULID"
        );
    }

    #[test]
    fn the_stored_key_debug_does_not_leak_the_hash() {
        let (stored, _token) = issued(&[Scope::ReadRollups], None);
        let rendered = format!("{stored:?}");
        assert!(rendered.contains("<redacted>"));
        // The tenant and scopes are fine to show; the hash is not.
        assert!(rendered.contains("ReadRollups"));
    }
}
