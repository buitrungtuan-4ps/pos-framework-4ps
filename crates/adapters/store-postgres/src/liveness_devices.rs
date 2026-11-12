// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which package version each paired till last read, as a store's heartbeat says, on its liveness
//! row (roadmap-v3 B7.3, migration 0094).

use pos_ports::PortError;
use pos_proto::ids::{StoreId, TenantId};

use crate::config_trees::PostgresConfigTrees;
use crate::store::{pool_unavailable, unavailable};

impl PostgresConfigTrees {
    /// Replaces the tills' reads on a store's liveness row with `reads_json`, a JSON array the
    /// caller has bounded, stamped `seen_at_ms`. A store with no row yet is left alone: the heartbeat
    /// that came with it creates the row first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_device_reads_beat(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        seen_at_ms: i64,
        reads_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE store_liveness SET device_reads = $3::text::jsonb, \
                 device_reads_reported_at = $4 WHERE tenant_id = $1 AND store_id = $2",
                &[
                    &tenant.to_string(),
                    &store_id.to_string(),
                    &reads_json,
                    &seen_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
