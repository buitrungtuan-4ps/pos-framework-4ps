// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Binding the listener and serving with graceful shutdown.

use std::sync::Arc;

use tokio::net::TcpListener;

use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;

use crate::app::Edge;
use crate::config::EdgeConfig;
use crate::discovery::{Advertiser, NoopAdvertiser};
use crate::error::EdgeError;
use crate::pairing::pairing_url;
use crate::state::AppState;

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
    // check (ADR-0084) validates tokens against the very set `/api/pair` issues them into.
    let pairing = state.pairing.clone();
    let app = crate::http::router(state).merge(crate::http::domain_router(edge, pairing));

    let listener = TcpListener::bind(bind)
        .await
        .map_err(|source| EdgeError::Bind { addr: bind, source })?;
    tracing::info!(
        %bind,
        protocol_version = pos_proto::PROTOCOL_VERSION,
        "pos_edge listening",
    );

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(EdgeError::Serve)?;

    tracing::info!("pos_edge stopped");
    Ok(())
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
