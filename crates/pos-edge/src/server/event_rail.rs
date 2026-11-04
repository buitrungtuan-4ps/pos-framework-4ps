// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The event rail an activated store publishes its outbox on, and the broker connection it shares
//! with the config wake.

use std::sync::Arc;

use cloud_sync_http::HttpLink;
use link_nats::{NatsConfig as StreamConfig, NatsLink, NatsStoreWake};
use pos_ports::event_store::EventStore;
use pos_ports::key_vault::{KeyVault, SecretName};
use pos_proto::ids::StoreId;
use tokio_rustls::rustls::pki_types::CertificateDer;

use super::{NATS_MAX_BYTES, NATS_MAX_MESSAGES, NATS_URL_ENV, vault_text, wait_for_shutdown};
use crate::app::Edge;
use crate::cloud_http::CloudHttpClient;
use crate::config::NatsConfig;
use crate::event_publish::EventPublisher;

/// Spawns the event-publish loop for an activated store
/// ([ADR-0087](../../../../docs/adr/0087-edge-relay-and-event-publish.md),
/// [ADR-0143](../../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)).
///
/// Two rails, chosen by configuration:
///
/// - **NATS**, when `config.toml` names a `[nats]` stream and subject *and* [`NATS_URL_ENV`] carries
///   the server URL (the one field that would carry a credential, so it stays out of `config.toml`
///   exactly as the sync key does, ADR-0086). This is how a deployment that runs a broker has always
///   published.
/// - **HTTPS**, otherwise: the cloud's own `/sync/stores/{store_id}/events`, presenting the box's
///   device credential. Nothing to configure and no broker port open to the internet, and the cloud
///   refuses a batch that names any store but this one.
///
/// The HTTPS rail presents the device credential even when a store key is set for the `/sync`
/// loops, because only the device credential is certain to carry `publish_events`: a store key
/// issued before that scope existed holds `read_config` and `relay_orders` alone.
///
/// Either way the store trades, and its outbox holds until the rail is up.
pub(super) async fn spawn_event_publish<S, V>(
    edge: &Arc<Edge<S>>,
    rail: Option<NatsRail>,
    cloud: (&url::Url, &[CertificateDer<'static>]),
    vault: &V,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>>
where
    S: EventStore + Send + Sync + 'static,
    V: KeyVault,
{
    if let Some(rail) = rail {
        let handle = spawn_publisher(edge, rail.link, shutdown_rx);
        tracing::info!(
            stream = %rail.stream,
            subject = %rail.subject,
            "event publish enabled over NATS: the outbox is draining to the cloud"
        );
        return Some(handle);
    }
    spawn_https_publish(edge, cloud, vault, shutdown_rx).await
}

/// The store's broker connection: the link the event rail publishes on and the client the config
/// wake listens on ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md):
/// the connection the edge already opens).
pub(super) struct NatsRail {
    link: NatsLink,
    stream: String,
    subject: String,
}

/// Connects the broker when `config.toml` names a `[nats]` stream and [`NATS_URL_ENV`] carries its
/// address; `None`, said once, otherwise, and the store publishes over HTTPS and keeps the poll.
///
/// A broker that is not reachable yet is not a failure here: the link connects in the background,
/// and the publisher reports the store offline until it does. Only an address that cannot be used at
/// all ends up in the error arm.
pub(super) async fn connect_nats(nats: Option<&NatsConfig>) -> Option<NatsRail> {
    let nats_url = std::env::var(NATS_URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty());
    let (nats, url) = match (nats, nats_url) {
        (Some(nats), Some(url)) => (nats, url),
        (Some(_), None) => {
            tracing::info!(
                "a [nats] stream is configured but {NATS_URL_ENV} is unset; publishing events over \
                 HTTPS to the cloud instead, and pulling config on the poll alone"
            );
            return None;
        }
        (None, _) => return None,
    };
    let config = StreamConfig {
        stream: nats.stream.clone(),
        subject: nats.subject.clone(),
        max_messages: NATS_MAX_MESSAGES,
        max_bytes: NATS_MAX_BYTES,
    };
    match NatsLink::connect(url.trim(), config).await {
        Ok(link) => Some(NatsRail {
            link,
            stream: nats.stream.clone(),
            subject: nats.subject.clone(),
        }),
        Err(error) => {
            tracing::warn!(
                %error,
                "the event-stream address in {NATS_URL_ENV} is unusable; falling back to publishing \
                 events over HTTPS to the cloud"
            );
            None
        }
    }
}

/// The config wake on the broker connection, or `None` on a store without one: that store keeps
/// the 30 s poll ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)
/// consequences).
pub(super) async fn config_wake(
    rail: Option<&NatsRail>,
    store_id: StoreId,
) -> Option<NatsStoreWake> {
    let rail = rail?;
    match NatsStoreWake::subscribe(rail.link.client().clone(), store_id).await {
        Ok(wake) => {
            tracing::info!("config wake enabled over NATS: a publish reaches this store at once");
            Some(wake)
        }
        Err(error) => {
            tracing::warn!(%error, "the config wake could not subscribe; pulling config on the poll alone");
            None
        }
    }
}

/// The HTTPS rail, over the box's device credential.
async fn spawn_https_publish<S, V>(
    edge: &Arc<Edge<S>>,
    (cloud_url, roots): (&url::Url, &[CertificateDer<'static>]),
    vault: &V,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>>
where
    S: EventStore + Send + Sync + 'static,
    V: KeyVault,
{
    let Some(credential) = vault_text(vault, SecretName::DeviceCredential).await else {
        tracing::warn!(
            "no device credential could be read from the vault; the outbox is not being published \
             (the store still trades and keeps every event)"
        );
        return None;
    };
    let client = match CloudHttpClient::trusting(cloud_url, credential, roots) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(%error, "the event link could not be built; the outbox holds");
            return None;
        }
    };
    let handle = spawn_publisher(edge, HttpLink::new(client, edge.store_id()), shutdown_rx);
    tracing::info!("event publish enabled over HTTPS: the outbox is draining to the cloud");
    Some(handle)
}

/// Starts the publish loop over `link`.
///
/// The handle is kept, not dropped. A stop asks this loop for one last drain, and a drain nobody
/// waits for is a drain that does not happen: `serve_until` returns as soon as `axum::serve` has
/// finished, and the runtime it returns into stops polling every detached task. On a placement that
/// is torn down rather than restarted, those unpublished events go with the volume
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
fn spawn_publisher<S, L>(
    edge: &Arc<Edge<S>>,
    link: L,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()>
where
    S: EventStore + Send + Sync + 'static,
    L: pos_ports::message_link::MessageLink + 'static,
{
    // The release the store is running, stamped at build time (R1b). Was `CARGO_PKG_VERSION`,
    // which is `0.0.0` in every artifact — so every store told the cloud the same thing and the
    // OTA progress model could not tell one release from another.
    let publisher = EventPublisher::new(Arc::clone(edge), link, crate::version::tag());
    tokio::spawn(publisher.run(wait_for_shutdown(shutdown_rx.clone())))
}
