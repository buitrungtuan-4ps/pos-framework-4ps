// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The migrations from 0089 on, in order, after [`PostgresStore::migrate`](crate::PostgresStore::migrate)'s
//! own: a module of their own so that one stays inside its size baseline. Each is forward-only and
//! idempotent, applied on every boot (ADR-0017).

use deadpool_postgres::Object;
use pos_ports::PortError;

use crate::store::unavailable;

const MIGRATION_0089: &str = include_str!("../migrations/0089_packages.sql");
const MIGRATION_0090: &str = include_str!("../migrations/0090_store_liveness_package.sql");
const MIGRATION_0091: &str = include_str!("../migrations/0091_store_liveness_config_wake.sql");
const MIGRATION_0092: &str = include_str!("../migrations/0092_custom_values.sql");
const MIGRATION_0093: &str = include_str!("../migrations/0093_custom_subjects.sql");
const MIGRATION_0094: &str = include_str!("../migrations/0094_store_liveness_device_reads.sql");
const MIGRATION_0095: &str = include_str!("../migrations/0095_terminal_device_class.sql");
const MIGRATION_0096: &str = include_str!("../migrations/0096_floor_area_parent.sql");
const MIGRATION_0097: &str = include_str!("../migrations/0097_webhook_event_types.sql");

/// Applies 0089 onwards on `connection`, one `batch_execute` each, as `store.rs`'s wiring tests read.
pub(crate) async fn migrate_from_0089(connection: &Object) -> Result<(), PortError> {
    connection
        .batch_execute(MIGRATION_0089)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0090)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0091)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0092)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0093)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0094)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0095)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0096)
        .await
        .map_err(unavailable)?;
    connection
        .batch_execute(MIGRATION_0097)
        .await
        .map_err(unavailable)
}
