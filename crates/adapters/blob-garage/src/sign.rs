// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Amazon `SigV4` request signing, by hand ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)).
//!
//! Small and self-contained because the `BlobStore` port is thin and scheduled for deletion
//! ([ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md)), so it does not earn an S3 SDK.
//! The one thing `SigV4` has to get exactly right — the canonical request, the string to sign, the
//! signing-key HMAC chain — is verified in this module's unit test against AWS's own published
//! `get-vanilla` vector, so the arithmetic is checked with no server in sight.

use core::fmt;

use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// The hex SHA-256 of an empty body — the payload hash for every request with no body.
pub(crate) const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The access key, secret, region and service a signature is scoped to.
///
/// The secret is redacted from [`fmt::Debug`], so a store that derives `Debug` cannot log it.
#[derive(Clone)]
pub(crate) struct Credentials {
    /// The access key id, which travels in the `Authorization` header in the clear.
    pub(crate) access_key: String,
    /// The secret, never logged.
    pub(crate) secret_key: String,
    /// The signing region (MinIO and Garage default to `us-east-1`).
    pub(crate) region: String,
    /// The signing service — `s3`.
    pub(crate) service: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key", &self.access_key)
            .field("secret_key", &"<redacted>")
            .field("region", &self.region)
            .field("service", &self.service)
            .finish()
    }
}

/// HMAC-SHA256. HMAC accepts a key of any length, so the error path is unreachable in practice; it
/// yields an empty tag, which would produce a signature the server rejects rather than a panic.
fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    match HmacSha256::new_from_slice(key) {
        Ok(mut mac) => {
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// Lowercase hex encoding.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// SHA-256 of `data`, hex.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex(&hasher.finalize())
}

/// RFC 3986 percent-encoding as `SigV4` requires it. When `keep_slash`, `/` is left as a path
/// separator rather than encoded — correct for a canonical URI, wrong for a query value.
pub(crate) fn uri_encode(input: &str, keep_slash: bool) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b'/' if keep_slash => out.push('/'),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The credential scope line, `YYYYMMDD/region/service/aws4_request`.
fn scope(credentials: &Credentials, date: &str) -> String {
    format!(
        "{date}/{}/{}/aws4_request",
        credentials.region, credentials.service
    )
}

/// The `SigV4` signature (hex) over a prepared canonical request.
///
/// `date` is `YYYYMMDD`; `amz_date` is `YYYYMMDDTHHMMSSZ`. Separated from header assembly so the
/// unit test can drive it with AWS's documented canonical request directly.
pub(crate) fn signature(
    credentials: &Credentials,
    date: &str,
    amz_date: &str,
    canonical_request: &str,
) -> String {
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{}\n{}",
        scope(credentials, date),
        sha256_hex(canonical_request.as_bytes())
    );
    let mut key = hmac(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date.as_bytes(),
    );
    key = hmac(&key, credentials.region.as_bytes());
    key = hmac(&key, credentials.service.as_bytes());
    key = hmac(&key, b"aws4_request");
    hex(&hmac(&key, string_to_sign.as_bytes()))
}

/// The full `Authorization` header value for a request whose canonical form is `canonical_request`.
pub(crate) fn authorization(
    credentials: &Credentials,
    date: &str,
    amz_date: &str,
    signed_headers: &str,
    canonical_request: &str,
) -> String {
    let signature = signature(credentials, date, amz_date, canonical_request);
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key,
        scope(credentials, date)
    )
}

#[cfg(test)]
mod tests {
    use super::{Credentials, EMPTY_PAYLOAD_SHA256, sha256_hex, signature, uri_encode};

    fn example_credentials() -> Credentials {
        Credentials {
            access_key: "AKIDEXAMPLE".to_owned(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_owned(),
            region: "us-east-1".to_owned(),
            service: "service".to_owned(),
        }
    }

    #[test]
    fn matches_the_aws_sigv4_get_vanilla_vector() {
        // Straight from AWS's `aws-sig-v4-test-suite` (`get-vanilla`): a GET of `/` on
        // example.amazonaws.com at a fixed instant, with the documented signature. If the canonical
        // request, string-to-sign, or the four-step signing-key derivation is wrong by one byte,
        // this fails — which is what lets the S3 layer above be trusted without a server.
        let canonical_request = concat!(
            "GET\n",
            "/\n",
            "\n",
            "host:example.amazonaws.com\n",
            "x-amz-date:20150830T123600Z\n",
            "\n",
            "host;x-amz-date\n",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        let signature = signature(
            &example_credentials(),
            "20150830",
            "20150830T123600Z",
            canonical_request,
        );
        assert_eq!(
            signature,
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn the_empty_payload_constant_is_the_hash_of_nothing() {
        // Used on every GET/DELETE/LIST, so a wrong constant would break every read.
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_SHA256);
    }

    #[test]
    fn uri_encoding_keeps_path_slashes_and_escapes_query_reserved() {
        assert_eq!(
            uri_encode("stores/1/backup.sqlite", true),
            "stores/1/backup.sqlite"
        );
        assert_eq!(uri_encode("stores/1", false), "stores%2F1");
        // A space and an ampersand are reserved in a query component.
        assert_eq!(uri_encode("a b&c", false), "a%20b%26c");
    }
}
