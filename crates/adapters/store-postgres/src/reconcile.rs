// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reconciliation membership query over the event log (P7,
//! [ADR-0040](../../../docs/adr/0040-reconciliation.md)).
//!
//! Reconciliation asks "which of these candidate ids does the cloud already hold?" for one store; the
//! caller returns the complement as the ids to re-push. This adapter answers only the SQL —
//! `event_id = ANY(candidates)` against the log, scoped by tenant and store — and returns the present
//! ids as plain strings; `pos-cloud` implements its `ReconcileStore` seam over this type.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The reconciliation query over a shared pool. Built by
/// [`PostgresStore::reconcile`](crate::PostgresStore::reconcile).
#[derive(Clone, Debug)]
pub struct PostgresReconcile {
    pool: Pool,
}

impl PostgresReconcile {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Of `candidates` (event-id ULID strings), returns those already present in the event log for
    /// `(tenant_id, store_id)`. The caller diffs against the candidate set to find what is missing.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn present_event_ids(
        &self,
        tenant_id: &str,
        store_id: &str,
        candidates: &[String],
    ) -> Result<Vec<String>, PortError> {
        // An empty candidate set has an empty answer without touching the database.
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT event_id FROM events \
                 WHERE tenant_id = $1 AND store_id = $2 AND event_id = ANY($3)",
                &[&tenant_id, &store_id, &candidates],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Records one reconciliation run into the history ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)):
    /// how many ids the edge offered, how many the cloud was missing, and when. Append-only.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_reconcile_run(
        &self,
        run_id: &str,
        tenant_id: &str,
        store_id: &str,
        candidates_offered: i32,
        missing_found: i32,
        ran_at: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO reconcile_runs \
                     (run_id, tenant_id, store_id, candidates_offered, missing_found, ran_at) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &run_id,
                    &tenant_id,
                    &store_id,
                    &candidates_offered,
                    &missing_found,
                    &ran_at,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's most recent reconciliation runs, newest first, capped at `limit`. An optional
    /// `store_id` narrows to one store; `None` reads across the tenant's stores.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_reconcile_runs(
        &self,
        tenant_id: &str,
        store_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ReconcileRunRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = match store_id {
            Some(store) => connection
                .query(
                    "SELECT run_id, store_id, candidates_offered, missing_found, ran_at \
                         FROM reconcile_runs \
                         WHERE tenant_id = $1 AND store_id = $2 \
                         ORDER BY ran_at DESC, run_id DESC LIMIT $3",
                    &[&tenant_id, &store, &limit],
                )
                .await
                .map_err(unavailable)?,
            None => connection
                .query(
                    "SELECT run_id, store_id, candidates_offered, missing_found, ran_at \
                         FROM reconcile_runs \
                         WHERE tenant_id = $1 \
                         ORDER BY ran_at DESC, run_id DESC LIMIT $2",
                    &[&tenant_id, &limit],
                )
                .await
                .map_err(unavailable)?,
        };
        Ok(rows
            .iter()
            .map(|row| ReconcileRunRow {
                run_id: row.get(0),
                store_id: row.get(1),
                candidates_offered: row.get(2),
                missing_found: row.get(3),
                ran_at: row.get(4),
            })
            .collect())
    }
}

/// One recorded reconciliation run read back from the history. Counts and a timestamp only — no event
/// contents and no customer identifier (`pos-cloud` maps this onto its `ReconcileRun` view type).
#[derive(Clone, Debug)]
pub struct ReconcileRunRow {
    /// The run's ULID string (chronological when ordered).
    pub run_id: String,
    /// The store the diff was for (a ULID string).
    pub store_id: String,
    /// How many ids the edge offered in its manifest.
    pub candidates_offered: i32,
    /// How many of them the cloud was missing (asked the edge to re-push).
    pub missing_found: i32,
    /// Unix ms of the diff.
    pub ran_at: i64,
}
