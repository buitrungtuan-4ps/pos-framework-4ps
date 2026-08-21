// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Minisign signature verification for over-the-air updates
//! ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)).
//!
//! The concrete [`Signer`] the P2 port anticipated: it verifies that a release artifact was signed by
//! an offline key before the binary trusts it, over the audited [`ed25519_dalek`] and [`blake2`]
//! crates. It **only verifies** — `docs/architecture.md` §4 keeps signing offline, so there is no
//! `sign` method here, and this adapter never holds a private key.
//!
//! # The bytes on the wire
//!
//! The port's opaque [`Signature`] is minisign's binary signature blob — `algorithm(2) ∥ key_id(8) ∥
//! ed25519_signature(64)`, 74 bytes, the base64-decoded first line of a `.minisig`. The `algorithm`
//! is `Ed` (legacy: Ed25519 over the raw artifact) or `ED` (prehashed: Ed25519 over
//! `BLAKE2b-512(artifact)`, minisign's default for large files). The port's [`PublicKey`] carries the
//! 8-byte key id and the raw 32-byte Ed25519 public key.
//!
//! # Total over hostile input
//!
//! Verification runs at startup on bytes an attacker chose, so every parse is a checked
//! `slice.get(..)` — never an index — and the crate inherits the backbone's denials of `panic`,
//! `indexing_slicing`, `unwrap_used`, and `expect_used`. The three status distinctions the port fixed
//! are honoured exactly ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §5): a wrong key is
//! `invalid_argument` ("try the other one"), a bad signature for the right key is `permission_denied`
//! (terminal — never auto-retried into an install), and malformed bytes are `invalid_argument`.

use blake2::{Blake2b512, Digest as _};
use ed25519_dalek::{Signature as Ed25519Signature, VerifyingKey};

use pos_ports::signer::{KeyId, PublicKey, Signature, Signer};
use pos_ports::{PortError, PortName};

/// The minisign algorithm tag for legacy Ed25519 over the raw artifact.
const ALGORITHM_LEGACY: [u8; 2] = *b"Ed";
/// The minisign algorithm tag for prehashed Ed25519 over `BLAKE2b-512(artifact)`.
const ALGORITHM_PREHASHED: [u8; 2] = *b"ED";
/// Bytes in a minisign signature blob: `algorithm(2) ∥ key_id(8) ∥ ed25519_signature(64)`.
const SIGNATURE_LEN: usize = 74;
/// Bytes in a raw Ed25519 public key.
const PUBLIC_KEY_LEN: usize = 32;

/// Verifies minisign signatures over the [`Signer`] port
/// ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)).
#[derive(Debug, Clone, Copy, Default)]
pub struct MinisignVerifier;

impl MinisignVerifier {
    /// A verifier. Stateless — the trusted keys are passed to [`Signer::verify`], not held here.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// A malformed-signature error — the caller supplied bytes that are not a well-formed minisign blob.
fn malformed(reason: &'static str) -> PortError {
    PortError::invalid_argument(PortName::Signer, reason)
}

/// The parsed pieces of a minisign signature blob: its algorithm, its claimed key id, and the raw
/// 64-byte Ed25519 signature (as a borrow into the blob).
struct Parsed<'a> {
    algorithm: [u8; 2],
    key_id: KeyId,
    signature: &'a [u8],
}

/// Splits a minisign blob into its parts, rejecting anything too short or of an unknown algorithm.
fn parse(signature: &Signature) -> Result<Parsed<'_>, PortError> {
    let bytes = signature.as_bytes();
    let algorithm: [u8; 2] = bytes
        .get(0..2)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| malformed("a minisign signature begins with two algorithm bytes"))?;
    let key_id: [u8; 8] = bytes
        .get(2..10)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| malformed("a minisign signature carries an eight-byte key id"))?;
    let sig = bytes.get(10..SIGNATURE_LEN).ok_or_else(|| {
        malformed("a minisign signature carries a sixty-four-byte Ed25519 signature")
    })?;
    if algorithm != ALGORITHM_LEGACY && algorithm != ALGORITHM_PREHASHED {
        return Err(malformed("unknown minisign algorithm"));
    }
    Ok(Parsed {
        algorithm,
        key_id: KeyId::new(key_id),
        signature: sig,
    })
}

impl Signer for MinisignVerifier {
    fn verify(
        &self,
        artifact: &[u8],
        signature: &Signature,
        key: &PublicKey,
    ) -> Result<(), PortError> {
        let parsed = parse(signature)?;
        if parsed.key_id != key.key_id() {
            // "Try the other baked-in key", not "this is an attack" — collapsing the two would make
            // a two-key rollout indistinguishable from tampering (ADR-0026 §5).
            return Err(malformed("the signature names a different key"));
        }
        let sig_bytes: [u8; 64] = parsed
            .signature
            .try_into()
            .map_err(|_| malformed("malformed Ed25519 signature"))?;
        let ed_sig = Ed25519Signature::from_bytes(&sig_bytes);
        let key_bytes: [u8; PUBLIC_KEY_LEN] = key
            .as_bytes()
            .try_into()
            .map_err(|_| malformed("an Ed25519 public key is thirty-two bytes"))?;
        let verifying_key = VerifyingKey::from_bytes(&key_bytes)
            .map_err(|_| malformed("not a valid Ed25519 public key"))?;

        let verified = if parsed.algorithm == ALGORITHM_PREHASHED {
            let mut hasher = Blake2b512::new();
            hasher.update(artifact);
            let digest = hasher.finalize();
            verifying_key.verify_strict(digest.as_slice(), &ed_sig)
        } else {
            verifying_key.verify_strict(artifact, &ed_sig)
        };
        verified.map_err(|_| {
            PortError::permission_denied(PortName::Signer, "the signature does not verify")
        })
    }

    fn key_id_of(&self, signature: &Signature) -> Result<KeyId, PortError> {
        // Reads the claimed key id without trusting anything else about the blob — this is what lets
        // a revocation check run before the artifact is verified. Total: too-short input is an error,
        // never a panic.
        let key_id: [u8; 8] = signature
            .as_bytes()
            .get(2..10)
            .and_then(|slice| slice.try_into().ok())
            .ok_or_else(|| malformed("a minisign signature carries an eight-byte key id"))?;
        Ok(KeyId::new(key_id))
    }
}
