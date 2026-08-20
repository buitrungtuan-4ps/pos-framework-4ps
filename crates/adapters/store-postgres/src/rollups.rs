// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The materialised-rollup table over PostgreSQL (P7, [ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
//!
//! One row per `(tenant, store)`; the row's `state` column is the whole `StoredRollups` — the
//! projector's cursor and the per-trading-day counts — held as `jsonb`. This adapter keeps only the
//! SQL and hands back the raw JSON text; `pos-cloud` implements its `RollupStore` seam over this type
//! and does the `StoredRollups` (de)serialisation, so no cloud-domain type leaks into the adapter.
//!
//! Tenant isolation is the `(tenant_id, store_id)` key: a load names both, so it can only ever return
//! the caller's own tenant's row (the migration also enables RLS as a second line for a query role).

use deadpool_postgres::Pool;

use pos_ports::PortError;
use pos_proto::ids::{StoreId, TenantId};

use crate::store::{pool_unavailable, unavailable};

/// The rollup read model over a shared pool. Built by [`PostgresStore::rollups`](crate::PostgresStore::rollups).
#[derive(Clone, Debug)]
pub struct PostgresRollups {
    pool: Pool,
}

impl PostgresRollups {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Loads a store's rollup state as the raw JSON text of a `StoredRollups`, or `None` if the
    /// `(tenant, store)` pair has no row yet.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn load_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT state::text FROM rollups WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Upserts a store's rollup state (the raw `StoredRollups` JSON).
    ///
    /// The `$3::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the
    /// same reason `store.rs` binds envelopes as `$5::text::json`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn save_state(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        state_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO rollups (tenant_id, store_id, state) \
                 VALUES ($1, $2, $3::text::jsonb) \
                 ON CONFLICT (tenant_id, store_id) \
                 DO UPDATE SET state = EXCLUDED.state, updated_at = now()",
                &[&tenant.to_string(), &store_id.to_string(), &state_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
