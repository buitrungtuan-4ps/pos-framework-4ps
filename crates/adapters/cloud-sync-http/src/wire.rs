// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The HTTP transport seam and its production TLS implementation
//! ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)).
//!
//! [`HttpTransport`] is the one thing that touches a socket; [`TlsHttpTransport`] is the concrete
//! sender the edge composes, on the rustls/hyper stack already in the tree
//! ([ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md)). Unlike the webhook sender, this dials
//! exactly one trusted host — the store's own cloud — so it resolves and connects the ordinary way;
//! there is no SSRF surface to defend.

use core::future::Future;
use core::time::Duration;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::header::{CONTENT_TYPE, HOST};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

/// One HTTP request/response over some transport — the seam the socket lives behind.
///
/// Only a JSON `POST` is needed, so that is all this exposes; keeping it to one method keeps the stub
/// the contract suite runs against trivial. The implementor owns the connection; the caller sees only
/// the status and body.
pub trait HttpTransport: Send + Sync {
    /// `POST`s `body` as `application/json` to `path` (joined onto the transport's base URL) and
    /// returns the response status and body.
    ///
    /// # Errors
    ///
    /// [`TransportError`] if the request could not be completed — the host could not be resolved,
    /// dialed, TLS-handshaken, or answered within the timeout. A non-2xx *response* is not an error
    /// here; it comes back in [`HttpResponse::status`] for the caller to map.
    fn post_json(
        &self,
        path: &str,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<HttpResponse, TransportError>> + Send;
}

/// A response from the cloud: the HTTP status and the body bytes.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The response body, verbatim.
    pub body: Vec<u8>,
}

/// A transport-level failure — the cloud could not be reached — as distinct from a response the
/// caller must interpret. Always maps to [`unavailable`](pos_ports::PortError::unavailable).
#[derive(Debug, thiserror::Error)]
#[error("the cloud transport failed: {0}")]
pub struct TransportError(String);

impl TransportError {
    /// A transport failure carrying a human-readable reason.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The production transport: one HTTPS request per call, over rustls/hyper
/// ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)).
///
/// Cheap to clone — it holds a shared rustls [`ClientConfig`], the parsed base URL, and a per-request
/// timeout — so the edge keeps one and reuses it across calls.
#[derive(Debug, Clone)]
pub struct TlsHttpTransport {
    base: url::Url,
    tls: Arc<ClientConfig>,
    timeout: Duration,
}

impl TlsHttpTransport {
    /// Builds a transport pointed at `base_url` (the store's cloud), trusting the bundled Mozilla
    /// roots and giving each request `timeout` to complete.
    ///
    /// The `ring` crypto provider is selected explicitly rather than via the ambient default, so no
    /// `aws-lc-rs` provider can slip in through feature unification (ADR-0038).
    ///
    /// # Errors
    ///
    /// [`TransportError`] if `base_url` is not a valid `https` URL with a host, or if the `ring`
    /// provider cannot supply the safe default TLS protocol versions (a build-time impossibility, so
    /// it is a `Result` here only to keep this constructor panic-free per the workspace lints).
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, TransportError> {
        let base = url::Url::parse(base_url).map_err(|error| {
            TransportError::new(format!("the cloud base URL is unparseable: {error}"))
        })?;
        if base.scheme() != "https" {
            return Err(TransportError::new("the cloud base URL must use https"));
        }
        if base.host_str().is_none() {
            return Err(TransportError::new("the cloud base URL has no host"));
        }
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            TransportError::new(format!(
                "the ring provider rejected the safe defaults: {error}"
            ))
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            base,
            tls: Arc::new(tls),
            timeout,
        })
    }

    /// Connects to the first resolved address that accepts a TCP connection.
    async fn connect(host: &str, port: u16) -> Result<TcpStream, TransportError> {
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| {
                TransportError::new(format!("resolving the cloud host failed: {error}"))
            })?
            .collect();
        let mut last_error = None;
        for address in &addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(TransportError::new(match last_error {
            Some(error) => format!("connecting to the cloud failed: {error}"),
            None => "the cloud host resolved to no addresses".to_owned(),
        }))
    }

    /// Performs one request: resolve → connect → TLS handshake against the hostname → one HTTP/1.1
    /// `POST` → read the whole response body.
    async fn send(&self, path: &str, body: Vec<u8>) -> Result<HttpResponse, TransportError> {
        let target = self.base.join(path).map_err(|error| {
            TransportError::new(format!("joining the request path failed: {error}"))
        })?;
        let host = target
            .host_str()
            .ok_or_else(|| TransportError::new("the request URL has no host"))?
            .to_owned();
        let port = target.port_or_known_default().unwrap_or(443);
        let mut request_target = target.path().to_owned();
        if let Some(query) = target.query() {
            request_target.push('?');
            request_target.push_str(query);
        }

        let stream = Self::connect(&host, port).await?;
        // SNI and certificate verification bind to the hostname, not the dialed address.
        let server_name = ServerName::try_from(host.clone()).map_err(|_ignored| {
            TransportError::new("the cloud host is not a valid TLS server name")
        })?;
        let connector = TlsConnector::from(Arc::clone(&self.tls));
        let tls = connector
            .connect(server_name, stream)
            .await
            .map_err(|error| TransportError::new(format!("the TLS handshake failed: {error}")))?;

        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|error| TransportError::new(format!("the HTTP handshake failed: {error}")))?;
        // The connection future must be driven for the request to make progress; it ends when the
        // request completes and `sender` is dropped.
        tokio::spawn(async move {
            let _ignored = connection.await;
        });

        let request = Request::builder()
            .method("POST")
            .uri(&request_target)
            .header(HOST, host.as_str())
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| {
                TransportError::new(format!("building the request failed: {error}"))
            })?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|error| TransportError::new(format!("sending the request failed: {error}")))?;
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|error| {
                TransportError::new(format!("reading the response body failed: {error}"))
            })?
            .to_bytes();
        Ok(HttpResponse {
            status,
            body: bytes.to_vec(),
        })
    }
}

impl HttpTransport for TlsHttpTransport {
    async fn post_json(&self, path: &str, body: Vec<u8>) -> Result<HttpResponse, TransportError> {
        // A black-hole cloud must not wedge the caller: the whole resolve→connect→handshake→send is
        // bounded, and a timeout is an ordinary transport failure the caller maps to `unavailable`.
        match tokio::time::timeout(self.timeout, self.send(path, body)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(TransportError::new("the request to the cloud timed out")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TlsHttpTransport;
    use core::time::Duration;

    #[test]
    fn a_plain_http_base_is_rejected() {
        let built = TlsHttpTransport::new("http://cloud.example.com", Duration::from_secs(5));
        assert!(built.is_err(), "the cloud base URL must be https");
    }

    #[test]
    fn an_unparseable_base_is_rejected() {
        let built = TlsHttpTransport::new("not a url", Duration::from_secs(5));
        assert!(built.is_err(), "a garbage base URL is a construction error");
    }

    #[test]
    fn a_valid_https_base_builds() {
        let built = TlsHttpTransport::new("https://cloud.example.com", Duration::from_secs(5));
        assert!(built.is_ok(), "a well-formed https base builds");
    }
}
