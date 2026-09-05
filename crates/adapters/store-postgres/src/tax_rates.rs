// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The per-(tax class × sales channel) tax rate table over PostgreSQL (Track M4, [ADR-0074](../../../docs/adr/0074-localization-and-tax.md)).
//!
//! One row per `(tenant, tax_class, sales_channel)` with the rate in basis points (`catalog_tax_rates`,
//! migration 0028). This adapter keeps only the SQL and returns plain rows; `pos-cloud` implements its
//! `TaxRateStore` seam over this type and assembles the rows into the `tax` config node. Tenant scoping
//! is an explicit `WHERE tenant_id = $1` (the cloud connects as the trusted pool owner, which bypasses
//! RLS; the migration's policy is the second line), exactly as the catalog adapter does. A save
//! **replaces** the tenant's whole table in one transaction — the table is small (classes × channels)
//! and an operator edits it as a grid, so a wholesale replace is simpler and races cleaner than a
//! per-row upsert/delete.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// One authored rate as stored: the class, the channel (wire token), and the rate in basis points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxRateRow {
    /// The tax-class id (a ULID string), the id `catalog_items.tax_class_id` references.
    pub tax_class_id: String,
    /// The sales-channel wire token (`SALES_CHANNEL_DINE_IN`).
    pub sales_channel: String,
    /// The rate in basis points (10% is 1000), bounded [0, 10000] by the table's CHECK.
    pub rate_bps: i32,
    /// How the rate is broken out on the invoice, as the JSON text of a `pos_proto::TaxComponent`
    /// list — `[{"name":"CGST","rate":250}]` (migration 0050, ADR-0104).
    ///
    /// Text rather than a decoded type, because this adapter's job is SQL: `pos-cloud` owns the
    /// shape on both sides of it, exactly as it owns what a `sales_channel` token means. `[]` is the
    /// ordinary case — one rate, printed as one line — and is what every row written before this
    /// column existed reads back as.
    pub components_json: String,
}

/// The tax-rate store over a shared pool. Built by
/// [`PostgresStore::tax_rates`](crate::PostgresStore::tax_rates).
#[derive(Clone, Debug)]
pub struct PostgresTaxRates {
    pool: Pool,
}

impl PostgresTaxRates {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Lists a tenant's tax-rate rows, ordered by class then channel for a stable read, together
    /// with **the version the table was read at** — `None` if this tenant has never saved rates.
    ///
    /// The version comes from the tenant's row in `catalog_tax_rate_versions` (migration 0039), not
    /// from any rate row. It cannot come from a rate row: a save replaces the whole table, deleting
    /// every row and its `xmin` with it, so there would be nothing left to compare against
    /// ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    ///
    /// The two reads are not in one transaction on purpose. If a save commits between them the
    /// caller ends up holding rows from before it and a version from after, or the reverse — and
    /// either way [`Self::replace`] refuses the write, because the version it names is not the one
    /// now stored. A torn read cannot become a lost update; it becomes a `412` and a reload.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(
        &self,
        tenant_id: &str,
    ) -> Result<(Vec<TaxRateRow>, Option<String>), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT tax_class_id, sales_channel, rate_bps, components::text \
                 FROM catalog_tax_rates \
                 WHERE tenant_id = $1 ORDER BY tax_class_id, sales_channel",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        let version = connection
            .query_opt(
                "SELECT xmin::text FROM catalog_tax_rate_versions WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok((
            rows.iter()
                .map(|row| TaxRateRow {
                    tax_class_id: row.get(0),
                    sales_channel: row.get(1),
                    rate_bps: row.get(2),
                    components_json: row.get(3),
                })
                .collect(),
            version.map(|row| row.get(0)),
        ))
    }

    /// Replaces a tenant's whole tax-rate table with `rows`, atomically and **only if the table is
    /// still at `expected`** ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    ///
    /// The version row, the delete and the inserts all run in one transaction, so a concurrent read
    /// sees either the old table or the new one, never a half-applied grid — and the precondition is
    /// evaluated inside that same transaction, so nothing can commit between the check and the swap.
    /// The version row is claimed **first**: it is the one row every concurrent save contends on, so
    /// taking it before touching the rate rows is what makes the loser lose cheaply, before it has
    /// deleted anything.
    ///
    /// - `expected = None` — this tenant has never saved rates, so the version row must not exist
    ///   yet. `ON CONFLICT DO NOTHING` returning zero rows means another save created it first: a
    ///   [`RowUpdate::VersionMismatch`], not a silent overwrite of that save's rates.
    /// - `expected = Some(v)` — the version row must still be at `v`. `updated_at = now()` is what
    ///   makes it an `UPDATE` at all, and updating it is what moves its `xmin`; the timestamp is a
    ///   side benefit, not the point.
    ///
    /// Zero rows on the conditional update is ambiguous, so the probe on the failure path separates
    /// a moved version from an absent row, exactly as the record-shaped writes in this adapter do.
    /// The comparison is on `xmin::text` rather than a cast of `expected` to `xid`, because casting
    /// caller-supplied text to `xid` raises `invalid input syntax for type xid` and would turn a
    /// stale token into a `500`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or a write fails.
    pub async fn replace(
        &self,
        tenant_id: &str,
        rows: &[TaxRateRow],
        expected: Option<&str>,
    ) -> Result<RowUpdate, PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;

        let claimed = match expected {
            None => transaction
                .query_opt(
                    "INSERT INTO catalog_tax_rate_versions (tenant_id) VALUES ($1) \
                     ON CONFLICT (tenant_id) DO NOTHING \
                     RETURNING xmin::text",
                    &[&tenant_id],
                )
                .await
                .map_err(unavailable)?,
            Some(expected) => transaction
                .query_opt(
                    "UPDATE catalog_tax_rate_versions SET updated_at = now() \
                     WHERE tenant_id = $1 AND xmin::text = $2 RETURNING xmin::text",
                    &[&tenant_id, &expected],
                )
                .await
                .map_err(unavailable)?,
        };
        let Some(claimed) = claimed else {
            // Nothing has been written yet, so rolling back is just letting the transaction drop.
            let Some(_) = expected else {
                // The create path. Zero rows means the insert hit a conflict, so a version row is
                // there and someone else claimed this tenant first. No probe: under READ COMMITTED
                // `ON CONFLICT DO NOTHING` also skips a row inserted by a transaction that has not
                // committed yet, which a `SELECT` in this transaction could not see — and reporting
                // that as `NotFound` would tell a caller to give up on a table about to exist.
                return Ok(RowUpdate::VersionMismatch);
            };
            // The update path. Zero rows is ambiguous, and the probe is what separates a version
            // that moved from a tenant that has never saved rates at all.
            let present = transaction
                .query_opt(
                    "SELECT 1 FROM catalog_tax_rate_versions WHERE tenant_id = $1",
                    &[&tenant_id],
                )
                .await
                .map_err(unavailable)?;
            return Ok(if present.is_some() {
                RowUpdate::VersionMismatch
            } else {
                RowUpdate::NotFound
            });
        };

        transaction
            .execute(
                "DELETE FROM catalog_tax_rates WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        for row in rows {
            transaction
                .execute(
                    // `$5::text::jsonb`, not `$5::jsonb`. The caller hands the components over as
                    // JSON *text* and this adapter stays free of a JSON type — but a bare
                    // `$5::jsonb` makes PostgreSQL deduce the parameter itself as `jsonb`, and
                    // `tokio-postgres` then refuses to serialise a Rust `String` into it ("error
                    // serializing parameter 4"). Casting through `text` pins the parameter to the
                    // type the caller actually sends. Same shape as `role_templates.permissions`.
                    "INSERT INTO catalog_tax_rates \
                     (tenant_id, tax_class_id, sales_channel, rate_bps, components) \
                     VALUES ($1, $2, $3, $4, $5::text::jsonb)",
                    &[
                        &tenant_id,
                        &row.tax_class_id,
                        &row.sales_channel,
                        &row.rate_bps,
                        &row.components_json,
                    ],
                )
                .await
                .map_err(unavailable)?;
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(RowUpdate::Updated(claimed.get(0)))
    }
}
