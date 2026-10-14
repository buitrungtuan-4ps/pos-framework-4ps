// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Time-based one-time passwords (RFC 6238), the mandatory second factor for the super-admin
//! ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
//!
//! TOTP over HMAC-**SHA1**, the 30-second time step, dynamically truncated to a 6-digit code. RFC
//! 6238 permits SHA1/SHA256/SHA512, and this picks SHA1 — the algorithm every authenticator app
//! computes by **default**, and the only one Google Authenticator and Microsoft Authenticator
//! actually honour (they ignore the `otpauth://` URI's `algorithm` field), so a code a real operator
//! types in verifies. ADR-0034 originally chose SHA256 to avoid a second `sha1` crate version; that
//! made enrolment unusable in practice, so it was amended — the duplicate `sha1` line is the accepted
//! cost, carried by a documented `deny.toml` skip. Two things beyond the RFC make it safe as a real
//! second factor:
//!
//!  * **A skew window.** [`verify`] accepts the code for the step before and after now (`±1`), so a
//!    clock a few seconds off still authenticates, but a code more than ~30s stale does not.
//!  * **Single use.** A code is valid for a whole step, so a captured one could be replayed within
//!    it. [`verify`] returns the step it matched and refuses any step at or below the last one
//!    accepted, so a code — indeed any code from an already-used step — cannot be used twice.
//!
//! `now` is a parameter, so the whole module is deterministic and tested against RFC 6238's published
//! vectors.

use core::fmt;

use hmac::digest::KeyInit as _;
use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// The time step, in seconds — the RFC 6238 default and what authenticator apps assume.
pub const STEP_SECONDS: u64 = 30;

/// The number of digits in a code — the near-universal default.
pub const DIGITS: u32 = 6;

/// How many steps either side of now [`verify`] will accept, for clock skew.
pub const SKEW_STEPS: u64 = 1;

/// A shared TOTP secret. Redacted from [`fmt::Debug`] so it cannot be logged.
#[derive(Clone)]
pub struct TotpSecret(Vec<u8>);

impl TotpSecret {
    /// Wraps raw secret bytes (the decoded `otpauth` secret).
    #[must_use]
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self(secret.into())
    }
}

impl fmt::Debug for TotpSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TotpSecret(<redacted>)")
    }
}

/// Why a submitted code was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TotpError {
    /// The code did not match any step within the skew window.
    #[error("the authenticator code is incorrect")]
    Incorrect,
    /// The code was correct but its step was already used (a replay).
    #[error("the authenticator code has already been used")]
    Reused,
}

/// The code for `secret` at `unix_seconds`, with the given number of digits.
///
/// Exposed mainly so a provisioning tool and the tests can compute a code; verification goes through
/// [`verify`].
#[must_use]
pub fn code_at(secret: &TotpSecret, unix_seconds: u64, digits: u32) -> String {
    code_for_step(secret, unix_seconds / STEP_SECONDS, digits)
}

/// Verifies `code` against `secret` at `now`, within the skew window and above `last_used_step`.
///
/// On success returns the step that matched, which the caller **must** persist as the new
/// `last_used_step` so that step — and every earlier one — cannot be used again.
///
/// # Errors
///
/// [`TotpError::Incorrect`] if no step in the window produces `code`; [`TotpError::Reused`] if the
/// matching step is not newer than `last_used_step`.
pub fn verify(
    secret: &TotpSecret,
    code: &str,
    now_unix_seconds: u64,
    last_used_step: Option<u64>,
) -> Result<u64, TotpError> {
    let current = now_unix_seconds / STEP_SECONDS;
    let low = current.saturating_sub(SKEW_STEPS);
    let high = current.saturating_add(SKEW_STEPS);

    let mut matched: Option<u64> = None;
    // Scan the whole window even after a match, so timing does not reveal which step hit.
    for step in low..=high {
        if constant_time_eq(
            code.as_bytes(),
            code_for_step(secret, step, DIGITS).as_bytes(),
        ) {
            matched = Some(step);
        }
    }

    match matched {
        None => Err(TotpError::Incorrect),
        Some(step) => {
            if last_used_step.is_some_and(|last| step <= last) {
                Err(TotpError::Reused)
            } else {
                Ok(step)
            }
        }
    }
}

/// The code for a specific step counter (RFC 6238 / RFC 4226 dynamic truncation).
fn code_for_step(secret: &TotpSecret, step: u64, digits: u32) -> String {
    let digest = match HmacSha1::new_from_slice(&secret.0) {
        Ok(mut mac) => {
            mac.update(&step.to_be_bytes());
            mac.finalize().into_bytes()
        }
        // HMAC takes any key length, so this is unreachable; an all-zero digest yields a code the
        // comparison rejects rather than a panic.
        Err(_) => return "0".repeat(digits as usize),
    };

    let offset = usize::from(digest.last().copied().unwrap_or(0) & 0x0f);
    let truncated = digest
        .get(offset..offset.saturating_add(4))
        .and_then(|window| <[u8; 4]>::try_from(window).ok())
        .map_or(0, u32::from_be_bytes)
        & 0x7fff_ffff;

    let modulo = 10_u32.checked_pow(digits).unwrap_or(1_000_000);
    let value = truncated % modulo;
    format!("{value:0width$}", width = digits as usize)
}

/// Constant-time byte-slice equality: no early return on the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{DIGITS, STEP_SECONDS, TotpError, TotpSecret, code_at, verify};

    /// The RFC 6238 Appendix B SHA1 seed: ASCII "12345678901234567890" (20 bytes).
    fn rfc_secret() -> TotpSecret {
        TotpSecret::new(b"12345678901234567890".to_vec())
    }

    #[test]
    fn it_matches_the_rfc_6238_test_vectors() {
        // The published SHA1 rows (8-digit) — RFC 6238 Appendix B.
        assert_eq!(code_at(&rfc_secret(), 59, 8), "94287082");
        assert_eq!(code_at(&rfc_secret(), 1_111_111_109, 8), "07081804");
        assert_eq!(code_at(&rfc_secret(), 1_234_567_890, 8), "89005924");
        // And the 6-digit truncation the product uses (the last six digits of the T=59 row).
        assert_eq!(code_at(&rfc_secret(), 59, DIGITS), "287082");
    }

    #[test]
    fn the_current_code_verifies() {
        let now = 1_700_000_000;
        let code = code_at(&rfc_secret(), now, DIGITS);
        let step = verify(&rfc_secret(), &code, now, None).expect("the current code verifies");
        assert_eq!(step, now / STEP_SECONDS);
    }

    #[test]
    fn a_code_one_step_off_still_verifies_but_two_steps_off_does_not() {
        let now = 1_700_000_000;
        let previous = code_at(&rfc_secret(), now - STEP_SECONDS, DIGITS);
        assert!(
            verify(&rfc_secret(), &previous, now, None).is_ok(),
            "one step of skew is tolerated"
        );
        let stale = code_at(&rfc_secret(), now - 3 * STEP_SECONDS, DIGITS);
        assert_eq!(
            verify(&rfc_secret(), &stale, now, None),
            Err(TotpError::Incorrect),
            "a code three steps old is outside the window"
        );
    }

    #[test]
    fn a_wrong_code_is_incorrect() {
        assert_eq!(
            verify(&rfc_secret(), "000000", 1_700_000_000, None),
            Err(TotpError::Incorrect)
        );
    }

    #[test]
    fn a_code_cannot_be_used_twice() {
        let now = 1_700_000_000;
        let code = code_at(&rfc_secret(), now, DIGITS);
        let step = verify(&rfc_secret(), &code, now, None).expect("first use");
        // Presenting it again, with the step recorded as used, is a replay.
        assert_eq!(
            verify(&rfc_secret(), &code, now, Some(step)),
            Err(TotpError::Reused)
        );
        // A later, unused step is fine again.
        let later = now + STEP_SECONDS;
        let next = code_at(&rfc_secret(), later, DIGITS);
        assert!(verify(&rfc_secret(), &next, later, Some(step)).is_ok());
    }

    #[test]
    fn the_secret_is_redacted_from_debug() {
        let rendered = format!("{:?}", TotpSecret::new(b"12345678901234567890".to_vec()));
        assert!(!rendered.contains("12345"), "the secret leaked into Debug");
    }
}
