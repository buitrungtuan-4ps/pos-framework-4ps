// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Signature verification for over-the-air updates.
//!
//! # This port is synchronous, and that is a correction to ADR-0013
//!
//! [ADR-0013](../../../docs/adr/0013-async-strategy.md) says every port but the two
//! determinism traits is asynchronous. Verification is the exception, for two reasons.
//! Verifying a signature is arithmetic over bytes — there is nothing to await. And it runs
//! during update verification at **startup**, potentially before an async runtime exists,
//! so an `async fn` would force a runtime into the one code path that most needs to work
//! when everything else is broken. See
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §5.
//!
//! # Verification only, never signing
//!
//! `docs/architecture.md` §4 keeps signing offline, on a machine with the private key on a
//! USB stick and a paper copy in a safe. There is deliberately no `sign` method anywhere in
//! this framework: a port that could sign would be a port an attacker could reach.
//!
//! # Two keys, and a revocation list
//!
//! Both public keys are baked into the binary so a compromised key can be retired without a
//! release that itself needs the compromised key to be trusted. Revocation is published by
//! the cloud and arrives through [`crate::ConfigStore`], not here — this port answers
//! "is this signature valid for this key?", and *whether that key is still trusted* is a
//! policy question with a different answer tomorrow.

use core::fmt;

use crate::error::PortError;

/// A detached signature over an artifact.
#[derive(Clone, PartialEq, Eq)]
pub struct Signature(Vec<u8>);

/// A public verification key.
///
/// Compared in constant time by [`PublicKey::eq`] — see that implementation for why a
/// public value still deserves it.
#[derive(Clone)]
pub struct PublicKey {
    key_id: KeyId,
    bytes: Vec<u8>,
}

/// Identifies which key signed something.
///
/// minisign calls this the key id and puts it in the signature, so a verifier can pick the
/// right key instead of trying both. Eight bytes, as minisign defines it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyId([u8; 8]);

impl KeyId {
    /// Wraps a key id.
    #[must_use]
    pub const fn new(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ")")
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Signature {
    /// Wraps signature bytes.
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Length only. A signature is not secret, but printing one into a log adds a
        // kilobyte of noise per line and teaches nobody anything.
        write!(f, "Signature({} bytes)", self.0.len())
    }
}

impl PublicKey {
    /// Wraps a public key and its id.
    #[must_use]
    pub const fn new(key_id: KeyId, bytes: Vec<u8>) -> Self {
        Self { key_id, bytes }
    }

    /// Which key this is.
    #[must_use]
    pub const fn key_id(&self) -> KeyId {
        self.key_id
    }

    /// The raw key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// Constant-time comparison, for a value that is not secret.
///
/// Public keys are public, so this is not about confidentiality. It is about **not leaking
/// which key a comparison was against** through timing: a fleet with two trusted keys,
/// one retired, gives an attacker a way to learn which one a given machine still accepts,
/// and that is reconnaissance for a downgrade attempt. The cost is a few nanoseconds on a
/// path that runs once per update.
impl PartialEq for PublicKey {
    fn eq(&self, other: &Self) -> bool {
        if self.bytes.len() != other.bytes.len() {
            return false;
        }
        let mut difference = 0_u8;
        for (left, right) in self.bytes.iter().zip(other.bytes.iter()) {
            difference |= left ^ right;
        }
        difference == 0 && self.key_id == other.key_id
    }
}

impl Eq for PublicKey {}

/// Verifies detached signatures.
///
/// # Contract
///
/// 1. **A signature is valid or it is an error.** There is no third state, and no
///    "valid but expired" — freshness is the caller's business, not the mathematics'.
/// 2. **A key-id mismatch is [`PortError::invalid_argument`], not a verification failure.**
///    They mean different things: the first says "wrong key, try the other one", the second
///    says "this artifact is not what it claims to be". Collapsing them would make a
///    two-key rollout indistinguishable from an attack.
/// 3. **Verification is total.** No input — truncated, empty, or hostile — may panic. The
///    backbone crates are compiled with `-F clippy::panic` and `-F clippy::indexing_slicing`
///    precisely so this obligation is checked rather than promised.
pub trait Signer {
    /// Verifies `signature` over `artifact` using `key`.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if the signature names a different key than `key`, or
    /// is malformed; [`PortError::permission_denied`] if the signature is well-formed for
    /// this key and does not verify — which is the case an operator must never see
    /// automatically retried.
    fn verify(
        &self,
        artifact: &[u8],
        signature: &Signature,
        key: &PublicKey,
    ) -> Result<(), PortError>;

    /// Which key a signature claims to be from, without verifying it.
    ///
    /// Lets a caller choose between the two baked-in keys, and lets a revocation check
    /// happen *before* the artifact is trusted rather than after.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if the signature is too short or malformed to carry a
    /// key id.
    fn key_id_of(&self, signature: &Signature) -> Result<KeyId, PortError>;
}

#[cfg(test)]
mod tests {
    use super::{KeyId, PublicKey, Signature};

    #[test]
    fn a_key_id_renders_as_lowercase_hex() {
        let id = KeyId::new([0x00, 0x01, 0x0a, 0xff, 0x10, 0x20, 0x30, 0x40]);
        assert_eq!(id.to_string(), "00010aff10203040");
        assert_eq!(format!("{id:?}"), "KeyId(00010aff10203040)");
    }

    #[test]
    fn debug_prints_a_signature_length_not_a_kilobyte_of_hex() {
        let signature = Signature::new(vec![7; 64]);
        assert_eq!(format!("{signature:?}"), "Signature(64 bytes)");
    }

    #[test]
    fn debug_of_a_key_does_not_dump_its_bytes() {
        let key = PublicKey::new(KeyId::new([1; 8]), vec![9; 32]);
        let rendered = format!("{key:?}");
        assert!(rendered.contains("key_id"));
        assert!(!rendered.contains('9'), "got {rendered}");
    }

    #[test]
    fn keys_compare_by_id_and_by_bytes() {
        let first = PublicKey::new(KeyId::new([1; 8]), vec![9; 32]);
        assert_eq!(first, PublicKey::new(KeyId::new([1; 8]), vec![9; 32]));
        assert_ne!(
            first,
            PublicKey::new(KeyId::new([2; 8]), vec![9; 32]),
            "same bytes, different id, is a different key"
        );
        assert_ne!(first, PublicKey::new(KeyId::new([1; 8]), vec![8; 32]));
        assert_ne!(
            first,
            PublicKey::new(KeyId::new([1; 8]), vec![9; 31]),
            "a truncated key must not compare equal to the key it was truncated from"
        );
    }
}
