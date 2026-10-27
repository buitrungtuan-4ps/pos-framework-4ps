//! Signing for tests: a throwaway Ed25519 key from a fixed seed, never a real directory key
//! (ADR-0179: the owner holds that one, offline). It writes what `minisign` writes, so the code under
//! test reads the same shapes an operator's files have.

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};

/// The throwaway key's seed and id.
const SEED: [u8; 32] = [0x5C; 32];
const KEY_ID: [u8; 8] = [0xD1; 8];
/// A second throwaway key, for a signature by the wrong key.
const OTHER_SEED: [u8; 32] = [0x6D; 32];

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The public key line, as the second line of `minisign.pub` carries it.
pub(crate) fn public_key_line() -> String {
    let mut blob = b"Ed".to_vec();
    blob.extend_from_slice(&KEY_ID);
    blob.extend_from_slice(&SigningKey::from_bytes(&SEED).verifying_key().to_bytes());
    encode(&blob)
}

fn minisig_with(seed: [u8; 32], document: &[u8]) -> Vec<u8> {
    let signature = SigningKey::from_bytes(&seed).sign(document);
    let mut blob = b"Ed".to_vec();
    blob.extend_from_slice(&KEY_ID);
    blob.extend_from_slice(&signature.to_bytes());
    format!(
        "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: test\nAAAA\n",
        encode(&blob)
    )
    .into_bytes()
}

/// A `.minisig` file over `document`, signed by the throwaway key.
pub(crate) fn minisig(document: &[u8]) -> Vec<u8> {
    minisig_with(SEED, document)
}

/// A `.minisig` file over `document` that names the key's id but is signed by another key.
pub(crate) fn forged_minisig(document: &[u8]) -> Vec<u8> {
    minisig_with(OTHER_SEED, document)
}
