// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`S3Blobs`]: the endpoint, `SigV4`-signed HTTP requests, and the `BlobStore` implementation.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use pos_ports::blob_store::{BlobKey, BlobStore};
use pos_ports::{PortError, PortName};

use crate::sign::{self, Credentials, EMPTY_PAYLOAD_SHA256};

/// The headers `SigV4` signs, in the fixed order S3 expects them listed.
const SIGNED_HEADERS: &str = "host;x-amz-content-sha256;x-amz-date";

/// The default port for a `host` with no `:port`.
const DEFAULT_PORT: u16 = 80;

/// A [`BlobStore`] over an S3-compatible object store (Garage in production, MinIO in tests),
/// reached path-style over plain HTTP.
///
/// Cloneable and shareable: it holds only configuration and opens a fresh connection per request.
#[derive(Debug, Clone)]
pub struct S3Blobs {
    host: String,
    port: u16,
    bucket: String,
    credentials: Credentials,
}

impl S3Blobs {
    /// Builds a store for `bucket` at `endpoint` (an `http://host:port`), signing with the given
    /// region and keys.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if `endpoint` is not a usable `http://` address.
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self, PortError> {
        let rest = endpoint
            .strip_prefix("http://")
            .ok_or_else(|| invalid("the S3 endpoint must be http://host:port"))?;
        let authority = rest.split('/').next().unwrap_or(rest);
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                let port = port.parse::<u16>().map_err(|error| {
                    invalid("the S3 endpoint port is not a number").with_source(error)
                })?;
                (host.to_owned(), port)
            }
            None => (authority.to_owned(), DEFAULT_PORT),
        };
        if host.is_empty() {
            return Err(invalid("the S3 endpoint has no host"));
        }
        Ok(Self {
            host,
            port,
            bucket: bucket.to_owned(),
            credentials: Credentials {
                access_key: access_key.to_owned(),
                secret_key: secret_key.to_owned(),
                region: region.to_owned(),
                service: "s3".to_owned(),
            },
        })
    }

    /// Creates the bucket, tolerating one that already exists — idempotent, for cloud bootstrap.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached, or [`PortError::internal`] on an
    /// unexpected status.
    pub async fn ensure_bucket(&self) -> Result<(), PortError> {
        let path = format!("/{}", self.bucket);
        let (status, _) = self.request("PUT", &path, "", &[]).await?;
        // 2xx created; 409 already exists / already owned. Both are success for an idempotent ensure.
        if is_success(status) || status == 409 {
            Ok(())
        } else {
            Err(unexpected("create bucket", status))
        }
    }

    /// The path for an object key, path-style.
    fn object_path(&self, key: &BlobKey) -> String {
        format!("/{}/{}", self.bucket, key.as_str())
    }

    /// Signs and sends one request, returning the status code and response body.
    async fn request(
        &self,
        method: &str,
        canonical_path: &str,
        query: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), PortError> {
        let (date, amz_date) = now_utc();
        let payload_hash = if body.is_empty() {
            EMPTY_PAYLOAD_SHA256.to_owned()
        } else {
            sign::sha256_hex(body)
        };
        let host_header = format!("{}:{}", self.host, self.port);
        let canonical_uri = sign::uri_encode(canonical_path, true);
        let canonical_headers = format!(
            "host:{host_header}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n"
        );
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{query}\n{canonical_headers}\n{SIGNED_HEADERS}\n{payload_hash}"
        );
        let authorization = sign::authorization(
            &self.credentials,
            &date,
            &amz_date,
            SIGNED_HEADERS,
            &canonical_request,
        );

        let target = if query.is_empty() {
            canonical_uri
        } else {
            format!("{canonical_uri}?{query}")
        };
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: {host_header}\r\nx-amz-date: {amz_date}\r\n\
             x-amz-content-sha256: {payload_hash}\r\nAuthorization: {authorization}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );

        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(unreachable)?;
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(unreachable)?;
        if !body.is_empty() {
            stream.write_all(body).await.map_err(unreachable)?;
        }
        stream.flush().await.map_err(unreachable)?;

        // `Connection: close`, so the server closes after the response and `read_to_end` yields the
        // whole thing; the object bytes are exactly what follows the header terminator.
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .map_err(unreachable)?;
        let (head_bytes, body_bytes) = split_response(&response);
        Ok((status_code(head_bytes), body_bytes.to_vec()))
    }
}

impl BlobStore for S3Blobs {
    async fn put(&self, key: &BlobKey, body: &[u8]) -> Result<(), PortError> {
        let (status, _) = self
            .request("PUT", &self.object_path(key), "", body)
            .await?;
        if is_success(status) {
            Ok(())
        } else if status == 507 {
            Err(PortError::resource_exhausted(
                PortName::BlobStore,
                "the object store is out of space",
            ))
        } else {
            Err(unexpected("put", status))
        }
    }

    async fn get(&self, key: &BlobKey) -> Result<Option<Vec<u8>>, PortError> {
        let (status, body) = self.request("GET", &self.object_path(key), "", &[]).await?;
        match status {
            200 => Ok(Some(body)),
            // An absent object is a fact, not an exception — the restore drill asks without handling
            // an error.
            404 => Ok(None),
            other => Err(unexpected("get", other)),
        }
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), PortError> {
        let (status, _) = self
            .request("DELETE", &self.object_path(key), "", &[])
            .await?;
        // 2xx deleted, 404 already gone — both success, because cleanup runs more than once.
        if is_success(status) || status == 404 {
            Ok(())
        } else {
            Err(unexpected("delete", status))
        }
    }

    async fn list(&self, prefix: &BlobKey) -> Result<Vec<BlobKey>, PortError> {
        let bucket_path = format!("/{}", self.bucket);
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let query = list_query(prefix, token.as_deref());
            let (status, body) = self.request("GET", &bucket_path, &query, &[]).await?;
            if !is_success(status) {
                return Err(unexpected("list", status));
            }
            let xml = String::from_utf8_lossy(&body);
            for raw in extract_tags(&xml, "Key") {
                // S3's `prefix` is a string match, so it also returns `stores/10` for `stores/1`;
                // `is_under` is segment-aware and is what keeps one tenant's listing out of another's.
                if let Ok(key) = BlobKey::parse(&raw)
                    && key.is_under(prefix)
                {
                    keys.push(key);
                }
            }
            let truncated = extract_tags(&xml, "IsTruncated")
                .into_iter()
                .next()
                .as_deref()
                == Some("true");
            if truncated {
                token = extract_tags(&xml, "NextContinuationToken")
                    .into_iter()
                    .next();
                if token.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        keys.sort();
        Ok(keys)
    }
}

/// The `list-type=2` query string, in the sorted, encoded form `SigV4` canonicalises to.
fn list_query(prefix: &BlobKey, token: Option<&str>) -> String {
    // Sorted by key: continuation-token < list-type < prefix.
    let mut parts = Vec::new();
    if let Some(token) = token {
        parts.push(format!(
            "continuation-token={}",
            sign::uri_encode(token, false)
        ));
    }
    parts.push("list-type=2".to_owned());
    parts.push(format!(
        "prefix={}",
        sign::uri_encode(prefix.as_str(), false)
    ));
    parts.join("&")
}

/// The current UTC time as `(YYYYMMDD, YYYYMMDDTHHMMSSZ)` for `SigV4`.
fn now_utc() -> (String, String) {
    let zoned = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    (
        zoned.strftime("%Y%m%d").to_string(),
        zoned.strftime("%Y%m%dT%H%M%SZ").to_string(),
    )
}

/// Whether a status code is 2xx.
fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Splits an HTTP response into its head and body at the first blank line.
fn split_response(response: &[u8]) -> (&[u8], &[u8]) {
    match find(response, b"\r\n\r\n") {
        Some(index) => (
            response.get(..index).unwrap_or(&[]),
            response.get(index + 4..).unwrap_or(&[]),
        ),
        None => (response, &[]),
    }
}

/// The status code from an HTTP response's head, or 0 if it cannot be read.
fn status_code(head: &[u8]) -> u16 {
    let first_line = head.split(|&byte| byte == b'\n').next().unwrap_or(&[]);
    let code = first_line.split(|&byte| byte == b' ').nth(1).unwrap_or(&[]);
    core::str::from_utf8(code)
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .unwrap_or(0)
}

/// Every `<tag>…</tag>` value in `xml`, in order. Adequate because S3 keys and tokens contain no
/// XML-special characters (`BlobKey`'s alphabet excludes `<`, `>` and `&`).
fn extract_tags(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let Some(tail) = rest.get(start + open.len()..) else {
            break;
        };
        let Some(end) = tail.find(&close) else { break };
        if let Some(value) = tail.get(..end) {
            out.push(value.to_owned());
        }
        rest = tail.get(end + close.len()..).unwrap_or("");
    }
    out
}

/// The first offset at which `needle` occurs in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn invalid(message: &'static str) -> PortError {
    PortError::invalid_argument(PortName::BlobStore, message)
}

fn unreachable(error: std::io::Error) -> PortError {
    PortError::unavailable(PortName::BlobStore, "the object store is unreachable")
        .with_source(error)
}

fn unexpected(operation: &str, status: u16) -> PortError {
    PortError::internal(
        PortName::BlobStore,
        format!("the object store answered {status} to a {operation} request"),
    )
}
