// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The rollup store over PostgreSQL.

use store_postgres::PostgresRollups;

use pos_proto::ids::{StoreId, TenantId};

use crate::dashboard::projection::{RollupError, RollupStore, StoredRollups};

impl RollupStore for PostgresRollups {
    async fn load(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<StoredRollups, RollupError> {
        match self
            .load_state(tenant, store_id)
            .await
            .map_err(|error| RollupError::store(error.to_string()))?
        {
            Some(json) => serde_json::from_str(&json).map_err(|error| {
                RollupError::store(format!("decoding the stored rollup state failed: {error}"))
            }),
            // A store with no row yet reads as an empty, cursor-less rollup — the same default the
            // projector starts from, so a first read and a first projection agree.
            None => Ok(StoredRollups::default()),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        rollups: &StoredRollups,
    ) -> Result<(), RollupError> {
        let json = serde_json::to_string(rollups).map_err(|error| {
            RollupError::store(format!("encoding the rollup state failed: {error}"))
        })?;
        self.save_state(tenant, store_id, &json)
            .await
            .map_err(|error| RollupError::store(error.to_string()))
    }
}
