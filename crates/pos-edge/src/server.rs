// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Binding the listener and serving with graceful shutdown.

use core::time::Duration;
use std::sync::Arc;

use tokio::net::TcpListener;

use cloud_sync_http::{HttpCloudSync, TlsHttpTransport};
use key_vault_keyring::{KeyringVault, OsKeyring};
use link_nats::{NatsConfig as StreamConfig, NatsLink};
use pos_core::activation::ActivationStanding;
use pos_ports::device_registry::DeviceRegistry;
use pos_ports::event_store::EventStore;
use pos_ports::intake_ledger::IntakeLedger;
use pos_ports::key_vault::{KeyVault, SecretName};
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, StoreId};
use pos_proto::text::ReleaseTag;

use crate::activation::{activation_router, boot_standing};
use crate::app::Edge;
use crate::auth::Sessions;
use crate::clock::SystemClock;
use crate::cloud_http::{
    CloudHttpClient, ConfigHttpTransport, HeartbeatHttpTransport, RelayHttpTransport,
};
use crate::config::{EdgeConfig, NatsConfig};
use crate::config_client::ConfigClient;
use crate::discovery::{Advertiser, NoopAdvertiser};
use crate::durable_auth::{DurableAuth, EdgeRegistry};
use crate::error::EdgeError;
use crate::event_publish::EventPublisher;
use crate::heartbeat_client::HeartbeatClient;
use crate::order_in::EdgeOrderIn;
use crate::pairing::{Pairing, pairing_url};
use crate::queue::QueueNumberAuthority;
use crate::relay_client::RelayClient;
use crate::state::AppState;

/// The environment variable carrying the store's scoped `read_config` API key
/// ([ADR-0085](../../../docs/adr/0085-edge-cloud-sync-transport.md)). Supplied by the service unit
/// from a mode-0600 env file — never in `config.toml`, never committed. Absent (or empty) means the
/// edge runs LAN-only and spawns no cloud loops, exactly as an unset `cloud_url` does.
const SYNC_KEY_ENV: &str = "POS_EDGE_SYNC_KEY";

/// The environment variable carrying the event stream's server URL
/// ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)). It lives here rather than in
/// `config.toml` because it is where a credential would be embedded (`nats://user:pass@host`), and a
/// credential never goes in `config.toml` (ADR-0086) — the same mode-0600 env file carries it. Absent
/// (or empty) means the outbox is not published; the store trades and keeps every event.
const NATS_URL_ENV: &str = "POS_EDGE_NATS_URL";

/// The stream limits the edge asks for when it ensures its own stream exists. Matched to the
/// store-box envelope in `docs/capacity-and-reliability.md`: a few days of a busy store's events, so
/// a weekend of cloud downtime drains rather than discards.
const NATS_MAX_MESSAGES: i64 = 1_000_000;

/// The byte ceiling for the same stream — 1 GiB, sized alongside [`NATS_MAX_MESSAGES`].
const NATS_MAX_BYTES: i64 = 1_073_741_824;

/// How often the config-pull loop pulls when nothing is failing. The cloud answers immediately (no
/// server-side long-poll yet, ADR-0039), so this paces the loop; a published change reaches the
/// counter within one interval.
const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How often the heartbeat loop pings the cloud, so a store that is up but not currently pulling
/// still reads as online in the fleet view ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)).
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// How long the activation exchange (`POST /activate`) may take end to end
/// ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)). Generous — activation is a
/// one-time, operator-driven action, not a hot path.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);

/// The box's own device identity, for the events it writes with no human behind them.
///
/// A relayed order came from the cloud, not from a paired till, so the honest answer to "which device
/// did this" is *the store server itself* — and the events an inbound order writes need a
/// [`DeviceId`] because there is no signed-in employee ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
/// Deriving it from the [`StoreId`] makes it **stable across restarts** (so one box's system events all
/// carry one id) and **unique per store** (so two shops never collide in the cloud's own analytics),
/// in the spirit of [`StoreIdentity::for_store`](crate::StoreIdentity::for_store)'s documented bootstrap
/// ids. Carrying the device id the cloud granted at activation is the durable answer and is the flagged
/// follow-up ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)); the cloud's device
/// registry remains the authority on fleet identity either way.
#[must_use]
pub const fn system_device_id(store_id: StoreId) -> DeviceId {
    DeviceId::new(store_id.as_ulid())
}

/// Builds the state and router, binds the configured address, and serves until a shutdown signal.
///
/// The composed [`Edge`] is generic over the store `S`, so the same server runs against `pos-fakes`
/// (the example) and `store-sqlite` (the real edge). The edge's fan-out is shared with the `/ws`
/// route, so a committed change reaches every device.
///
/// Graceful shutdown means an in-flight request finishes before the process exits — what keeps "kill
/// the process mid-sale and lose only the uncommitted transaction"
/// ([`docs/roadmap.md`](../../../docs/roadmap.md) P5) true: a committed sale is durable and an
/// interrupted one was never acknowledged.
///
/// # Errors
///
/// [`EdgeError::Bind`] if the address is unavailable (most often already in use), or
/// [`EdgeError::Serve`] if the server stops with an error after starting.
pub async fn serve<S, Q>(config: EdgeConfig, edge: Arc<Edge<S>>, queue: Q) -> Result<(), EdgeError>
where
    S: EventStore + IntakeLedger + DeviceRegistry + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    // Refuse a configuration that would misbehave rather than starting with it (ADR-0091).
    config.validate()?;
    // Refuse to start if the compiled-in country modules disagree, and log which countries this
    // build can serve (ADR-0027).
    let countries = crate::countries::registry();
    countries.validate().map_err(EdgeError::Country)?;
    tracing::info!(countries = ?countries.country_codes(), "country modules loaded");

    let bind = config.bind;
    let advertised_host = config.advertised_host();
    // The store's cloud and identity, read before `config` moves into the app state: they decide
    // whether the config-pull and heartbeat loops run (ADR-0085).
    let cloud_url = config.cloud_url.clone();
    let store_id = config.store_id;
    let nats = config.nats.clone();
    let idle_timeout = config.sign_in_idle_timeout();
    // Pairing and sign-in are recorded in the store's own database, so a power blip, an OTA install
    // or a `systemctl restart` no longer unpairs every tablet in the shop (ADR-0091). The registry
    // is the edge's existing store, reached through the seam in `crate::durable_auth` — no new
    // parameter, and `main.rs` is unchanged.
    let registry: Arc<dyn DurableAuth> = Arc::new(EdgeRegistry(Arc::clone(&edge)));
    let pairing = Arc::new(Pairing::durable(Arc::clone(&registry)));
    let sessions = Arc::new(Sessions::durable(registry, idle_timeout));

    // Load both tables before the first request can arrive. Fatal if either fails: starting with an
    // empty pairing table would silently unpair a store that *is* paired, and an operator would then
    // re-pair every till to fix a problem that was never theirs.
    let restored_devices = pairing.load().await.map_err(EdgeError::DeviceRegistry)?;
    let restored_sessions = sessions
        .load(SystemClock.now())
        .await
        .map_err(EdgeError::DeviceRegistry)?;
    tracing::info!(
        devices = restored_devices,
        sign_ins = restored_sessions,
        idle_timeout_minutes = idle_timeout.as_secs() / 60,
        "restored device pairings and sign-ins from the store"
    );

    // Share the edge's fan-out with the /ws route so a committed change reaches every device, and
    // the loaded pairing state so `/api/pair` and the gates agree.
    let state =
        AppState::with_fanout(config, edge.fanout().clone()).with_pairing(Arc::clone(&pairing));

    // Mint a pairing code and show the operator how to reach the edge (ADR-0030). The code is a
    // secret and is not logged on its own; it appears only inside the pairing URL an operator scans.
    match state.pairing.mint(state.clock.now()) {
        Ok(code) => {
            if let Some(host) = advertised_host {
                tracing::info!(
                    pairing_url = %pairing_url(host, bind.port(), &code),
                    "scan or type this to pair a device",
                );
            } else {
                tracing::warn!(
                    "a device pairs at http://<edge-ip>:{}/pair?code={} — set advertised_ip or read the LAN IP off this machine",
                    bind.port(),
                    code.as_str(),
                );
            }
        }
        Err(_) => {
            tracing::error!("could not mint a pairing code: the OS entropy source is unavailable");
        }
    }

    // mDNS is a convenience behind the Advertiser trait; the default advertises nothing and the
    // raw-IP pairing URL above still works (ADR-0030).
    NoopAdvertiser.advertise("pos", bind.port());

    // The domain routes share the same pairing state the infra router serves, so the device-token
    // check (ADR-0084) validates tokens against the very set `/api/pair` issues them into. The config
    // loop keeps its own handle on the edge, so a menu published from the cloud hot-swaps the live
    // session the same routes serve.
    let config_edge = Arc::clone(&edge);
    let mut app = crate::http::router(state).merge(crate::http::domain_router(
        edge,
        Arc::clone(&pairing),
        sessions,
    ));

    // One shutdown signal, fanned to the server and every background loop so a Ctrl-C / SIGTERM drains
    // them together. A task translates the OS signal into a watched flag; each consumer waits on its
    // own clone.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ignored = shutdown_tx.send(true);
    });

    // Compose the cloud surface when the store is provisioned for a cloud (ADR-0086). A `cloud_url`
    // means: mount the activation routes so a fresh box can be set up at `/setup`, and — once the box
    // is activated (a device credential is in the OS keyring) — start the config-pull and heartbeat
    // loops. An unactivated or LAN-only box still binds, pairs, and serves the counter offline
    // (ADR-0001); the gate withholds cloud sync, never local trading.
    if let Some(cloud_url) = cloud_url {
        app = compose_cloud_surface(
            app,
            &cloud_url,
            store_id,
            &config_edge,
            queue,
            nats.as_ref(),
            &shutdown_rx,
        )
        .await;
    } else {
        tracing::info!("no cloud_url set; running LAN-only (no activation or cloud sync)");
    }

    let listener = TcpListener::bind(bind)
        .await
        .map_err(|source| EdgeError::Bind { addr: bind, source })?;
    tracing::info!(
        %bind,
        protocol_version = pos_proto::PROTOCOL_VERSION,
        "pos_edge listening",
    );

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx))
        .await
        .map_err(EdgeError::Serve)?;

    tracing::info!("pos_edge stopped");
    Ok(())
}

/// Composes the cloud surface onto `app` for a store that has a `cloud_url` (ADR-0086): the OS-keyring
/// vault and a `cloud-sync-http` `CloudSync`, the activation routes (`POST /api/activate`,
/// `GET /api/activation`) so a fresh box can be set up, and — when the box is already activated — the
/// config-pull and heartbeat loops. Returns the router with the activation routes merged (or `app`
/// unchanged if the cloud transport could not be built). Never blocks local trading: an unactivated
/// box still binds, pairs, and serves the LAN, and the operator UI routes it to `/setup`.
async fn compose_cloud_surface<S, Q>(
    app: axum::Router,
    cloud_url: &url::Url,
    store_id: StoreId,
    edge: &Arc<Edge<S>>,
    queue: Q,
    nats: Option<&NatsConfig>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> axum::Router
where
    S: EventStore + IntakeLedger + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    // The device credential (activation) and the scoped sync key both live in the OS keyring (ADR-0086).
    let vault = Arc::new(KeyringVault::new(OsKeyring::new()));
    // One HTTPS transport for the activation exchange; the config-pull/heartbeat loops dial over their
    // own bearer-carrying client (cloud_http), keyed by the scoped sync key rather than the device
    // credential (the `/sync` routes verify the former today, ADR-0085/0086).
    let transport = match TlsHttpTransport::new(cloud_url.as_str(), ACTIVATION_TIMEOUT) {
        Ok(transport) => transport,
        Err(error) => {
            tracing::error!(
                %error,
                "cloud_url is set but the cloud transport could not be built; running LAN-only"
            );
            return app;
        }
    };
    let cloud = Arc::new(HttpCloudSync::new(transport));
    let app = app.merge(activation_router(
        Arc::clone(edge),
        cloud,
        Arc::clone(&vault),
    ));

    // Boot gate: start the cloud loops only once the box holds a device credential (ADR-0086). Reading
    // the vault can itself fail (a locked or absent keyring) — that is not "unactivated", so it is
    // logged and the box runs without cloud sync rather than refusing to start.
    match boot_standing(&*vault).await {
        Ok(ActivationStanding::Activated) => {
            let sync_key = resolve_sync_key(&*vault).await;
            spawn_cloud_loops(cloud_url, store_id, edge, queue, sync_key, shutdown_rx);
            // The event stream is a second rail with its own endpoint and its own credential, so it
            // is spawned beside the `/sync` loops rather than inside them: a store with no sync key
            // still ships the events it has committed (ADR-0087).
            spawn_event_publish(edge, nats, shutdown_rx).await;
        }
        Ok(ActivationStanding::NeedsActivation) => {
            tracing::info!(
                "device not activated: open /setup to enter the activation code (no cloud sync until then)"
            );
        }
        Err(error) => {
            tracing::warn!(%error, "could not read the vault to check activation; running without cloud sync");
        }
    }
    app
}

/// The scoped `read_config` key the config-pull and heartbeat loops present: the vault first
/// (ADR-0086), then `POS_EDGE_SYNC_KEY` from the environment as a headless bring-up override
/// (ADR-0085). A stored value that is not valid UTF-8, or is blank, is treated as absent.
async fn resolve_sync_key<V: KeyVault>(vault: &V) -> Option<String> {
    if let Ok(Some(secret)) = vault.load(SecretName::SyncKey).await
        && let Ok(text) = core::str::from_utf8(secret.expose())
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    std::env::var(SYNC_KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty())
}

/// Spawns the config-pull, heartbeat, and order-relay loops for an activated store, keyed by
/// `sync_key`. A `None` key (neither in the vault nor the environment) is logged and skipped — the box
/// is activated but has nothing to authenticate the `/sync` surface with, so it trades locally and
/// awaits a provisioned key. The loops share the passed shutdown, so they drain with the server.
fn spawn_cloud_loops<S, Q>(
    cloud_url: &url::Url,
    store_id: StoreId,
    edge: &Arc<Edge<S>>,
    queue: Q,
    sync_key: Option<String>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) where
    S: EventStore + IntakeLedger + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let Some(sync_key) = sync_key else {
        tracing::warn!(
            "the store is activated but no scoped sync key is set (vault or {SYNC_KEY_ENV}); config-pull and heartbeat are idle"
        );
        return;
    };
    let client = match CloudHttpClient::new(cloud_url, sync_key) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(
                %error,
                "cloud_url is set but the sync client could not be built; running without cloud sync"
            );
            return;
        }
    };

    let config_client = ConfigClient::new(
        ConfigHttpTransport::new(client.clone(), store_id),
        Arc::clone(edge),
    );
    tokio::spawn(config_client.run(CONFIG_POLL_INTERVAL, wait_for_shutdown(shutdown_rx.clone())));

    let heartbeat_client = HeartbeatClient::new(
        HeartbeatHttpTransport::new(client.clone(), store_id),
        HEARTBEAT_INTERVAL,
    );
    tokio::spawn(heartbeat_client.run(wait_for_shutdown(shutdown_rx.clone())));

    // The order relay (ADR-0061, ADR-0087): pull the store's parked orders, make each one through the
    // edge's own `OrderIn` — repriced from this store's menu, deduped in the order's transaction — and
    // ack the outcome. The same scoped key as the loops above, which must also carry `relay_orders`.
    // The relay paces itself on the cloud's long-poll, so it takes no interval of its own.
    let relay_client = RelayClient::new(
        RelayHttpTransport::new(client, store_id),
        EdgeOrderIn::new(Arc::clone(edge), queue, system_device_id(store_id)),
    );
    tokio::spawn(relay_client.run(wait_for_shutdown(shutdown_rx.clone())));

    tracing::info!(
        %cloud_url,
        "cloud sync enabled: config-pull, heartbeat, and order-relay loops running"
    );
}

/// Spawns the event-publish loop for an activated store, when a stream is configured
/// ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)).
///
/// Needs two things the operator supplies separately: the `[nats]` stream and subject from
/// `config.toml` (not secrets, and they must match the cloud consumer's `stream`/`filter_subject`),
/// and the server URL from [`NATS_URL_ENV`] — the one field that would carry a credential, so it
/// stays out of `config.toml` exactly as the sync key does (ADR-0086). Either missing is logged and
/// skipped: the store trades, and its outbox holds until a stream exists.
async fn spawn_event_publish<S>(
    edge: &Arc<Edge<S>>,
    nats: Option<&NatsConfig>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) where
    S: EventStore + Send + Sync + 'static,
{
    let Some(nats) = nats else {
        tracing::info!(
            "no [nats] section; the outbox is not being published (the store still trades and keeps every event)"
        );
        return;
    };
    let Some(url) = std::env::var(NATS_URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty())
    else {
        tracing::warn!(
            "a [nats] stream is configured but {NATS_URL_ENV} is unset; the outbox is not being published"
        );
        return;
    };

    let config = StreamConfig {
        stream: nats.stream.clone(),
        subject: nats.subject.clone(),
        max_messages: NATS_MAX_MESSAGES,
        max_bytes: NATS_MAX_BYTES,
    };
    // Connecting is I/O and can fail on a box whose network is not up yet. That is not fatal: the
    // events are durable in the outbox, so the box trades and the next boot (or the next slice's
    // reconnect) picks them up.
    let link = match NatsLink::connect(url.trim(), config).await {
        Ok(link) => link,
        Err(error) => {
            tracing::warn!(
                %error,
                "could not connect to the event stream; the outbox holds and the store keeps trading"
            );
            return;
        }
    };

    let publisher = EventPublisher::new(
        Arc::clone(edge),
        link,
        ReleaseTag::new(env!("CARGO_PKG_VERSION")),
    );
    tokio::spawn(publisher.run(wait_for_shutdown(shutdown_rx.clone())));
    tracing::info!(
        stream = %nats.stream,
        subject = %nats.subject,
        "event publish enabled: the outbox is draining to the cloud"
    );
}

/// Resolves when the shutdown flag flips true (or its sender drops) — one per background consumer.
async fn wait_for_shutdown(mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let _ignored = shutdown_rx.wait_for(|stopped| *stopped).await;
}

/// Resolves when the process is asked to stop: Ctrl-C anywhere, or `SIGTERM` on Unix (what systemd
/// and `docker stop` send).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            // If the handler cannot be installed, there is simply no SIGTERM path; Ctrl-C still works.
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received; draining in-flight requests");
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    use super::system_device_id;

    #[test]
    fn the_system_device_id_is_stable_for_a_store() {
        // One box, one identity: every system event this store writes carries the same device id, so
        // the cloud can group them however many times the box restarts.
        let store_id = StoreId::new(Ulid::from_u128(7));
        assert_eq!(system_device_id(store_id), system_device_id(store_id));
    }

    #[test]
    fn two_stores_never_share_a_system_device_id() {
        // The reason it is derived from the store rather than a fixed sentinel: two shops must not
        // collide in the cloud's own analytics.
        let one = system_device_id(StoreId::new(Ulid::from_u128(7)));
        let other = system_device_id(StoreId::new(Ulid::from_u128(8)));
        assert_ne!(one, other);
    }
}
