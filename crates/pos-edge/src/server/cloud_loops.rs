// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/sync` loops an activated store runs: the config pull, the heartbeat, the order relay and
//! the archive, over one keyed client, with the server's shutdown.

use std::sync::Arc;

use cloud_sync_http::HttpCloudSync;
use pos_ports::config_store::ConfigStore;
use pos_ports::event_store::EventStore;
use pos_ports::intake_ledger::IntakeLedger;
use pos_proto::ids::StoreId;

use super::{
    BackupPlan, CONFIG_POLL_INTERVAL, CloudWiring, HEARTBEAT_INTERVAL, SYNC_KEY_ENV,
    system_device_id, wait_for_shutdown,
};
use crate::app::Edge;
use crate::cloud_http::{
    CloudHttpClient, ConfigHttpTransport, HeartbeatHttpTransport, OtaHttpTransport,
    RelayHttpTransport,
};
use crate::config_client::ConfigClient;
use crate::device_revocations::DeviceRevocations;
use crate::heartbeat_client::{HeartbeatClient, StoreReport};
use crate::lease_state::{LeaseAuthority, LeaseWatch};
use crate::order_in::EdgeOrderIn;
use crate::queue::QueueNumberAuthority;
use crate::relay_client::RelayClient;

/// What [`spawn_cloud_loops`] hands back when it starts: the keyed client the OTA loop dials on, and
/// the heartbeat task a stop has to release and then wait for.
pub(super) struct CloudLoops {
    pub(super) client: CloudHttpClient,
    pub(super) heartbeat: tokio::task::JoinHandle<()>,
}

/// Spawns the config-pull, heartbeat, and order-relay loops for an activated store, keyed by
/// `sync_key` ([`resolve_sync_key`]). A `None` key means not even the device credential could be read
/// back: that is logged and skipped, and the box trades locally. The loops share the passed
/// shutdown, so they drain with the server.
///
/// `drained` is the one exception to sharing that shutdown: the heartbeat loop stops *ticking* with
/// the others and then waits for this before sending its last report, so the report carries the
/// outbox as the final drain left it rather than as the stop found it ([`FarewellBeat`]). It is a
/// parameter precisely because the heartbeat loop must not decide for itself when a stopping store
/// has finished draining — the publish loop it has no handle on decides that.
pub(super) fn spawn_cloud_loops<S, Q, L, P>(
    wiring: CloudWiring<'_, S, Q, L, P>,
    sync_key: Option<String>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
    drained: tokio::sync::oneshot::Receiver<()>,
) -> Option<CloudLoops>
where
    S: EventStore + IntakeLedger + ConfigStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
    L: LeaseAuthority + 'static,
    P: crate::print_agent::PrintAgents + 'static,
{
    let CloudWiring {
        cloud_url,
        store_id,
        edge,
        queue,
        lease,
        lease_watch,
        agents,
        held_config_version,
        origins,
        revocations,
        backup,
    } = wiring;
    let client = sync_client(cloud_url, sync_key)?;
    let config_client = config_client(
        &client,
        store_id,
        edge,
        held_config_version,
        origins,
        revocations,
        lease_watch,
    );
    tokio::spawn(config_client.run(CONFIG_POLL_INTERVAL, wait_for_shutdown(shutdown_rx.clone())));
    let heartbeat = spawn_heartbeat(&client, store_id, edge, lease, agents, shutdown_rx, drained);
    spawn_relay(&client, store_id, edge, queue, shutdown_rx);
    if let Some(plan) = backup {
        spawn_archiver(&client, store_id, edge, plan, shutdown_rx);
    }

    tracing::info!(
        %cloud_url,
        "cloud sync enabled: config-pull, heartbeat, and order-relay loops running"
    );
    // Handed back for the OTA loop, which `serve` composes because it needs the shutdown sender, and
    // for the stop, which has to release the heartbeat's last beat once the drain is done.
    Some(CloudLoops { client, heartbeat })
}

/// The keyed client the `/sync` loops dial on, or `None`, said once, when there is no key to key it
/// with or it cannot be built.
fn sync_client(cloud_url: &url::Url, sync_key: Option<String>) -> Option<CloudHttpClient> {
    let Some(sync_key) = sync_key else {
        tracing::warn!(
            "the store is activated but neither a sync key (vault or {SYNC_KEY_ENV}) nor its device \
             credential could be read; config-pull and heartbeat are idle"
        );
        return None;
    };
    match CloudHttpClient::new(cloud_url, sync_key) {
        Ok(client) => Some(client),
        Err(error) => {
            tracing::error!(
                %error,
                "cloud_url is set but the sync client could not be built; running without cloud sync"
            );
            None
        }
    }
}

/// The config-pull loop's client: seeded with what the boot restore found on disk, so the first
/// pull asks the cloud for a *change* rather than re-fetching the document the counter is already
/// selling on (C1).
fn config_client<S>(
    client: &CloudHttpClient,
    store_id: StoreId,
    edge: &Arc<Edge<S>>,
    held_config_version: Option<String>,
    origins: &Arc<crate::origins::Origins>,
    revocations: &Arc<DeviceRevocations>,
    lease_watch: Arc<LeaseWatch>,
) -> ConfigClient<ConfigHttpTransport, S>
where
    S: EventStore + ConfigStore + Send + Sync + 'static,
{
    ConfigClient::new(
        ConfigHttpTransport::new(client.clone(), store_id),
        Arc::clone(edge),
        held_config_version,
    )
    // The same list the CORS layer reads on every request, handed to the only writer there is: a
    // published `origins` node reaches the layer through this handle and nowhere else (ADR-0111).
    .with_origins(Arc::clone(origins))
    // And the same pairing state the request gates answer from, so a `revoked_devices` node retires
    // a till on this box rather than only in the console's picture of it (ADR-0118 §6).
    .with_device_revocations(Arc::clone(revocations))
    // …and this box's own lease, so a pull that carries a newer generation makes the box stop
    // seating tables rather than only stop installing updates (ADR-0123). The same `Arc` the
    // application layer refuses on and the response header reports.
    .with_lease(lease_watch)
}

/// The heartbeat reports the store's own publish backlog alongside its liveness, so the fleet
/// console can see a box whose events are piling up behind a down link (ADR-0068) — the lease
/// generation it holds, so a box that has been *replaced* is distinguishable from one that is
/// merely quiet (ADR-0108) — and one standing per print agent, so a ticket stuck behind a terminal
/// reaches an operator before the night ends rather than in a phone call the next morning
/// (ADR-0112).
fn spawn_heartbeat<S, L, P>(
    client: &CloudHttpClient,
    store_id: StoreId,
    edge: &Arc<Edge<S>>,
    lease: L,
    agents: P,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
    drained: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()>
where
    S: EventStore + IntakeLedger + ConfigStore + Send + Sync + 'static,
    L: LeaseAuthority + 'static,
    P: crate::print_agent::PrintAgents + 'static,
{
    let heartbeat_client = HeartbeatClient::reporting(
        HeartbeatHttpTransport::new(client.clone(), store_id),
        StoreReport::new(Arc::clone(edge), lease, agents),
        HEARTBEAT_INTERVAL,
    );
    // A dropped sender resolves the receiver too, so a caller that never signals still gets the last
    // beat rather than a task parked forever — the beat simply reports whatever the outbox holds.
    tokio::spawn(
        heartbeat_client.run(wait_for_shutdown(shutdown_rx.clone()), async move {
            let _ignored = drained.await;
        }),
    )
}

/// The order relay (ADR-0061, ADR-0087): pull the store's parked orders, make each one through the
/// edge's own `OrderIn` — repriced from this store's menu, deduped in the order's transaction — and
/// ack the outcome. The same scoped key as the other loops, which must also carry `relay_orders`.
/// The relay paces itself on the cloud's long-poll, so it takes no interval of its own.
fn spawn_relay<S, Q>(
    client: &CloudHttpClient,
    store_id: StoreId,
    edge: &Arc<Edge<S>>,
    queue: Q,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) where
    S: EventStore + IntakeLedger + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let relay_client = RelayClient::new(
        RelayHttpTransport::new(client.clone(), store_id),
        EdgeOrderIn::new(Arc::clone(edge), queue, system_device_id(store_id)),
    );
    tokio::spawn(relay_client.run(wait_for_shutdown(shutdown_rx.clone())));
}

/// The store's own off-box backup (ADR-0124): a `VACUUM INTO` snapshot, sealed at the till under
/// a key the cloud hands out but cannot use, shipped to the cloud as ciphertext. It dials the
/// same keyed client as the OTA loop — `/sync/stores/{id}/archive` is behind the store's scoped
/// key, and the archive key is minted on the first ask.
///
/// Deliberately *not* gated on this box's lease standing (ADR-0123): a superseded box holds
/// exactly the events its replacement does not, so it is the box whose database most needs
/// saving. See `crate::backup_client` for the rest of that argument.
///
/// It follows the store's `backup` node, whose interval wins from the next archive on; this box's
/// own interval, from its file or the default, applies while the node sets none (ADR-0160
/// decision 6). The loop says which at start-up and whenever it changes.
fn spawn_archiver<S>(
    client: &CloudHttpClient,
    store_id: StoreId,
    edge: &Arc<Edge<S>>,
    plan: BackupPlan,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) where
    S: EventStore + ConfigStore + Send + Sync + 'static,
{
    let archiver = crate::backup_client::BackupClient::new(
        HttpCloudSync::new(
            OtaHttpTransport::new(client.clone()),
            store_id,
            crate::version::target(),
        ),
        store_id,
        plan.database,
        plan.interval,
    )
    .following(Arc::<Edge<S>>::clone(edge), plan.source);
    tokio::spawn(archiver.run(wait_for_shutdown(shutdown_rx.clone())));
    tracing::info!("store archiving enabled");
}
