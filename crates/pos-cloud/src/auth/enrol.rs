// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! First-boot super-admin enrolment ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md)).
//!
//! The pure half of the `/admin/setup` route: the request/response shapes, a constant-time token
//! compare, and the TOTP enrolment rendering — RFC 4648 base32 and the `otpauth://` URI an
//! authenticator app imports. Generating the secret and the password salt is I/O (OS entropy) and
//! lives at the HTTP edge ([`crate::http`]); everything here is deterministic and unit-tested.
//!
//! The enrolment carries the freshly-minted TOTP secret, so it is sensitive: it is serialized to the
//! token-bearing caller once, over TLS, and never re-emitted. Both this and the request redact their
//! secrets from [`core::fmt::Debug`], so neither reaches a log through a derived `Debug`.

use core::fmt;

use serde::{Deserialize, Serialize};

/// The shortest password first-boot will accept, so a fork cannot mint a trivially weak super-admin
/// ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md)).
pub const MIN_PASSWORD_LEN: usize = 12;

/// The length of a freshly-minted TOTP shared secret, in bytes — 256 bits, well above RFC 4226's
/// 160-bit floor and independent of the HMAC hash the verifier uses
/// ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
pub const TOTP_SECRET_BYTES: usize = 32;

/// The issuer label the `otpauth://` URI carries — deliberately free of characters that would need
/// percent-encoding.
const ISSUER: &str = "Pizza4Ps";
/// The account label the `otpauth://` URI carries — there is one super-admin.
const ACCOUNT: &str = "super-admin";

/// The RFC 4648 base32 alphabet (upper-case), the encoding `otpauth://` secrets use.
const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// A first-boot enrolment request: the authorising setup token and the password the operator chooses.
///
/// [`fmt::Debug`] redacts both, so a logged request cannot leak the token or the password.
#[derive(Clone, Deserialize)]
pub struct SetupRequest {
    /// The one-time setup token (`bootstrap.sh` minted it into `cloud.toml`).
    pub setup_token: String,
    /// The password the operator is choosing for the super-admin.
    pub password: String,
}

impl fmt::Debug for SetupRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupRequest")
            .field("setup_token", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The TOTP enrolment returned once on a successful first-boot: the `otpauth://` URI an authenticator
/// app imports (as a QR or by paste) and the raw base32 secret for manual entry.
///
/// Both fields carry the shared secret, so [`fmt::Debug`] redacts them — the value reaches the
/// operator through the HTTP response body, never a log.
#[derive(Clone, Serialize)]
pub struct Enrolment {
    /// The `otpauth://totp/…` provisioning URI (SHA1, 6 digits, 30-second period).
    pub otpauth_uri: String,
    /// The base32-encoded shared secret, for an authenticator that takes a typed key.
    pub secret_base32: String,
}

impl fmt::Debug for Enrolment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Enrolment")
            .field("otpauth_uri", &"<redacted>")
            .field("secret_base32", &"<redacted>")
            .finish()
    }
}

/// Renders the enrolment for a freshly-minted `secret`.
#[must_use]
pub fn build_enrolment(secret: &[u8]) -> Enrolment {
    Enrolment {
        otpauth_uri: otpauth_uri(secret),
        secret_base32: base32_encode(secret),
    }
}

/// The `otpauth://totp` provisioning URI for `secret`, fixing the parameters the verifier expects
/// ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)): HMAC-SHA1, 6 digits, a 30-second step.
/// `algorithm=SHA1` is stated explicitly even though it is the default, so a URI-honouring app agrees
/// with the ones (Google/Microsoft Authenticator) that assume it regardless.
#[must_use]
pub fn otpauth_uri(secret: &[u8]) -> String {
    let encoded = base32_encode(secret);
    format!(
        "otpauth://totp/{ISSUER}:{ACCOUNT}?secret={encoded}&issuer={ISSUER}&algorithm=SHA1&digits=6&period=30"
    )
}

/// RFC 4648 base32, upper-case and unpadded (authenticator apps accept the unpadded form).
#[must_use]
pub fn base32_encode(input: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in input {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(
                BASE32_ALPHABET[((buffer >> bits) & 0x1f) as usize],
            ));
        }
        // Keep only the leftover low bits, so the accumulator stays bounded across a long input.
        buffer &= (1_u32 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(
            BASE32_ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize],
        ));
    }
    out
}

/// Compares two strings in time independent of their contents (given equal length), so a remote
/// guesser cannot learn a correct prefix of the setup token from response timing. Unequal lengths
/// short-circuit — the token's length is fixed and not itself a secret.
#[must_use]
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= *x ^ *y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{base32_encode, build_enrolment, constant_time_eq, otpauth_uri};

    #[test]
    fn base32_matches_the_rfc4648_vectors_unpadded() {
        // RFC 4648 §10, with the trailing '=' padding removed (the `otpauth` form).
        assert_eq!(base32_encode(b""), "");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_encode(b"fo"), "MZXQ");
        assert_eq!(base32_encode(b"foo"), "MZXW6");
        assert_eq!(base32_encode(b"foob"), "MZXW6YQ");
        assert_eq!(base32_encode(b"fooba"), "MZXW6YTB");
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn base32_stays_in_alphabet_for_a_long_input() {
        // 100 bytes would overflow a naive accumulator; assert the output is well-formed base32.
        let secret = [0xAB_u8; 100];
        let encoded = base32_encode(&secret);
        assert_eq!(
            encoded.len(),
            160,
            "100 bytes -> 800 bits -> 160 base32 chars"
        );
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)),
            "every character is in the RFC 4648 alphabet: {encoded}"
        );
    }

    #[test]
    fn the_otpauth_uri_fixes_the_verifier_parameters() {
        let uri = otpauth_uri(b"fooba"); // -> MZXW6YTB
        assert_eq!(
            uri,
            "otpauth://totp/Pizza4Ps:super-admin?secret=MZXW6YTB&issuer=Pizza4Ps&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn build_enrolment_carries_both_forms_of_the_secret() {
        let enrolment = build_enrolment(b"fooba");
        assert_eq!(enrolment.secret_base32, "MZXW6YTB");
        assert!(enrolment.otpauth_uri.contains("secret=MZXW6YTB"));
    }

    #[test]
    fn constant_time_eq_is_value_equality() {
        assert!(constant_time_eq("a-token-value", "a-token-value"));
        assert!(!constant_time_eq("a-token-value", "a-token-valuf"));
        assert!(!constant_time_eq("short", "a-much-longer-token"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn the_request_debug_hides_the_secrets() {
        let request = super::SetupRequest {
            setup_token: "super-secret-token".to_owned(),
            password: "hunter2-hunter2".to_owned(),
        };
        let rendered = format!("{request:?}");
        assert!(
            !rendered.contains("super-secret-token") && !rendered.contains("hunter2"),
            "a secret leaked into Debug: {rendered}"
        );
    }
}
