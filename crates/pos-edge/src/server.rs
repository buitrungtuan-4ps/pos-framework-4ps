// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Binding the listener and serving with graceful shutdown.

use core::time::Duration;
use std::sync::Arc;

use tokio::net::TcpListener;

use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;

use crate::app::Edge;
use crate::cloud_http::{CloudHttpClient, ConfigHttpTransport, HeartbeatHttpTransport};
use crate::config::EdgeConfig;
use crate::config_client::ConfigClient;
use crate::discovery::{Advertiser, NoopAdvertiser};
use crate::error::EdgeError;
use crate::heartbeat_client::HeartbeatClient;
use crate::pairing::pairing_url;
use crate::state::AppState;

/// The environment variable carrying the store's scoped `read_config` API key
/// ([ADR-0085](../../../docs/adr/0085-edge-cloud-sync-transport.md)). Supplied by the service unit
/// from a mode-0600 env file — never in `config.toml`, never committed. Absent (or empty) means the
/// edge runs LAN-only and spawns no cloud loops, exactly as an unset `cloud_url` does.
const SYNC_KEY_ENV: &str = "POS_EDGE_SYNC_KEY";

/// How often the config-pull loop pulls when nothing is failing. The cloud answers immediately (no
/// server-side long-poll yet, ADR-0039), so this paces the loop; a published change reaches the
/// counter within one interval.
const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How often the heartbeat loop pings the cloud, so a store that is up but not currently pulling
/// still reads as online in the fleet view ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)).
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

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
pub async fn serve<S>(config: EdgeConfig, edge: Arc<Edge<S>>) -> Result<(), EdgeError>
where
    S: EventStore + Send + Sync + 'static,
{
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
    // Share the edge's fan-out with the /ws route so a committed change reaches every device.
    let state = AppState::with_fanout(config, edge.fanout().clone());

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
    let pairing = state.pairing.clone();
    let config_edge = Arc::clone(&edge);
    let app = crate::http::router(state).merge(crate::http::domain_router(edge, pairing));

    // One shutdown signal, fanned to the server and every background loop so a Ctrl-C / SIGTERM drains
    // them together. A task translates the OS signal into a watched flag; each consumer waits on its
    // own clone.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ignored = shutdown_tx.send(true);
    });

    // Start the cloud loops when the store is provisioned for a cloud (a `cloud_url` and a scoped key);
    // otherwise the edge trades LAN-only exactly as before (ADR-0085).
    spawn_cloud_loops(cloud_url, store_id, config_edge, &shutdown_rx);

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

/// Spawns the config-pull and heartbeat loops when the store is provisioned for a cloud: a `cloud_url`
/// in the config and a scoped `read_config` key in the environment ([ADR-0085](../../../docs/adr/0085-edge-cloud-sync-transport.md)).
///
/// Missing either is the ordinary LAN-only case — a demo, or a box not yet provisioned — so it logs
/// what is missing and spawns nothing rather than failing to start. A key that is present but blank is
/// treated as absent. The loops share the passed shutdown, so they drain with the server.
fn spawn_cloud_loops<S>(
    cloud_url: Option<url::Url>,
    store_id: pos_proto::ids::StoreId,
    edge: Arc<Edge<S>>,
    shutdown_rx: &tokio::sync::watch::Receiver<bool>,
) where
    S: EventStore + Send + Sync + 'static,
{
    let sync_key = std::env::var(SYNC_KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty());
    let (Some(cloud_url), Some(sync_key)) = (cloud_url, sync_key) else {
        // Say which half is missing, so an operator who meant to connect a store can tell a
        // deliberate LAN-only edge from a misconfigured one.
        tracing::info!(
            "running LAN-only: no cloud loops (set both cloud_url in config and {SYNC_KEY_ENV})"
        );
        return;
    };
    let client = match CloudHttpClient::new(&cloud_url, sync_key) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(
                %error,
                "cloud_url is set but the cloud client could not be built; running LAN-only"
            );
            return;
        }
    };

    let config_client = ConfigClient::new(ConfigHttpTransport::new(client.clone(), store_id), edge);
    tokio::spawn(config_client.run(CONFIG_POLL_INTERVAL, wait_for_shutdown(shutdown_rx.clone())));

    let heartbeat_client = HeartbeatClient::new(
        HeartbeatHttpTransport::new(client, store_id),
        HEARTBEAT_INTERVAL,
    );
    tokio::spawn(heartbeat_client.run(wait_for_shutdown(shutdown_rx.clone())));

    tracing::info!(%cloud_url, "cloud sync enabled: config-pull and heartbeat loops running");
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
