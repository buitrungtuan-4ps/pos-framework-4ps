// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's outbound cloud HTTP client and the config-pull, heartbeat, and order-relay transports
//! ([ADR-0085](../../../docs/adr/0085-edge-cloud-sync-transport.md),
//! [ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)).
//!
//! The config-pull ([`config_client`](crate::config_client)), heartbeat
//! ([`heartbeat_client`](crate::heartbeat_client)), and order-relay
//! ([`relay_client`](crate::relay_client)) loops each hide their HTTP behind a seam so the loop logic
//! is tested with no socket. This module is the field implementation of those seams: one
//! small rustls/hyper client ([`CloudHttpClient`]) — the same stack the webhook sender
//! ([ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md)) and `cloud-sync-http`
//! ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)) pin — reused by
//! [`ConfigHttpTransport`], [`HeartbeatHttpTransport`], and the order relay's
//! [`RelayHttpTransport`] ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)).
//!
//! It dials exactly one trusted host — the store's own cloud, its base URL set at provisioning — so,
//! like `cloud-sync-http`, it resolves and connects the ordinary way; there is no SSRF surface to
//! defend. Every request carries the store's scoped `read_config` API key as a bearer (ADR-0037/0039),
//! the credential the `/sync` routes verify today; the key is supplied out-of-band (never in
//! `config.toml`, never committed — ADR-0085) and the caller passes it here.
//!
//! The socket lives in [`CloudHttpClient::request`]; everything the tests need — the request line the
//! path and query produce, and the interpretation of a config-sync response — is a pure function
//! beside it, so the branching is checked in the fast gate without a peer.

use core::time::Duration;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use pos_proto::ids::StoreId;

use crate::config_client::{ConfigTransport, ConfigTransportError, SyncedConfig};
use crate::heartbeat_client::{HeartbeatError, HeartbeatReport, HeartbeatTransport};
use crate::relay_client::{PendingOrderDto, RelayTransport, RelayTransportError, StoreOutcome};

/// How long any one request may take, end to end (resolve → connect → handshake → send → read). A
/// black-hole cloud must never wedge a loop; a timeout is an ordinary transport failure the loop
/// backs off from, and the store keeps trading locally meanwhile (ADR-0001).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a relay pull may take. The cloud **parks** a pull with nothing to give for up to 20
/// seconds ([ADR-0061](../../../docs/adr/0061-order-relay.md)) before answering with an empty batch,
/// so the ordinary [`REQUEST_TIMEOUT`] would cut every poll short and a quiet store would never see a
/// parked order (ADR-0087). This allows the full park plus margin.
const RELAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A failure of the outbound cloud transport — the cloud could not be reached, or answered in a way
/// the client could not use. Carries a human-readable reason for the store's log; configuration and
/// heartbeats carry no personal data, so the reason is safe to log.
#[derive(Debug, thiserror::Error)]
#[error("the cloud transport failed: {0}")]
pub struct CloudHttpError(String);

impl CloudHttpError {
    /// Wraps a reason.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// One response from the store's cloud: the status, the body, and the headers as `(name, value)`
/// with names lowercased.
///
/// Private, because only [`OtaHttpTransport`] needs the headers and it converts straight into
/// `cloud-sync-http`'s own `HttpResponse` — which is the type the artifact parser reads them
/// through.
#[derive(Debug)]
struct CloudResponse {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

/// One HTTPS client to the store's own cloud, reused across calls.
///
/// Cheap to clone — it holds a shared rustls [`ClientConfig`], the parsed base URL, and the bearer
/// key — so the edge builds one and hands a clone to each transport.
#[derive(Debug, Clone)]
pub struct CloudHttpClient {
    base: url::Url,
    bearer: Arc<str>,
    tls: Arc<ClientConfig>,
    timeout: Duration,
}

impl CloudHttpClient {
    /// Builds a client pointed at `base_url` (the store's cloud), presenting `bearer` (the scoped
    /// `read_config` API key) on every request, trusting the bundled Mozilla roots.
    ///
    /// The `ring` crypto provider is selected explicitly, not via the ambient default, so feature
    /// unification can never pull `aws-lc-rs` (ADR-0038).
    ///
    /// # Errors
    ///
    /// [`CloudHttpError`] if `base_url` is not an `https` URL with a host, or if the `ring` provider
    /// cannot supply the safe default TLS versions (a build-time impossibility, a `Result` only to
    /// keep the constructor panic-free per the workspace lints).
    pub fn new(base_url: &url::Url, bearer: impl Into<String>) -> Result<Self, CloudHttpError> {
        if base_url.scheme() != "https" {
            return Err(CloudHttpError::new("the cloud base URL must use https"));
        }
        if base_url.host_str().is_none() {
            return Err(CloudHttpError::new("the cloud base URL has no host"));
        }
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            CloudHttpError::new(format!(
                "the ring provider rejected the safe defaults: {error}"
            ))
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            base: base_url.clone(),
            bearer: Arc::from(bearer.into()),
            tls: Arc::new(tls),
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// The same client with a different per-request timeout.
    ///
    /// One route on the `/sync` surface — the relay pull — is deliberately slow, because the cloud
    /// parks it until an order arrives (ADR-0061). Rather than raise the timeout for every caller,
    /// each transport takes the client it wants: config-pull and heartbeat keep [`REQUEST_TIMEOUT`],
    /// the relay takes [`RELAY_REQUEST_TIMEOUT`].
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The absolute request URL for an origin-rooted `path` (e.g. `/sync/stores/…/config`) and an
    /// optional raw `query` (e.g. `held_version=…`). The base is treated as an origin — its own path,
    /// if any, is replaced — because the cloud's `/sync` routes are rooted at the origin.
    fn target(&self, path: &str, query: Option<&str>) -> url::Url {
        let mut target = self.base.clone();
        target.set_path(path);
        target.set_query(query);
        target
    }

    /// Performs one request under the timeout: resolve → connect → TLS handshake against the hostname
    /// → one HTTP/1.1 request carrying the bearer → read the whole body. Returns the status and body.
    ///
    /// # Errors
    ///
    /// [`CloudHttpError`] for any transport failure (resolution, connection, handshake, timeout, or a
    /// body that could not be read). A non-2xx *response* is not an error here — it comes back in the
    /// status for the caller to map.
    async fn request(
        &self,
        method: &hyper::Method,
        path: &str,
        query: Option<&str>,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), CloudHttpError> {
        let response = self.request_full(method, path, query, body).await?;
        Ok((response.status, response.body))
    }

    /// The same request, keeping the response headers.
    ///
    /// Config-pull, heartbeat and the relay read nothing but the status and the body, so
    /// [`Self::request`] drops the headers for them. The OTA artifact fetch cannot: its signature
    /// travels in `X-Pos-Artifact-Signature` while the body stays the raw binary
    /// ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)), and a fetch that could not read
    /// that header would come back as bytes with nothing to judge them.
    ///
    /// # Errors
    ///
    /// As [`Self::request`].
    async fn request_full(
        &self,
        method: &hyper::Method,
        path: &str,
        query: Option<&str>,
        body: Vec<u8>,
    ) -> Result<CloudResponse, CloudHttpError> {
        match tokio::time::timeout(self.timeout, self.send(method, path, query, body)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(CloudHttpError::new("the request to the cloud timed out")),
        }
    }

    /// The unbounded body of one request; [`Self::request_full`] wraps it in the timeout.
    async fn send(
        &self,
        method: &hyper::Method,
        path: &str,
        query: Option<&str>,
        body: Vec<u8>,
    ) -> Result<CloudResponse, CloudHttpError> {
        let target = self.target(path, query);
        let host = target
            .host_str()
            .ok_or_else(|| CloudHttpError::new("the request URL has no host"))?
            .to_owned();
        let port = target.port_or_known_default().unwrap_or(443);
        let request_target = request_line(&target);

        let stream = Self::connect(&host, port).await?;
        // SNI and certificate verification bind to the hostname, not the dialed address.
        let server_name = ServerName::try_from(host.clone())
            .map_err(|_ignored| CloudHttpError::new("the cloud host is not a valid TLS name"))?;
        let connector = TlsConnector::from(Arc::clone(&self.tls));
        let tls = connector
            .connect(server_name, stream)
            .await
            .map_err(|error| CloudHttpError::new(format!("the TLS handshake failed: {error}")))?;

        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|error| CloudHttpError::new(format!("the HTTP handshake failed: {error}")))?;
        // The connection future must be driven for the request to progress; it ends when the request
        // completes and `sender` drops.
        tokio::spawn(async move {
            let _ignored = connection.await;
        });

        let has_body = !body.is_empty();
        let mut builder = Request::builder()
            .method(method)
            .uri(&request_target)
            .header(HOST, host.as_str())
            .header(AUTHORIZATION, format!("Bearer {}", self.bearer));
        if has_body {
            builder = builder.header(CONTENT_TYPE, "application/json");
        }
        let request = builder
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| {
                CloudHttpError::new(format!("building the request failed: {error}"))
            })?;

        let response = sender
            .send_request(request)
            .await
            .map_err(|error| CloudHttpError::new(format!("sending the request failed: {error}")))?;
        let status = response.status().as_u16();
        // Collected before the body is consumed, because `into_body` takes the response. A value
        // that is not valid UTF-8 is dropped rather than failing the whole response: a header no
        // caller reads must not be able to break a fetch.
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|text| (name.as_str().to_ascii_lowercase(), text.to_owned()))
            })
            .collect();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|error| {
                CloudHttpError::new(format!("reading the response body failed: {error}"))
            })?
            .to_bytes();
        Ok(CloudResponse {
            status,
            body: bytes.to_vec(),
            headers,
        })
    }

    /// Connects to the first resolved address that accepts a TCP connection.
    async fn connect(host: &str, port: u16) -> Result<TcpStream, CloudHttpError> {
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| {
                CloudHttpError::new(format!("resolving the cloud host failed: {error}"))
            })?
            .collect();
        let mut last_error = None;
        for address in &addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(CloudHttpError::new(match last_error {
            Some(error) => format!("connecting to the cloud failed: {error}"),
            None => "the cloud host resolved to no addresses".to_owned(),
        }))
    }
}

/// The origin-form request target for a URL: its path, plus `?query` when there is one. Pure, so the
/// path-and-query the transports build is checked without a socket.
fn request_line(target: &url::Url) -> String {
    match target.query() {
        Some(query) => format!("{}?{}", target.path(), query),
        None => target.path().to_owned(),
    }
}

// -------------------------------------------------------------------------------------------------
// Config-pull transport
// -------------------------------------------------------------------------------------------------

/// The store-facing config-sync response, mirroring `pos_cloud`'s `ConfigSyncResponse` (the edge must
/// not depend on the cloud crate). Only the two shapes the transport acts on are modelled.
#[derive(serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ConfigSyncWire {
    /// The store already holds the current version; apply nothing.
    UpToDate,
    /// The store should apply this update.
    Update {
        /// A full snapshot or a one-version delta.
        update: ConfigUpdateWire,
    },
}

/// The update inside an [`ConfigSyncWire::Update`], mirroring `pos_ports::config_store::ConfigUpdate`
/// (tag `update_kind`). A `Delta` is a patch the edge cannot apply without the prior document (which
/// it does not yet persist — ADR-0085), so its fields are ignored: the transport re-pulls a full
/// snapshot instead.
#[derive(serde::Deserialize)]
#[serde(tag = "update_kind", rename_all = "snake_case")]
enum ConfigUpdateWire {
    /// A complete document at a version — always applicable.
    Snapshot {
        /// The version this document is.
        config_version_id: String,
        /// The full effective config tree (merged Tenant→Brand→Store→Device).
        document: serde_json::Value,
    },
    /// A forward patch from one version to the next — not applied here.
    Delta {},
}

/// What one config pull told the transport to do.
enum ConfigPull {
    /// The store is current; nothing to apply.
    UpToDate,
    /// A full document to apply, at its version.
    Full(SyncedConfig),
    /// The cloud sent a delta; the edge needs a full snapshot instead (re-pull holding nothing).
    NeedFullSnapshot,
}

/// Maps a parsed config-sync response to the action to take. Pure, so the delta/snapshot/up-to-date
/// branching is a socket-free test.
fn interpret(wire: ConfigSyncWire) -> ConfigPull {
    match wire {
        ConfigSyncWire::UpToDate => ConfigPull::UpToDate,
        ConfigSyncWire::Update {
            update:
                ConfigUpdateWire::Snapshot {
                    config_version_id,
                    document,
                },
        } => ConfigPull::Full(SyncedConfig {
            config_version_id,
            document,
        }),
        ConfigSyncWire::Update {
            update: ConfigUpdateWire::Delta {},
        } => ConfigPull::NeedFullSnapshot,
    }
}

/// The field [`ConfigTransport`]: pulls the store's effective config from the cloud over HTTPS
/// ([ADR-0039](../../../docs/adr/0039-config-delivery.md), [ADR-0085](../../../docs/adr/0085-edge-cloud-sync-transport.md)).
///
/// The cloud may answer a pull with a full snapshot or a one-version delta. The edge does not yet
/// persist the last document, so it cannot apply a delta; on one, this re-pulls as a store holding
/// nothing, which the cloud must answer with a full snapshot — correct, at the cost of one extra
/// round-trip on an incremental publish. (Applying deltas in place is the flagged store-sqlite
/// follow-up, ADR-0085.)
#[derive(Debug, Clone)]
pub struct ConfigHttpTransport {
    client: CloudHttpClient,
    store_id: StoreId,
}

impl ConfigHttpTransport {
    /// Builds a config transport over `client` for `store_id`.
    #[must_use]
    pub fn new(client: CloudHttpClient, store_id: StoreId) -> Self {
        Self { client, store_id }
    }

    /// One `GET /sync/stores/{store_id}/config`, reporting the held version if any.
    async fn pull(&self, held_version: Option<&str>) -> Result<ConfigPull, ConfigTransportError> {
        let path = format!("/sync/stores/{}/config", self.store_id);
        let query = held_version.map(|held| format!("held_version={held}"));
        let (status, body) = self
            .client
            .request(&hyper::Method::GET, &path, query.as_deref(), Vec::new())
            .await
            .map_err(|error| ConfigTransportError::new(error.to_string()))?;
        match status {
            200 => {
                let wire = serde_json::from_slice::<ConfigSyncWire>(&body).map_err(|error| {
                    ConfigTransportError::new(format!("the config response did not parse: {error}"))
                })?;
                Ok(interpret(wire))
            }
            // 404 is a store with nothing published yet — no config to apply, not a fault. Treat it as
            // "up to date with nothing" so the loop backs off calmly rather than erroring every pull.
            404 => Ok(ConfigPull::UpToDate),
            other => Err(ConfigTransportError::new(format!(
                "the cloud refused the config pull with status {other}"
            ))),
        }
    }
}

impl ConfigTransport for ConfigHttpTransport {
    async fn fetch(
        &self,
        held_version: Option<&str>,
    ) -> Result<Option<SyncedConfig>, ConfigTransportError> {
        match self.pull(held_version).await? {
            ConfigPull::UpToDate => Ok(None),
            ConfigPull::Full(synced) => Ok(Some(synced)),
            ConfigPull::NeedFullSnapshot => match self.pull(None).await? {
                ConfigPull::Full(synced) => Ok(Some(synced)),
                // Holding nothing, the cloud answers a published store with a snapshot; up-to-date or a
                // second delta would mean the config vanished under us — treat as nothing to apply.
                ConfigPull::UpToDate | ConfigPull::NeedFullSnapshot => Ok(None),
            },
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Heartbeat transport
// -------------------------------------------------------------------------------------------------

/// The field [`HeartbeatTransport`]: POSTs the store's liveness ping over HTTPS
/// ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md), ADR-0085). The cloud advances `last_seen_at`
/// and answers `204`; anything else is a transport failure the loop retries next tick.
///
/// A report with nothing in it is sent as no body at all — the shape the route has always accepted —
/// so a store whose log could not be read keeps pinging exactly as it did before there was a body to
/// send.
#[derive(Debug, Clone)]
pub struct HeartbeatHttpTransport {
    client: CloudHttpClient,
    store_id: StoreId,
}

impl HeartbeatHttpTransport {
    /// Builds a heartbeat transport over `client` for `store_id`.
    #[must_use]
    pub fn new(client: CloudHttpClient, store_id: StoreId) -> Self {
        Self { client, store_id }
    }
}

impl HeartbeatTransport for HeartbeatHttpTransport {
    async fn beat(&self, report: HeartbeatReport) -> Result<(), HeartbeatError> {
        let path = format!("/sync/stores/{}/heartbeat", self.store_id);
        // Only the fields the store actually has an answer for. An empty body is the older edge,
        // and the cloud's route reads it as "nothing to report" rather than as zeros — so omitting a
        // key the box could not answer leaves whatever the cloud last recorded alone (ADR-0068,
        // ADR-0108), which is the difference between "did not say" and "nothing pending".
        let mut fields = serde_json::Map::new();
        if let Some(depth) = report.outbox_depth {
            fields.insert("outbox_depth".to_owned(), depth.into());
        }
        if let Some(generation) = report.lease_generation {
            fields.insert("lease_generation".to_owned(), generation.into());
        }
        // The same rule one level down (ADR-0112): the key is absent when the box did not look, and
        // present-and-empty when it looked and the store has no bound agent — which a manager
        // releasing the last terminal produces, and which the cloud has to record rather than ignore.
        if let Some(agents) = report.print_agents {
            let agents: Vec<serde_json::Value> = agents
                .into_iter()
                .map(|agent| {
                    let mut standing = serde_json::Map::new();
                    standing.insert(
                        "agent_device_id".to_owned(),
                        agent.agent_device_id.to_string().into(),
                    );
                    standing.insert(
                        "paired_device_id".to_owned(),
                        agent.paired_device_id.to_string().into(),
                    );
                    // Absent rather than zero when nothing is waiting, for the reason the field's own
                    // doc gives: an empty queue and a ticket queued this instant are different states.
                    if let Some(secs) = agent.oldest_unacknowledged_secs {
                        standing.insert("oldest_unacknowledged_secs".to_owned(), secs.into());
                    }
                    serde_json::Value::Object(standing)
                })
                .collect();
            fields.insert("print_agents".to_owned(), agents.into());
        }
        let body = if fields.is_empty() {
            Vec::new()
        } else {
            serde_json::to_vec(&serde_json::Value::Object(fields))
                .map_err(|error| HeartbeatError::new(error.to_string()))?
        };
        let (status, _body) = self
            .client
            .request(&hyper::Method::POST, &path, None, body)
            .await
            .map_err(|error| HeartbeatError::new(error.to_string()))?;
        match status {
            204 => Ok(()),
            other => Err(HeartbeatError::new(format!(
                "the cloud refused the heartbeat with status {other}"
            ))),
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Order-relay transport
// -------------------------------------------------------------------------------------------------

/// The field [`RelayTransport`]: pulls the store's parked orders and acks each outcome over HTTPS
/// ([ADR-0061](../../../docs/adr/0061-order-relay.md), [ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)).
///
/// Outbound-only, like every other rail the store runs: the cloud never dials the shop. The pull is a
/// long-poll — the cloud holds it open until an order arrives or its cap elapses — so this transport
/// carries [`RELAY_REQUEST_TIMEOUT`] rather than the client's ordinary one. Both routes require the
/// store key to hold the `relay_orders` scope alongside `read_config`; a key without it is answered
/// `403`, which surfaces here as an ordinary transport failure the loop backs off from (ADR-0087).
#[derive(Debug, Clone)]
pub struct RelayHttpTransport {
    client: CloudHttpClient,
    store_id: StoreId,
}

impl RelayHttpTransport {
    /// Builds a relay transport over `client` for `store_id`, lengthening the client's timeout to
    /// outlast the cloud's park.
    #[must_use]
    pub fn new(client: CloudHttpClient, store_id: StoreId) -> Self {
        Self {
            client: client.with_timeout(RELAY_REQUEST_TIMEOUT),
            store_id,
        }
    }
}

/// The pull route for a store. Pure, so the path the transport builds is checked without a socket.
fn relay_pull_path(store_id: StoreId) -> String {
    format!("/sync/stores/{store_id}/orders")
}

/// The ack route for one pulled order. Pure, for the same reason.
fn relay_ack_path(store_id: StoreId, queued_id: &str) -> String {
    format!("/sync/stores/{store_id}/orders/{queued_id}/ack")
}

impl RelayTransport for RelayHttpTransport {
    async fn pull(&self) -> Result<Vec<PendingOrderDto>, RelayTransportError> {
        let path = relay_pull_path(self.store_id);
        let (status, body) = self
            .client
            .request(&hyper::Method::GET, &path, None, Vec::new())
            .await
            .map_err(|error| RelayTransportError::new(error.to_string()))?;
        match status {
            200 => serde_json::from_slice::<Vec<PendingOrderDto>>(&body).map_err(|error| {
                RelayTransportError::new(format!("the pull response did not parse: {error}"))
            }),
            other => Err(RelayTransportError::new(format!(
                "the cloud refused the order pull with status {other}"
            ))),
        }
    }

    async fn ack(
        &self,
        queued_id: &str,
        outcome: &StoreOutcome,
    ) -> Result<(), RelayTransportError> {
        let path = relay_ack_path(self.store_id, queued_id);
        let body = serde_json::to_vec(outcome).map_err(|error| {
            RelayTransportError::new(format!("the ack body could not be encoded: {error}"))
        })?;
        let (status, _body) = self
            .client
            .request(&hyper::Method::POST, &path, None, body)
            .await
            .map_err(|error| RelayTransportError::new(error.to_string()))?;
        match status {
            // The cloud answers 204 whether or not the row was still pending: acking twice is not an
            // error, which is what makes at-least-once redelivery safe.
            204 => Ok(()),
            other => Err(RelayTransportError::new(format!(
                "the cloud refused the order ack with status {other}"
            ))),
        }
    }
}

// -------------------------------------------------------------------------------------------------
// OTA transport
// -------------------------------------------------------------------------------------------------

/// The bearer-carrying [`HttpTransport`](cloud_sync_http::HttpTransport) the OTA loop's `CloudSync`
/// runs on
/// ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md),
/// [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 1).
///
/// `cloud-sync-http`'s own [`TlsHttpTransport`](cloud_sync_http::TlsHttpTransport) exists for the
/// activation exchange, which is deliberately unauthenticated — a box has no key yet. The artifact
/// fetch and the update report are on `/sync`, which requires the store's scoped key, and this
/// client already attaches it to every request. So the OTA loop composes `HttpCloudSync` over *this*
/// transport rather than that one; nothing else changes about the adapter.
///
/// It is the one transport that needs the response headers, which is why
/// [`CloudHttpClient::request_full`] exists: the artifact's signature rides
/// `X-Pos-Artifact-Signature` and the body stays the raw binary.
#[derive(Debug, Clone)]
pub struct OtaHttpTransport {
    client: CloudHttpClient,
}

/// How long an artifact fetch may take.
///
/// Five minutes rather than the ordinary fifteen seconds: the response body is a whole edge binary,
/// and a store on a slow shop connection would otherwise time out every attempt and never update —
/// while the ordinary timeout stays short for the request-sized routes, which is what keeps a
/// black-hole cloud from wedging them.
pub const ARTIFACT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

impl OtaHttpTransport {
    /// Builds an OTA transport over `client`, lengthening its timeout to allow for a binary-sized
    /// body.
    #[must_use]
    pub fn new(client: CloudHttpClient) -> Self {
        Self {
            client: client.with_timeout(ARTIFACT_REQUEST_TIMEOUT),
        }
    }
}

impl cloud_sync_http::HttpTransport for OtaHttpTransport {
    async fn post_json(
        &self,
        path: &str,
        body: Vec<u8>,
    ) -> Result<cloud_sync_http::HttpResponse, cloud_sync_http::TransportError> {
        let response = self
            .client
            .request_full(&hyper::Method::POST, path, None, body)
            .await
            .map_err(|error| cloud_sync_http::TransportError::new(error.to_string()))?;
        Ok(cloud_sync_http::HttpResponse {
            status: response.status,
            body: response.body,
            headers: response.headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    use super::{
        ARTIFACT_REQUEST_TIMEOUT, CloudHttpClient, ConfigPull, ConfigSyncWire, OtaHttpTransport,
        RELAY_REQUEST_TIMEOUT, REQUEST_TIMEOUT, RelayHttpTransport, interpret, relay_ack_path,
        relay_pull_path, request_line,
    };
    use crate::relay_client::StoreOutcome;

    fn client() -> CloudHttpClient {
        CloudHttpClient::new(
            &url::Url::parse("https://acme.pos.example").expect("a valid base"),
            "pos_key_secret",
        )
        .expect("an https base builds")
    }

    #[test]
    fn a_plain_http_base_is_rejected() {
        let built = CloudHttpClient::new(
            &url::Url::parse("http://cloud.example").expect("parse"),
            "pos_key",
        );
        assert!(built.is_err(), "the cloud base URL must be https");
    }

    #[test]
    fn the_request_target_is_origin_rooted_with_the_query() {
        // The config pull's path and held-version query become the origin-form request line, whatever
        // path the base URL carried (the /sync routes live at the origin).
        let based = CloudHttpClient::new(
            &url::Url::parse("https://acme.pos.example/ignored/base/path").expect("parse"),
            "pos_key",
        )
        .expect("builds");
        let target = based.target("/sync/stores/01STORE/config", Some("held_version=01V"));
        assert_eq!(target.host_str(), Some("acme.pos.example"));
        assert_eq!(
            request_line(&target),
            "/sync/stores/01STORE/config?held_version=01V",
        );
    }

    #[test]
    fn a_request_target_without_a_query_is_just_the_path() {
        let target = client().target("/sync/stores/01STORE/heartbeat", None);
        assert_eq!(request_line(&target), "/sync/stores/01STORE/heartbeat");
    }

    #[test]
    fn an_up_to_date_response_is_nothing_to_apply() {
        let wire =
            serde_json::from_str::<ConfigSyncWire>(r#"{"status":"up_to_date"}"#).expect("parses");
        assert!(matches!(interpret(wire), ConfigPull::UpToDate));
    }

    #[test]
    fn a_snapshot_response_becomes_a_full_document() {
        let body = r#"{
            "status": "update",
            "update": {
                "update_kind": "snapshot",
                "config_version_id": "01VERSION",
                "store_id": "01STORE",
                "document": { "menu": { "channels": {} } }
            }
        }"#;
        let wire = serde_json::from_str::<ConfigSyncWire>(body).expect("parses");
        match interpret(wire) {
            ConfigPull::Full(synced) => {
                assert_eq!(synced.config_version_id, "01VERSION");
                assert!(
                    synced.document.get("menu").is_some(),
                    "the full document is carried"
                );
            }
            _ => panic!("a snapshot must interpret as a full document"),
        }
    }

    #[test]
    fn a_delta_response_asks_for_a_full_snapshot() {
        // The edge cannot apply a delta yet, so a delta must route to a snapshot re-pull, never be
        // mistaken for a full document.
        let body = r#"{
            "status": "update",
            "update": {
                "update_kind": "delta",
                "from_config_version_id": "01FROM",
                "to_config_version_id": "01TO",
                "store_id": "01STORE",
                "patch": { "menu": {} }
            }
        }"#;
        let wire = serde_json::from_str::<ConfigSyncWire>(body).expect("parses");
        assert!(matches!(interpret(wire), ConfigPull::NeedFullSnapshot));
    }

    #[test]
    fn the_relay_routes_are_the_clouds_sync_paths() {
        let store_id = StoreId::new(Ulid::from_u128(1));
        assert_eq!(
            relay_pull_path(store_id),
            format!("/sync/stores/{store_id}/orders"),
        );
        assert_eq!(
            relay_ack_path(store_id, "queued-7"),
            format!("/sync/stores/{store_id}/orders/queued-7/ack"),
        );
    }

    #[test]
    fn the_relay_transport_outlasts_the_clouds_long_poll() {
        // The regression this guards: the ordinary 15s timeout is shorter than the cloud's 20s park,
        // so a relay built on an unmodified client would time out every poll on a quiet store and
        // never see an order (ADR-0087).
        let transport = RelayHttpTransport::new(client(), StoreId::new(Ulid::from_u128(1)));
        assert_eq!(transport.client.timeout, RELAY_REQUEST_TIMEOUT);
        assert!(
            RELAY_REQUEST_TIMEOUT > Duration::from_secs(20),
            "the relay timeout must outlast the cloud's long-poll cap",
        );
        // The other transports are untouched: only the parked route pays for the longer wait.
        assert_eq!(client().timeout, REQUEST_TIMEOUT);
    }

    #[test]
    fn the_ota_transport_allows_for_a_binary_sized_body() {
        // The regression this guards: an edge binary over a shop's uplink does not arrive inside the
        // fifteen seconds a config pull is given, so a fetch on an unmodified client would time out
        // on every attempt and the store would never update — with a timeout, not a refusal, in the
        // log, which reads like a network fault rather than a misconfiguration.
        let transport = OtaHttpTransport::new(client());
        assert_eq!(transport.client.timeout, ARTIFACT_REQUEST_TIMEOUT);
        assert!(ARTIFACT_REQUEST_TIMEOUT > RELAY_REQUEST_TIMEOUT);
        assert_eq!(
            client().timeout,
            REQUEST_TIMEOUT,
            "and nothing else changes"
        );
    }

    #[test]
    fn an_ack_body_is_the_shape_the_cloud_parses() {
        // The cloud reads `StoreOutcome` with `#[serde(tag = "outcome")]`; a refusal must carry its
        // class and reason at the top level, not nested.
        let body = serde_json::to_value(StoreOutcome::Rejected {
            status: "invalid_argument".to_owned(),
            message: "an order must have at least one line".to_owned(),
        })
        .expect("the outcome serialises");
        assert_eq!(body["outcome"], "rejected");
        assert_eq!(body["status"], "invalid_argument");
        assert_eq!(body["message"], "an order must have at least one line");
    }
}
