// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The translation-grid table over PostgreSQL (P7, [ADR-0043](../../../docs/adr/0043-translation-grid.md)).
//!
//! One row per tenant; the `grid` column is the whole `key → { locale → string }` map held as
//! `jsonb`. This adapter keeps only the SQL and hands back the raw JSON text; `pos-cloud` implements
//! its `TranslationStore` seam over this type and does the grid (de)serialisation, so no cloud-domain
//! type leaks into the adapter — the same split the config tree and rollup tables use.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The translation store over a shared pool. Built by
/// [`PostgresStore::translations`](crate::PostgresStore::translations).
#[derive(Clone, Debug)]
pub struct PostgresTranslations {
    pool: Pool,
}

impl PostgresTranslations {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Loads a tenant's grid as the raw JSON text, or `None` if the tenant has no row yet.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn load_grid(&self, tenant_id: &str) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT grid::text FROM translations WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Upserts a tenant's grid (the raw grid JSON).
    ///
    /// The `$2::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the
    /// same reason the config-tree and rollup tables cast their bound documents.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn save_grid(&self, tenant_id: &str, grid_json: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO translations (tenant_id, grid) \
                 VALUES ($1, $2::text::jsonb) \
                 ON CONFLICT (tenant_id) \
                 DO UPDATE SET grid = EXCLUDED.grid, updated_at = now()",
                &[&tenant_id, &grid_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
