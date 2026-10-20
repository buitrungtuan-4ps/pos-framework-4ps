// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The steps of the cloud's boot, in the order `run` takes them: the configuration, then the
//! store.

use pos_cloud::CloudConfig;
use pos_cloud::clock::SystemClock;
use store_postgres::PostgresStore;

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
) -> Result<(PostgresStore, pos_cloud::qr_ordering::OneSwitch), Box<dyn std::error::Error>> {
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
