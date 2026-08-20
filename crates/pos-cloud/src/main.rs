// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_cloud` entry point: load config, open the PostgreSQL store, serve.
//!
//! Thin by design ([ADR-0013](../../../docs/adr/0013-async-strategy.md)) — the ingest and rollup
//! logic lives in the library ([`pos_cloud`]) so it is tested against the fakes without a database.

use core::time::Duration;
use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

use link_nats::{ConsumerConfig, NatsConsumer};
use pos_cloud::clock::SystemClock;
use pos_cloud::http::CloudApp;
use pos_cloud::{Cloud, CloudConfig, NatsIngestConfig, cursor, dashboard, http};
use store_postgres::PostgresStore;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "pos_cloud failed to start");
            ExitCode::FAILURE
        }
    }
}

/// Boots the cloud: config from `POS_CLOUD_CONFIG`, the PostgreSQL store migrated, the HTTP server
/// bound and served until shutdown.
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = std::env::var("POS_CLOUD_CONFIG")
        .map_err(|_| "POS_CLOUD_CONFIG must name a configuration file")?;
    let config = CloudConfig::from_toml(&std::fs::read_to_string(&config_path)?)?;

    let store = PostgresStore::connect(&config.database_url).map_err(|error| error.to_string())?;
    store.migrate().await.map_err(|error| error.to_string())?;
    // One pool, four views of it: the event-store application layer, the materialised-rollup read
    // model the `/v1` dashboard answers from, the API-key store the `/v1` bearer check consults, and
    // the super-admin store the `/admin` login and session guard use.
    let cloud = Cloud::new(store.clone());
    let app = CloudApp::new(
        cloud.clone(),
        store.rollups(),
        store.api_keys(),
        SystemClock,
        store.admin(),
    )
    .with_admin_session_ttl_secs(config.admin_session_ttl_secs);

    // The production ingest feed, if configured: a durable NATS cursor driving the same
    // `Cloud::ingest` the HTTP re-push target uses. Absent config leaves the cursor off, so the
    // cloud still serves and still ingests re-pushes — useful for a reconciliation-only deployment.
    let cursor_task = if let Some(nats) = config.nats.clone() {
        let consumer = NatsConsumer::connect(&nats.url, consumer_config(&nats))
            .await
            .map_err(|error| error.to_string())?;
        tracing::info!(stream = %nats.stream, durable = %nats.durable, "ingest cursor started");
        Some(tokio::spawn(cursor::run(
            consumer,
            cloud.clone(),
            shutdown_signal(),
        )))
    } else {
        tracing::info!("no [nats] config; ingest cursor off (reconciliation re-push only)");
        None
    };

    // The rollup projector: the single writer of the materialised rollup the `/v1` dashboard reads.
    // Ingest only appends to the log; this sweeps the fleet on an interval and folds each store's
    // new events into its rollup, so a dashboard is never more than one interval stale. The store is
    // both the event log it reads and the catalog of stores it iterates.
    let projector_interval = Duration::from_secs(config.projector_interval_secs);
    tracing::info!(
        interval_secs = config.projector_interval_secs,
        "rollup projector started"
    );
    let projector_task = tokio::spawn(dashboard::projector::run(
        store.clone(),
        store.rollups(),
        store.clone(),
        projector_interval,
        shutdown_signal(),
    ));

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %config.bind, "pos_cloud listening");
    axum::serve(listener, http::router(app))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // The server has stopped, so wind the background tasks down too. Their own shutdown signals have
    // already fired (same SIGINT); this awaits their clean exit, or aborts a stuck one.
    if let Some(task) = cursor_task {
        task.abort();
        let _ = task.await;
    }
    projector_task.abort();
    let _ = projector_task.await;
    Ok(())
}

/// Maps the config's NATS section to `link-nats`'s consumer configuration.
fn consumer_config(nats: &NatsIngestConfig) -> ConsumerConfig {
    ConsumerConfig {
        stream: nats.stream.clone(),
        durable: nats.durable.clone(),
        filter_subject: nats.filter_subject.clone(),
        batch: nats.batch,
        expires: Duration::from_secs(nats.expires_secs),
    }
}

/// Resolves when the process is asked to stop (SIGINT / Ctrl-C), so both the HTTP server and the
/// ingest cursor shut down together.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
