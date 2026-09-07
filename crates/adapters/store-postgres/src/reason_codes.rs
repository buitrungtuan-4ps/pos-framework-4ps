// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reason-code authoring table over PostgreSQL
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md), roadmap B2.2).
//!
//! One row per `(tenant, entity_id)`; the `doc` column is the whole authored record (a wire
//! `PublishedReasonCode`, `active` and `applies_to` included) held as `jsonb` (`reason_codes`,
//! migration 0060). This adapter keeps only the SQL and hands back the raw JSON text; `pos-cloud`
//! implements its `ReasonCodeStore` seam over this type and does the (de)serialisation, so no
//! cloud-domain type leaks into the adapter — the same split `inventory` and the config-tree tables
//! use. Tenant scoping is an explicit `WHERE tenant_id = $1` (the cloud connects as the trusted pool
//! owner, which bypasses RLS; the migration's policy is the second line).
//!
//! No `kind` column, unlike `inventory_items`: there is one entity here, and a discriminator with a
//! single value is a column that only ever lies about being useful.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// One authored reason code as stored: its id (a ULID string), the record document as JSON text, and
/// the row version the read saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReasonCodeRow {
    /// The reason code's id (a ULID string).
    pub entity_id: String,
    /// The whole authored record as JSON text, as stored in the `doc` jsonb column.
    pub doc_json: String,
    /// `xmin` as text — the row's version (ADR-0094), carried on the read so a caller can hand it
    /// back to [`update_at`](PostgresReasonCodes::update_at). Opaque above this adapter.
    pub version: String,
}

/// The columns every read returns, in a stable order matching [`reason_code_row`].
const REASON_CODE_COLUMNS: &str = "entity_id, doc::text, xmin::text";

/// The reason-code store over a shared pool. Built by
/// [`PostgresStore::reason_codes`](crate::PostgresStore::reason_codes).
#[derive(Clone, Debug)]
pub struct PostgresReasonCodes {
    pool: Pool,
}

impl PostgresReasonCodes {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Lists a tenant's reason codes, oldest first (id order is creation order for a ULID).
    ///
    /// Retired entries are included: the console has to show what is out of service in order to
    /// bring it back, and a publish needs them so a till resolves the same ids the audit trail
    /// names. Filtering `active` here would make a retired reason invisible *and* unresolvable.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<ReasonCodeRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {REASON_CODE_COLUMNS} FROM reason_codes \
                     WHERE tenant_id = $1 ORDER BY entity_id"
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(reason_code_row).collect())
    }

    /// One reason code by id, or `None` if the tenant has none.
    ///
    /// `(tenant_id, entity_id)` is the table's primary key (migration 0060), so this is an index
    /// lookup of one row rather than a list read the cloud then scans.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        entity_id: &str,
    ) -> Result<Option<ReasonCodeRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {REASON_CODE_COLUMNS} FROM reason_codes \
                     WHERE tenant_id = $1 AND entity_id = $2"
                ),
                &[&tenant_id, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(reason_code_row))
    }

    /// Inserts a reason code, refusing if one already holds its id.
    ///
    /// `ON CONFLICT DO NOTHING ... RETURNING` makes this one round trip: on a conflict nothing is
    /// written and no row comes back, so `None` *is* the "already taken" answer, with no window
    /// between a check and the write.
    ///
    /// The `$3::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the
    /// same reason the inventory and config-tree tables cast their bound documents.
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
                "INSERT INTO reason_codes (tenant_id, entity_id, doc) \
                 VALUES ($1, $2, $3::text::jsonb) \
                 ON CONFLICT (tenant_id, entity_id) DO NOTHING \
                 RETURNING xmin::text",
                &[&tenant_id, &entity_id, &doc_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted.map(|row| row.get(0)))
    }

    /// Replaces a reason code's document, only at `expected`.
    ///
    /// This is also the retire and restore path: `active` is a field on the document, so taking an
    /// entry out of service goes through the same version check as any other edit and two managers
    /// cannot silently undo one another.
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
                "UPDATE reason_codes SET doc = $3::text::jsonb, updated_at = now() \
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
                "SELECT 1 FROM reason_codes WHERE tenant_id = $1 AND entity_id = $2",
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

    /// Removes a reason code by id. Removing one that does not exist is not an error.
    ///
    /// For the entry created by mistake and never cited. Anything an event may already name is
    /// *retired* instead (`active: false` through [`update_at`](Self::update_at)) — a historic
    /// `sales.order_line.voided` names the id forever, and a deleted row makes that event
    /// unresolvable.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete(&self, tenant_id: &str, entity_id: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "DELETE FROM reason_codes WHERE tenant_id = $1 AND entity_id = $2",
                &[&tenant_id, &entity_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}

/// Reads one queried row into a [`ReasonCodeRow`]. The column order matches
/// [`REASON_CODE_COLUMNS`].
///
/// Shared by the list and the single-row read so the two cannot disagree about which column is
/// which — the shape of bug a hand-written `row.get(1)` in each would eventually produce.
fn reason_code_row(row: &tokio_postgres::Row) -> ReasonCodeRow {
    ReasonCodeRow {
        entity_id: row.get(0),
        doc_json: row.get(1),
        version: row.get(2),
    }
}
