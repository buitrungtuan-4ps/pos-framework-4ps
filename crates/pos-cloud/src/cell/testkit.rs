//! Signed directories for tests, read from `fixtures/`.
//!
//! The fixtures were signed once, offline, with a throwaway key that was never written down. Only the
//! signed documents, their `.minisig` files and the public key are committed: no key, throwaway or
//! not, is in the repository (ADR-0179: the owner holds the real directory key). Each document is
//! the same two-cell directory, `vn1` and `jp1`, at a different version:
//!
//! | fixture | what it is |
//! |---|---|
//! | `v2`, `v3`, `v4` | well-signed and valid, for version ordering |
//! | `v5-forged` | names the key's id, signed by another key |
//! | `v6-no-fallback` | well-signed, with no `*` country |
//! | `v7-elsewhere` | well-signed, listing `vn1` at another address |
//!
//! A tampered document is made at run time, by flipping one byte of a signed one.

/// One signed fixture: the document and its `.minisig` file.
pub(crate) struct Signed {
    pub(crate) document: &'static [u8],
    pub(crate) minisig: &'static [u8],
}

macro_rules! signed {
    ($name:literal) => {
        Signed {
            document: include_bytes!(concat!("fixtures/directory-", $name, ".json")),
            minisig: include_bytes!(concat!("fixtures/directory-", $name, ".json.minisig")),
        }
    };
}

pub(crate) const V2: Signed = signed!("v2");
pub(crate) const V3: Signed = signed!("v3");
pub(crate) const V4: Signed = signed!("v4");
pub(crate) const V5_FORGED: Signed = signed!("v5-forged");
pub(crate) const V6_NO_FALLBACK: Signed = signed!("v6-no-fallback");
pub(crate) const V7_ELSEWHERE: Signed = signed!("v7-elsewhere");

/// The public key line: the second line of `fixtures/directory.pub`, as an operator copies it.
pub(crate) fn public_key_line() -> &'static str {
    include_str!("fixtures/directory.pub")
        .lines()
        .nth(1)
        .unwrap_or_default()
}

/// A copy of `document` with one byte flipped, so its signature no longer matches.
pub(crate) fn tampered(document: &[u8]) -> Vec<u8> {
    let mut bytes = document.to_vec();
    if let Some(byte) = bytes
        .iter_mut()
        .find(|byte| **byte == b'3' || **byte == b'4')
    {
        *byte = if *byte == b'3' { b'8' } else { b'9' };
    }
    bytes
}
