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
use pos_cloud::retention::{self, RetentionPolicy};
use pos_cloud::webhook::{self, TlsWebhookSender};
use pos_cloud::{Cloud, CloudConfig, NatsIngestConfig, assets, cursor, dashboard, http};
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
#[expect(
    clippy::too_many_lines,
    reason = "the boot sequence wires the store, four background tasks (cursor, projector, \
              retention, webhook dispatch), the merged router, and their shared shutdown in one \
              linear flow; splitting it would scatter the handles the shutdown join needs"
)]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = std::env::var("POS_CLOUD_CONFIG")
        .map_err(|_| "POS_CLOUD_CONFIG must name a configuration file")?;
    let config = CloudConfig::from_toml(&std::fs::read_to_string(&config_path)?)?;

    let store = PostgresStore::connect(&config.database_url).map_err(|error| error.to_string())?;
    store.migrate().await.map_err(|error| error.to_string())?;
    // One pool, six views of it: the event-store application layer, the materialised-rollup read
    // model the `/v1` dashboard answers from, the API-key store the `/v1` bearer check consults, the
    // super-admin store the `/admin` login and session guard use, the config-tree store the `/admin`
    // config routes author, and the webhook-endpoint store the `/admin` webhook routes register into.
    let cloud = Cloud::new(store.clone());
    let app = CloudApp::new(
        cloud.clone(),
        store.rollups(),
        store.api_keys(),
        SystemClock,
        store.admin(),
        store.config_trees(),
        store.webhooks(),
    )
    .with_admin_session_ttl_secs(config.admin_session_ttl_secs)
    .with_admin_setup_token(config.admin_setup_token.clone());

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

    // The retention / PII-masking cron, only if a retention period is configured. The period is a
    // legal decision, not a code default (ADR-0035), so an absent `retention_days` leaves the cron
    // off rather than masking on a guessed schedule. When on, it sweeps the subject store fleet-wide
    // and masks every record past its period; masking (not deletion) keeps the books reconcilable,
    // and erasure/access requests stay escalated to the Data Protection contact, never on this cron.
    let retention_task = config.retention_days.map(|days| {
        let interval = Duration::from_secs(config.retention_sweep_interval_secs);
        tracing::info!(
            retention_days = days,
            interval_secs = config.retention_sweep_interval_secs,
            "retention/PII-masking cron started"
        );
        tokio::spawn(retention::run(
            store.subjects(),
            RetentionPolicy::from_days(days),
            SystemClock,
            interval,
            shutdown_signal(),
        ))
    });
    if retention_task.is_none() {
        tracing::warn!(
            "no retention_days configured; the PII-masking cron is off (set it from the country's \
             configured retention period — ADR-0035)"
        );
    }

    // The webhook dispatcher: loads the enabled endpoints fleet-wide each tick, re-vets each URL,
    // delivers the events after each cursor over TLS, and persists progress (ADR-0032, ADR-0038). It
    // always runs — with no registered endpoints it is a cheap per-tick no-op — and reads the same
    // event log the projector does, as the trusted role.
    let webhook_interval = Duration::from_secs(config.webhook_dispatch_interval_secs);
    let webhook_timeout = Duration::from_secs(config.webhook_delivery_timeout_secs);
    tracing::info!(
        interval_secs = config.webhook_dispatch_interval_secs,
        delivery_timeout_secs = config.webhook_delivery_timeout_secs,
        "webhook dispatcher started"
    );
    let webhook_task = tokio::spawn(webhook::runner::run(
        store.clone(),
        store.webhooks(),
        TlsWebhookSender::new(webhook_timeout),
        SystemClock,
        webhook_interval,
        shutdown_signal(),
    ));

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %config.bind, "pos_cloud listening");
    // The reconciliation diff (ADR-0040), device-onboarding (ADR-0041), translation-grid (ADR-0043)
    // and activation-exchange (ADR-0050/0051) endpoints carry their own state, so they are merged in
    // rather than threaded through CloudApp.
    let service = http::router(app)
        .merge(http::reconcile_router(store.reconcile()))
        .merge(http::device_router(
            store.device_proposals(),
            store.admin(),
            store.api_keys(),
            SystemClock,
        ))
        .merge(http::translation_router(
            store.translations(),
            store.admin(),
            SystemClock,
        ))
        .merge(http::activation_router(
            store.activation_codes(),
            store.admin(),
            SystemClock,
        ))
        // The embedded back-office dashboard (ADR-0060) is the fallback: the API routes above match
        // first, and everything else — `/`, client-routed paths, the built static assets — is served
        // the single-page app, with an unknown path returning index.html for client-side routing.
        .fallback(assets::serve);
    axum::serve(listener, service)
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
    if let Some(task) = retention_task {
        task.abort();
        let _ = task.await;
    }
    webhook_task.abort();
    let _ = webhook_task.await;
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
