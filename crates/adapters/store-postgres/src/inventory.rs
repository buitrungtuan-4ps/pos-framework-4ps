// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The inventory authoring table over PostgreSQL (Track M6, [ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md)).
//!
//! One row per `(tenant, kind, entity_id)`; the `doc` column is the whole authored record (a wire
//! `PublishedIngredient` / `PublishedRecipe` / `PublishedSupplier`) held as `jsonb` (`inventory_items`,
//! migration 0037). The three entity kinds share one shape, so they share one table discriminated by
//! `kind`. This adapter keeps only the SQL and hands back the raw JSON text; `pos-cloud` implements its
//! `InventoryStore` seam over this type and does the (de)serialisation, so no cloud-domain type leaks
//! into the adapter — the same split `campaigns` and the config-tree tables use. Tenant scoping is an
//! explicit `WHERE tenant_id = $1` (the cloud connects as the trusted pool owner, which bypasses RLS;
//! the migration's policy is the second line).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One authored inventory record as stored: its id (a ULID string) within its `(tenant, kind)`, and the
/// record document as JSON text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryRow {
    /// The record's id within its tenant and kind (a ULID string).
    pub entity_id: String,
    /// The whole authored record as JSON text, as stored in the `doc` jsonb column.
    pub doc_json: String,
}

/// The inventory store over a shared pool. Built by
/// [`PostgresStore::inventory`](crate::PostgresStore::inventory).
#[derive(Clone, Debug)]
pub struct PostgresInventory {
    pool: Pool,
}

impl PostgresInventory {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Lists a tenant's records of one `kind`, oldest first (id order is creation order for a ULID).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str, kind: &str) -> Result<Vec<InventoryRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT entity_id, doc::text FROM inventory_items \
                 WHERE tenant_id = $1 AND kind = $2 ORDER BY entity_id",
                &[&tenant_id, &kind],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| InventoryRow {
                entity_id: row.get(0),
                doc_json: row.get(1),
            })
            .collect())
    }

    /// Creates a record, or replaces the one that already has its `(kind, entity_id)`.
    ///
    /// The `$4::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the same
    /// reason the campaign and config-tree tables cast their bound documents.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn upsert(
        &self,
        tenant_id: &str,
        kind: &str,
        entity_id: &str,
        doc_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO inventory_items (tenant_id, kind, entity_id, doc) \
                 VALUES ($1, $2, $3, $4::text::jsonb) \
                 ON CONFLICT (tenant_id, kind, entity_id) \
                 DO UPDATE SET doc = EXCLUDED.doc, updated_at = now()",
                &[&tenant_id, &kind, &entity_id, &doc_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Removes a record by `(kind, entity_id)`. Removing one that does not exist is not an error.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete(
        &self,
        tenant_id: &str,
        kind: &str,
        entity_id: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM inventory_items WHERE tenant_id = $1 AND kind = $2 AND entity_id = $3",
                &[&tenant_id, &kind, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
