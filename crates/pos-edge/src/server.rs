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
use pos_ports::config_store::ConfigStore;
use pos_ports::device_registry::DeviceRegistry;
use pos_ports::event_store::EventStore;
use pos_ports::intake_ledger::IntakeLedger;
use pos_ports::key_vault::{KeyVault, SecretName};
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, StoreId};
use updater_minisign::MinisignVerifier;

use crate::activation::{activation_router, boot_standing};
use crate::app::Edge;
use crate::auth::Sessions;
use crate::clock::SystemClock;
use crate::cloud_http::{
    CloudHttpClient, ConfigHttpTransport, HeartbeatHttpTransport, OtaHttpTransport,
    RelayHttpTransport,
};
use crate::config::{EdgeConfig, NatsConfig};
use crate::config_client::{ConfigClient, restore_session_from_store};
use crate::discovery::{Advertiser, NoopAdvertiser};
use crate::durable_auth::{DurableAuth, EdgeRegistry};
use crate::error::EdgeError;
use crate::event_publish::EventPublisher;
use crate::heartbeat_client::HeartbeatClient;
use crate::installer::SystemdInstaller;
use crate::order_in::EdgeOrderIn;
use crate::ota_client::{BootStanding, OtaClient, RestartIntent};
use crate::ota_state::OtaStateAuthority;
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
/// `config.toml` because it is where a credential would be embedded (`tls://:<token>@host:4222`), and
/// a credential never goes in `config.toml` (ADR-0086) — the same mode-0600 env file carries it.
/// Absent (or empty) means the outbox is not published; the store trades and keeps every event.
///
/// The console cannot fill this in: the broker token is one **fleet-wide** secret held on the cloud
/// box, unlike the per-store sync key the new-store wizard does emit, so the generated `env` carries
/// the line commented and the operator completes it (ADR-0087 Amendment 1).
const NATS_URL_ENV: &str = "POS_EDGE_NATS_URL";

/// The stream limits the edge asks for when it ensures the fleet's stream exists. Matched to the
/// store-box envelope in `docs/capacity-and-reliability.md`: a few days of a busy store's events, so
/// a weekend of cloud downtime drains rather than discards.
///
/// **This is a fleet ceiling, not a per-store one** (ADR-0087 Amendment 1): every store publishes
/// into `POS_FLEET`, so the fill rate is the whole estate's. Reaching it refuses new events rather
/// than dropping old ones, and the outbox holds — visible as the 80% capacity alert (ADR-0073), not
/// as loss. Sizing these two against a real estate is the A·P4 **O4** capacity probe.
const NATS_MAX_MESSAGES: i64 = 1_000_000;

/// The byte ceiling for the same stream — 1 GiB, sized alongside [`NATS_MAX_MESSAGES`], and a fleet
/// ceiling for the same reason.
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

/// Why the store stopped serving — and therefore whether the operating system should start it again.
///
/// Every stop drains the same way, so the *manner* of stopping carries no information; the reason
/// does. On `systemd` the distinction is invisible, because `Restart=always` starts the binary again
/// whatever the exit code was. On Windows it is the whole difference between a store that comes back
/// after an update and one that sits dark (roadmap v3 **E4**), which is why this is a return value
/// rather than a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    /// An operator, a `SIGTERM`, or a machine shutdown asked the store to stop. Leave it stopped.
    Stopped,
    /// The binary on disk is no longer the one this process is running — an update was installed, or
    /// a version that never booted healthy was reverted — so the process ended in order to be
    /// started again. Nothing is wrong; the store is waiting to come back.
    RestartWanted,
}

/// Builds the state and router, binds the configured address, and serves until an operating-system
/// signal (`SIGTERM`, or Ctrl-C) or an installed update asks it to stop.
///
/// For a caller that supplies its own stop — a Windows service wrapper, whose stop arrives from the
/// Service Control Manager rather than as a signal — see [`serve_until`].
///
/// # Errors
///
/// As [`serve_until`].
pub async fn serve<S, Q, A>(
    config: EdgeConfig,
    edge: Arc<Edge<S>>,
    queue: Q,
    ota_authority: A,
) -> Result<ServeOutcome, EdgeError>
where
    S: EventStore + IntakeLedger + DeviceRegistry + ConfigStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
    A: OtaStateAuthority + 'static,
{
    serve_until(config, edge, queue, ota_authority, shutdown_signal()).await
}

/// As [`serve`], but the caller says what a stop is.
///
/// The composed [`Edge`] is generic over the store `S`, so the same server runs against `pos-fakes`
/// (the example) and `store-sqlite` (the real edge). The edge's fan-out is shared with the `/ws`
/// route, so a committed change reaches every device.
///
/// Graceful shutdown means an in-flight request finishes before the process exits — what keeps "kill
/// the process mid-sale and lose only the uncommitted transaction"
/// ([`docs/roadmap.md`](../../../docs/roadmap.md) P5) true: a committed sale is durable and an
/// interrupted one was never acknowledged. `stop` resolving is what starts that drain; so is the OTA
/// loop after it has installed a release, and the two are told apart by the returned
/// [`ServeOutcome`].
///
/// # Errors
///
/// [`EdgeError::Bind`] if the address is unavailable (most often already in use), or
/// [`EdgeError::Serve`] if the server stops with an error after starting.
pub async fn serve_until<S, Q, A, F>(
    config: EdgeConfig,
    edge: Arc<Edge<S>>,
    queue: Q,
    ota_authority: A,
    stop: F,
) -> Result<ServeOutcome, EdgeError>
where
    S: EventStore + IntakeLedger + DeviceRegistry + ConfigStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
    A: OtaStateAuthority + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    // Read what binding and the startup banner need before `config` moves into the composition.
    let bind = config.bind;
    let advertised_host = config.advertised_host();
    let store_id = config.store_id;

    // The install seam, when this box is laid out for over-the-air updates (ADR-0055 Amendment 1).
    // Settling the boot marker comes *first*, before anything is composed or bound: if a committed
    // version has now used up its attempts, the seam has already pointed the running binary back at
    // the previous one, and the only correct next step is to exit so the service manager starts it.
    let installer = ota_installer(&config);
    let boot = match settle_boot(installer.as_ref()) {
        Ok(standing) => standing,
        // The seam has already pointed `current` back at the version that worked, so the binary on
        // disk is not this one: the process must end *and be started again*, which is what a
        // reverted box coming back trading depends on.
        Err(BootGaveUp) => return Ok(ServeOutcome::RestartWanted),
    };

    // One shutdown signal, fanned to the server and every background loop so a Ctrl-C / SIGTERM
    // drains them together. A task translates the OS signal into a watched flag; each consumer waits
    // on its own clone. It is created here rather than in `compose` because installing an OS signal
    // handler is a property of *running*, not of composing — a test composes without one.
    //
    // Behind an `Arc` because the OTA loop holds it too: an installed binary only becomes the
    // running one after a restart, and asking for it through this channel means the restart drains
    // in-flight requests exactly as a `SIGTERM` does rather than dropping a sale.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_tx = Arc::new(shutdown_tx);
    // The OTA loop's handle on that shutdown, which also records *why* the stop happened. The stop
    // the caller passed is not a restart, so it flips the watch directly and leaves the intent
    // alone: a store told to stop must stay stopped.
    let restart = RestartIntent::new(Arc::clone(&shutdown_tx));
    let signal_tx = Arc::clone(&shutdown_tx);
    tokio::spawn(async move {
        stop.await;
        let _ignored = signal_tx.send(true);
    });

    let ota_edge = Arc::clone(&edge);
    let composed = compose(config, edge, queue, &shutdown_rx).await?;

    // The OTA loop, once there is a keyed cloud client to fetch a release through and a box laid out
    // to install one into. Either missing is an ordinary state, not a fault: a LAN-only store and a
    // store provisioned before the layout existed both simply do not update over the air.
    spawn_ota(
        OtaWiring {
            client: composed.cloud.clone(),
            installer,
            authority: ota_authority,
            edge: ota_edge,
            store_id,
            boot,
            restart: restart.clone(),
        },
        &shutdown_rx,
    );

    // Mint a pairing code and show the operator how to reach the edge (ADR-0030). The code is a
    // secret and is not logged on its own; it appears only inside the pairing URL an operator scans.
    match composed.pairing.mint(SystemClock.now()) {
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

    let listener = TcpListener::bind(bind)
        .await
        .map_err(|source| EdgeError::Bind { addr: bind, source })?;
    tracing::info!(
        %bind,
        protocol_version = pos_proto::PROTOCOL_VERSION,
        "pos_edge listening",
    );

    axum::serve(listener, composed.app.into_make_service())
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx))
        .await
        .map_err(EdgeError::Serve)?;

    let outcome = if restart.wanted() {
        ServeOutcome::RestartWanted
    } else {
        ServeOutcome::Stopped
    };
    tracing::info!(?outcome, "pos_edge stopped");
    Ok(outcome)
}

/// Everything [`serve`] assembles before it binds a socket: the router the shipped binary serves,
/// and the two auth tables it loaded from the store.
///
/// # Why this is a separate function
///
/// So that *composition* is testable, not only the pieces. A test can build a router by hand —
/// `http::domain_router(edge, pairing, sessions)` — and pass while `serve` mounts something
/// different, or nothing at all. That is not hypothetical: roadmap v3 records **seven** slices whose
/// code was written, unit-tested and unreachable from the running binary, and an eighth found since.
/// The acceptance suite (roadmap v3 **Q1**, `tests/acceptance.rs`) drives the router *this* function
/// returns, so a route or a gate `serve` fails to mount fails a test rather than a store.
#[derive(Debug)]
#[non_exhaustive]
pub struct Composed {
    /// The router the shipped binary serves — infra routes, domain routes, and the cloud surface
    /// when the store is provisioned for one.
    pub app: axum::Router,
    /// The device pairing table, already refilled from the store (ADR-0091).
    pub pairing: Arc<Pairing>,
    /// The signed-in bindings, already refilled from the store (ADR-0091).
    pub sessions: Arc<Sessions>,
    /// The keyed HTTPS client the `/sync` loops run on, when this store is activated and has a
    /// scoped key — `None` for a LAN-only, unactivated or unkeyed box.
    ///
    /// Handed back rather than kept private because the OTA loop is composed in [`serve`], not
    /// here: it needs the shutdown *sender* to ask for the restart that makes an installed binary
    /// the running one ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1), and
    /// installing a signal handler — hence owning that sender — is a property of running rather
    /// than of composing. A test that composes gets `None` and no OTA loop, which is correct: it
    /// has no cloud to fetch a release from.
    pub cloud: Option<CloudHttpClient>,
}

/// Builds the state, the router and the background loops — everything [`serve`] does except
/// installing a signal handler, printing the pairing banner and binding the socket.
///
/// Callers pass their own `shutdown_rx` so the loops this spawns drain with whatever owns the
/// lifetime: `serve` passes the one fed by the OS signal, and a test passes a channel it never
/// fires.
///
/// # Errors
///
/// [`EdgeError::Config`] if the configuration would misbehave, [`EdgeError::Country`] if the
/// compiled-in country modules disagree, or [`EdgeError::DeviceRegistry`] if the pairing or sign-in
/// table could not be read.
pub async fn compose<S, Q>(
    config: EdgeConfig,
    edge: Arc<Edge<S>>,
    queue: Q,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> Result<Composed, EdgeError>
where
    S: EventStore + IntakeLedger + DeviceRegistry + ConfigStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    // Refuse a configuration that would misbehave rather than starting with it (ADR-0091).
    config.validate()?;
    // Refuse to start if the compiled-in country modules disagree, and log which countries this
    // build can serve (ADR-0027).
    let countries = crate::countries::registry();
    countries.validate().map_err(EdgeError::Country)?;
    tracing::info!(countries = ?countries.country_codes(), "country modules loaded");

    // The configuration this store last synced, back into the live session before anything binds
    // (C1). A box that reboots with its broadband down — or that an OTA install has just restarted —
    // otherwise comes up on `EdgeSession::bootstrap`: no menu, no roster, no floor, and no way to
    // sell until the cloud answers. A local-store read failure is the same fault that would stop the
    // event log opening, so it stops the boot rather than being papered over.
    let held_config_version = restore_session_from_store(&edge)
        .await
        .map_err(EdgeError::ConfigRestore)?;

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

    // The domain routes share the same pairing state the infra router serves, so the device-token
    // check (ADR-0084) validates tokens against the very set `/api/pair` issues them into. The config
    // loop keeps its own handle on the edge, so a menu published from the cloud hot-swaps the live
    // session the same routes serve.
    let config_edge = Arc::clone(&edge);
    // ONE queue-number authority, shared: the counter's order list must show the numbers the intake
    // path actually allocated, and two authorities over the same store would be two counters
    // (ADR-0093). `Arc<Q>` implements `QueueNumberAuthority` by delegation, which is what lets the
    // same value be held in two places — the trait returns `impl Future`, so it cannot be erased
    // behind `dyn`.
    let queue = Arc::new(queue);
    // The printers the settle and fire paths dispatch to (ADR-0100, production-readiness C2). Layered
    // rather than threaded through `domain_router`'s signature because a printer is an *effect* the
    // routes run after a commit, not state a route reads: the application loop deliberately holds no
    // printer, and every composition that omits this — the fakes-backed example, a route test —
    // reports `NO_PRINTER`, which is the truth for it rather than a silent no-op.
    let printers = Arc::new(crate::printing::Printers::tcp());
    let mut app = crate::http::router(state)
        .merge(crate::http::domain_router(
            edge,
            Arc::clone(&queue),
            Arc::clone(&pairing),
            Arc::clone(&sessions),
        ))
        .layer(axum::Extension(printers));

    // Compose the cloud surface when the store is provisioned for a cloud (ADR-0086). A `cloud_url`
    // means: mount the activation routes so a fresh box can be set up at `/setup`, and — once the box
    // is activated (a device credential is in the OS keyring) — start the config-pull and heartbeat
    // loops. An unactivated or LAN-only box still binds, pairs, and serves the counter offline
    // (ADR-0001); the gate withholds cloud sync, never local trading.
    let mut cloud = None;
    if let Some(cloud_url) = cloud_url {
        let (composed_app, keyed_client) = compose_cloud_surface(
            app,
            &cloud_url,
            store_id,
            &config_edge,
            queue,
            nats.as_ref(),
            held_config_version,
            shutdown_rx,
        )
        .await;
        app = composed_app;
        cloud = keyed_client;
    } else {
        tracing::info!("no cloud_url set; running LAN-only (no activation or cloud sync)");
    }

    Ok(Composed {
        app,
        pairing,
        sessions,
        cloud,
    })
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
    held_config_version: Option<String>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> (axum::Router, Option<CloudHttpClient>)
where
    S: EventStore + IntakeLedger + ConfigStore + Send + Sync + 'static,
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
            return (app, None);
        }
    };
    // Activation only, so the store id and target carried here are never used for a path: `/activate`
    // is not store-scoped and is deliberately unauthenticated (the box has no key yet). They are
    // passed because the adapter is one type — the OTA loop builds its own over a keyed transport.
    let cloud = Arc::new(HttpCloudSync::new(
        transport,
        store_id,
        crate::version::target(),
    ));
    let app = app.merge(activation_router(
        Arc::clone(edge),
        cloud,
        Arc::clone(&vault),
    ));

    // Boot gate: start the cloud loops only once the box holds a device credential (ADR-0086). Reading
    // the vault can itself fail (a locked or absent keyring) — that is not "unactivated", so it is
    // logged and the box runs without cloud sync rather than refusing to start.
    let mut keyed_client = None;
    match boot_standing(&*vault).await {
        Ok(ActivationStanding::Activated) => {
            let sync_key = resolve_sync_key(&*vault).await;
            keyed_client = spawn_cloud_loops(
                cloud_url,
                store_id,
                edge,
                queue,
                sync_key,
                held_config_version,
                shutdown_rx,
            );
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
    (app, keyed_client)
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
    held_config_version: Option<String>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) -> Option<CloudHttpClient>
where
    S: EventStore + IntakeLedger + ConfigStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let Some(sync_key) = sync_key else {
        tracing::warn!(
            "the store is activated but no scoped sync key is set (vault or {SYNC_KEY_ENV}); config-pull and heartbeat are idle"
        );
        return None;
    };
    let client = match CloudHttpClient::new(cloud_url, sync_key) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(
                %error,
                "cloud_url is set but the sync client could not be built; running without cloud sync"
            );
            return None;
        }
    };

    // Seeded with what the boot restore found on disk, so the first pull asks the cloud for a
    // *change* rather than re-fetching the document the counter is already selling on (C1).
    let config_client = ConfigClient::new(
        ConfigHttpTransport::new(client.clone(), store_id),
        Arc::clone(edge),
        held_config_version,
    );
    tokio::spawn(config_client.run(CONFIG_POLL_INTERVAL, wait_for_shutdown(shutdown_rx.clone())));

    // The heartbeat reports the store's own publish backlog alongside its liveness, so the fleet
    // console can see a box whose events are piling up behind a down link (ADR-0068).
    let heartbeat_client = HeartbeatClient::reporting(
        HeartbeatHttpTransport::new(client.clone(), store_id),
        Arc::clone(edge),
        HEARTBEAT_INTERVAL,
    );
    tokio::spawn(heartbeat_client.run(wait_for_shutdown(shutdown_rx.clone())));

    // The order relay (ADR-0061, ADR-0087): pull the store's parked orders, make each one through the
    // edge's own `OrderIn` — repriced from this store's menu, deduped in the order's transaction — and
    // ack the outcome. The same scoped key as the loops above, which must also carry `relay_orders`.
    // The relay paces itself on the cloud's long-poll, so it takes no interval of its own.
    let relay_client = RelayClient::new(
        RelayHttpTransport::new(client.clone(), store_id),
        EdgeOrderIn::new(Arc::clone(edge), queue, system_device_id(store_id)),
    );
    tokio::spawn(relay_client.run(wait_for_shutdown(shutdown_rx.clone())));

    tracing::info!(
        %cloud_url,
        "cloud sync enabled: config-pull, heartbeat, and order-relay loops running"
    );
    // Handed back for the OTA loop, which `serve` composes because it needs the shutdown sender.
    Some(client)
}

// -------------------------------------------------------------------------------------------------
// Over-the-air updates (roadmap R4 + R5-d, ADR-0055 Amendment 1)
// -------------------------------------------------------------------------------------------------

/// The install seam for this box, or `None` when it is not laid out for over-the-air updates.
///
/// "Laid out" means `<state>/bin/current` exists as the symlink `ExecStart` runs. Every store
/// provisioned before ADR-0055 Amendment 1 answers `None`, and so does the on-fakes example — both
/// keep trading and simply do not self-update, which is why this is a detection rather than a
/// refusal to start.
fn ota_installer(config: &EdgeConfig) -> Option<SystemdInstaller> {
    let installer = SystemdInstaller::new(
        crate::installer::binary_directory(&config.store_path),
        config.store_path.clone(),
    );
    if installer.is_ready() {
        Some(installer)
    } else {
        tracing::info!(
            "this box has no bin/current symlink, so it does not update over the air; see \
             deploy/edge/README.md to lay one out"
        );
        None
    }
}

/// The marker returned when a committed version has used up its boot attempts: the seam has already
/// put the previous binary back, and the caller must exit rather than serve.
struct BootGaveUp;

/// Reads the unconfirmed-boot marker, if there is a seam to read it with.
///
/// # Errors
///
/// [`BootGaveUp`] when the version this process is has exhausted its attempts. The previous binary
/// is already `current` and the database is already restored, so the correct next step is to exit
/// with success and let the service manager start what works. Anything else — including a marker
/// that could not be read — is a boot that continues.
fn settle_boot(installer: Option<&SystemdInstaller>) -> Result<BootStanding, BootGaveUp> {
    let Some(installer) = installer else {
        return Ok(BootStanding::Settled);
    };
    match installer.begin_boot() {
        Ok(BootStanding::Reverted) => {
            tracing::warn!(
                version = crate::version::VERSION,
                "this version never reached a healthy boot; the previous one is restored — exiting \
                 so the service manager starts it"
            );
            Err(BootGaveUp)
        }
        Ok(standing) => {
            if let BootStanding::Unconfirmed { attempt } = standing {
                tracing::info!(
                    attempt,
                    version = crate::version::VERSION,
                    "this version is on trial; it is confirmed once the store is serving"
                );
            }
            Ok(standing)
        }
        Err(error) => {
            // A marker that cannot be read must not stop a store trading. It does mean the watchdog
            // is blind for this boot, which is why it is a warning rather than a debug line.
            tracing::warn!(%error, "could not read the unconfirmed-boot marker; continuing");
            Ok(BootStanding::Settled)
        }
    }
}

/// Everything the OTA loop needs and nothing else does, grouped so [`spawn_ota`] takes one argument
/// instead of seven.
struct OtaWiring<A, S> {
    /// The keyed `/sync` client, from [`Composed::cloud`]. `None` for a LAN-only, unactivated or
    /// unkeyed box.
    client: Option<CloudHttpClient>,
    /// The install seam, from [`ota_installer`].
    installer: Option<SystemdInstaller>,
    /// Where the durable self-test lives (ADR-0048's highest-precedence rule reads it).
    authority: A,
    /// The live session the published rollout and this box's placement are read from.
    edge: Arc<Edge<S>>,
    store_id: StoreId,
    /// What the boot marker said, from [`settle_boot`].
    boot: BootStanding,
    /// The shared shutdown, which is how an installed binary becomes the running one — and the
    /// record of *why* the stop happened, which is how the service manager knows to start it again
    /// (roadmap v3 **E4**).
    restart: RestartIntent,
}

/// Reports which binary this store is running, and — when the box can actually install one — starts
/// the loop that weighs the published rollout
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md), ADR-0055 Amendment 1).
///
/// # The report is not conditional on the update loop
///
/// It was, and that was production-readiness **R1**: this function returned early when there was no
/// `bin/current` or no signing keys baked in, taking `confirm_boot` with it — so the fleet view held
/// `NULL` for the installed version of exactly the boxes an upgrade campaign has to find, while
/// `confirm_boot`'s own contract says the report "is sent in every case".
///
/// The two needs are different and are now separated. **Reporting** needs a keyed cloud client and
/// nothing else; a store with no cloud has nobody to tell, which is the one honest reason to stay
/// silent. **Updating** needs somewhere to put a binary (`bin/current`) and keys to judge one with;
/// without either the box keeps trading on what it has, and still says what that is.
fn spawn_ota<A, S>(wiring: OtaWiring<A, S>, shutdown_rx: &tokio::sync::watch::Receiver<bool>)
where
    A: OtaStateAuthority + 'static,
    S: EventStore + Send + Sync + 'static,
{
    let Some(client) = wiring.client else {
        return;
    };
    let store_id = wiring.store_id;
    let boot = wiring.boot;
    let cloud = HttpCloudSync::new(
        OtaHttpTransport::new(client),
        store_id,
        crate::version::target(),
    );

    // Can this box install what it is told to? Both halves are required, and a missing one is not an
    // error — a LAN-installed box legitimately has no update layout.
    let updater = match (wiring.installer, crate::trusted_keys()) {
        (Some(installer), Ok(trusted)) => Some((installer, trusted)),
        // `ota_installer` already said why, at info.
        (None, _) => None,
        (Some(_), Err(error)) => {
            tracing::warn!(
                %error,
                "no release signing keys are baked into this build; over-the-air updates are off \
                 (this box still reports which binary it is running)"
            );
            None
        }
    };

    let Some((installer, trusted)) = updater else {
        let authority = wiring.authority;
        tokio::spawn(async move {
            crate::ota_client::confirm_boot(
                &cloud,
                &authority,
                &crate::ota_client::NoUpdateLayout,
                store_id,
                boot,
            )
            .await;
        });
        tracing::info!(
            "this box does not update over the air; it still reports which binary it is running"
        );
        return;
    };

    let ota = OtaClient::new(
        cloud.clone(),
        MinisignVerifier,
        installer.clone(),
        trusted,
        wiring.authority,
        wiring.edge,
        store_id,
        wiring.restart,
    );
    let shutdown = wait_for_shutdown(shutdown_rx.clone());
    tokio::spawn(async move {
        // The report comes first: it is what tells the console which binary this store is running,
        // and after a revert it is the only place the failure is visible.
        crate::ota_client::confirm_boot(&cloud, ota.authority(), &installer, store_id, boot).await;
        ota.run(crate::ota_client::OTA_POLL_INTERVAL, shutdown)
            .await;
    });
    tracing::info!("over-the-air updates enabled");
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

    // The release the store is running, stamped at build time (R1b). Was `CARGO_PKG_VERSION`,
    // which is `0.0.0` in every artifact — so every store told the cloud the same thing and the
    // OTA progress model could not tell one release from another.
    let publisher = EventPublisher::new(Arc::clone(edge), link, crate::version::tag());
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
pub async fn shutdown_signal() {
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
