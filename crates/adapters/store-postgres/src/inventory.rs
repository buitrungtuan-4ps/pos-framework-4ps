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

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// One authored inventory record as stored: its id (a ULID string) within its `(tenant, kind)`, the
/// record document as JSON text, and the row version the read saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryRow {
    /// The record's id within its tenant and kind (a ULID string).
    pub entity_id: String,
    /// The whole authored record as JSON text, as stored in the `doc` jsonb column.
    pub doc_json: String,
    /// `xmin` as text — the row's version (ADR-0094), carried on the read so a caller can hand it
    /// back to [`update_at`](PostgresInventory::update_at). Opaque above this adapter.
    pub version: String,
}

/// The columns every read returns, in a stable order matching [`inventory_row`].
const INVENTORY_COLUMNS: &str = "entity_id, doc::text, xmin::text";

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
                &format!(
                    "SELECT {INVENTORY_COLUMNS} FROM inventory_items \
                     WHERE tenant_id = $1 AND kind = $2 ORDER BY entity_id"
                ),
                &[&tenant_id, &kind],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(inventory_row).collect())
    }

    /// One record by `(kind, entity_id)`, or `None` if the tenant has none.
    ///
    /// `(tenant_id, kind, entity_id)` is the table's primary key (migration 0037), so this is an
    /// index lookup of one row — which is the point. The nine `/admin/inventory` handlers that need
    /// one record used to read the tenant's whole list of that kind and scan it in the cloud, which
    /// grew with the tenant's catalogue on every read, edit and delete of a single ingredient.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        kind: &str,
        entity_id: &str,
    ) -> Result<Option<InventoryRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {INVENTORY_COLUMNS} FROM inventory_items \
                     WHERE tenant_id = $1 AND kind = $2 AND entity_id = $3"
                ),
                &[&tenant_id, &kind, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(inventory_row))
    }

    /// Inserts a record, refusing if one already holds its `(kind, entity_id)`.
    ///
    /// `ON CONFLICT DO NOTHING ... RETURNING` makes this one round trip: on a conflict nothing is
    /// written and no row comes back, so `None` *is* the "already taken" answer, with no window
    /// between a check and the write.
    ///
    /// The `$4::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the same
    /// reason the campaign and config-tree tables cast their bound documents.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn insert(
        &self,
        tenant_id: &str,
        kind: &str,
        entity_id: &str,
        doc_json: &str,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let inserted = connection
            .query_opt(
                "INSERT INTO inventory_items (tenant_id, kind, entity_id, doc) \
                 VALUES ($1, $2, $3, $4::text::jsonb) \
                 ON CONFLICT (tenant_id, kind, entity_id) DO NOTHING \
                 RETURNING xmin::text",
                &[&tenant_id, &kind, &entity_id, &doc_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted.map(|row| row.get(0)))
    }

    /// Replaces a record's document, only at `expected`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn update_at(
        &self,
        tenant_id: &str,
        kind: &str,
        entity_id: &str,
        doc_json: &str,
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE inventory_items SET doc = $4::text::jsonb, updated_at = now() \
                 WHERE tenant_id = $1 AND kind = $2 AND entity_id = $3 AND xmin::text = $5 \
                 RETURNING xmin::text",
                &[&tenant_id, &kind, &entity_id, &doc_json, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt(
                "SELECT 1 FROM inventory_items \
                 WHERE tenant_id = $1 AND kind = $2 AND entity_id = $3",
                &[&tenant_id, &kind, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
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

/// Reads one queried row into an [`InventoryRow`]. The column order matches [`INVENTORY_COLUMNS`].
///
/// Shared by the list and the single-row read so the two cannot disagree about which column is
/// which — the shape of bug a hand-written `row.get(1)` in each would eventually produce.
fn inventory_row(row: &tokio_postgres::Row) -> InventoryRow {
    InventoryRow {
        entity_id: row.get(0),
        doc_json: row.get(1),
        version: row.get(2),
    }
}
