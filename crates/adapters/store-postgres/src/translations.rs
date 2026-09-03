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

use crate::store::{RowUpdate, pool_unavailable, unavailable};

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

    /// Loads a tenant's grid as the raw JSON text **and the version the row was read at**, or
    /// `None` if the tenant has no row yet.
    ///
    /// The version is `xmin::text`, the same opaque token every other conditional write in this
    /// adapter uses ([ADR-0094](../../../../docs/adr/0094-console-optimistic-concurrency.md)). The
    /// grid is a *collection* the console edits whole, but it is stored as one row per tenant, so
    /// the row's own system column versions it and no version table is needed — see
    /// [`Self::save_grid`].
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn load_grid(&self, tenant_id: &str) -> Result<Option<(String, String)>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT grid::text, xmin::text FROM translations WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| (row.get(0), row.get(1))))
    }

    /// Writes a tenant's grid (the raw grid JSON) **only if the row is still at `expected`**
    /// ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    ///
    /// The whole grid is one `jsonb` row keyed by tenant, which is what lets this stay on `xmin` and
    /// need no version table: a save *updates that row in place*, so its `xmin` moves and the next
    /// caller's stale token no longer matches. The tax-rate table next door cannot do this — it is
    /// many rows and a save deletes and reinserts them, destroying every `xmin` — which is why that
    /// one carries a version row and this one does not.
    ///
    /// The two cases are separate statements rather than one `ON CONFLICT` that papers over them:
    ///
    /// - `expected = None` — the caller read no row, so this must *create* one. `ON CONFLICT DO
    ///   NOTHING` returns zero rows if another save created it first, which is a
    ///   [`RowUpdate::VersionMismatch`], not a silent overwrite of that save.
    /// - `expected = Some(v)` — the row must still be at `v`. Zero rows means the version moved or
    ///   the row is gone, and the probe on the failure path separates them.
    ///
    /// The `$2::text::jsonb` cast pins the bound parameter's inference to `text` before jsonb, the
    /// same reason the config-tree and rollup tables cast their bound documents. The comparison is on
    /// `xmin::text` rather than a cast of `expected` to `xid`, because casting caller-supplied text
    /// to `xid` raises `invalid input syntax for type xid` and would turn a stale token into a `500`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn save_grid(
        &self,
        tenant_id: &str,
        grid_json: &str,
        expected: Option<&str>,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;

        let Some(expected) = expected else {
            let inserted = connection
                .query_opt(
                    "INSERT INTO translations (tenant_id, grid) \
                     VALUES ($1, $2::text::jsonb) \
                     ON CONFLICT (tenant_id) DO NOTHING \
                     RETURNING xmin::text",
                    &[&tenant_id, &grid_json],
                )
                .await
                .map_err(unavailable)?;
            return Ok(inserted.map_or(RowUpdate::VersionMismatch, |row| {
                RowUpdate::Updated(row.get(0))
            }));
        };

        let updated = connection
            .query_opt(
                "UPDATE translations SET grid = $2::text::jsonb, updated_at = now() \
                 WHERE tenant_id = $1 AND xmin::text = $3 RETURNING xmin::text",
                &[&tenant_id, &grid_json, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }

        // Zero rows is ambiguous on its own: the version moved, or the row is not there. The probe
        // is what makes a conflict distinguishable from an absence.
        let present = connection
            .query_opt(
                "SELECT 1 FROM translations WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
    }
}
