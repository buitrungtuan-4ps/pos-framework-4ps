// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The agent's two HTTP calls, over the stack
//! [ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md) pins.
//!
//! # Both schemes, because the edge is in two places
//!
//! [ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md) made where the edge
//! runs a deployment axis. In-store it is a box on the shop LAN serving plain HTTP behind no proxy;
//! hosted, it is behind the same TLS terminator everything else is. So `http` and `https` are both
//! accepted, and which one a store uses is a fact about that store rather than a posture this binary
//! gets to hold. Nothing else is: a scheme this build does not know is refused at construction,
//! where an operator can still read the message.
//!
//! # One request per call, no pooling
//!
//! The claim parks for up to twenty seconds and then returns; an acknowledgement is a few bytes. A
//! connection pool would buy nothing at that rate and would add a class of failure — a half-closed
//! socket a proxy reaped — that this shape simply does not have.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::Request;
use hyper::header::{AUTHORIZATION, HOST, USER_AGENT};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use pos_proto::ids::EventId;

use crate::{AgentError, EdgeTransport, LeasedJob, LeasedJobs, VERSION};

/// How long a single call may take.
///
/// Longer than the edge's own twenty-second park, because a claim that returns at the deadline is
/// the *ordinary* case and must not read as a timeout. The margin is what covers a slow link on top
/// of it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

/// The edge this agent claims from.
///
/// Cheap to clone: a parsed URL, a shared rustls configuration, and the device token.
#[derive(Debug, Clone)]
pub struct HttpEdge {
    base: url::Url,
    token: String,
    tls: Option<Arc<ClientConfig>>,
}

impl HttpEdge {
    /// Builds a client pointed at `edge_url`, presenting `device_token` on every call.
    ///
    /// # Errors
    ///
    /// [`AgentError::Config`] if the URL is unparseable, carries no host, or names a scheme other
    /// than `http` or `https`; or if the `ring` provider cannot supply the safe default TLS
    /// versions (a build-time impossibility, so it is a `Result` here only to stay panic-free).
    pub fn new(edge_url: &str, device_token: &str) -> Result<Self, AgentError> {
        let base = url::Url::parse(edge_url)
            .map_err(|error| AgentError::Config(format!("edge_url is unparseable: {error}")))?;
        if base.host_str().is_none() {
            return Err(AgentError::Config("edge_url has no host".to_owned()));
        }
        let tls = match base.scheme() {
            "http" => None,
            "https" => {
                let mut roots = RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                let config = ClientConfig::builder_with_provider(Arc::new(
                    tokio_rustls::rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|error| {
                    AgentError::Config(format!(
                        "the ring provider rejected the safe defaults: {error}"
                    ))
                })?
                .with_root_certificates(roots)
                .with_no_client_auth();
                Some(Arc::new(config))
            }
            other => {
                return Err(AgentError::Config(format!(
                    "edge_url must use http or https, not {other}"
                )));
            }
        };
        Ok(Self {
            base,
            token: device_token.to_owned(),
            tls,
        })
    }

    /// Resolves and connects to the first address that accepts a TCP connection.
    async fn connect(host: &str, port: u16) -> Result<TcpStream, AgentError> {
        let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| AgentError::Edge(format!("resolving the edge failed: {error}")))?
            .collect();
        let mut last = None;
        for address in &addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last = Some(error),
            }
        }
        Err(AgentError::Edge(match last {
            Some(error) => format!("connecting to the edge failed: {error}"),
            None => "the edge's host resolved to no addresses".to_owned(),
        }))
    }

    /// One request: connect, optionally handshake, send, read the whole body.
    async fn send(&self, method: &str, path: &str) -> Result<(u16, Vec<u8>), AgentError> {
        let target = self.base.join(path).map_err(|error| {
            AgentError::Edge(format!("joining the request path failed: {error}"))
        })?;
        let host = target
            .host_str()
            .ok_or_else(|| AgentError::Edge("the request URL has no host".to_owned()))?
            .to_owned();
        let port = target.port_or_known_default().unwrap_or(80);
        let mut request_target = target.path().to_owned();
        if let Some(query) = target.query() {
            request_target.push('?');
            request_target.push_str(query);
        }

        let stream = Self::connect(&host, port).await?;
        let io: TokioIo<Box<dyn Stream>> = match self.tls.as_ref() {
            Some(tls) => {
                // SNI and certificate verification bind to the hostname, not the dialed address.
                let server_name = ServerName::try_from(host.clone()).map_err(|_ignored| {
                    AgentError::Edge("the edge's host is not a valid TLS server name".to_owned())
                })?;
                let connected = TlsConnector::from(Arc::clone(tls))
                    .connect(server_name, stream)
                    .await
                    .map_err(|error| {
                        AgentError::Edge(format!("the TLS handshake failed: {error}"))
                    })?;
                TokioIo::new(Box::new(connected))
            }
            None => TokioIo::new(Box::new(stream)),
        };

        let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|error| AgentError::Edge(format!("the HTTP handshake failed: {error}")))?;
        // The connection future must be driven for the request to make progress; it ends when the
        // request completes and `sender` is dropped.
        tokio::spawn(async move {
            let _ignored = connection.await;
        });

        let request = Request::builder()
            .method(method)
            .uri(&request_target)
            .header(HOST, host.as_str())
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(USER_AGENT, format!("pos_print_agent/{VERSION}"))
            .body(Full::new(Bytes::new()))
            .map_err(|error| AgentError::Edge(format!("building the request failed: {error}")))?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|error| AgentError::Edge(format!("sending the request failed: {error}")))?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|error| AgentError::Edge(format!("reading the response failed: {error}")))?
            .to_bytes();
        Ok((status, body.to_vec()))
    }

    /// The same, bounded, so a black-hole edge cannot wedge the agent.
    async fn call(&self, method: &str, path: &str) -> Result<(u16, Vec<u8>), AgentError> {
        match tokio::time::timeout(REQUEST_TIMEOUT, self.send(method, path)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(AgentError::Edge(
                "the request to the edge timed out".to_owned(),
            )),
        }
    }
}

/// What `TokioIo` needs from either kind of stream.
trait Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> Stream for T {}

impl EdgeTransport for HttpEdge {
    async fn claim(&self) -> Result<Vec<LeasedJob>, AgentError> {
        let (status, body) = self.call("GET", "/api/print/jobs").await?;
        match status {
            200 => serde_json::from_slice::<LeasedJobs>(&body)
                .map(|leased| leased.jobs)
                .map_err(|error| {
                    AgentError::Edge(format!("the edge's answer could not be read: {error}"))
                }),
            // The two the edge actually returns, named rather than lumped: they send an operator to
            // different places, and this log line is what somebody reads when a kitchen has no
            // tickets.
            401 => Err(AgentError::Edge(
                "this device is not paired with that edge; pair it again".to_owned(),
            )),
            409 => Err(AgentError::Edge(
                "this device answers for no print agent; a manager binds one at the till"
                    .to_owned(),
            )),
            other => Err(AgentError::Edge(format!(
                "the edge answered {other} to a claim"
            ))),
        }
    }

    async fn acknowledge(&self, job: EventId) -> Result<(), AgentError> {
        let (status, _body) = self
            .call("POST", &format!("/api/print/jobs/{job}/ack"))
            .await?;
        if status == 204 {
            return Ok(());
        }
        Err(AgentError::Edge(format!(
            "the edge answered {status} to an acknowledgement"
        )))
    }
}
