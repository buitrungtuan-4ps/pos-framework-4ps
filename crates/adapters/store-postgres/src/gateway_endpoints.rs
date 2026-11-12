// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's gateway endpoints over PostgreSQL
//! ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)).
//!
//! One row per `(tenant, entity_id)`; the `doc` column is the whole endpoint record as `jsonb`
//! (`gateway_endpoints`, migration 0098), its secret headers already sealed by `pos-cloud`. The same
//! SQL as [`crate::connections`] over its own table: the adapter hands back raw JSON text, and
//! `pos-cloud` does the (de)serialisation and the sealing.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// One endpoint as stored: its id (a ULID string), the record document as JSON text, and the row
/// version the read saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayEndpointRow {
    /// The endpoint's id (a ULID string).
    pub entity_id: String,
    /// The whole record as JSON text, as stored in the `doc` jsonb column. Its secret fields are
    /// sealed values, never plaintext.
    pub doc_json: String,
    /// `xmin` as text — the row's version (ADR-0094), handed back to
    /// [`update_at`](PostgresGatewayEndpoints::update_at). Opaque above this adapter.
    pub version: String,
    /// When the row was last written, in Unix milliseconds.
    pub updated_at_ms: i64,
}

/// The columns every read returns, in the order [`endpoint_row`] reads them.
const ENDPOINT_COLUMNS: &str =
    "entity_id, doc::text, xmin::text, (EXTRACT(EPOCH FROM updated_at) * 1000)::bigint";

/// The endpoint store over a shared pool. Built by
/// [`PostgresStore::gateway_endpoints`](crate::PostgresStore::gateway_endpoints).
#[derive(Clone, Debug)]
pub struct PostgresGatewayEndpoints {
    pool: Pool,
}

impl PostgresGatewayEndpoints {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Lists a tenant's endpoints, oldest first (id order is creation order for a ULID).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<GatewayEndpointRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {ENDPOINT_COLUMNS} FROM gateway_endpoints \
                     WHERE tenant_id = $1 ORDER BY entity_id"
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(endpoint_row).collect())
    }

    /// One endpoint by id, or `None` if the tenant has none. A primary-key lookup.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        entity_id: &str,
    ) -> Result<Option<GatewayEndpointRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {ENDPOINT_COLUMNS} FROM gateway_endpoints \
                     WHERE tenant_id = $1 AND entity_id = $2"
                ),
                &[&tenant_id, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(endpoint_row))
    }

    /// Inserts a endpoint, refusing if one already holds its id: `None` is "already taken", in
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
                "INSERT INTO gateway_endpoints (tenant_id, entity_id, doc) \
                 VALUES ($1, $2, $3::text::jsonb) \
                 ON CONFLICT (tenant_id, entity_id) DO NOTHING \
                 RETURNING xmin::text",
                &[&tenant_id, &entity_id, &doc_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted.map(|row| row.get(0)))
    }

    /// Replaces a endpoint's document, only at `expected`, so two admins editing one endpoint's
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
                "UPDATE gateway_endpoints SET doc = $3::text::jsonb, updated_at = now() \
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
                "SELECT 1 FROM gateway_endpoints WHERE tenant_id = $1 AND entity_id = $2",
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

    /// Removes a endpoint and its sealed secrets. Removing one that does not exist is not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete(&self, tenant_id: &str, entity_id: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM gateway_endpoints WHERE tenant_id = $1 AND entity_id = $2",
                &[&tenant_id, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}

/// Reads one queried row into a [`GatewayEndpointRow`], in [`ENDPOINT_COLUMNS`] order — shared by the
/// list and the single-row read so the two cannot disagree about which column is which.
fn endpoint_row(row: &tokio_postgres::Row) -> GatewayEndpointRow {
    GatewayEndpointRow {
        entity_id: row.get(0),
        doc_json: row.get(1),
        version: row.get(2),
        updated_at_ms: row.get(3),
    }
}
