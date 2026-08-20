// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Webhook payload signing: HMAC-SHA256 over a timestamped payload, and the ±5-minute replay
//! window a receiver enforces.
//!
//! Every delivery carries two headers — a Unix-seconds timestamp and a signature. The signature is
//! `HMAC-SHA256(secret, "{timestamp}.{body}")`, hex-encoded and prefixed `v1=` so the scheme can
//! grow a `v2` without breaking existing receivers. Binding the timestamp *into* the signed payload
//! is what makes the replay window real: a captured delivery cannot be replayed with a fresh
//! timestamp, because changing the timestamp invalidates the signature, and it cannot be replayed
//! with its original timestamp once that is more than [`REPLAY_TOLERANCE`] seconds old.
//!
//! [`verify`] is the reference receiver check — used by the tests here, and the exact algorithm an
//! integrator implements. It is constant-time in the signature comparison (via [`Mac::verify_slice`])
//! so a receiver cannot leak the expected signature a byte at a time.

use core::fmt;

use hmac::digest::KeyInit as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use pos_proto::time::Timestamp;

type HmacSha256 = Hmac<Sha256>;

/// The header carrying the Unix-seconds timestamp a delivery was signed at.
pub const TIMESTAMP_HEADER: &str = "X-Pos-Webhook-Timestamp";

/// The header carrying the `v1=<hex>` signature.
pub const SIGNATURE_HEADER: &str = "X-Pos-Webhook-Signature";

/// How far a delivery's timestamp may sit from the receiver's clock, in seconds, before it is
/// rejected as a replay (`docs/roadmap.md` P7: a ±5-minute window).
pub const REPLAY_TOLERANCE: i64 = 300;

/// A per-endpoint webhook signing secret.
///
/// Redacted from [`fmt::Debug`], so an endpoint that derives `Debug` cannot log it.
#[derive(Clone)]
pub struct SigningSecret(String);

impl SigningSecret {
    /// Wraps a secret string.
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret's bytes, for the HMAC key.
    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// The secret string, for the persistence layer to store and reload — the cloud must keep this
    /// secret because it *signs* deliveries with it (unlike an API-key secret, which the cloud only
    /// ever verifies and so stores as a hash, [ADR-0037](../../../docs/adr/0037-api-keys.md)). Named to
    /// make every read of the raw secret conspicuous at the call site.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SigningSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SigningSecret(<redacted>)")
    }
}

/// The two header values a signed delivery carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The value of [`TIMESTAMP_HEADER`] — Unix seconds.
    pub timestamp: i64,
    /// The value of [`SIGNATURE_HEADER`] — `v1=<hex>`.
    pub signature: String,
}

/// Signs `body` for delivery at `timestamp`, returning the header values to send.
#[must_use]
pub fn sign(secret: &SigningSecret, timestamp: Timestamp, body: &[u8]) -> Signature {
    let seconds = unix_seconds(timestamp);
    Signature {
        timestamp: seconds,
        signature: format!("v1={}", to_hex(&mac(secret, seconds, body))),
    }
}

/// Verifies a received delivery: the signature matches, and the timestamp is within
/// [`REPLAY_TOLERANCE`] of `now`.
///
/// This is the reference receiver algorithm. The timestamp check is what closes the replay window;
/// the signature check is constant-time.
///
/// # Errors
///
/// [`VerifyError`] if the timestamp is stale, the signature header is malformed, or the signature
/// does not match.
pub fn verify(
    secret: &SigningSecret,
    timestamp: i64,
    body: &[u8],
    signature: &str,
    now: Timestamp,
) -> Result<(), VerifyError> {
    // The window first: a stale delivery is rejected without spending a hash, and the check is on
    // the absolute skew so neither a slow sender nor a fast one slips through.
    let skew = unix_seconds(now).saturating_sub(timestamp).abs();
    if skew > REPLAY_TOLERANCE {
        return Err(VerifyError::StaleTimestamp);
    }

    let hex = signature
        .strip_prefix("v1=")
        .ok_or(VerifyError::MalformedSignature)?;
    let expected = from_hex(hex).ok_or(VerifyError::MalformedSignature)?;

    let mut hasher = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| VerifyError::MalformedSignature)?;
    hasher.update(&signed_payload(timestamp, body));
    hasher
        .verify_slice(&expected)
        .map_err(|_| VerifyError::BadSignature)
}

/// Why a received delivery failed [`verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The timestamp is more than [`REPLAY_TOLERANCE`] seconds from now — a replay, or a badly
    /// skewed clock.
    #[error("the delivery timestamp is outside the replay window")]
    StaleTimestamp,
    /// The signature header is not `v1=<hex>`.
    #[error("the signature header is malformed")]
    MalformedSignature,
    /// The signature does not match the body and timestamp.
    #[error("the signature does not match")]
    BadSignature,
}

/// HMAC-SHA256 over the signed payload. HMAC takes a key of any length, so `new_from_slice` cannot
/// fail here; an empty tag on the impossible error path yields a signature a receiver rejects rather
/// than a panic.
fn mac(secret: &SigningSecret, timestamp: i64, body: &[u8]) -> Vec<u8> {
    match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(mut hasher) => {
            hasher.update(&signed_payload(timestamp, body));
            hasher.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// The exact bytes signed: `"{timestamp}."` followed by the body. Building it as a `String` prefix
/// and appending the body keeps sender and receiver on one definition.
fn signed_payload(timestamp: i64, body: &[u8]) -> Vec<u8> {
    let mut payload = format!("{timestamp}.").into_bytes();
    payload.extend_from_slice(body);
    payload
}

/// Unix seconds from a millisecond [`Timestamp`].
fn unix_seconds(timestamp: Timestamp) -> i64 {
    timestamp.as_milliseconds_since_epoch().div_euclid(1000)
}

/// Lower-case hex, no separators.
fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        // A byte is always two hex digits; `write!` to a String is infallible.
        let _ = fmt::write(&mut out, format_args!("{byte:02x}"));
    }
    out
}

/// Decodes lower- or upper-case hex, or `None` if the input is not an even run of hex digits.
fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = core::str::from_utf8(pair).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        REPLAY_TOLERANCE, Signature, SigningSecret, VerifyError, from_hex, sign, to_hex, verify,
    };

    use pos_proto::time::Timestamp;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(seconds.saturating_mul(1000)).expect("valid")
    }

    #[test]
    fn a_known_vector_signs_stably() {
        // Pin the exact wire bytes so an accidental change to the signed-payload format is a visible
        // test failure, not a silent break of every receiver. HMAC-SHA256("topsecret",
        // "1700000000.{\"hello\":\"world\"}").
        let secret = SigningSecret::new("topsecret");
        let signed = sign(&secret, at(1_700_000_000), br#"{"hello":"world"}"#);
        assert_eq!(
            signed,
            Signature {
                timestamp: 1_700_000_000,
                signature: "v1=79883357e4c4c4abee43cf4b32367d67a1344520479e3e8c85e98406a6d6a2a5"
                    .to_owned(),
            }
        );
    }

    #[test]
    fn a_fresh_signature_verifies() {
        let secret = SigningSecret::new("topsecret");
        let body = br#"[{"event":1}]"#;
        let signed = sign(&secret, at(1_700_000_000), body);
        assert_eq!(
            verify(
                &secret,
                signed.timestamp,
                body,
                &signed.signature,
                at(1_700_000_010)
            ),
            Ok(())
        );
    }

    #[test]
    fn a_tampered_body_fails() {
        let secret = SigningSecret::new("topsecret");
        let signed = sign(&secret, at(1_700_000_000), b"original");
        assert_eq!(
            verify(
                &secret,
                signed.timestamp,
                b"tampered",
                &signed.signature,
                at(1_700_000_010)
            ),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn the_wrong_secret_fails() {
        let signed = sign(&SigningSecret::new("right"), at(1_700_000_000), b"body");
        assert_eq!(
            verify(
                &SigningSecret::new("wrong"),
                signed.timestamp,
                b"body",
                &signed.signature,
                at(1_700_000_010)
            ),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn a_replay_outside_the_window_fails_even_with_a_valid_signature() {
        let secret = SigningSecret::new("topsecret");
        let body = b"body";
        let signed = sign(&secret, at(1_700_000_000), body);
        // The signature is genuine, but the delivery is older than the tolerance.
        let now = at(1_700_000_000 + REPLAY_TOLERANCE + 1);
        assert_eq!(
            verify(&secret, signed.timestamp, body, &signed.signature, now),
            Err(VerifyError::StaleTimestamp)
        );
        // A clock the other side of the window is rejected too.
        let past = at(1_700_000_000 - REPLAY_TOLERANCE - 1);
        assert_eq!(
            verify(&secret, signed.timestamp, body, &signed.signature, past),
            Err(VerifyError::StaleTimestamp)
        );
    }

    #[test]
    fn a_delivery_at_the_edge_of_the_window_still_verifies() {
        let secret = SigningSecret::new("topsecret");
        let body = b"body";
        let signed = sign(&secret, at(1_700_000_000), body);
        let now = at(1_700_000_000 + REPLAY_TOLERANCE);
        assert_eq!(
            verify(&secret, signed.timestamp, body, &signed.signature, now),
            Ok(())
        );
    }

    #[test]
    fn a_malformed_signature_header_is_rejected() {
        let secret = SigningSecret::new("topsecret");
        assert_eq!(
            verify(
                &secret,
                1_700_000_000,
                b"body",
                "deadbeef",
                at(1_700_000_000)
            ),
            Err(VerifyError::MalformedSignature),
            "a signature with no v1= prefix is malformed"
        );
        assert_eq!(
            verify(
                &secret,
                1_700_000_000,
                b"body",
                "v1=nothex",
                at(1_700_000_000)
            ),
            Err(VerifyError::MalformedSignature)
        );
    }

    #[test]
    fn the_secret_is_redacted_from_debug() {
        let rendered = format!("{:?}", SigningSecret::new("supersecret"));
        assert!(
            !rendered.contains("supersecret"),
            "the secret leaked into Debug"
        );
    }

    #[test]
    fn hex_round_trips() {
        let bytes = vec![0x00, 0x0f, 0xa5, 0xff];
        assert_eq!(to_hex(&bytes), "000fa5ff");
        assert_eq!(from_hex("000fa5ff"), Some(bytes));
        assert_eq!(from_hex("abc"), None, "an odd length is not hex");
        assert_eq!(from_hex("zz"), None, "non-hex digits are rejected");
    }
}
