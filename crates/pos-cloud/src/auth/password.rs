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
use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};

/// Hashing a password failed. Carries no detail, so nothing about the password reaches a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("hashing the password failed")]
pub struct HashError;

/// Hashes `password` with Argon2id and `salt`, returning a PHC string to store.
///
/// The salt is a parameter so this is deterministic under test; production passes a fresh
/// cryptographically-random salt (`SaltString::generate`), which is where the only randomness lives.
///
/// # Errors
///
/// [`HashError`] if the underlying hash fails (e.g. an impossibly long password).
pub fn hash_password(password: &str, salt: &SaltString) -> Result<String, HashError> {
    Argon2::default()
        .hash_password(password.as_bytes(), salt)
        .map(|hash| hash.to_string())
        .map_err(|_| HashError)
}

/// Verifies `password` against a stored Argon2id PHC hash.
///
/// A malformed stored hash verifies nothing — `false`, not an error — so a corrupted credential can
/// never become a way in, the same rule the edge's PIN verification follows.
#[must_use]
pub fn verify_password(phc_hash: &str, password: &str) -> bool {
    match PasswordHash::new(phc_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{hash_password, verify_password};

    use argon2::password_hash::SaltString;

    fn fixed_salt() -> SaltString {
        SaltString::encode_b64(b"a-fixed-test-salt").expect("a valid salt")
    }

    #[test]
    fn a_correct_password_verifies_and_a_wrong_one_does_not() {
        let hash = hash_password("correct horse battery staple", &fixed_salt()).expect("hash");
        assert!(verify_password(&hash, "correct horse battery staple"));
        assert!(!verify_password(&hash, "Correct Horse Battery Staple"));
        assert!(!verify_password(&hash, ""));
    }

    #[test]
    fn the_stored_hash_is_a_phc_string_not_the_password() {
        let hash = hash_password("super-secret-passphrase", &fixed_salt()).expect("hash");
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
