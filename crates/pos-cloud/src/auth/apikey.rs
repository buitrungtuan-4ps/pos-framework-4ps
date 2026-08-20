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
use core::future::Future;
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
    /// Pull the store's own configuration updates (`GET /sync/stores/{id}/config`). The credential a
    /// first-party store holds to keep its config current ([ADR-0039](../../../docs/adr/0039-config-delivery.md)).
    ReadConfig,
    /// Propose discovered devices and read the store's approved ones (`/sync/stores/{id}/devices`)
    /// ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
    ManageDevices,
}

impl Scope {
    /// This scope's wire name — how it is stored in the key's `scopes` column and named when a key is
    /// provisioned. `snake_case`, per the naming standard.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::ReadRollups => "read_rollups",
            Self::ReadEvents => "read_events",
            Self::ManageWebhooks => "manage_webhooks",
            Self::ReadConfig => "read_config",
            Self::ManageDevices => "manage_devices",
        }
    }

    /// Parses a wire name back to a scope, or `None` for a name this build does not know.
    ///
    /// Deny-by-default on read: an unrecognised name — a capability a newer issuer granted that this
    /// binary predates — is dropped rather than guessed at, so an older reader can never
    /// over-authorise a key it does not fully understand.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "read_rollups" => Some(Self::ReadRollups),
            "read_events" => Some(Self::ReadEvents),
            "manage_webhooks" => Some(Self::ManageWebhooks),
            "read_config" => Some(Self::ReadConfig),
            "manage_devices" => Some(Self::ManageDevices),
            _ => None,
        }
    }
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

impl StoredApiKey {
    /// Rebuilds a stored key from its persisted columns — the inverse of what a row holds, used by
    /// the persistence adapter to rehydrate a key on lookup.
    ///
    /// Unknown scope names are dropped ([`Scope::from_wire`], deny-by-default). This does not check
    /// the secret; the caller [`verify`]s a presented secret against the rebuilt record.
    ///
    /// # Errors
    ///
    /// A human-readable message if `tenant_id` is not a ULID, `secret_hash` is not exactly 32 bytes,
    /// or `expires_at_ms` is out of the representable range.
    pub fn from_parts(
        id: ApiKeyId,
        tenant_id: &str,
        secret_hash: &[u8],
        scopes: &[String],
        revoked: bool,
        expires_at_ms: Option<i64>,
    ) -> Result<Self, String> {
        let tenant_id: TenantId = tenant_id
            .parse()
            .map_err(|_| format!("api key {id}: tenant id is not a ULID"))?;
        let secret_hash: [u8; 32] = secret_hash
            .try_into()
            .map_err(|_| format!("api key {id}: secret hash is not 32 bytes"))?;
        let scopes = scopes
            .iter()
            .filter_map(|name| Scope::from_wire(name))
            .collect();
        let expires_at = match expires_at_ms {
            Some(milliseconds) => Some(
                Timestamp::from_milliseconds_since_epoch(milliseconds)
                    .map_err(|_| format!("api key {id}: expiry is out of range"))?,
            ),
            None => None,
        };
        Ok(Self {
            id,
            tenant_id,
            secret_hash,
            scopes,
            revoked,
            expires_at,
        })
    }

    /// The stored `SHA-256(secret)` — for the persistence adapter to write on [`issue`]. This is the
    /// hash, never the secret (which is unrecoverable after issuance).
    #[must_use]
    pub const fn secret_hash(&self) -> [u8; 32] {
        self.secret_hash
    }

    /// The granted scopes as their wire names, sorted — for the persistence adapter to write and for
    /// a listing to show.
    #[must_use]
    pub fn scope_wire_names(&self) -> Vec<String> {
        // `scopes` is a `BTreeSet`, so this is already sorted and deduplicated.
        self.scopes
            .iter()
            .map(|scope| scope.as_wire().to_owned())
            .collect()
    }

    /// The expiry as milliseconds since the Unix epoch, or `None` if the key never expires — for the
    /// persistence adapter.
    #[must_use]
    pub fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at.map(Timestamp::as_milliseconds_since_epoch)
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

/// The store that persists issued API keys (a table in `store-postgres`; a fake in tests).
///
/// Lookup is by [`ApiKeyId`] — the public half of the token — so [`verify`] can fetch the single
/// candidate record and check the secret against it in constant time. A miss (`Ok(None)`) is *not* a
/// store error: it is an unknown key, and the caller must treat it exactly as it treats a bad secret,
/// so a prober cannot tell "no such key" from "wrong secret". A returned [`ApiKeyStoreError`] means
/// the backing store itself failed — the database is unreachable — which is the caller's cue to
/// answer retryably rather than deny.
pub trait ApiKeyStore {
    /// Fetches the stored key with `id`, or `None` if there is no such key.
    ///
    /// # Errors
    ///
    /// [`ApiKeyStoreError`] only if the store itself could not be read — never for a missing key.
    fn lookup(
        &self,
        id: ApiKeyId,
    ) -> impl Future<Output = Result<Option<StoredApiKey>, ApiKeyStoreError>> + Send;
}

/// The write side of the API-key store: provisioning, listing, and revoking keys
/// ([ADR-0037](../../../docs/adr/0037-api-keys.md)). Kept separate from [`ApiKeyStore`] so the
/// per-request bearer read path depends only on `lookup` and nothing on the far larger admin surface.
///
/// The super-admin drives all three, through the [`super::admin`]-guarded `/admin/api-keys` routes;
/// the tenant a key acts for is named by the admin, since the super-admin is global.
pub trait ApiKeyAdminStore {
    /// Persists a freshly [`issue`]d key. The secret is already gone — only [`StoredApiKey`] (its
    /// hash and metadata) is stored.
    ///
    /// # Errors
    ///
    /// [`ApiKeyStoreError`] if the store could not be written (including a duplicate id, which a
    /// CSPRNG id makes astronomically unlikely).
    fn insert(
        &self,
        key: &StoredApiKey,
    ) -> impl Future<Output = Result<(), ApiKeyStoreError>> + Send;

    /// Lists a tenant's keys as metadata only — never a secret or its hash.
    ///
    /// # Errors
    ///
    /// [`ApiKeyStoreError`] if the store could not be read.
    fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<ApiKeySummary>, ApiKeyStoreError>> + Send;

    /// Revokes the key with `id`, returning whether a key was found to revoke. Idempotent: revoking
    /// an already-revoked or absent key is `Ok(false)`, not an error.
    ///
    /// # Errors
    ///
    /// [`ApiKeyStoreError`] if the store could not be written.
    fn revoke(&self, id: ApiKeyId) -> impl Future<Output = Result<bool, ApiKeyStoreError>> + Send;
}

/// A key's metadata for a listing — everything but the secret and its hash, which never leave the
/// store. Serialises to the `/admin/api-keys` list response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApiKeySummary {
    /// The public id (the ULID half of the token).
    pub id: String,
    /// The granted scopes, as their wire names, sorted.
    pub scopes: Vec<String>,
    /// Whether the key has been revoked.
    pub revoked: bool,
    /// When the key expires, in milliseconds since the Unix epoch, if ever.
    pub expires_at_ms: Option<i64>,
}

/// A failure of the API-key store itself — the database is unreachable — as distinct from a key
/// simply not being present, which is `Ok(None)`.
#[derive(Debug, thiserror::Error)]
#[error("the API-key store failed: {0}")]
pub struct ApiKeyStoreError(String);

impl ApiKeyStoreError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
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
