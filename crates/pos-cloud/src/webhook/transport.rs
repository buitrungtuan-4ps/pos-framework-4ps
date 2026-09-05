// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The concrete TLS webhook sender ([ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md)).
//!
//! One HTTPS `POST` per delivery, over the rustls/hyper stack already in the tree
//! ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)). The one thing this does that an
//! ordinary HTTP client does not: it **owns the dial**. A [`super::ssrf::VettedUrl`] carries both the
//! destination and the exact addresses the SSRF vet already approved, so the sender opens a TCP
//! connection to one of *those* addresses and performs the TLS handshake against the URL's hostname —
//! never re-resolving. That closes the DNS-rebinding gap between check and connect by construction: the
//! transport cannot reach anywhere the vet did not approve.
//!
//! Everything but the socket bytes is a pure function (`prepare`) so it is unit-tested without a
//! network; the handshake itself belongs to the gated integration lane and the soak (ADR-0038).

use core::time::Duration;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use hyper::header::{CONTENT_TYPE, HOST};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use super::dispatch::{DeliveryError, WebhookTransport};
use super::sign::{DELIVERY_ID_HEADER, SIGNATURE_HEADER, Signature, TIMESTAMP_HEADER};
use super::ssrf::VettedUrl;

/// A TLS webhook sender: one HTTPS `POST` per delivery, connecting only to pre-vetted addresses.
///
/// Cheap to clone — it holds a shared rustls [`ClientConfig`] and a per-delivery timeout — so the
/// dispatch task keeps one and reuses it across every endpoint.
#[derive(Clone, Debug)]
pub struct TlsWebhookSender {
    tls: Arc<ClientConfig>,
    timeout: Duration,
}

impl TlsWebhookSender {
    /// Builds a sender that trusts the bundled Mozilla roots and gives each delivery `timeout` to
    /// complete.
    ///
    /// The `ring` crypto provider is selected explicitly rather than via the ambient default, so no
    /// `aws-lc-rs` provider can slip in through feature unification (ADR-0038).
    ///
    /// # Panics
    ///
    /// If the `ring` provider cannot supply the safe default TLS protocol versions — which it always
    /// can, so this is a build-time impossibility, not a runtime path.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supplies the safe default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Self {
            tls: Arc::new(tls),
            timeout,
        }
    }

    /// Connects to the first vetted address that accepts a TCP connection.
    async fn connect(addresses: &[SocketAddr]) -> Result<TcpStream, DeliveryError> {
        let mut last_error = None;
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(DeliveryError::new(match last_error {
            Some(error) => format!("connecting to the webhook host failed: {error}"),
            None => "the webhook endpoint had no vetted address to dial".to_owned(),
        }))
    }

    /// Performs one delivery: connect → TLS handshake against the hostname → one HTTP/1.1 `POST`.
    async fn send(
        &self,
        prepared: &Prepared,
        signature: &Signature,
        delivery_id: Option<&str>,
        body: &[u8],
    ) -> Result<(), DeliveryError> {
        let stream = Self::connect(&prepared.addresses).await?;
        // SNI and certificate verification bind to the hostname, not the vetted IP we dialed.
        let server_name = ServerName::try_from(prepared.host.clone())
            .map_err(|_| DeliveryError::new("the webhook host is not a valid TLS server name"))?;
        let connector = TlsConnector::from(Arc::clone(&self.tls));
        let tls = connector
            .connect(server_name, stream)
            .await
            .map_err(|error| DeliveryError::new(format!("the TLS handshake failed: {error}")))?;

        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|error| DeliveryError::new(format!("the HTTP handshake failed: {error}")))?;
        // The connection future must be driven for the request to make progress; it ends when the
        // request completes and `sender` is dropped.
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut request = Request::builder()
            .method("POST")
            .uri(&prepared.request_target)
            .header(HOST, prepared.host.as_str())
            .header(CONTENT_TYPE, "application/json")
            .header(TIMESTAMP_HEADER, signature.timestamp.to_string())
            .header(SIGNATURE_HEADER, signature.signature.as_str());
        // Absent rather than empty when there is nothing stable to key on (production-readiness R6):
        // a receiver that sees the header can trust it identifies the page, and one that does not see
        // it knows this body is not a cursor page.
        if let Some(delivery_id) = delivery_id {
            request = request.header(DELIVERY_ID_HEADER, delivery_id);
        }
        let request = request
            .body(Full::new(Bytes::copy_from_slice(body)))
            .map_err(|error| DeliveryError::new(format!("building the request failed: {error}")))?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|error| DeliveryError::new(format!("sending the delivery failed: {error}")))?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(DeliveryError::new(format!(
                "the receiver returned HTTP {status}"
            )))
        }
    }
}

impl WebhookTransport for TlsWebhookSender {
    async fn deliver(
        &self,
        target: &VettedUrl,
        signature: &Signature,
        delivery_id: Option<&str>,
        body: &[u8],
    ) -> Result<(), DeliveryError> {
        let prepared = prepare(target)?;
        // A black-hole endpoint must not wedge the dispatch loop: the whole connect→handshake→send is
        // bounded, and a timeout is an ordinary failed delivery (the breaker backs off, ADR-0032).
        match tokio::time::timeout(
            self.timeout,
            self.send(&prepared, signature, delivery_id, body),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(DeliveryError::new("the webhook delivery timed out")),
        }
    }
}

/// The connection facts derived from a vetted destination: the hostname to verify against, the
/// origin-form request target, and the `ip:port` addresses to dial.
#[derive(Debug, PartialEq, Eq)]
struct Prepared {
    /// The URL host — the TLS server name and the `Host` header.
    host: String,
    /// The HTTP/1.1 origin-form request target (`/path?query`).
    request_target: String,
    /// The vetted addresses paired with the URL's port.
    addresses: Vec<SocketAddr>,
}

/// Derives the connection facts from an already-vetted destination.
///
/// The URL was validated at registration, so a parse failure here is an internal invariant breach,
/// not tenant input — it becomes a plain [`DeliveryError`] rather than panicking.
fn prepare(target: &VettedUrl) -> Result<Prepared, DeliveryError> {
    let parsed = url::Url::parse(&target.url).map_err(|error| {
        DeliveryError::new(format!("the stored webhook URL is unparseable: {error}"))
    })?;
    let host = parsed
        .host_str()
        .ok_or_else(|| DeliveryError::new("the stored webhook URL has no host"))?
        .to_owned();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let mut request_target = parsed.path().to_owned();
    if let Some(query) = parsed.query() {
        request_target.push('?');
        request_target.push_str(query);
    }
    let addresses = target
        .addresses
        .iter()
        .map(|ip| SocketAddr::new(*ip, port))
        .collect();
    Ok(Prepared {
        host,
        request_target,
        addresses,
    })
}

#[cfg(test)]
mod tests {
    use super::prepare;
    use crate::webhook::ssrf::VettedUrl;
    use std::net::SocketAddr;

    #[test]
    fn prepare_derives_host_target_and_dial_addresses() {
        let target = VettedUrl {
            url: "https://hooks.example.com/pos/deliver?tenant=42".to_owned(),
            addresses: vec!["93.184.216.34".parse().expect("addr")],
        };
        let prepared = prepare(&target).expect("prepare");
        assert_eq!(
            prepared.host, "hooks.example.com",
            "the Host header and SNI are the URL host, never the dialed IP"
        );
        assert_eq!(
            prepared.request_target, "/pos/deliver?tenant=42",
            "the origin-form target keeps the path and query"
        );
        assert_eq!(
            prepared.addresses,
            vec![SocketAddr::from(([93, 184, 216, 34], 443))],
            "it dials the vetted address at the default https port"
        );
    }

    #[test]
    fn prepare_honours_an_explicit_port_and_a_bare_path() {
        let target = VettedUrl {
            url: "https://hooks.example.com:8443/hook".to_owned(),
            addresses: vec!["93.184.216.34".parse().expect("addr")],
        };
        let prepared = prepare(&target).expect("prepare");
        assert_eq!(
            prepared.addresses,
            vec![SocketAddr::from(([93, 184, 216, 34], 8443))],
            "an explicit port is dialed"
        );
        assert_eq!(prepared.request_target, "/hook");
    }

    #[test]
    fn prepare_pairs_every_vetted_address_with_the_port() {
        let target = VettedUrl {
            url: "https://hooks.example.com/hook".to_owned(),
            addresses: vec![
                "93.184.216.34".parse().expect("addr"),
                "2606:2800:220:1:248:1893:25c8:1946".parse().expect("addr"),
            ],
        };
        let prepared = prepare(&target).expect("prepare");
        assert_eq!(
            prepared.addresses.len(),
            2,
            "both vetted addresses are dial candidates"
        );
        assert!(
            prepared
                .addresses
                .iter()
                .all(|address| address.port() == 443),
            "each is paired with the https port"
        );
    }
}
