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

use link_nats::{ConsumerConfig, NatsConsumer};
use metrics_vm::VmMetrics;
use pos_cloud::alerts::AlertThresholds;
use pos_cloud::audit::{AuditRecorder, AuditSink};
use pos_cloud::clock::SystemClock;
use pos_cloud::http::CloudApp;
use pos_cloud::qr::TableTokenSecret;
use pos_cloud::qr_http;
use pos_cloud::relay::OrderRelay;
use pos_cloud::retention::{self, RetentionPolicy};
use pos_cloud::webhook::{self, TlsWebhookSender};
use pos_cloud::{
    Cloud, CloudConfig, NatsIngestConfig, alerts, assets, cursor, dashboard, http, orders, relay,
};
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
    reason = "the boot sequence wires the store, five background tasks (cursor, projector, \
              retention, webhook dispatch, metrics heartbeat), the merged router, and their shared \
              shutdown in one linear flow; splitting it would scatter the handles the shutdown join \
              needs"
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
    // The console audit recorder (ADR-0069): every `/admin` write route records who changed what to
    // the append-only `audit_log`, best-effort after the mutation. One recorder, shared as an
    // `Arc<dyn AuditRecorder>` across the CloudApp router and the registry sub-router, so a handler
    // can emit without threading an `AuditStore` generic through the already-large router types.
    let audit: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(store.audit()));
    let app = CloudApp::new(
        cloud.clone(),
        store.rollups(),
        store.api_keys(),
        SystemClock,
        store.admin(),
        store.config_trees(),
        store.webhooks(),
    )
    .with_audit(Arc::clone(&audit))
    .with_admin_session_ttl_secs(config.admin_session_ttl_secs)
    .with_admin_session_idle_ttl_secs(config.admin_session_idle_ttl_secs)
    .with_admin_invite_ttl_secs(config.admin_invite_ttl_secs)
    .with_login_rate_limit(
        config.admin_login_max_attempts,
        config.admin_login_window_secs,
    )
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
        SystemClock,
        store.task_health(),
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
            store.task_health(),
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
        store.task_health(),
        webhook_interval,
        shutdown_signal(),
    ));

    // The alert evaluator (ADR-0073, Track O2): each tick it reads the fleet, task-health, and webhook
    // read models, decides which operational conditions are firing, and reconciles them against the
    // alert store (open/refresh/resolve). It always runs — with nothing wrong it is a cheap per-tick
    // no-op — and, like every loop, records its own health so the watcher is itself watched.
    let alert_interval = Duration::from_secs(config.alert_eval_interval_secs);
    let alert_thresholds = AlertThresholds {
        store_offline_secs: config.alert_store_offline_secs,
        relay_backlog_max: config.alert_relay_backlog_max,
        relay_oldest_secs: config.alert_relay_oldest_secs,
        jetstream_capacity_percent: config.alert_jetstream_capacity_percent,
        projector_stale_slack_secs: config.alert_projector_stale_slack_secs,
    };
    tracing::info!(
        interval_secs = config.alert_eval_interval_secs,
        "alert evaluator started"
    );
    let alert_task = tokio::spawn(alerts::evaluator::run(
        store.registry(),
        store.fleet(),
        store.webhooks(),
        store.task_health(),
        store.alerts(),
        SystemClock,
        alert_thresholds,
        alert_interval,
        shutdown_signal(),
    ));

    // The optional monitoring profile (metrics-vm → VictoriaMetrics, ADR-0031): a sparse liveness
    // heartbeat off the sales path, gated by [metrics] and off by default. Per
    // `docs/capacity-and-reliability.md` the profile is off below ~50 stores in favour of sparse
    // sampling, so a pilot cell leaves it unset. The sink's own bounded queue drops under pressure,
    // so a slow or dead backend never touches trading.
    let metrics_task = if let Some(metrics) = config.metrics.as_ref() {
        match VmMetrics::connect(&metrics.url) {
            Ok(sink) => {
                let interval = Duration::from_secs(metrics.sample_interval_secs);
                tracing::info!(
                    url = %metrics.url,
                    interval_secs = metrics.sample_interval_secs,
                    "metrics heartbeat started (monitoring profile on)"
                );
                Some(tokio::spawn(pos_cloud::metrics::run(
                    sink,
                    SystemClock,
                    interval,
                    shutdown_signal(),
                )))
            }
            Err(error) => {
                tracing::error!(%error, "metrics url is not usable; the monitoring heartbeat is off");
                None
            }
        }
    } else {
        tracing::info!(
            "no [metrics] configured; the monitoring profile is off (sparse-sampling posture \
             below ~50 stores)"
        );
        None
    };

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %config.bind, "pos_cloud listening");

    // The background loops this deployment turned on, for the health endpoint to check against: the
    // projector and webhook dispatcher always run; the retention cron only when a period is set. A loop
    // in this set that has never ticked reads as unhealthy rather than being silently absent.
    let mut expected_tasks = vec![
        pos_cloud::health::ROLLUP_PROJECTOR.to_owned(),
        pos_cloud::health::WEBHOOK_DISPATCHER.to_owned(),
        pos_cloud::health::ALERT_EVALUATOR.to_owned(),
    ];
    if config.retention_days.is_some() {
        expected_tasks.push(pos_cloud::health::RETENTION.to_owned());
    }

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
            Arc::clone(&audit),
        ))
        .merge(http::translation_router(
            store.translations(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        .merge(http::activation_router(
            store.activation_codes(),
            store.admin(),
            SystemClock,
        ))
        // The org registry (ADR-0065): named Tenant/Brand/Store/Device under the super-admin session,
        // the source of the dashboard's named pickers. Its tables are backfilled from `config_trees`
        // by migration 0011, so an existing cell's fleet appears here on the first boot after upgrade.
        .merge(http::registry_router(
            store.registry(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // People & access (ADR-0070): employees, role templates over the pos-core catalogue, and
        // per-store assignments, with PIN set/reset. Every write is audited (id/code/role, never the
        // name or PIN). `store.people()` is the employee, role-template, and assignment seam at once.
        .merge(http::people_router(
            store.people(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // People publish (ADR-0070 slice 5): compile a store's people + roles + assignments into the
        // edge-shaped `permissions` document and write it onto the store's `permissions` config node,
        // so it rides the config tree to the store like every other config change.
        .merge(http::people_publish_router(
            store.people(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Floor & kitchen master data (ADR-0072, Track M2): a store's areas + tables and its kitchen
        // stations + item→station routing rules. Reads behind Read; writes behind the new ManageFloor
        // and audited. `store.floor()` is the area, table, station, and routing-rule seam at once.
        .merge(http::floor_router(
            store.floor(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Floor & kitchen publish (ADR-0072 slice 4): compile the store's areas/tables + stations/
        // routing into the `floor`/`stations` config nodes and version them through the config tree,
        // so the edge applies the real floor plan and station routing.
        .merge(http::floor_publish_router(
            store.floor(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Console audit read (ADR-0069 slice 4): the filterable Audit screen reads the append-only
        // trail here. It carries the concrete audit store (the recorder the write routes hold exposes
        // only `record`), behind the same super-admin session guard as the other reads.
        .merge(http::audit_router(
            store.audit(),
            store.admin(),
            SystemClock,
        ))
        // Fleet liveness (ADR-0068): the read-only console view of whether each store is up and in
        // sync — a join across the registry, `store_liveness` (captured on config pulls/heartbeats),
        // the config tree, and the order-relay queue, with online/offline derived at read.
        .merge(http::fleet_router(
            store.fleet(),
            store.admin(),
            SystemClock,
        ))
        // Operational alerts (ADR-0073, Track O2): the console reads the fleet-wide alert list the
        // evaluator loop maintains, and acknowledges/resolves alerts. Reads are behind the session
        // guard; acknowledge/resolve need console.alerts.manage and are audited.
        .merge(http::alerts_router(
            store.alerts(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Capability catalogue (ADR-0071): the §10 flags, presets, and inter-flag rules the Config
        // screen's form editor renders toggles and conflict previews from — static framework data
        // behind the session guard.
        .merge(http::capabilities_router(store.admin(), SystemClock))
        // Capability publish (ADR-0071): the form editor writes a store's flags here; the flags are
        // merged into the store's Store config layer (preserving menu/layout/permissions) and versioned
        // through the config tree, which runs the §10 inter-flag rules.
        .merge(http::config_capabilities_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Background-task health (ADR-0068 slice 4): the read-only console view of whether the
        // off-request loops are alive and keeping up. `expected_tasks` names the loops this
        // deployment actually turned on, so a loop dead since boot reads as unhealthy, not missing.
        .merge(http::health_router(
            store.task_health(),
            store.admin(),
            SystemClock,
            expected_tasks,
        ))
        // Catalog authoring (ADR-0066): the write surface for the menu source of truth — items,
        // menus with inheritance, and per-channel placements — from which a MenuBook is compiled.
        .merge(http::catalog_router(
            store.catalog(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Tax rates (ADR-0074, Track M4): the per-(tax class × channel) rate an operator authors,
        // validated against the tenant's tax classes and published as the `tax` config node.
        .merge(http::tax_rate_router(
            store.tax_rates(),
            store.catalog(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Catalog publish (ADR-0066): compile a menu → write the MenuBook onto the store's `menu`
        // config node, so it rides the config tree to the store like every other config change.
        .merge(http::catalog_publish_router(
            store.catalog(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Tax publish (ADR-0074, Track M4): assemble the tenant's authored rates into the store's
        // `tax` config node, so the edge applies them to its session's tax table.
        .merge(http::config_tax_router(
            store.tax_rates(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Public order intake + the cloud→store relay (ADR-0056, ADR-0061). The served `POST/GET
        // /v1/orders` calls the relay (an `OrderIn` over the durable per-store queue); the store
        // pulls and acks its queue over the store-facing `/sync/.../orders` routes. The relay
        // resolves the owning tenant and the per-store `order_relay` config from the config tree,
        // and the handler binds the request's store to the caller's tenant through the same
        // directory.
        .merge(orders::orders_router(
            OrderRelay::new(
                store.store_directory(),
                store.config_trees(),
                store.order_queue(),
                SystemClock,
            ),
            store.api_keys(),
            SystemClock,
            store.store_directory(),
        ))
        .merge(relay::orders_sync_router(
            store.order_queue(),
            store.api_keys(),
            SystemClock,
        ));

    // Guest QR ordering (ADR-0057), only when a signing secret is configured: the guest carries no
    // API key, so the HMAC-signed table token is the only credential and an absent secret leaves the
    // endpoint off (any token would be unverifiable). It forwards accepted orders into the same relay
    // `POST /v1/orders` uses, guardrailed by the store's `qr` config.
    let service = if let Some(secret) = config.table_token_secret.clone() {
        tracing::info!("QR ordering enabled (POST /v1/qr/orders)");
        service
            .merge(qr_http::qr_router(
                TableTokenSecret::new(secret.clone()),
                OrderRelay::new(
                    store.store_directory(),
                    store.config_trees(),
                    store.order_queue(),
                    SystemClock,
                ),
                store.config_trees(),
                SystemClock,
            ))
            // Table QR minting (ADR-0072): the console reads each table's signed token here to print a
            // QR sheet. Wired only with a secret, the same gate as the guest endpoint above.
            .merge(http::table_qr_router(
                store.floor(),
                store.admin(),
                SystemClock,
                TableTokenSecret::new(secret),
            ))
    } else {
        tracing::warn!("no table_token_secret configured; the QR ordering endpoint is off");
        service
    };

    // The embedded back-office dashboard (ADR-0060) is the fallback: the API routes above match
    // first, and everything else — `/`, client-routed paths, the built static assets — is served
    // the single-page app, with an unknown path returning index.html for client-side routing.
    let service = service.fallback(assets::serve);
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
    alert_task.abort();
    let _ = alert_task.await;
    if let Some(task) = metrics_task {
        task.abort();
        let _ = task.await;
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
