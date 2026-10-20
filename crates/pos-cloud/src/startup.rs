// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The steps of the cloud's boot, in the order `run` takes them: the configuration, the store,
//! then the application over it.

pub(super) mod loops;

use std::sync::Arc;

use pos_cloud::audit::AuditRecorder;
use pos_cloud::clock::SystemClock;
use pos_cloud::http::CloudApp;
use pos_cloud::qr_ordering::OneSwitch;
use pos_cloud::{Cloud, CloudConfig};
use store_postgres::{
    PostgresAdmin, PostgresApiKeys, PostgresConfigTrees, PostgresRollups, PostgresStore,
    PostgresWebhooks,
};

/// The HTTP application state `run` serves, over the store's PostgreSQL views.
pub(super) type App = CloudApp<
    PostgresStore,
    PostgresRollups,
    PostgresApiKeys,
    SystemClock,
    PostgresAdmin,
    PostgresConfigTrees,
    PostgresWebhooks,
>;

/// The boot configuration: the file `POS_CLOUD_CONFIG` names, parsed and range-checked.
pub(super) fn load_config() -> Result<CloudConfig, Box<dyn std::error::Error>> {
    let config_path = std::env::var("POS_CLOUD_CONFIG")
        .map_err(|_| "POS_CLOUD_CONFIG must name a configuration file")?;
    let config = CloudConfig::from_toml(&std::fs::read_to_string(&config_path)?)?;
    // Range checks serde cannot express (ADR-0090). Fail the boot rather than run with a value that
    // silently disables a control — a wrong `trusted_proxy_hops` is a wrong rate-limit key.
    config.validate()?;
    Ok(config)
}

/// The PostgreSQL store, connected and migrated, and QR ordering's one switch run over it.
///
/// QR ordering's one switch (ADR-0160 decision 5): once, before anything serves, every store's
/// switch is published from what it did. A store that could not be set is logged by id where it
/// failed; the run aborts the boot only when the stores cannot be listed or the run recorded.
pub(super) async fn open_store(
    config: &CloudConfig,
) -> Result<(PostgresStore, OneSwitch), Box<dyn std::error::Error>> {
    let store = PostgresStore::connect(&config.database_url).map_err(|error| error.to_string())?;
    store.migrate().await.map_err(|error| error.to_string())?;
    let one_switch = pos_cloud::qr_ordering::run_once(
        &store.registry(),
        &store.config_trees(),
        &store.data_migrations(),
        &SystemClock,
    )
    .await
    .map_err(|error| error.to_string())?;
    if let Some(report) = &one_switch.report {
        tracing::info!(
            published = report.published,
            unchanged = report.unchanged,
            unconfigured = report.unconfigured,
            failed = report.failed.len(),
            "set every store's QR ordering switch from what it did"
        );
    }
    Ok((store, one_switch))
}

/// The cloud's application layer over the store's one pool, with the store registry ingest stamps
/// each event's owner from, the anchor ledger, and the chain window.
pub(super) fn cloud(store: &PostgresStore) -> Cloud<PostgresStore> {
    // One pool, seven views of it: the event-store application layer, the materialised-rollup read
    // model the `/v1` dashboard answers from, the API-key store the `/v1` bearer check consults, the
    // super-admin store the `/admin` login and session guard use, the config-tree store the `/admin`
    // config routes author, the webhook-endpoint store the `/admin` webhook routes register into,
    // and the anchor ledger ingest files each store's published chain head in.
    // Stamped, not trusted: every ingested event's tenant and brand come from this registry lookup,
    // overwriting what the publishing box claimed
    // ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md), production-readiness **S2**).
    // The tenant is the column row-level isolation is defined on, and until this line every store in
    // the fleet stamped the same constant.
    let cloud = Cloud::with_store_owners(store.clone(), Arc::new(store.store_directory()));
    // Every ingested `store.chain.anchored` event is offered to the anchor ledger
    // ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4). A store's hash chain
    // catches careless editing and not fraud — the algorithm is in the source, so anyone holding the
    // shop's database file can rewrite a record and re-derive every later link. What makes the chain
    // mean something is this: a head recorded where the store cannot reach it.
    cloud
        .with_anchor_ledger(Arc::new(store.chain_anchors()))
        // And the log reader that recomputes the chain behind each accepted head
        // ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)). The anchor
        // closes recomputation; this is what closes truncation, because a store cut back and traded
        // on leaves two different events claiming one position and only the events show it.
        .with_chain_window(Arc::new(store.chain_anchors()))
}

/// The cloud's HTTP application state: the application layer, the store's views the routes read
/// and write, the audit recorder, and every limit and secret the configuration sets.
pub(super) fn cloud_app(
    cloud: &Cloud<PostgresStore>,
    store: &PostgresStore,
    config: &CloudConfig,
    audit: &Arc<dyn AuditRecorder>,
    one_switch: &OneSwitch,
) -> App {
    CloudApp::new(
        cloud.clone(),
        store.rollups(),
        store.api_keys(),
        SystemClock,
        store.admin(),
        store.config_trees(),
        store.webhooks(),
    )
    .with_audit(Arc::clone(audit))
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
    .with_internal_shared_secret(config.internal_shared_secret.clone())
    .with_qr_switch_since(one_switch.since)
}
