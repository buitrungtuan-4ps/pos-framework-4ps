// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The queue of correction requests a store manager sent from the till, over PostgreSQL
//! ([ADR-0250](../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)
//! decision 3, migration 0109). The adapter keeps only the SQL; `pos-cloud` owns the rule.

use deadpool_postgres::Pool;
use tokio_postgres::Row;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One request in the queue. People are named by id only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectionRequestRow {
    /// The tenant.
    pub tenant_id: String,
    /// The `fiscal.correction.requested` event's id.
    pub request_id: String,
    /// The store that asked.
    pub store_id: String,
    /// The bill whose invoice is to be corrected.
    pub bill_id: String,
    /// The amount to take back, in the currency's minor unit.
    pub amount_minor: i64,
    /// The currency, such as `VND`.
    pub currency_code: String,
    /// `ADJUSTMENT` or `REPLACEMENT`.
    pub kind: String,
    /// The `VOID_BILL` reason code, where one was given.
    pub reason_code_id: Option<String>,
    /// The requesting employee's id.
    pub requested_by: Option<String>,
    /// When the till recorded it, in Unix milliseconds.
    pub requested_time_ms: i64,
    /// `PENDING`, `APPROVED` or `DECLINED`.
    pub status: String,
    /// The console user who decided, by id.
    pub decided_by_user_id: Option<String>,
    /// When it was decided, in Unix milliseconds.
    pub decided_time_ms: Option<i64>,
}

const COLUMNS: &str = "tenant_id, request_id, store_id, bill_id, amount_minor, currency_code, kind, \
                       reason_code_id, requested_by, requested_time_ms, status, decided_by_user_id, \
                       decided_time_ms";

fn from_row(row: &Row) -> CorrectionRequestRow {
    CorrectionRequestRow {
        tenant_id: row.get(0),
        request_id: row.get(1),
        store_id: row.get(2),
        bill_id: row.get(3),
        amount_minor: row.get(4),
        currency_code: row.get(5),
        kind: row.get(6),
        reason_code_id: row.get(7),
        requested_by: row.get(8),
        requested_time_ms: row.get(9),
        status: row.get(10),
        decided_by_user_id: row.get(11),
        decided_time_ms: row.get(12),
    }
}

/// The queue over a shared pool. Built by
/// [`PostgresStore::einvoice_correction_requests`](crate::PostgresStore::einvoice_correction_requests).
#[derive(Clone, Debug)]
pub struct PostgresCorrectionRequests {
    pool: Pool,
}

impl PostgresCorrectionRequests {
    /// Records `row` unless its request is recorded; `true` when it was new.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn record(&self, row: &CorrectionRequestRow) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let written = connection
            .execute(
                &format!(
                    "INSERT INTO einvoice_correction_requests ({COLUMNS}) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                     ON CONFLICT (tenant_id, request_id) DO NOTHING"
                ),
                &[
                    &row.tenant_id,
                    &row.request_id,
                    &row.store_id,
                    &row.bill_id,
                    &row.amount_minor,
                    &row.currency_code,
                    &row.kind,
                    &row.reason_code_id,
                    &row.requested_by,
                    &row.requested_time_ms,
                    &row.status,
                    &row.decided_by_user_id,
                    &row.decided_time_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(written == 1)
    }

    /// The tenant's requests, oldest first, optionally of one status, at most `limit`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list(
        &self,
        tenant_id: &str,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<CorrectionRequestRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM einvoice_correction_requests \
                     WHERE tenant_id = $1 AND ($2::text IS NULL OR status = $2) \
                     ORDER BY requested_time_ms, request_id LIMIT $3"
                ),
                &[&tenant_id, &status, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(from_row).collect())
    }

    /// One request.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn get(
        &self,
        tenant_id: &str,
        request_id: &str,
    ) -> Result<Option<CorrectionRequestRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {COLUMNS} FROM einvoice_correction_requests \
                     WHERE tenant_id = $1 AND request_id = $2"
                ),
                &[&tenant_id, &request_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(from_row))
    }

    /// Decides a `PENDING` request: `true` when it was pending and is now `status`, `false` when it
    /// was already decided or is not there.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn decide(
        &self,
        tenant_id: &str,
        request_id: &str,
        status: &str,
        decided_by_user_id: &str,
        decided_time_ms: i64,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let written = connection
            .execute(
                "UPDATE einvoice_correction_requests \
                 SET status = $3, decided_by_user_id = $4, decided_time_ms = $5 \
                 WHERE tenant_id = $1 AND request_id = $2 AND status = 'PENDING'",
                &[
                    &tenant_id,
                    &request_id,
                    &status,
                    &decided_by_user_id,
                    &decided_time_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(written == 1)
    }
}

impl crate::PostgresStore {
    /// The correction request queue over this pool (ADR-0250, migration 0109).
    #[must_use]
    pub fn einvoice_correction_requests(&self) -> PostgresCorrectionRequests {
        PostgresCorrectionRequests {
            pool: self.pool.clone(),
        }
    }
}
