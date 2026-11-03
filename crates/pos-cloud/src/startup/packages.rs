// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The settings routes' store, and the package catch-up worker built over the same stores
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md)).

use core::future::Future;
use std::sync::Arc;

use pos_cloud::audit::AuditRecorder;
use pos_cloud::clock::SystemClock;
use pos_cloud::{countries, http};
use store_postgres::PostgresStore;

/// The package catch-up hand-off and its worker, over the same stores the settings routes use: a
/// store that waited for an edge release is republished once it reports one (ADR-0189 decision 5).
pub(crate) fn package_catch_up(
    store: &PostgresStore,
    audit: &Arc<dyn AuditRecorder>,
) -> (
    http::PackageCatchUp,
    impl Future<Output = ()> + Send + 'static + use<>,
) {
    http::package_catch_up(
        &countries::registry(),
        settings_store(store),
        store.registry(),
        store.store_groups(),
        store.config_trees(),
        store.admin(),
        SystemClock,
        Arc::clone(audit),
    )
}

/// The settings routes' store: the settings, the packages they name and each store's release.
pub(super) fn settings_store(
    store: &PostgresStore,
) -> pos_cloud_config::package_pin::WithPackages<
    store_postgres::PostgresSettings,
    store_postgres::PostgresPackages,
    pos_cloud::fleet::FleetReleases<store_postgres::PostgresFleet>,
> {
    pos_cloud_config::package_pin::WithPackages::new(
        store.settings(),
        store.packages(),
        pos_cloud::fleet::FleetReleases(store.fleet()),
    )
}
