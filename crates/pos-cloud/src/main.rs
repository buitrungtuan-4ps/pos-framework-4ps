// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_cloud` entry point: load config, open the PostgreSQL store, serve.
//!
//! Thin by design ([ADR-0013](../../../docs/adr/0013-async-strategy.md)) — the ingest and rollup
//! logic lives in the library ([`pos_cloud`]) so it is tested against the fakes without a database.

use core::time::Duration;
use std::process::ExitCode;
use std::sync::Arc;

use axum::Router;
use tracing_subscriber::EnvFilter;

use blob_garage::S3Blobs;
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
    Cloud, CloudConfig, NatsIngestConfig, alerts, assets, countries, cursor, dashboard, http,
    orders, relay,
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
    // Range checks serde cannot express (ADR-0090). Fail the boot rather than run with a value that
    // silently disables a control — a wrong `trusted_proxy_hops` is a wrong rate-limit key.
    config.validate()?;

    let store = PostgresStore::connect(&config.database_url).map_err(|error| error.to_string())?;
    store.migrate().await.map_err(|error| error.to_string())?;
    // One pool, six views of it: the event-store application layer, the materialised-rollup read
    // model the `/v1` dashboard answers from, the API-key store the `/v1` bearer check consults, the
    // super-admin store the `/admin` login and session guard use, the config-tree store the `/admin`
    // config routes author, and the webhook-endpoint store the `/admin` webhook routes register into.
    // Stamped, not trusted: every ingested event's tenant and brand come from this registry lookup,
    // overwriting what the publishing box claimed
    // ([ADR-0101](../../docs/adr/0101-the-cloud-stamps-the-tenant.md), production-readiness **S2**).
    // The tenant is the column row-level isolation is defined on, and until this line every store in
    // the fleet stamped the same constant.
    let cloud = Cloud::with_store_owners(store.clone(), Arc::new(store.store_directory()));
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
    .with_orders_rate_limit(config.orders_max_requests, config.orders_window_secs)
    .with_sync_rate_limit(config.sync_max_requests, config.sync_window_secs)
    .with_trusted_proxy_hops(config.trusted_proxy_hops)
    .with_admin_setup_token(config.admin_setup_token.clone())
    .with_internal_shared_secret(config.internal_shared_secret.clone());

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
    // The off-console channel (ADR-0073 slice 4), when one is configured. The URL is SSRF-vetted here
    // rather than per tick: a boot refusal is the right place to learn the ops endpoint resolves to a
    // private address, and re-vetting every minute would put a DNS lookup on the alert path.
    let alert_channel = match (&config.alert_webhook_url, &config.alert_webhook_secret) {
        (Some(url), Some(secret)) => match webhook::vet_blocking(url).await {
            Ok(destination) => {
                tracing::info!(destination = %destination.url, "alert webhook channel armed");
                Some(alerts::WebhookAlertChannel::new(
                    TlsWebhookSender::new(webhook_timeout),
                    destination,
                    secret.clone(),
                ))
            }
            Err(rejection) => {
                // Not fatal: console alerting still works, and refusing to boot would take the whole
                // cloud down over a mistyped ops URL. Loud, though — somebody believes this is armed.
                tracing::error!(
                    %rejection,
                    "alert_webhook_url was refused; alerts stay console-only until it is corrected"
                );
                None
            }
        },
        // Console-only. Validated in pairs at load (`CloudConfig::validate`), so a half-set pair
        // never reaches here.
        _ => None,
    };
    let alert_task = tokio::spawn(alerts::evaluator::run(
        store.registry(),
        store.fleet(),
        store.webhooks(),
        store.task_health(),
        store.alerts(),
        alert_channel,
        SystemClock,
        alert_thresholds,
        alert_interval,
        shutdown_signal(),
    ));

    // The scheduled-publish activator (ADR-0077, Track M3): applies effective-dated publishes (the
    // Tết-menu case) when their time arrives, through the same config tree the immediate publishes use.
    let scheduled_publish_interval = Duration::from_secs(config.scheduled_publish_interval_secs);
    tracing::info!(
        interval_secs = config.scheduled_publish_interval_secs,
        "scheduled-publish activator started"
    );
    let scheduled_publish_task = tokio::spawn(pos_cloud::scheduling::run(
        store.scheduled_publishes(),
        store.config_trees(),
        store.task_health(),
        SystemClock,
        scheduled_publish_interval,
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

    // The OTA artifact route exists only where an object store is configured (ADR-0088). A cloud that
    // ships no edge releases needs none, and the alternative — a route answering with an empty
    // success — would tell a store it is up to date when nothing was ever published. An absent
    // endpoint is a `404`, which the adapter already reads as "the cloud publishes no such release".
    let artifacts = match config.artifacts.as_ref() {
        None => {
            tracing::info!(
                "no [artifacts] configured; the OTA artifact route is off and stores cannot fetch releases"
            );
            None
        }
        Some(artifacts) => match S3Blobs::new(
            &artifacts.endpoint,
            &artifacts.bucket,
            &artifacts.region,
            &artifacts.access_key_id,
            &artifacts.secret_access_key,
        ) {
            Ok(blobs) => Some(blobs),
            // Configured but unusable is a boot-time mistake worth being loud about, and not worth
            // refusing to start over: selling, config delivery and reporting are unaffected, and a
            // cloud that will not boot because OTA is misconfigured takes the fleet's dashboards
            // down with it.
            Err(error) => {
                tracing::error!(%error, "the [artifacts] endpoint is unusable; the OTA artifact route is off");
                None
            }
        },
    };

    // The reconciliation diff (ADR-0040), device-onboarding (ADR-0041), translation-grid (ADR-0043)
    // and activation-exchange (ADR-0050/0051) endpoints carry their own state, so they are merged in
    // rather than threaded through CloudApp.
    // Pulled out before `router` takes ownership: the intake sub-router and the `/sync` throttle
    // layer both need a handle, and the whole point of one process holding one limiter each is that
    // every route throttles against the same counter (roadmap **Q5**).
    let orders_limiter = app.orders_rate_limiter();
    let sync_throttle = app.sync_throttle();
    let service = http::router(app)
        .merge(http::reconcile_router(
            store.reconcile(),
            store.admin(),
            SystemClock,
            config.internal_shared_secret.clone(),
            // The store-facing reconcile route authenticates the box by its scoped key, exactly as
            // the config pull and the OTA report beside it do — a store is off the cloud's private
            // network, so `/internal` was never reachable from one (production-readiness R3).
            store.api_keys(),
        ))
        .merge(http::ota_report_router(
            store.config_trees(),
            SystemClock,
            // The store-facing report route authenticates the box by its scoped key, so a report
            // carries a tenant the cloud established rather than one the body claimed
            // (ADR-0097). The `/internal` route beside it keeps the shared secret.
            store.api_keys(),
            config.internal_shared_secret.clone(),
        ))
        // Both halves of R2's artifact hosting, and both need the object store: the store-facing
        // route a box downloads from, and the `/admin` route a release is uploaded to. Absent an
        // `[artifacts]` block there is nowhere for bytes to live, so both are honestly absent rather
        // than present and answering `503`.
        .merge(match artifacts {
            Some(blobs) => http::ota_artifact_router(
                store.api_keys(),
                SystemClock,
                blobs.clone(),
                store.releases(),
            )
            .merge(http::release_admin_router(
                blobs,
                store.releases(),
                store.admin(),
                SystemClock,
                Arc::clone(&audit),
            )),
            None => Router::new(),
        })
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
        // Device publish (ADR-0100, C2 slice 2b): compile the store's *approved* printers and kitchen
        // displays into the `devices` config node, so the edge learns where they are through the
        // config-pull it already runs — and still knows after a reboot with the WAN down, because
        // that node is persisted locally and restored at boot.
        .merge(http::device_publish_router(
            store.device_proposals(),
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
        // Campaigns (ADR-0077, Track M3): author a tenant's promotions over the finished pricing
        // engine (per-campaign CRUD, behind console.campaigns.manage, audited by summary).
        .merge(http::campaign_router(
            store.campaigns(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Inventory (ADR-0079, Track M6): author a tenant's ingredients, per-item recipes (BOM), and
        // supplier references (per-record CRUD, behind console.inventory.manage, audited by summary —
        // never the recipe amounts). The composed `inventory` node publish is a separate route.
        .merge(http::inventory_router(
            store.inventory(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Vouchers (ADR-0077, Track M3): mint and list the distributable codes a voucher-kind
        // campaign redeems (behind console.campaigns.manage, audited by count only).
        .merge(http::voucher_router(
            store.vouchers(),
            store.campaigns(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Scheduled publishes (ADR-0077, Track M3): schedule the campaigns node to publish to a store
        // at a future instant (the Tết-menu case), list a store's pending publishes, and cancel one.
        // A background activator applies them at their time.
        .merge(http::scheduled_publish_router(
            store.scheduled_publishes(),
            store.campaigns(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Media (ADR-0075, Track M5): upload an image → re-encode → store two bounded renditions;
        // serve, list, and delete them.
        .merge(http::media_router(
            store.media(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Subject-request tooling (ADR-0076): per-subject PDPD/GDPR lookup / export / erase over the
        // subject store, owner-only and audited — the Data Protection contact's deliberate instrument.
        .merge(http::subjects_router(
            store.subjects(),
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
        // Campaign publish (ADR-0077, Track M3): assemble the tenant's authored campaigns into the
        // store's `campaigns` config node, so the edge holds them for its pricing engine.
        .merge(http::config_campaigns_router(
            store.campaigns(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Inventory publish (ADR-0079, Track M6): assemble the tenant's authored ingredients, recipes,
        // and suppliers into the store's `inventory` config node, so the edge builds its RecipeBook and
        // auto-86 thresholds.
        .merge(http::config_inventory_router(
            store.inventory(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // OTA rollout levers (ADR-0078, Track O3): publish a `fleet_update` rollout or engage its
        // kill switch from typed fields, instead of hand-editing the config node.
        .merge(http::ota_config_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
            // The promote guard's source of truth: a rollout naming a version nobody hosts is
            // refused here rather than discovered as a fleet-wide `404` (ADR-0088 Amendment 2).
            store.releases(),
        ))
        // Countries & locales (ADR-0074, Track M4): the compiled country modules surfaced as
        // read-only master data — the currency picker and the translation grid's locale catalogue.
        .merge(http::country_router(
            &countries::registry(),
            store.admin(),
            SystemClock,
        ))
        // Locale publish (ADR-0074, Track M4): a store's currency, timezone, and business-date cutoff
        // as the `locale` config node the edge applies (killing the hardcoded UTC/04:00 bootstrap).
        .merge(http::config_locale_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(&audit),
        ))
        // Channels & tender (ADR-0080, Track M7): author which sales channels a store accepts and
        // which payment methods it takes, as the `channels` and `tender` settings nodes; the edge
        // applies each as a policy gate. Publish behind console.config.publish, audited.
        .merge(http::config_channels_router(
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
            orders_limiter,
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
    // The `/sync` budget, over the fully-composed service (roadmap **Q5**). Here rather than inside
    // `http::router` because the store-facing surface spans more than one sub-router — the relay's
    // `/sync/.../orders` routes are merged in above — and a layer applied to only one of them would
    // leave the busiest path unthrottled. The layer passes every other prefix straight through, so
    // `/admin`, `/v1` and the console SPA are untouched.
    let service = service.layer(axum::middleware::from_fn_with_state(
        sync_throttle,
        http::throttle_sync,
    ));
    // The console security headers over the **fully-composed** service (ADR-0067 slice 5,
    // production-readiness **S3**). `http::router` layers the same middleware over its own routes, and
    // a comment there claimed this line already existed — it did not, so the console's own document,
    // its assets, and every `/admin` sub-router merged in above (devices, registry, people, floor,
    // catalog, audit, fleet, alerts, …) were served with no `Content-Security-Policy`, no
    // `X-Frame-Options` and no `Referrer-Policy`. The SPA fallback is the one that matters most: it is
    // the document a browser renders, and the inner layer never reached it because `.fallback` is
    // added out here.
    //
    // Outermost on purpose, so it also covers the throttle's own `429` and any rejection a layer
    // produces before a route is matched. The headers are `insert`ed, so the two layers agree rather
    // than stacking.
    let service = service.layer(axum::middleware::from_fn(http::security_headers));
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
    scheduled_publish_task.abort();
    let _ = scheduled_publish_task.await;
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
