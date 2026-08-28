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

use crate::store::{pool_unavailable, unavailable};

/// One authored rate as stored: the class, the channel (wire token), and the rate in basis points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxRateRow {
    /// The tax-class id (a ULID string), the id `catalog_items.tax_class_id` references.
    pub tax_class_id: String,
    /// The sales-channel wire token (`SALES_CHANNEL_DINE_IN`).
    pub sales_channel: String,
    /// The rate in basis points (10% is 1000), bounded [0, 10000] by the table's CHECK.
    pub rate_bps: i32,
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

    /// Lists a tenant's tax-rate rows, ordered by class then channel for a stable read.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<TaxRateRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT tax_class_id, sales_channel, rate_bps FROM catalog_tax_rates \
                 WHERE tenant_id = $1 ORDER BY tax_class_id, sales_channel",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| TaxRateRow {
                tax_class_id: row.get(0),
                sales_channel: row.get(1),
                rate_bps: row.get(2),
            })
            .collect())
    }

    /// Replaces a tenant's whole tax-rate table with `rows`, atomically.
    ///
    /// The delete and the inserts run in one transaction, so a concurrent read sees either the old
    /// table or the new one, never a half-applied grid.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or a write fails.
    pub async fn replace(&self, tenant_id: &str, rows: &[TaxRateRow]) -> Result<(), PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
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
                    "INSERT INTO catalog_tax_rates \
                     (tenant_id, tax_class_id, sales_channel, rate_bps) \
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &tenant_id,
                        &row.tax_class_id,
                        &row.sales_channel,
                        &row.rate_bps,
                    ],
                )
                .await
                .map_err(unavailable)?;
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(())
    }
}
