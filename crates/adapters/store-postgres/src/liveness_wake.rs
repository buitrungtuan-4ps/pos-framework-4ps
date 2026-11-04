// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a store's heartbeat says about config delivery, on its liveness row
//! ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md), migration 0091).

use pos_ports::PortError;
use pos_proto::ids::{StoreId, TenantId};

use crate::config_trees::PostgresConfigTrees;
use crate::store::{pool_unavailable, unavailable};

/// What one heartbeat said about config delivery, as the row holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigWakeRow<'a> {
    /// `CONFIG_DELIVERY_WAKE` or `CONFIG_DELIVERY_POLL`.
    pub delivery: &'a str,
    /// How many wake-to-apply times the figures are drawn from.
    pub samples: i32,
    /// Their 95th percentile, in milliseconds.
    pub p95_ms: Option<i64>,
    /// The longest of them.
    pub max_ms: Option<i64>,
}

impl PostgresConfigTrees {
    /// Replaces the config-delivery report on a store's liveness row, stamped `seen_at_ms`. A store
    /// with no row yet is left alone: the heartbeat that came with it creates the row first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_config_wake_beat(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        seen_at_ms: i64,
        beat: ConfigWakeRow<'_>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE store_liveness SET config_delivery = $3, config_wake_samples = $4, \
                 config_wake_p95_ms = $5, config_wake_max_ms = $6, config_wake_reported_at = $7 \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &beat.delivery,
                    &beat.samples,
                    &beat.p95_ms,
                    &beat.max_ms,
                    &seen_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
