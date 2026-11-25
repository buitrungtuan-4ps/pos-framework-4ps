// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's e-invoice submissions and the feed's cursors, over PostgreSQL
//! ([ADR-0241](../../../../docs/adr/0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md),
//! migration 0106). The adapter keeps only the SQL; `pos-cloud` owns the retry rule and the states.

use deadpool_postgres::Pool;
use tokio_postgres::Row;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One invoice's submission, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmissionRow {
    /// The tenant.
    pub tenant_id: String,
    /// The invoice's id.
    pub invoice_id: String,
    /// The store that issued it.
    pub store_id: String,
    /// The bill it invoices.
    pub bill_id: String,
    /// `1C26MAA/00000042`.
    pub invoice_number: String,
    /// `ORIGINAL`, `ADJUSTMENT` or `REPLACEMENT`.
    pub kind: String,
    /// The calendar date, `YYYY-MM-DD`.
    pub issued_on: String,
    /// The total in the minor unit.
    pub total_minor: i64,
    /// The currency code.
    pub currency: String,
    /// The buyer's subject id, never their details.
    pub buyer_subject_id: Option<String>,
    /// The e-invoice connection it was issued under.
    pub connection_id: Option<String>,
    /// For a correction, the invoice it corrects.
    pub corrects_invoice_number: Option<String>,
    /// `PENDING`, `RETRYING`, `SUBMITTED` or `REJECTED`.
    pub state: String,
    /// Attempts that failed.
    pub failed_attempts: i32,
    /// When it is next due, in Unix milliseconds.
    pub next_attempt_ms: i64,
    /// The provider's reference once submitted.
    pub authority_reference: Option<String>,
    /// The provider's reason for a refusal.
    pub refusal: Option<String>,
    /// When it was recorded.
    pub recorded_at_ms: i64,
    /// When it last changed.
    pub updated_at_ms: i64,
}

const COLUMNS: &str = "tenant_id, invoice_id, store_id, bill_id, invoice_number, kind, issued_on, \
     total_minor, currency, buyer_subject_id, connection_id, corrects_invoice_number, state, \
     failed_attempts, next_attempt_ms, authority_reference, refusal, recorded_at_ms, updated_at_ms";

fn from_row(row: &Row) -> SubmissionRow {
    SubmissionRow {
        tenant_id: row.get(0),
        invoice_id: row.get(1),
        store_id: row.get(2),
        bill_id: row.get(3),
        invoice_number: row.get(4),
        kind: row.get(5),
        issued_on: row.get(6),
        total_minor: row.get(7),
        currency: row.get(8),
        buyer_subject_id: row.get(9),
        connection_id: row.get(10),
        corrects_invoice_number: row.get(11),
        state: row.get(12),
        failed_attempts: row.get(13),
        next_attempt_ms: row.get(14),
        authority_reference: row.get(15),
        refusal: row.get(16),
        recorded_at_ms: row.get(17),
        updated_at_ms: row.get(18),
    }
}

/// The submissions over a shared pool. Built by
/// [`PostgresStore::einvoice_submissions`](crate::PostgresStore::einvoice_submissions).
#[derive(Clone, Debug)]
pub struct PostgresEInvoiceSubmissions {
    pool: Pool,
}

impl PostgresEInvoiceSubmissions {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The last event of the store's log the feed has read.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn cursor(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT last_event_id FROM einvoice_feed_cursors \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Moves the store's cursor to `event_id`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn advance(
        &self,
        tenant_id: &str,
        store_id: &str,
        event_id: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO einvoice_feed_cursors (tenant_id, store_id, last_event_id) \
                 VALUES ($1, $2, $3) ON CONFLICT (tenant_id, store_id) \
                 DO UPDATE SET last_event_id = EXCLUDED.last_event_id",
                &[&tenant_id, &store_id, &event_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Records `row` unless its invoice is already recorded; `true` when it was new.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn record(&self, row: &SubmissionRow) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let inserted = connection
            .execute(
                &format!(
                    "INSERT INTO einvoice_submissions ({COLUMNS}) VALUES \
                     ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                     $17, $18, $19) ON CONFLICT (tenant_id, invoice_id) DO NOTHING"
                ),
                &[
                    &row.tenant_id,
                    &row.invoice_id,
                    &row.store_id,
                    &row.bill_id,
                    &row.invoice_number,
                    &row.kind,
                    &row.issued_on,
                    &row.total_minor,
                    &row.currency,
                    &row.buyer_subject_id,
                    &row.connection_id,
                    &row.corrects_invoice_number,
                    &row.state,
                    &row.failed_attempts,
                    &row.next_attempt_ms,
                    &row.authority_reference,
                    &row.refusal,
                    &row.recorded_at_ms,
                    &row.updated_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted == 1)
    }

    /// Up to `limit` submissions across the fleet that are due at `now_ms`, oldest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn due(&self, now_ms: i64, limit: i64) -> Result<Vec<SubmissionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM einvoice_submissions \
                     WHERE state IN ('PENDING', 'RETRYING') AND next_attempt_ms <= $1 \
                     ORDER BY next_attempt_ms, invoice_id LIMIT $2"
                ),
                &[&now_ms, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(from_row).collect())
    }

    /// Writes a submission's outcome.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn settle(&self, row: &SubmissionRow) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE einvoice_submissions SET state = $3, failed_attempts = $4, \
                 next_attempt_ms = $5, authority_reference = $6, refusal = $7, \
                 updated_at_ms = $8 WHERE tenant_id = $1 AND invoice_id = $2",
                &[
                    &row.tenant_id,
                    &row.invoice_id,
                    &row.state,
                    &row.failed_attempts,
                    &row.next_attempt_ms,
                    &row.authority_reference,
                    &row.refusal,
                    &row.updated_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// A tenant's submissions, newest first, optionally of one store and only those not yet
    /// submitted, at most `limit`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list(
        &self,
        tenant_id: &str,
        store_id: Option<&str>,
        unsubmitted_only: bool,
        limit: i64,
    ) -> Result<Vec<SubmissionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM einvoice_submissions WHERE tenant_id = $1 \
                     AND ($2::text IS NULL OR store_id = $2) \
                     AND (NOT $3 OR state <> 'SUBMITTED') \
                     ORDER BY recorded_at_ms DESC, invoice_id DESC LIMIT $4"
                ),
                &[&tenant_id, &store_id, &unsubmitted_only, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(from_row).collect())
    }

    /// One submission.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn get(
        &self,
        tenant_id: &str,
        invoice_id: &str,
    ) -> Result<Option<SubmissionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {COLUMNS} FROM einvoice_submissions \
                     WHERE tenant_id = $1 AND invoice_id = $2"
                ),
                &[&tenant_id, &invoice_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(from_row))
    }
}

impl PostgresEInvoiceSubmissions {
    /// The highest number the tenant recorded under `symbol`, such as `1C26MAA/00000042`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn last_number(
        &self,
        tenant_id: &str,
        symbol: &str,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let prefix = format!("{symbol}/%");
        let row = connection
            .query_opt(
                "SELECT invoice_number FROM einvoice_submissions \
                 WHERE tenant_id = $1 AND invoice_number LIKE $2 \
                 ORDER BY invoice_number DESC LIMIT 1",
                &[&tenant_id, &prefix],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }
}

impl PostgresEInvoiceSubmissions {
    /// The corrections recorded against the invoice numbered `number`, oldest first (then by number), at most
    /// 1,000: what the cloud reads to keep an adjustment from taking the invoice below zero.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn corrections_of(
        &self,
        tenant_id: &str,
        number: &str,
    ) -> Result<Vec<SubmissionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM einvoice_submissions \
                     WHERE tenant_id = $1 AND corrects_invoice_number = $2 \
                     ORDER BY recorded_at_ms, invoice_number LIMIT 1000"
                ),
                &[&tenant_id, &number],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(from_row).collect())
    }
}

impl crate::PostgresStore {
    /// The e-invoice submissions over this pool (ADR-0241, migration 0106). Here rather than in
    /// `store.rs`, which is at its size baseline.
    #[must_use]
    pub fn einvoice_submissions(&self) -> PostgresEInvoiceSubmissions {
        PostgresEInvoiceSubmissions::new(self.pool.clone())
    }
}
