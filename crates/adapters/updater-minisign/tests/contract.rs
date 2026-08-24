// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `Signer` suite over the real minisign verifier, plus a legacy-algorithm round trip.
//!
//! The harness signs with throwaway Ed25519 keypairs derived from fixed seeds — real signatures,
//! real verification, but never a production key (those are generated offline and never enter this
//! repository, [ADR-0047](../../../docs/adr/0047-minisign-verification.md)). The port is synchronous,
//! so the suite runs under pos-fakes' one-poll `run_ready` rather than a runtime.

use blake2::{Blake2b512, Digest as _};
use ed25519_dalek::{Signer as _, SigningKey};

use pos_contract_tests::harness::{Setup, SignerHarness};
use pos_ports::signer::{KeyId, PublicKey, Signature};
use updater_minisign::MinisignVerifier;

/// Throwaway signing-key seeds — never production keys (those are generated offline, ADR-0047).
const SEED_A: [u8; 32] = [0x11; 32];
const SEED_B: [u8; 32] = [0x22; 32];
/// The 8-byte key ids the two test keys advertise (a minisign key id is arbitrary per keypair).
const KEY_ID_A: [u8; 8] = [0xA1; 8];
const KEY_ID_B: [u8; 8] = [0xB2; 8];

fn signing_key(seed: [u8; 32]) -> SigningKey {
    SigningKey::from_bytes(&seed)
}

fn public_key(seed: [u8; 32], key_id: [u8; 8]) -> PublicKey {
    let bytes = signing_key(seed).verifying_key().to_bytes().to_vec();
    PublicKey::new(KeyId::new(key_id), bytes)
}

/// Builds a minisign signature blob (`algorithm ∥ key_id ∥ ed25519_sig`) over `artifact`, prehashed
/// (`ED`, BLAKE2b-512) or legacy (`Ed`, raw).
fn signature(prehashed: bool, artifact: &[u8], seed: [u8; 32], key_id: [u8; 8]) -> Signature {
    let key = signing_key(seed);
    let sig = if prehashed {
        let mut hasher = Blake2b512::new();
        hasher.update(artifact);
        key.sign(hasher.finalize().as_slice())
    } else {
        key.sign(artifact)
    };
    let mut blob = Vec::with_capacity(74);
    blob.extend_from_slice(if prehashed { b"ED" } else { b"Ed" });
    blob.extend_from_slice(&key_id);
    blob.extend_from_slice(&sig.to_bytes());
    Signature::new(blob)
}

/// The suite's fixture: a real minisign verifier, and prehashed signatures to feed it.
#[derive(Debug, Default)]
struct MinisignHarness;

impl SignerHarness for MinisignHarness {
    type Signer = MinisignVerifier;

    async fn fresh(&self) -> Setup<Self::Signer> {
        Ok(MinisignVerifier::new())
    }

    fn valid_triple(&self) -> Setup<(Vec<u8>, Signature, PublicKey)> {
        let artifact = b"pos_edge 1.4.2 release artifact".to_vec();
        let sig = signature(true, &artifact, SEED_A, KEY_ID_A);
        Ok((artifact, sig, public_key(SEED_A, KEY_ID_A)))
    }

    fn other_key(&self) -> Setup<PublicKey> {
        Ok(public_key(SEED_B, KEY_ID_B))
    }
}

mod signer {
    use super::MinisignHarness;
    pos_contract_tests::signer_suite!(MinisignHarness, pos_fakes::run_ready);
}

/// The legacy `Ed` algorithm (Ed25519 over the raw artifact) verifies too, and `key_id_of` reads the
/// claimed key id regardless of algorithm.
#[test]
fn verifies_a_legacy_algorithm_signature() {
    use pos_ports::signer::Signer as _;
    let artifact = b"a legacy-signed artifact".to_vec();
    let sig = signature(false, &artifact, SEED_A, KEY_ID_A);
    let key = public_key(SEED_A, KEY_ID_A);
    let verifier = MinisignVerifier::new();
    assert!(
        verifier.verify(&artifact, &sig, &key).is_ok(),
        "a valid legacy Ed signature verifies"
    );
    assert_eq!(
        verifier.key_id_of(&sig).ok(),
        Some(KeyId::new(KEY_ID_A)),
        "key_id_of reads the claimed key id"
    );
}
