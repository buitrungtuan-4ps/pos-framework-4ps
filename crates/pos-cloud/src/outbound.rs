// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One HTTPS GET, to an address the SSRF vet approved, over the tree's rustls stack.
//!
//! Moved here unchanged from [`crate::release_source`] when a second caller needed it: the HQ
//! federation of rollups ([ADR-0179](../../../docs/adr/0179-cells-the-directory-and-the-front-door.md)
//! C10). It is the third outbound HTTPS client in the crate, after the webhook sender and the release
//! source, which is when `docs/design-principles.md` says to share one. It follows no redirect and
//! decides nothing about status codes: each caller reads the [`Answer`] by its own rules, so the
//! release source still follows its forge's redirects and the federation follows none.
//!
//! The `ring` crypto provider is selected explicitly rather than via the ambient default, so no
//! `aws-lc-rs` provider can slip in through feature unification, the same care
//! [ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md) took for the webhook sender.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Limited};
use hyper::Request;
use hyper::header::{HOST, HeaderName, LOCATION};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use crate::webhook::ssrf::vet_blocking;

/// What one GET answered: its status, where it pointed if it redirected, and its body.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) location: Option<String>,
    pub(crate) body: Vec<u8>,
}

/// An HTTPS client for single GETs. Cheap to clone: a shared rustls config.
#[derive(Clone)]
pub(crate) struct HttpsGet {
    tls: Arc<ClientConfig>,
}

impl HttpsGet {
    /// A client trusting the `webpki` roots, over the `ring` provider.
    ///
    /// # Panics
    ///
    /// If the `ring` provider cannot supply the safe default TLS protocol versions — which it always
    /// can, so this is a build-time impossibility, not a runtime path.
    #[must_use]
    pub(crate) fn new() -> Self {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supplies the safe default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Self { tls: Arc::new(tls) }
    }

    /// One GET over TLS, to the address the SSRF vet approved for this URL, with `headers` after
    /// `Host`, and a body of at most `max_bytes`. `subject` names the far end in a failed dial.
    ///
    /// # Errors
    ///
    /// A sentence saying which step failed: the vet, the dial, the TLS or HTTP handshake, the
    /// request, or a body over `max_bytes`.
    pub(crate) async fn get_once(
        &self,
        url: &str,
        headers: &[(HeaderName, String)],
        max_bytes: usize,
        subject: &str,
    ) -> Result<Answer, String> {
        let vetted = vet_blocking(url)
            .await
            .map_err(|reason| format!("{url}: {reason}"))?;
        let parsed = url::Url::parse(&vetted.url).map_err(|error| format!("{url}: {error}"))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| format!("{url}: no host"))?
            .to_owned();
        let port = parsed.port_or_known_default().unwrap_or(443);
        let mut target = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            target.push('?');
            target.push_str(query);
        }

        let stream = connect(&vetted.addresses, port, subject).await?;
        // SNI and certificate verification bind to the hostname, never the vetted IP we dialed.
        let server_name = ServerName::try_from(host.clone())
            .map_err(|_| format!("{host} is not a valid TLS server name"))?;
        let tls = TlsConnector::from(Arc::clone(&self.tls))
            .connect(server_name, stream)
            .await
            .map_err(|error| format!("the TLS handshake with {host} failed: {error}"))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|error| format!("the HTTP handshake with {host} failed: {error}"))?;
        // The connection future must be driven for the request to make progress; it ends when the
        // response completes and `sender` is dropped.
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut request = Request::builder()
            .method("GET")
            .uri(&target)
            .header(HOST, host.as_str());
        for (name, value) in headers {
            request = request.header(name, value.as_str());
        }
        let request = request
            .body(Empty::<Bytes>::new())
            .map_err(|error| format!("building the request failed: {error}"))?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|error| format!("the request to {host} failed: {error}"))?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = Limited::new(response.into_body(), max_bytes)
            .collect()
            .await
            .map_err(|_over| format!("{host} answered with more than {max_bytes} bytes"))?
            .to_bytes()
            .to_vec();
        Ok(Answer {
            status,
            location,
            body,
        })
    }
}

/// Connects to the first vetted address that accepts a TCP connection.
async fn connect(
    addresses: &[std::net::IpAddr],
    port: u16,
    subject: &str,
) -> Result<TcpStream, String> {
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect(SocketAddr::new(*address, port)).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(match last_error {
        Some(error) => format!("connecting to {subject} failed: {error}"),
        None => format!("{subject} had no vetted address to dial"),
    })
}
