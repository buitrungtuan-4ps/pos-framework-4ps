// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's vendor connections over PostgreSQL
//! ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)).
//!
//! One row per `(tenant, entity_id)`; the `doc` column is the whole connection record as `jsonb`
//! (`integration_connections`, migration 0070), its secret fields already sealed by `pos-cloud`.
//! This adapter keeps only the SQL and hands back the raw JSON text; `pos-cloud` implements its
//! `ConnectionStore` seam over this type and does the (de)serialisation and the sealing, so neither
//! a cloud-domain type nor a key reaches the adapter — the split `reason_codes` uses. Tenant scoping
//! is an explicit `WHERE tenant_id = $1` (the cloud connects as the trusted pool owner, which
//! bypasses RLS; the migration's policy is the second line).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// One connection as stored: its id (a ULID string), the record document as JSON text, and the row
/// version the read saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionRow {
    /// The connection's id (a ULID string).
    pub entity_id: String,
    /// The whole record as JSON text, as stored in the `doc` jsonb column. Its secret fields are
    /// sealed values, never plaintext.
    pub doc_json: String,
    /// `xmin` as text — the row's version (ADR-0094), handed back to
    /// [`update_at`](PostgresConnections::update_at). Opaque above this adapter.
    pub version: String,
    /// When the row was last written, in Unix milliseconds.
    pub updated_at_ms: i64,
}

/// The columns every read returns, in the order [`connection_row`] reads them.
const CONNECTION_COLUMNS: &str =
    "entity_id, doc::text, xmin::text, (EXTRACT(EPOCH FROM updated_at) * 1000)::bigint";

/// The connection store over a shared pool. Built by
/// [`PostgresStore::connections`](crate::PostgresStore::connections).
#[derive(Clone, Debug)]
pub struct PostgresConnections {
    pool: Pool,
}

impl PostgresConnections {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Lists a tenant's connections, oldest first (id order is creation order for a ULID).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<ConnectionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
                     WHERE tenant_id = $1 ORDER BY entity_id"
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(connection_row).collect())
    }

    /// One connection by id, or `None` if the tenant has none. A primary-key lookup.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        entity_id: &str,
    ) -> Result<Option<ConnectionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {CONNECTION_COLUMNS} FROM integration_connections \
                     WHERE tenant_id = $1 AND entity_id = $2"
                ),
                &[&tenant_id, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(connection_row))
    }

    /// Inserts a connection, refusing if one already holds its id: `None` is "already taken", in
    /// one round trip with no window between a check and the write.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn insert(
        &self,
        tenant_id: &str,
        entity_id: &str,
        doc_json: &str,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let inserted = connection
            .query_opt(
                "INSERT INTO integration_connections (tenant_id, entity_id, doc) \
                 VALUES ($1, $2, $3::text::jsonb) \
                 ON CONFLICT (tenant_id, entity_id) DO NOTHING \
                 RETURNING xmin::text",
                &[&tenant_id, &entity_id, &doc_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted.map(|row| row.get(0)))
    }

    /// Replaces a connection's document, only at `expected`, so two admins editing one vendor's
    /// settings cannot silently undo one another.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn update_at(
        &self,
        tenant_id: &str,
        entity_id: &str,
        doc_json: &str,
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE integration_connections SET doc = $3::text::jsonb, updated_at = now() \
                 WHERE tenant_id = $1 AND entity_id = $2 AND xmin::text = $4 \
                 RETURNING xmin::text",
                &[&tenant_id, &entity_id, &doc_json, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt(
                "SELECT 1 FROM integration_connections WHERE tenant_id = $1 AND entity_id = $2",
                &[&tenant_id, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
    }

    /// Removes a connection and its sealed secrets. Removing one that does not exist is not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete(&self, tenant_id: &str, entity_id: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM integration_connections WHERE tenant_id = $1 AND entity_id = $2",
                &[&tenant_id, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}

/// Reads one queried row into a [`ConnectionRow`], in [`CONNECTION_COLUMNS`] order — shared by the
/// list and the single-row read so the two cannot disagree about which column is which.
fn connection_row(row: &tokio_postgres::Row) -> ConnectionRow {
    ConnectionRow {
        entity_id: row.get(0),
        doc_json: row.get(1),
        version: row.get(2),
        updated_at_ms: row.get(3),
    }
}
