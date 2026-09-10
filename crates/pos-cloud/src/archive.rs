// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's half of the store archive: the key a store seals with, and the index of what it has
//! shipped ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
//!
//! # The cloud cannot read an archive, and that is the design
//!
//! A store seals its own database before it leaves the shop, because `subjects` is the one place at
//! a till where personal data sits and nothing else publishes it
//! ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)). What arrives here is bytes: this
//! module stores them, indexes them, and hands the store back the key it seals with. Nothing in
//! `pos-cloud` opens an archive, and there is no code here that could.
//!
//! # Why the key is wrapped in the database — Amendment 1
//!
//! ADR-0124 said the cloud keeps a copy of each store's key so an operator who loses the printout
//! can still restore, and that what the seal buys is *the tier beyond the cloud*: the object store,
//! and the off-box destination `rclone` syncs to.
//!
//! That was only true if the key does not travel to that tier as well — and by default it would
//! have. `deploy/backup.sh` ships a `pg_dump` of this database off-box with `rclone`
//! ([ADR-0046](../../../docs/adr/0046-backups-and-restore.md)), to the same place the archives go.
//! A plaintext key column would put the key and the ciphertext it opens in one bucket, and the
//! seal would buy nothing at exactly the tier it was bought for.
//!
//! So a key is stored **wrapped** under [`ArchiveSecret`] — a value `bootstrap.sh` mints into the
//! box's `cloud.toml`, which is not in the dump, exactly as `internal_shared_secret` already is
//! ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)). The residue, stated:
//! the wrapping secret lives on the same box as the database, so a compromise of the *box* reaches
//! both. What this buys is the tier beyond it.
//!
//! # Why the hex and the cipher are written twice
//!
//! `pos_edge::backup` has its own key type with the same text form and the same cipher. The two are
//! not shared, and should not be: the edge seals a *database* and the cloud wraps a *key*, and the
//! only alternative is `pos-cloud` depending on `pos-edge` — a binary's library, with the bundled
//! SQLite behind it — to reuse forty lines. What must stay in step is the **text form**, which is
//! the operator's copy of the key and the thing typed back in at a bench: 64 lowercase hexadecimal
//! characters, both sides.

use core::fmt;
use core::future::Future;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::Deserialize;
use zeroize::Zeroize;

use pos_proto::ids::StoreId;

/// Bytes in an archive key, and in the secret that wraps one.
const KEY_LEN: usize = 32;

/// Characters in the text form of either.
pub const KEY_TEXT_LEN: usize = KEY_LEN * 2;

/// Bytes of nonce — XChaCha20-Poly1305, so a nonce drawn per wrap needs no counter.
const NONCE_LEN: usize = 24;

/// What a wrap is bound to besides the secret, so a wrapped key cannot be moved between columns.
const WRAP_CONTEXT: &[u8] = b"pos-cloud/store-archive-key/v1";

/// The box-local secret that wraps every store's archive key.
///
/// `bootstrap.sh` mints it into `cloud.toml` and it never leaves the box — the point of the whole
/// arrangement (see the module note). 64 hexadecimal characters, like the keys it wraps, so there
/// is one length and one alphabet for a human to get right.
///
/// [`fmt::Debug`] prints nothing, because [`crate::config::CloudConfig`] derives `Debug`.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct ArchiveSecret(String);

impl ArchiveSecret {
    /// Wraps a secret string as read from configuration. Validity is checked by
    /// [`Self::material`], not here, so a malformed value is one boot refusal rather than a
    /// deserialization error with no context.
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The 32 bytes, or `None` if the configured value is not 64 hexadecimal characters.
    #[must_use]
    pub fn material(&self) -> Option<[u8; KEY_LEN]> {
        from_hex(self.0.trim())
    }

    /// Whether this secret is usable, for [`crate::config::CloudConfig::validate`].
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.material().is_some()
    }
}

impl fmt::Debug for ArchiveSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArchiveSecret(redacted)")
    }
}

/// One store's archive key: the 32 bytes its till seals with.
#[derive(Clone)]
pub struct StoreArchiveKey([u8; KEY_LEN]);

impl StoreArchiveKey {
    /// Mints a key from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`ArchiveKeyError::Entropy`] if the OS entropy source is unavailable — a key is never faked.
    pub fn mint() -> Result<Self, ArchiveKeyError> {
        let mut bytes = [0_u8; KEY_LEN];
        getrandom::fill(&mut bytes).map_err(|_error| ArchiveKeyError::Entropy)?;
        Ok(Self(bytes))
    }

    /// Reads a key from its 64-character hex form.
    ///
    /// # Errors
    ///
    /// [`ArchiveKeyError::Malformed`] if the text is not 64 hexadecimal characters.
    pub fn parse(text: &str) -> Result<Self, ArchiveKeyError> {
        from_hex(text.trim())
            .map(Self)
            .ok_or(ArchiveKeyError::Malformed)
    }

    /// The key as 64 lowercase hexadecimal characters — what a till receives and what the console
    /// shows an operator once.
    #[must_use]
    pub fn to_text(&self) -> String {
        to_hex(&self.0)
    }

    /// Seals this key under the box's secret, for storage.
    ///
    /// # Errors
    ///
    /// [`ArchiveKeyError::Secret`] if the secret is malformed, or [`ArchiveKeyError::Entropy`] if
    /// no nonce could be drawn.
    pub fn wrap(&self, secret: &ArchiveSecret) -> Result<String, ArchiveKeyError> {
        let material = secret.material().ok_or(ArchiveKeyError::Secret)?;
        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_error| ArchiveKeyError::Entropy)?;
        let sealed = XChaCha20Poly1305::new((&material).into())
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &self.0,
                    aad: WRAP_CONTEXT,
                },
            )
            .map_err(|_error| ArchiveKeyError::Secret)?;
        let mut wrapped = Vec::with_capacity(NONCE_LEN + sealed.len());
        wrapped.extend_from_slice(&nonce);
        wrapped.extend_from_slice(&sealed);
        Ok(to_hex(&wrapped))
    }

    /// Recovers a key from what [`Self::wrap`] stored.
    ///
    /// # Errors
    ///
    /// [`ArchiveKeyError::Secret`] if the secret is malformed or is not the one this key was
    /// wrapped under, or [`ArchiveKeyError::Malformed`] if the stored value is not a wrap.
    pub fn unwrap_from(wrapped: &str, secret: &ArchiveSecret) -> Result<Self, ArchiveKeyError> {
        let material = secret.material().ok_or(ArchiveKeyError::Secret)?;
        let bytes = hex_bytes(wrapped.trim()).ok_or(ArchiveKeyError::Malformed)?;
        let nonce = bytes.get(..NONCE_LEN).ok_or(ArchiveKeyError::Malformed)?;
        let body = bytes.get(NONCE_LEN..).ok_or(ArchiveKeyError::Malformed)?;
        let nonce = XNonce::try_from(nonce).map_err(|_error| ArchiveKeyError::Malformed)?;
        let opened = XChaCha20Poly1305::new((&material).into())
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: WRAP_CONTEXT,
                },
            )
            .map_err(|_error| ArchiveKeyError::Secret)?;
        let key: [u8; KEY_LEN] = opened
            .as_slice()
            .try_into()
            .map_err(|_error| ArchiveKeyError::Malformed)?;
        Ok(Self(key))
    }
}

impl Drop for StoreArchiveKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for StoreArchiveKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoreArchiveKey(…)")
    }
}

/// Why a key could not be minted, read or wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveKeyError {
    /// The OS entropy source was unavailable.
    #[error("the OS entropy source is unavailable, so no archive key could be minted")]
    Entropy,
    /// Not 64 hexadecimal characters, or not a wrap this build wrote.
    #[error("an archive key is 64 hexadecimal characters")]
    Malformed,
    /// `archive_key_secret` is absent, malformed, or not the one this key was wrapped under.
    ///
    /// One variant for all three deliberately: an authenticated cipher cannot tell "wrong secret"
    /// from "altered bytes", and the operator's next step — check `cloud.toml` — is the same.
    #[error(
        "archive_key_secret is missing, malformed, or not the secret this key was wrapped under"
    )]
    Secret,
}

/// One archive a store has shipped, as the cloud records it.
///
/// `taken_at` is when the **store** took the snapshot, not when this arrived: a shop that was
/// offline for a day ships yesterday's archive today, and the recovery point is the former.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreArchive {
    /// The store the archive came from.
    pub store_id: StoreId,
    /// Milliseconds since the epoch, from the store's own clock.
    pub taken_at: i64,
    /// Where the bytes are in the object store.
    pub object_key: String,
    /// How many bytes, sealed.
    pub size_bytes: i64,
    /// The cloud's own hex SHA-256 of the sealed bytes as they arrived.
    ///
    /// Not verification — the archive authenticates itself and the cloud cannot open it. It is
    /// what lets a restore drill say "what I downloaded is what the store uploaded" before
    /// spending anybody's time on a key.
    pub sha256: String,
    /// Milliseconds since the epoch, from the cloud's clock, when this arrived.
    pub received_at: i64,
}

/// Why an archive read or write failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveStoreError {
    /// The database could not be reached.
    #[error("the archive registry is unavailable: {0}")]
    Unavailable(String),
}

/// Where the cloud keeps each store's wrapped key and the index of its archives
/// ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
///
/// A seam, in the shape the rest of `pos-cloud` uses: `store-postgres` implements it for the real
/// cloud and a fake implements it for the tests, so the handler logic is exercised without a
/// database.
///
/// The key crosses this boundary **wrapped**, never as itself. Unwrapping happens in the one place
/// that holds [`ArchiveSecret`], which keeps "the database never sees a usable key" a property of
/// the seam rather than a rule somebody has to remember.
pub trait ArchiveStore: Send + Sync {
    /// The wrapped key recorded for `store`, or `None` if it has never asked for one.
    ///
    /// # Errors
    ///
    /// [`ArchiveStoreError::Unavailable`] if the registry could not be read.
    fn wrapped_key(
        &self,
        tenant: &str,
        store: StoreId,
    ) -> impl Future<Output = Result<Option<String>, ArchiveStoreError>> + Send;

    /// Records `wrapped` as this store's key **only if it has none**, and returns the wrapped key
    /// that is now current — which is the existing one when two tills ask at once.
    ///
    /// Insert-if-absent rather than upsert, deliberately: two tills at one shop asking in the same
    /// second must not end up sealing under two different keys, one of which is then unrecoverable.
    /// Rotation is a separate, deliberate act and not this.
    ///
    /// # Errors
    ///
    /// [`ArchiveStoreError::Unavailable`] if the registry could not be written.
    fn adopt_key(
        &self,
        tenant: &str,
        store: StoreId,
        wrapped: &str,
        minted_at: i64,
    ) -> impl Future<Output = Result<String, ArchiveStoreError>> + Send;

    /// Records an archive that arrived. Idempotent on `(tenant, store, taken_at)`: a retried upload
    /// of the same snapshot replaces the row rather than making a second one.
    ///
    /// # Errors
    ///
    /// [`ArchiveStoreError::Unavailable`] if the registry could not be written.
    fn record_archive(
        &self,
        tenant: &str,
        archive: &StoreArchive,
    ) -> impl Future<Output = Result<(), ArchiveStoreError>> + Send;

    /// A store's archives, newest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// [`ArchiveStoreError::Unavailable`] if the registry could not be read.
    fn list_archives(
        &self,
        tenant: &str,
        store: StoreId,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<StoreArchive>, ArchiveStoreError>> + Send;
}

/// Where a store's archives live in the object store.
///
/// Under the store's own id, so [`pos_ports::BlobKey::is_under`]'s segment-aware prefix genuinely
/// scopes a listing to one shop. `taken_at` is zero-padded so lexical order is chronological order,
/// which is what makes a prefix listing useful without sorting it.
#[must_use]
pub fn archive_object_key(store: StoreId, taken_at: i64) -> String {
    format!("stores/{store}/archives/{taken_at:013}.p4p")
}

fn to_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String cannot fail, and the alternative is a panic path in a helper that
        // has no other one.
        let _ = fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"));
    }
    text
}

fn from_hex(text: &str) -> Option<[u8; KEY_LEN]> {
    if text.len() != KEY_TEXT_LEN {
        return None;
    }
    let bytes = hex_bytes(text)?;
    bytes.as_slice().try_into().ok()
}

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(*pair.first()?)?;
            let low = hex_digit(*pair.get(1)?)?;
            Some((high << 4) | low)
        })
        .collect()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveKeyError, ArchiveSecret, KEY_TEXT_LEN, StoreArchiveKey, archive_object_key,
    };

    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    fn secret() -> ArchiveSecret {
        ArchiveSecret::new("00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff")
    }

    fn other_secret() -> ArchiveSecret {
        ArchiveSecret::new("ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100")
    }

    #[test]
    fn a_key_survives_wrapping_and_unwrapping() {
        let minted = StoreArchiveKey::mint().expect("entropy");
        let text = minted.to_text();
        assert_eq!(text.len(), KEY_TEXT_LEN);

        let wrapped = minted.wrap(&secret()).expect("wrap");
        let recovered = StoreArchiveKey::unwrap_from(&wrapped, &secret()).expect("unwrap");

        assert_eq!(recovered.to_text(), text, "the key came back as itself");
    }

    #[test]
    fn what_lands_in_the_column_is_not_the_key() {
        // The whole of Amendment 1 in one assertion: `deploy/backup.sh` ships this column off-box
        // to the same tier the sealed archives go to, so if the stored text contained the key,
        // that bucket would hold both halves.
        let minted = StoreArchiveKey::mint().expect("entropy");
        let wrapped = minted.wrap(&secret()).expect("wrap");

        assert!(
            !wrapped.contains(&minted.to_text()),
            "the wrapped column must not contain the key it wraps"
        );
        assert_ne!(wrapped, minted.to_text());
    }

    #[test]
    fn the_dump_alone_does_not_open_a_key() {
        // A reader who has the database (and therefore the wrapped column) but not the box's
        // `cloud.toml` gets nothing. That reader is exactly the off-box archive tier.
        let wrapped = StoreArchiveKey::mint()
            .expect("entropy")
            .wrap(&secret())
            .expect("wrap");

        assert_eq!(
            StoreArchiveKey::unwrap_from(&wrapped, &other_secret()).map(|_| ()),
            Err(ArchiveKeyError::Secret),
            "another box's secret does not open it"
        );
        assert_eq!(
            StoreArchiveKey::unwrap_from(&wrapped, &ArchiveSecret::new("")).map(|_| ()),
            Err(ArchiveKeyError::Secret),
            "and neither does an absent one"
        );
    }

    #[test]
    fn an_altered_wrap_is_refused_rather_than_yielding_a_wrong_key() {
        let wrapped = StoreArchiveKey::mint()
            .expect("entropy")
            .wrap(&secret())
            .expect("wrap");
        let mut altered = wrapped.clone();
        altered.replace_range(altered.len() - 1.., "0");
        let altered = if altered == wrapped {
            // The last nibble already was zero; flip it the other way.
            let mut other = wrapped.clone();
            other.replace_range(other.len() - 1.., "1");
            other
        } else {
            altered
        };

        assert!(
            matches!(
                StoreArchiveKey::unwrap_from(&altered, &secret()),
                Err(ArchiveKeyError::Secret)
            ),
            "the wrap is authenticated, so a tampered column fails rather than decrypting to junk"
        );
    }

    #[test]
    fn a_secret_that_is_not_a_key_is_refused_before_it_is_used() {
        // The boot refusal `CloudConfig::validate` raises. A passphrase here would be a config
        // that looks armed and produces keys nobody can unwrap.
        assert!(!ArchiveSecret::new("hunter2").is_well_formed());
        assert!(!ArchiveSecret::new("00112233").is_well_formed());
        assert!(!ArchiveSecret::new("z".repeat(KEY_TEXT_LEN)).is_well_formed());
        assert!(secret().is_well_formed());
    }

    #[test]
    fn a_key_round_trips_through_the_text_an_operator_keeps() {
        let text = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let key = StoreArchiveKey::parse(&format!("  {}  ", text.to_uppercase())).expect("parse");
        assert_eq!(
            key.to_text(),
            text,
            "shouted and padded, it is the same key"
        );
        // `map(|_| ())` rather than comparing keys: `StoreArchiveKey` deliberately has no
        // `PartialEq`, because a derived one is a byte-by-byte compare on secret material.
        assert_eq!(
            StoreArchiveKey::parse("nope").map(|_| ()),
            Err(ArchiveKeyError::Malformed)
        );
    }

    #[test]
    fn an_object_key_sorts_chronologically_and_stays_under_its_store() {
        let store = StoreId::new(Ulid::from_u128(0xB00));
        let early = archive_object_key(store, 1_700_000_000_000);
        let late = archive_object_key(store, 1_800_000_000_000);
        assert!(early < late, "lexical order is chronological order");

        let prefix = pos_ports::BlobKey::parse(&format!("stores/{store}")).expect("prefix");
        let key = pos_ports::BlobKey::parse(&late).expect("key");
        assert!(key.is_under(&prefix), "an archive is under its own store");

        let neighbour = pos_ports::BlobKey::parse("stores/OTHER").expect("prefix");
        assert!(!key.is_under(&neighbour));
    }
}
