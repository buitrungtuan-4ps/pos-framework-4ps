// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_cloud` entry point: load config, open the PostgreSQL store, serve.
//!
//! Thin by design ([ADR-0013](../../../docs/adr/0013-async-strategy.md)) — the ingest and rollup
//! logic lives in the library ([`pos_cloud`]) so it is tested against the fakes without a database.

use core::time::Duration;
use std::process::ExitCode;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use link_nats::ConsumerConfig;
use pos_cloud::NatsIngestConfig;
use pos_cloud::audit::{AuditRecorder, AuditSink};
use pos_cloud::wake;

mod startup;

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
    let config = startup::load_config()?;
    let (cell, directory_reload) = startup::cell_directory(&config)?;
    let (store, one_switch) = startup::open_store(&config).await?;
    let cloud = startup::cloud(&store);
    // The console audit recorder (ADR-0069): every `/admin` write route records who changed what to
    // the append-only `audit_log`, best-effort after the mutation. One recorder, shared as an
    // `Arc<dyn AuditRecorder>` across the CloudApp router and the registry sub-router, so a handler
    // can emit without threading an `AuditStore` generic through the already-large router types.
    let audit: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(store.audit()));
    let app = startup::cloud_app(&cloud, &store, &config, &audit, &one_switch);

    let loops = startup::loops::Loops::spawn(&config, &cloud, &store, &audit).await?;

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %config.bind, "pos_cloud listening");

    let expected_tasks = startup::expected_tasks(&config);
    let artifacts = startup::artifact_store(&config);
    let release_source = startup::release_source(&config);
    let archive_retention_task =
        startup::loops::archive_retention_sweep(&config, &store, artifacts.as_ref());

    // Pulled out before `router` takes ownership: the intake sub-router and the `/sync` throttle
    // layer both need a handle, and the whole point of one process holding one limiter each is that
    // every route throttles against the same counter (roadmap **Q5**).
    let orders_limiter = app.orders_rate_limiter();
    let sync_throttle = app.sync_throttle();
    let relay_wake = wake::SharedWake::new();

    let chain_anchors: Arc<dyn pos_cloud::anchor::AnchorLedger> = Arc::new(store.chain_anchors());

    let providers = startup::vendor_catalogue(&config)?;

    let deps = startup::routes::RouteDeps {
        store: &store,
        config: &config,
        audit: &audit,
        chain_anchors: &chain_anchors,
        providers: &providers,
        artifacts: artifacts.as_ref(),
        release_source: release_source.as_ref(),
        expected_tasks: &expected_tasks,
        orders_limiter: &orders_limiter,
        relay_wake: &relay_wake,
        cell: &cell,
    };
    let service = startup::routes::api_routes(app, &deps);
    let service = startup::routes::with_guest_ordering(service, &deps);
    let service = startup::routes::served(service, sync_throttle);
    axum::serve(listener, service)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    loops.wind_down(archive_retention_task).await;
    if let Some(task) = directory_reload {
        task.abort();
    }
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
