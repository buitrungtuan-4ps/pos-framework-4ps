// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Argon2id password hashing and verification for the super-admin
//! ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
//!
//! The same primitive the edge uses for offline PIN hashes ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)),
//! at `argon2`'s default Argon2id cost. Unlike the four-digit PIN — whose defence is the cost plus a
//! lockout — the super-admin password is expected to be high-entropy, and the *mandatory* TOTP second
//! factor ([`super::totp`]) is what makes an online guess of the password alone useless. Only the PHC
//! hash is ever stored; the password itself is never logged, spanned, or persisted.

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};

/// Hashing a password failed. Carries no detail, so nothing about the password reaches a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("hashing the password failed")]
pub struct HashError;

/// Hashes `password` with Argon2id and `salt`, returning a PHC string to store.
///
/// The salt is raw bytes and a parameter, so this is deterministic under test; production passes
/// 16 fresh bytes from the OS, which is where the only randomness lives. The PHC recommendation is
/// 16 bytes and the callers honour it.
///
/// # Errors
///
/// [`HashError`] if the underlying hash fails (e.g. an impossibly long password, or a salt outside
/// the length Argon2 accepts).
pub fn hash_password(password: &str, salt: &[u8]) -> Result<String, HashError> {
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), salt)
        .map(|hash: PasswordHash| hash.to_string())
        .map_err(|_| HashError)
}

/// Verifies `password` against a stored Argon2id PHC hash.
///
/// A malformed stored hash verifies nothing — `false`, not an error — so a corrupted credential can
/// never become a way in, the same rule the edge's PIN verification follows.
#[must_use]
pub fn verify_password(phc_hash: &str, password: &str) -> bool {
    // `PasswordVerifier<str>` parses the PHC string itself, so a malformed one is an `Err` here
    // and becomes `false` — the same fail-closed rule the edge's PIN verification follows.
    Argon2::default()
        .verify_password(password.as_bytes(), phc_hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::{hash_password, verify_password};

    /// A fixed 16-byte salt, so a hashed fixture is deterministic. Tests only.
    const SALT: &[u8] = b"a-fixed-test-slt";

    /// A PHC string produced by `argon2` **0.5.3** — the version shipped in `v0.9.0` — for the
    /// password below. Captured from that build before the 0.6 upgrade and pinned here, because the
    /// question an upgrade of a password hasher has to answer is not "does it compile" but "does a
    /// credential written by the old version still open the door". Every super-admin password and
    /// every employee PIN in a live deployment is a string of exactly this shape; if this test ever
    /// fails, upgrading locks every one of them out.
    const PHC_WRITTEN_BY_0_5_3: &str = "$argon2id$v=19$m=19456,t=2,p=1$dGVzdHNhbHR0ZXN0c2FsdA$4kBl/zrrvHnVH5qdfy6Ndjp8Q5Ki81vzKYQRJcEC7qg";

    #[test]
    fn a_hash_written_by_the_previous_argon2_still_verifies() {
        assert!(
            verify_password(PHC_WRITTEN_BY_0_5_3, "correct horse battery staple"),
            "a credential stored before the argon2 0.6 upgrade must still open the door"
        );
        assert!(
            !verify_password(PHC_WRITTEN_BY_0_5_3, "the wrong password"),
            "and a wrong password must still be refused against it"
        );
    }

    #[test]
    fn a_correct_password_verifies_and_a_wrong_one_does_not() {
        let hash = hash_password("correct horse battery staple", SALT).expect("hash");
        assert!(verify_password(&hash, "correct horse battery staple"));
        assert!(!verify_password(&hash, "Correct Horse Battery Staple"));
        assert!(!verify_password(&hash, ""));
    }

    #[test]
    fn the_stored_hash_is_a_phc_string_not_the_password() {
        let hash = hash_password("super-secret-passphrase", SALT).expect("hash");
        assert!(hash.starts_with("$argon2id$"), "an Argon2id PHC string");
        assert!(
            !hash.contains("super-secret-passphrase"),
            "the password does not appear in its hash"
        );
    }

    #[test]
    fn a_malformed_stored_hash_is_never_a_way_in() {
        assert!(!verify_password("not-a-phc-string", "anything"));
        assert!(!verify_password("", ""));
    }
}
