// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cloud authentication — two mechanisms for two kinds of caller (P7).
//!
//! The **super-admin** is a human signing in interactively
//! ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)): the most privileged identity, so its
//! sign-in is two-factor and the second factor is not optional. [`SuperAdminCredential::authenticate`]
//! accepts a login only when the [`password`] verifies *and* the [`totp`] code verifies and has not
//! been used before; there is no password-only path, and the [`session`] cookie it leads to is scoped
//! to a single subdomain so an admin session cannot travel between tenants. Both factors are always
//! evaluated before a verdict, and the client is told only that sign-in failed — the specific
//! [`AuthError`] is for the server's own log, so a prober cannot learn which factor was wrong.
//! [`admin`] is the seam that turns this into a login route: it loads the stored credential, runs the
//! two-factor check, and — on success — mints and persists the server-side session the [`session`]
//! cookie carries.
//!
//! A **machine integrator** presents a scoped per-tenant [`apikey`] instead
//! ([ADR-0037](../../../docs/adr/0037-api-keys.md)): a bearer token whose secret is stored only as a
//! hash, bound to one tenant and a deny-by-default scope set — the isolation and least-privilege
//! controls for the public `/v1` surface. [`bearer`] is the HTTP seam over it: it reads the
//! `Authorization` header, verifies the key against the clock, and yields the [`apikey::Grant`] a
//! `/v1` handler then gates by scope and scopes to a tenant — refusing every credential problem with
//! one indistinguishable `401`.

pub mod admin;
pub mod apikey;
pub mod bearer;
pub mod password;
pub mod session;
pub mod totp;

use password::verify_password;
use totp::{TotpError, TotpSecret, verify as verify_totp};

/// A super-admin's stored credential: the password hash and the TOTP secret.
///
/// Holds a hash, never a password. Its [`core::fmt::Debug`] redacts both fields, so neither the PHC
/// hash nor the TOTP secret can reach a log through a derived `Debug`.
#[derive(Clone)]
pub struct SuperAdminCredential {
    password_phc: String,
    totp: TotpSecret,
}

impl core::fmt::Debug for SuperAdminCredential {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SuperAdminCredential")
            .field("password_phc", &"<redacted>")
            .field("totp", &self.totp)
            .finish()
    }
}

/// A successful two-factor authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authenticated {
    /// The TOTP step the code matched. The caller **must** persist this as the credential's new
    /// last-used step so the same code — and any earlier one — cannot be used again.
    pub totp_step: u64,
}

/// Why a sign-in was refused. For the server's log; the client is told only that sign-in failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// The password did not verify.
    #[error("the password is incorrect")]
    BadPassword,
    /// The password verified but the TOTP code did not.
    #[error("the authenticator code is incorrect")]
    BadTotp,
    /// The TOTP code was correct but already used (a replay).
    #[error("the authenticator code has already been used")]
    TotpReused,
}

impl SuperAdminCredential {
    /// Builds a credential from a stored Argon2id PHC hash and a TOTP secret.
    #[must_use]
    pub fn new(password_phc: impl Into<String>, totp: TotpSecret) -> Self {
        Self {
            password_phc: password_phc.into(),
            totp,
        }
    }

    /// Authenticates a two-factor sign-in.
    ///
    /// Both factors are checked before the verdict. Succeeds only when the password verifies and the
    /// TOTP code verifies within its skew window and is newer than `last_used_step`.
    ///
    /// # Errors
    ///
    /// [`AuthError`] naming the factor that failed — for logging only; the client sees one generic
    /// failure regardless.
    pub fn authenticate(
        &self,
        password: &str,
        totp_code: &str,
        now_unix_seconds: u64,
        last_used_step: Option<u64>,
    ) -> Result<Authenticated, AuthError> {
        // Evaluate both factors regardless, so the work does not depend on which one is wrong.
        let password_ok = verify_password(&self.password_phc, password);
        let totp = verify_totp(&self.totp, totp_code, now_unix_seconds, last_used_step);

        if !password_ok {
            return Err(AuthError::BadPassword);
        }
        match totp {
            Ok(step) => Ok(Authenticated { totp_step: step }),
            Err(TotpError::Incorrect) => Err(AuthError::BadTotp),
            Err(TotpError::Reused) => Err(AuthError::TotpReused),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthError, SuperAdminCredential};

    use argon2::password_hash::SaltString;

    use crate::auth::password::hash_password;
    use crate::auth::totp::{DIGITS, TotpSecret, code_at};

    const NOW: u64 = 1_700_000_000;

    fn credential() -> SuperAdminCredential {
        let salt = SaltString::encode_b64(b"a-fixed-test-salt").expect("salt");
        let phc = hash_password("a-strong-passphrase", &salt).expect("hash");
        SuperAdminCredential::new(phc, TotpSecret::new(b"12345678901234567890".to_vec()))
    }

    fn current_code() -> String {
        code_at(
            &TotpSecret::new(b"12345678901234567890".to_vec()),
            NOW,
            DIGITS,
        )
    }

    #[test]
    fn both_factors_correct_authenticates() {
        let credential = credential();
        let authenticated = credential
            .authenticate("a-strong-passphrase", &current_code(), NOW, None)
            .expect("both factors are correct");
        assert_eq!(
            authenticated.totp_step,
            NOW / crate::auth::totp::STEP_SECONDS
        );
    }

    #[test]
    fn a_correct_password_with_no_valid_code_is_refused() {
        // Mandatory TOTP: the password alone is never enough.
        assert_eq!(
            credential().authenticate("a-strong-passphrase", "000000", NOW, None),
            Err(AuthError::BadTotp)
        );
    }

    #[test]
    fn a_wrong_password_with_a_valid_code_is_refused() {
        assert_eq!(
            credential().authenticate("wrong-passphrase", &current_code(), NOW, None),
            Err(AuthError::BadPassword)
        );
    }

    #[test]
    fn a_code_cannot_be_replayed_across_logins() {
        let credential = credential();
        let code = current_code();
        let first = credential
            .authenticate("a-strong-passphrase", &code, NOW, None)
            .expect("first login");
        // A second login presenting the same code, with the step recorded as used, is refused.
        assert_eq!(
            credential.authenticate("a-strong-passphrase", &code, NOW, Some(first.totp_step)),
            Err(AuthError::TotpReused)
        );
    }

    #[test]
    fn the_credential_debug_redacts_both_secrets() {
        let rendered = format!("{:?}", credential());
        assert!(
            !rendered.contains("argon2id"),
            "the hash leaked: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
    }
}
