// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which signing keys this binary trusts to have signed an update
//! ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md), roadmap v3 slice **R5**).
//!
//! # The problem this closes
//!
//! [`OtaUpdater::new`](crate::OtaUpdater::new) takes `trusted_keys: Vec<PublicKey>` and **nothing in
//! production ever built that vector**. [ADR-0047](../../../docs/adr/0047-minisign-verification.md)
//! said the keys are "baked into this binary"; the mechanism to bake them was never written, so the
//! only source in the tree was a test fixture.
//!
//! That mattered more than a missing accessor usually does. When R5 wires the updater in, the
//! easiest place to find a list of keys is the cloud-published configuration tree — it is already
//! parsed, already typed, and already sitting in `EdgeSession`. A key taken from there is a key an
//! attacker who controls the cloud can choose, which turns the signature check into a formality that
//! verifies the attacker's artifact against the attacker's key. A trust anchor cannot live inside the
//! channel it protects.
//!
//! # So the shape enforces it, not a comment
//!
//! [`trusted_keys`] takes **no arguments**. The only input is `option_env!`, read at compile time.
//! The parser is private, so there is no public function anywhere in `pos-edge` that turns a runtime
//! string into a [`PublicKey`] — a future caller who wants to feed keys from config has to add one,
//! visibly, rather than passing a value into something that already exists.
//!
//! # The format, and why it is minisign's own
//!
//! `POS_EDGE_TRUSTED_KEYS` carries one or more keys separated by commas. Each is the **second line
//! of a `minisign.pub` file** verbatim, which is base64 of `Ed` ‖ key id (8 bytes) ‖ Ed25519 public
//! key (32 bytes).
//!
//! Taking minisign's own encoding means a fork pastes exactly what the tool produced:
//!
//! ```text
//! POS_EDGE_TRUSTED_KEYS="$(sed -n 2p minisign.pub)"
//! ```
//!
//! Hex would have matched the artifact signature header
//! ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md) Correction 1) and needed no decoder
//! here, and it was rejected: it forces the operator to convert the file by hand, on a step done
//! once per fork, where a mistake produces a binary that cannot install an update and says so only
//! at the first rollout. Ergonomics is a security property on a one-shot unrecoverable step.
//!
//! The base64 decoder is hand-rolled for the same reason the signature header's hex is — there is no
//! base64 crate in this workspace, and adding one is an ADR-first change. It is a smaller risk than
//! it looks: this input is not attacker-controlled, it is whatever the fork's *build* put there, and
//! every stage is checked — alphabet, padding bits, decoded length, and the `Ed` marker.
//!
//! # A binary with no keys refuses; it does not proceed
//!
//! [`trusted_keys`] returns [`TrustedKeyError::NotBakedIn`] rather than an empty vector, so the
//! wiring cannot mistake "no anchor" for "nothing to check". An updater with no trust anchor must
//! refuse to install, which is the correct failure — and it is a *build* mistake, so it should be
//! loud at startup rather than discovered when a rollout reaches the store.

use core::fmt;

use pos_ports::signer::{KeyId, PublicKey};

/// The build-time variable carrying the trusted keys.
pub const TRUSTED_KEYS_VAR: &str = "POS_EDGE_TRUSTED_KEYS";

/// Bytes in a minisign public-key blob: two algorithm bytes, an eight-byte key id, a 32-byte key.
const PUBLIC_KEY_BLOB_LEN: usize = 42;

/// minisign's Ed25519 marker, the two bytes a public-key blob begins with.
const ALGORITHM: [u8; 2] = *b"Ed";

/// Why the baked-in trust anchor could not be read.
///
/// Both variants are build mistakes rather than runtime conditions, which is why they are separate:
/// "nobody set the variable" and "somebody set it wrong" need different fixes, and collapsing them
/// would send an operator to check a value that is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrustedKeyError {
    /// The variable was not set when this binary was built, so it trusts no signing key at all.
    NotBakedIn,
    /// The variable was set and could not be read as minisign public keys.
    Malformed {
        /// Which comma-separated entry failed, counting from one, so an operator can find it in a
        /// list without counting characters.
        entry: usize,
        /// What was wrong with it.
        reason: &'static str,
    },
}

impl fmt::Display for TrustedKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBakedIn => write!(
                formatter,
                "{TRUSTED_KEYS_VAR} was not set when this binary was built, so it trusts no update \
                 signing key and cannot install an update"
            ),
            Self::Malformed { entry, reason } => write!(
                formatter,
                "{TRUSTED_KEYS_VAR} entry {entry} is not a minisign public key: {reason}. Each \
                 entry is the second line of a minisign.pub file, verbatim"
            ),
        }
    }
}

impl core::error::Error for TrustedKeyError {}

/// The public keys this binary was built to trust.
///
/// # Errors
///
/// [`TrustedKeyError::NotBakedIn`] if `POS_EDGE_TRUSTED_KEYS` was unset at build time, and
/// [`TrustedKeyError::Malformed`] if it was set but an entry is not a minisign public key. Both are
/// build mistakes; neither is recoverable at runtime, and neither may be treated as "install
/// anyway".
pub fn trusted_keys() -> Result<Vec<PublicKey>, TrustedKeyError> {
    match option_env!("POS_EDGE_TRUSTED_KEYS") {
        Some(baked) => parse_keys(baked),
        None => Err(TrustedKeyError::NotBakedIn),
    }
}

/// Parses the comma-separated list.
///
/// Private on purpose: see the module documentation. The only public path to a [`PublicKey`] here
/// reads `option_env!`, so no runtime value can reach this.
fn parse_keys(text: &str) -> Result<Vec<PublicKey>, TrustedKeyError> {
    let mut keys = Vec::new();
    for (index, entry) in text.split(',').map(str::trim).enumerate() {
        let entry_number = index.saturating_add(1);
        let fail = |reason| TrustedKeyError::Malformed {
            entry: entry_number,
            reason,
        };
        if entry.is_empty() {
            return Err(fail("it is empty"));
        }
        let blob = decode_base64(entry).ok_or_else(|| fail("it is not valid base64"))?;
        if blob.len() != PUBLIC_KEY_BLOB_LEN {
            return Err(fail(
                "it does not decode to 42 bytes (two algorithm bytes, an eight-byte key id, and a \
                 32-byte key) — the `untrusted comment:` first line of minisign.pub is not the one \
                 to use",
            ));
        }
        let algorithm = blob.get(0..2).ok_or_else(|| fail("it has no algorithm"))?;
        if algorithm != ALGORITHM {
            return Err(fail("its algorithm is not minisign's Ed25519 `Ed`"));
        }
        let id: [u8; 8] = blob
            .get(2..10)
            .and_then(|slice| slice.try_into().ok())
            .ok_or_else(|| fail("it has no key id"))?;
        let key = blob
            .get(10..PUBLIC_KEY_BLOB_LEN)
            .ok_or_else(|| fail("it has no key"))?;
        keys.push(PublicKey::new(KeyId::new(id), key.to_vec()));
    }
    Ok(keys)
}

/// Decodes standard base64, or `None` if `text` is not valid base64.
///
/// Hand-rolled because this workspace carries no base64 crate and adding one is an ADR-first change
/// (`docs/adr/README.md`). Strict in the ways that matter for a trust anchor: an out-of-alphabet
/// character is refused, and leftover bits after the last full byte must be zero — so a string that
/// merely *looks* close to a real key is rejected rather than decoded into a near-miss.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    let mut padded = false;
    for character in text.bytes() {
        if character == b'=' {
            // Padding only ever ends the data; anything after it is not base64.
            padded = true;
            continue;
        }
        if padded {
            return None;
        }
        let sextet = sextet(character)?;
        buffer = (buffer << 6) | u32::from(sextet);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
            buffer &= (1_u32 << bits).saturating_sub(1);
        }
    }
    // A well-formed encoding leaves only zero bits over. Anything else means the input carried
    // information the decoding threw away, which is not a value to trust a key from.
    if buffer != 0 {
        return None;
    }
    Some(bytes)
}

/// The six bits one standard-base64 character stands for.
///
/// A match rather than a lookup table because `indexing_slicing` is denied workspace-wide — the same
/// shape `pos_ports::device_registry` uses for hex.
const fn sextet(character: u8) -> Option<u8> {
    match character {
        b'A'..=b'Z' => Some(character - b'A'),
        b'a'..=b'z' => Some(character - b'a' + 26),
        b'0'..=b'9' => Some(character - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PUBLIC_KEY_BLOB_LEN, TRUSTED_KEYS_VAR, TrustedKeyError, decode_base64, parse_keys,
        trusted_keys,
    };

    /// Encodes standard base64. Test-only: production only ever decodes.
    fn encode_base64(bytes: &[u8]) -> String {
        const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut text = String::new();
        let mut buffer: u32 = 0;
        let mut bits: u32 = 0;
        for byte in bytes {
            buffer = (buffer << 8) | u32::from(*byte);
            bits += 8;
            while bits >= 6 {
                bits -= 6;
                let index = usize::try_from((buffer >> bits) & 0x3f).expect("six bits fit a usize");
                text.push(
                    ALPHABET
                        .chars()
                        .nth(index)
                        .expect("six bits index the 64-character alphabet"),
                );
            }
        }
        if bits > 0 {
            let index = usize::try_from((buffer << (6 - bits)) & 0x3f).expect("six bits fit");
            text.push(ALPHABET.chars().nth(index).expect("indexes the alphabet"));
            while !text.len().is_multiple_of(4) {
                text.push('=');
            }
        }
        text
    }

    /// A well-formed minisign public-key line: `Ed`, an eight-byte id, a 32-byte key.
    fn key_line(seed: u8) -> String {
        let mut blob = b"Ed".to_vec();
        blob.extend_from_slice(&[seed; 8]);
        blob.extend_from_slice(&[seed.wrapping_add(1); 32]);
        assert_eq!(
            blob.len(),
            PUBLIC_KEY_BLOB_LEN,
            "the fixture is a real blob"
        );
        encode_base64(&blob)
    }

    #[test]
    fn a_minisign_public_key_line_parses_into_its_id_and_key() {
        let keys = parse_keys(&key_line(7)).expect("a well-formed key line parses");
        assert_eq!(keys.len(), 1);
        let key = keys.first().expect("one key");
        assert_eq!(key.key_id().as_bytes(), &[7_u8; 8], "the id is bytes 2..10");
        assert_eq!(key.as_bytes(), &[8_u8; 32], "the key is bytes 10..42");
    }

    #[test]
    fn a_real_minisign_public_key_parses() {
        // The format assumption checked against minisign itself rather than against this module's
        // own encoder — which could otherwise be consistently wrong with the decoder and pass every
        // round-trip test. This is minisign's own published public key (the one in its README, used
        // to verify its releases): a public key is not a secret, and it is here purely as a
        // known-good sample of the on-disk encoding.
        //
        // Its bytes are `Ed` ‖ key id `1fe8b442180f62e7` ‖ 32 key bytes, which is exactly the layout
        // `parse_keys` assumes and `updater-minisign` reads at the same offsets in a signature.
        const REAL: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let keys = parse_keys(REAL).expect("a real minisign public key line parses");
        let key = keys.first().expect("one key");
        assert_eq!(
            key.key_id().to_string(),
            "1fe8b442180f62e7",
            "the eight bytes after the `Ed` marker are the key id"
        );
        assert_eq!(
            key.as_bytes().len(),
            32,
            "an Ed25519 public key is 32 bytes"
        );
    }

    #[test]
    fn two_keys_are_carried_so_a_signing_key_can_be_retired() {
        // ADR-0047 keeps two keys baked in precisely so a compromised one can be retired without a
        // release that itself needs the compromised key to be trusted.
        let both = format!("{}, {}", key_line(1), key_line(2));
        let keys = parse_keys(&both).expect("two keys parse, whitespace and all");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys.first().expect("first").key_id().as_bytes(), &[1_u8; 8]);
        assert_eq!(keys.get(1).expect("second").key_id().as_bytes(), &[2_u8; 8]);
    }

    #[test]
    fn the_comment_line_of_a_pub_file_is_refused_rather_than_half_read() {
        // The likeliest operator mistake by far: pasting the whole file, or its first line.
        let error = parse_keys("untrusted comment: minisign public key ABCDEF0123456789")
            .expect_err("the comment line is not a key");
        assert!(matches!(error, TrustedKeyError::Malformed { entry: 1, .. }));
    }

    #[test]
    fn a_blob_of_the_wrong_length_is_refused() {
        let short = encode_base64(b"Edshort");
        let error = parse_keys(&short).expect_err("seven bytes is not a key");
        assert!(matches!(error, TrustedKeyError::Malformed { entry: 1, .. }));
    }

    #[test]
    fn a_blob_with_the_wrong_algorithm_is_refused() {
        // Right length, wrong marker: an `ED` prehashed *signature* marker, or anything else.
        let mut blob = b"ED".to_vec();
        blob.extend_from_slice(&[3_u8; 40]);
        let error = parse_keys(&encode_base64(&blob)).expect_err("a non-`Ed` blob is not a key");
        assert!(matches!(
            error,
            TrustedKeyError::Malformed {
                entry: 1,
                reason: "its algorithm is not minisign's Ed25519 `Ed`"
            }
        ));
    }

    #[test]
    fn an_empty_entry_names_its_position() {
        let error =
            parse_keys(&format!("{},", key_line(1))).expect_err("a trailing comma is not a key");
        assert_eq!(
            error,
            TrustedKeyError::Malformed {
                entry: 2,
                reason: "it is empty"
            },
            "the position counts from one, so an operator can find it in a list"
        );
    }

    #[test]
    fn base64_decoding_round_trips_and_refuses_what_is_not_base64() {
        for bytes in [&b""[..], &b"a"[..], &b"ab"[..], &b"abc"[..], &b"abcd"[..]] {
            assert_eq!(
                decode_base64(&encode_base64(bytes)).as_deref(),
                Some(bytes),
                "round trip for {bytes:?}"
            );
        }
        assert_eq!(
            decode_base64("not base64!"),
            None,
            "an out-of-alphabet character"
        );
        assert_eq!(decode_base64("AB=C"), None, "data after padding");
        // `AB` carries 12 bits; the low four are not zero, so the input meant something the
        // decoding would have thrown away.
        assert_eq!(decode_base64("AB"), None, "leftover bits must be zero");
    }

    #[test]
    fn the_error_messages_say_what_to_do() {
        // These are read by whoever built a binary that cannot update itself, so they name the
        // variable and the file line to use.
        let absent = TrustedKeyError::NotBakedIn.to_string();
        assert!(absent.contains(TRUSTED_KEYS_VAR), "got {absent}");
        assert!(absent.contains("cannot install an update"), "got {absent}");
        let malformed = TrustedKeyError::Malformed {
            entry: 2,
            reason: "it is empty",
        }
        .to_string();
        assert!(malformed.contains("entry 2"), "got {malformed}");
        assert!(malformed.contains("minisign.pub"), "got {malformed}");
    }

    /// The invariant, run against the *actual* compiled-in value — R1b's `version_parses` in the
    /// trust-anchor register.
    ///
    /// Whatever a build put in `POS_EDGE_TRUSTED_KEYS`, it either yields at least one key or says it
    /// was never set. `Malformed` here means this build baked in something that is not a key, so the
    /// binary would refuse every update in the field; failing the build instead is the whole point.
    #[test]
    fn the_baked_in_value_is_either_absent_or_usable_never_malformed() {
        match trusted_keys() {
            Ok(keys) => assert!(
                !keys.is_empty(),
                "a build that set {TRUSTED_KEYS_VAR} must yield at least one key"
            ),
            Err(TrustedKeyError::NotBakedIn) => {
                assert!(
                    option_env!("POS_EDGE_TRUSTED_KEYS").is_none(),
                    "NotBakedIn is only honest when the variable really is unset"
                );
            }
            Err(error @ TrustedKeyError::Malformed { .. }) => panic!(
                "this build baked in a value that is not a minisign public key, so it could never \
                 install an update: {error}"
            ),
        }
    }
}
