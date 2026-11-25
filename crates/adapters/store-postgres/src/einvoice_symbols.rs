// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The ledger of every e-invoice symbol a tenant's stores were given, over PostgreSQL
//! ([ADR-0240](../../../../docs/adr/0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)
//! decision 1, migration 0105). The adapter keeps only the SQL; `pos-cloud` owns the rule. Rows are
//! only ever inserted: a symbol once used stays used.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The most rows [`PostgresEInvoiceSymbols::list`] returns: a seller has at most 676 allocated
/// symbols a year, so this holds years of them.
pub const SYMBOL_LIST_LIMIT: i64 = 10_000;

/// One symbol in the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EInvoiceSymbolRow {
    /// The symbol.
    pub symbol: String,
    /// The store it was given to, or `None` for the tenant's correction symbol.
    pub store_id: Option<String>,
    /// The store's lease generation when it was given, where it had one.
    pub lease_generation: Option<i64>,
    /// `MANUAL` or `ALLOCATED`.
    pub source: String,
    /// When it was recorded, in Unix milliseconds.
    pub recorded_at_ms: i64,
}

/// The ledger over a shared pool. Built by
/// [`PostgresStore::einvoice_symbols`](crate::PostgresStore::einvoice_symbols).
#[derive(Clone, Debug)]
pub struct PostgresEInvoiceSymbols {
    pool: Pool,
}

impl PostgresEInvoiceSymbols {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Records `row` for `tenant_id` unless the tenant already holds its symbol, and returns the row
    /// the ledger holds for that symbol afterwards: `row` itself when it was recorded, the earlier
    /// one when it was not.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn record(
        &self,
        tenant_id: &str,
        row: &EInvoiceSymbolRow,
    ) -> Result<EInvoiceSymbolRow, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO einvoice_symbols \
                 (tenant_id, symbol, store_id, lease_generation, source, recorded_at_ms) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, symbol) DO NOTHING",
                &[
                    &tenant_id,
                    &row.symbol,
                    &row.store_id,
                    &row.lease_generation,
                    &row.source,
                    &row.recorded_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        let held = connection
            .query_one(
                "SELECT symbol, store_id, lease_generation, source, recorded_at_ms \
                 FROM einvoice_symbols WHERE tenant_id = $1 AND symbol = $2",
                &[&tenant_id, &row.symbol],
            )
            .await
            .map_err(unavailable)?;
        Ok(EInvoiceSymbolRow {
            symbol: held.get(0),
            store_id: held.get(1),
            lease_generation: held.get(2),
            source: held.get(3),
            recorded_at_ms: held.get(4),
        })
    }

    /// The tenant's symbols, oldest first, at most [`SYMBOL_LIST_LIMIT`].
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list(&self, tenant_id: &str) -> Result<Vec<EInvoiceSymbolRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT symbol, store_id, lease_generation, source, recorded_at_ms \
                 FROM einvoice_symbols WHERE tenant_id = $1 \
                 ORDER BY recorded_at_ms, symbol LIMIT $2",
                &[&tenant_id, &SYMBOL_LIST_LIMIT],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| EInvoiceSymbolRow {
                symbol: row.get(0),
                store_id: row.get(1),
                lease_generation: row.get(2),
                source: row.get(3),
                recorded_at_ms: row.get(4),
            })
            .collect())
    }
}

impl crate::PostgresStore {
    /// The e-invoice symbol ledger over this pool (ADR-0240, migration 0105). Here rather than in
    /// `store.rs`, which is at its size baseline.
    #[must_use]
    pub fn einvoice_symbols(&self) -> PostgresEInvoiceSymbols {
        PostgresEInvoiceSymbols::new(self.pool.clone())
    }
}
