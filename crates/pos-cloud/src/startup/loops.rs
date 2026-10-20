// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's background loops, each started by its own step, in the order `run` starts them:
//! the ingest cursor, the rollup projector, the retention cron, the webhook dispatcher, the alert
//! evaluator, the scheduled-publish activator, the people republisher, and the metrics heartbeat.
//! Each says at start whether it is on, and each drains on the process's shutdown.

use core::time::Duration;
use std::sync::Arc;

use blob_garage::S3Blobs;
use link_nats::NatsConsumer;
use metrics_vm::VmMetrics;
use pos_cloud::alerts::{self, AlertThresholds};
use pos_cloud::archive_retention;
use pos_cloud::audit::AuditRecorder;
use pos_cloud::clock::SystemClock;
use pos_cloud::people_republish;
use pos_cloud::retention::{self, RetentionPolicy};
use pos_cloud::webhook::{self, TlsWebhookSender};
use pos_cloud::{Cloud, CloudConfig, cursor, dashboard};
use pos_ports::PortError;
use store_postgres::PostgresStore;
use tokio::task::JoinHandle;

use crate::{consumer_config, shutdown_signal};

/// The production ingest feed, if configured, and the probe the alert evaluator reads the stream
/// through.
pub(crate) async fn ingest_cursor(
    config: &CloudConfig,
    cloud: &Cloud<PostgresStore>,
) -> Result<
    (
        Option<JoinHandle<Result<(), PortError>>>,
        Option<alerts::probe::CursorProbe>,
    ),
    Box<dyn std::error::Error>,
> {
    // The production ingest feed, if configured: a durable NATS cursor driving the same
    // `Cloud::ingest` the HTTP re-push target uses. Absent config leaves the cursor off, so the
    // cloud still serves and still ingests re-pushes — useful for a reconciliation-only deployment.
    // The probe shares this connection: the fleet stream's ceiling is reconciled and its fill read
    // on the alert tick (ADR-0087 Amendment 2), and opening a second connection to ask the same
    // broker the same question would be a second thing to configure and to fail.
    let mut stream_probe = None;
    let cursor_task = if let Some(nats) = config.nats.clone() {
        let consumer = Arc::new(
            NatsConsumer::connect(&nats.url, consumer_config(&nats))
                .await
                .map_err(|error| error.to_string())?,
        );
        tracing::info!(
            stream = %nats.stream,
            durable = %nats.durable,
            max_messages = nats.max_messages,
            max_bytes = nats.max_bytes,
            "ingest cursor started"
        );
        stream_probe = Some(alerts::probe::CursorProbe::new(
            Arc::clone(&consumer),
            nats.max_messages,
            nats.max_bytes,
        ));
        Some(tokio::spawn(cursor::run(
            consumer,
            cloud.clone(),
            shutdown_signal(),
        )))
    } else {
        tracing::info!(
            "no [nats] config; ingest cursor off (reconciliation re-push only), and the fleet \
             stream's ceiling is left as the stores created it"
        );
        None
    };
    Ok((cursor_task, stream_probe))
}

/// The rollup projector, the single writer of the materialised rollup the `/v1` dashboard reads.
pub(crate) fn rollup_projector(config: &CloudConfig, store: &PostgresStore) -> JoinHandle<()> {
    // The rollup projector: the single writer of the materialised rollup the `/v1` dashboard reads.
    // Ingest only appends to the log; this sweeps the fleet on an interval and folds each store's
    // new events into its rollup, so a dashboard is never more than one interval stale. The store is
    // both the event log it reads and the catalog of stores it iterates.
    let projector_interval = Duration::from_secs(config.projector_interval_secs);
    tracing::info!(
        interval_secs = config.projector_interval_secs,
        "rollup projector started"
    );
    tokio::spawn(dashboard::projector::run(
        store.clone(),
        store.rollups(),
        store.clone(),
        SystemClock,
        store.task_health(),
        projector_interval,
        shutdown_signal(),
    ))
}

/// The retention / PII-masking cron, when a retention period is configured.
pub(crate) fn retention_cron(
    config: &CloudConfig,
    store: &PostgresStore,
) -> Option<JoinHandle<()>> {
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
    retention_task
}

/// The webhook dispatcher, which always runs.
pub(crate) fn webhook_dispatcher(config: &CloudConfig, store: &PostgresStore) -> JoinHandle<()> {
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
    tokio::spawn(webhook::runner::run(
        store.clone(),
        store.webhooks(),
        TlsWebhookSender::new(webhook_timeout),
        SystemClock,
        store.task_health(),
        webhook_interval,
        shutdown_signal(),
    ))
}

/// The alert evaluator, with the off-console channel when one is configured.
pub(crate) async fn alert_evaluator(
    config: &CloudConfig,
    store: &PostgresStore,
    stream_probe: Option<alerts::probe::CursorProbe>,
) -> JoinHandle<()> {
    // The dispatcher's delivery timeout, which the off-console channel shares.
    let webhook_timeout = Duration::from_secs(config.webhook_delivery_timeout_secs);
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
        print_agent_stalled_secs: config.alert_print_agent_stalled_secs,
    };
    tracing::info!(
        interval_secs = config.alert_eval_interval_secs,
        "alert evaluator started"
    );
    let alert_channel = alert_channel(config, webhook_timeout).await;
    tokio::spawn(alerts::evaluator::run(
        store.registry(),
        store.fleet(),
        store.webhooks(),
        store.task_health(),
        store.alerts(),
        alert_channel,
        stream_probe,
        SystemClock,
        alert_thresholds,
        alert_interval,
        shutdown_signal(),
    ))
}

/// The off-console alert channel, when `alert_webhook_url` and its secret are configured and the
/// URL passes the SSRF vetting.
async fn alert_channel(
    config: &CloudConfig,
    webhook_timeout: Duration,
) -> Option<alerts::WebhookAlertChannel<TlsWebhookSender>> {
    // The off-console channel (ADR-0073 slice 4), when one is configured. The URL is SSRF-vetted here
    // rather than per tick: a boot refusal is the right place to learn the ops endpoint resolves to a
    // private address, and re-vetting every minute would put a DNS lookup on the alert path.
    match (&config.alert_webhook_url, &config.alert_webhook_secret) {
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
    }
}

/// The scheduled-publish activator, which also settles releases.
pub(crate) fn scheduled_publish_activator(
    config: &CloudConfig,
    store: &PostgresStore,
) -> JoinHandle<()> {
    // The scheduled-publish activator (ADR-0077, Track M3): applies effective-dated publishes (the
    // Tết-menu case) when their time arrives, through the same config tree the immediate publishes use.
    // It also settles releases (ADR-0125): a release's state is the roll-up of these same rows, so the
    // loop that applies them is the one that re-derives it.
    let scheduled_publish_interval = Duration::from_secs(config.scheduled_publish_interval_secs);
    tracing::info!(
        interval_secs = config.scheduled_publish_interval_secs,
        "scheduled-publish activator started"
    );
    tokio::spawn(pos_cloud::scheduling::run(
        store.scheduled_publishes(),
        store.config_trees(),
        store.config_releases(),
        store.task_health(),
        SystemClock,
        scheduled_publish_interval,
        shutdown_signal(),
    ))
}

/// The people republisher, draining the stores whose people need publishing again.
pub(crate) fn people_republisher(
    store: &PostgresStore,
    audit: &Arc<dyn AuditRecorder>,
) -> JoinHandle<()> {
    // The people republisher (ADR-0158): a migration that changes what roles grant queues the stores
    // whose people have been published (`people_republishes`, migration 0083), and this drains the
    // queue, at once and then every few minutes, publishing each store's people again exactly as
    // **Publish** on People does, under the system actor. A store whose node is already current is
    // given no new version; one that cannot be published keeps its row for the next drain.
    tracing::info!(
        interval_secs = people_republish::DRAIN_INTERVAL.as_secs(),
        "people republisher started"
    );
    tokio::spawn(people_republish::run(
        store.people_republishes(),
        store.people(),
        store.config_trees(),
        Arc::clone(audit),
        store.task_health(),
        SystemClock,
        people_republish::DRAIN_INTERVAL,
        shutdown_signal(),
    ))
}

/// The monitoring profile's heartbeat, when `[metrics]` is configured.
pub(crate) fn metrics_heartbeat(config: &CloudConfig) -> Option<JoinHandle<()>> {
    // The optional monitoring profile (metrics-vm → VictoriaMetrics, ADR-0031): a sparse liveness
    // heartbeat off the sales path, gated by [metrics] and off by default. Per
    // `docs/capacity-and-reliability.md` the profile is off below ~50 stores in favour of sparse
    // sampling, so a pilot cell leaves it unset. The sink's own bounded queue drops under pressure,
    // so a slow or dead backend never touches trading.
    if let Some(metrics) = config.metrics.as_ref() {
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
    }
}

/// The store-archive retention sweep, where archiving is on at all.
pub(crate) fn archive_retention_sweep(
    config: &CloudConfig,
    store: &PostgresStore,
    artifacts: Option<&S3Blobs>,
) -> Option<JoinHandle<()>> {
    // The store-archive retention sweep (ADR-0124 slice 3). It needs the same object store the
    // archives were uploaded to, which is why it is spawned here rather than beside the subject
    // cron above — and only where archiving is on at all, because a sweep with nothing to sweep is
    // a daily query for no reason.
    //
    // Why it exists is not disk. A store's archive is its whole database, buyer details included
    // (ADR-0107), taken *before* the masking cron redacted them; capping the archive window below
    // the subject window is what makes that redaction eventually true in the copies too. The
    // configuration refuses to start with the two windows the wrong way round, so by the time this
    // spawns the ordering already holds.
    match (artifacts.cloned(), config.archive_key_secret.as_ref()) {
        (Some(blobs), Some(_secret)) => {
            let interval = Duration::from_secs(config.archive_retention_sweep_interval_secs);
            tracing::info!(
                archive_retention_days = config.archive_retention_days,
                interval_secs = config.archive_retention_sweep_interval_secs,
                "store-archive retention sweep started"
            );
            Some(tokio::spawn(archive_retention::run(
                store.archives(),
                blobs,
                archive_retention::ArchiveWindow::days(config.archive_retention_days),
                SystemClock,
                store.task_health(),
                interval,
                shutdown_signal(),
            )))
        }
        _other => None,
    }
}
