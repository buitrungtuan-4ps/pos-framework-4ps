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
}
