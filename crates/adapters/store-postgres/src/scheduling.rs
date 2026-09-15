// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Scheduled config publishes over PostgreSQL (Track M3, [ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
//!
//! One row per scheduled publish (`scheduled_publishes`, migration 0034). This adapter keeps only the
//! SQL and returns plain rows; `pos-cloud` implements its `ScheduledPublishStore` seam over this type.
//! The activator's `due` read spans all tenants as the trusted pool owner (RLS bypassed, like the
//! rollup projector's fleet scan); the per-store read and the schedule/cancel/mark writes name the
//! tenant explicitly. Timestamps cross the boundary as Unix milliseconds, converted with
//! `to_timestamp(... / 1000.0)` on write and `extract(epoch ...)*1000` on read, binding the parameter
//! as `bigint` (an `i64`) to keep the extended-protocol type check happy.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// A scheduled publish to insert.
#[derive(Clone, Debug)]
pub struct NewScheduledPublishRow<'a> {
    /// The row id (a ULID string).
    pub id: &'a str,
    /// The tenant id (a ULID string).
    pub tenant_id: &'a str,
    /// The store id (a ULID string).
    pub store_id: &'a str,
    /// The Store-layer key to write.
    pub node_key: &'a str,
    /// The snapshotted node value as JSON text.
    pub node_value_json: &'a str,
    /// When to apply it, Unix milliseconds.
    pub effective_at_ms: i64,
    /// The admin who scheduled it (id string).
    pub created_by: &'a str,
    /// The release this pair belongs to, or `None` for a standalone schedule (ADR-0125).
    pub release_id: Option<&'a str>,
}

/// One stored scheduled-publish row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledPublishRow {
    /// The row id.
    pub id: String,
    /// The tenant id (a ULID string).
    pub tenant_id: String,
    /// The store id (a ULID string).
    pub store_id: String,
    /// The Store-layer key.
    pub node_key: String,
    /// The snapshotted node value as JSON text.
    pub node_value_json: String,
    /// When it applies, Unix milliseconds.
    pub effective_at_ms: i64,
    /// The status token (`PENDING` / `APPLIED` / `CANCELLED`).
    pub status: String,
    /// When it was scheduled, Unix milliseconds.
    pub created_at_ms: i64,
    /// The config version it produced, once applied.
    pub applied_version_id: Option<String>,
    /// The release this pair belongs to, or `None` for a standalone schedule.
    pub release_id: Option<String>,
    /// Why it last failed to apply, or `None`.
    pub failure: Option<String>,
}

/// The scheduled-publish store over a shared pool. Built by
/// [`PostgresStore::scheduled_publishes`](crate::PostgresStore::scheduled_publishes).
#[derive(Clone, Debug)]
pub struct PostgresScheduledPublishes {
    pool: Pool,
}

const SELECT_COLUMNS: &str = "id, tenant_id, store_id, node_key, node_value::text, \
     (extract(epoch from effective_at) * 1000)::bigint, status, \
     (extract(epoch from created_at) * 1000)::bigint, applied_version_id, release_id, failure";

fn row_from(row: &tokio_postgres::Row) -> ScheduledPublishRow {
    ScheduledPublishRow {
        id: row.get(0),
        tenant_id: row.get(1),
        store_id: row.get(2),
        node_key: row.get(3),
        node_value_json: row.get(4),
        effective_at_ms: row.get(5),
        status: row.get(6),
        created_at_ms: row.get(7),
        applied_version_id: row.get(8),
        release_id: row.get(9),
        failure: row.get(10),
    }
}

impl PostgresScheduledPublishes {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts a pending scheduled publish.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn schedule(&self, row: &NewScheduledPublishRow<'_>) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO scheduled_publishes \
                 (id, tenant_id, store_id, node_key, node_value, effective_at, created_by, release_id) \
                 VALUES ($1, $2, $3, $4, $5::text::jsonb, \
                 to_timestamp($6::bigint::double precision / 1000.0), $7, $8)",
                &[
                    &row.id,
                    &row.tenant_id,
                    &row.store_id,
                    &row.node_key,
                    &row.node_value_json,
                    &row.effective_at_ms,
                    &row.created_by,
                    &row.release_id,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Every pending publish whose effective time is at or before `now_ms`, across all tenants,
    /// soonest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn due(&self, now_ms: i64) -> Result<Vec<ScheduledPublishRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let query = format!(
            "SELECT {SELECT_COLUMNS} FROM scheduled_publishes \
             WHERE status = 'PENDING' \
             AND effective_at <= to_timestamp($1::bigint::double precision / 1000.0) \
             ORDER BY effective_at ASC"
        );
        let rows = connection
            .query(&query, &[&now_ms])
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_from).collect())
    }

    /// A store's pending publishes, soonest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_for_store(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Vec<ScheduledPublishRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let query = format!(
            "SELECT {SELECT_COLUMNS} FROM scheduled_publishes \
             WHERE tenant_id = $1 AND store_id = $2 AND status = 'PENDING' \
             ORDER BY effective_at ASC"
        );
        let rows = connection
            .query(&query, &[&tenant_id, &store_id])
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_from).collect())
    }

    /// Cancels a pending publish. Returns whether a pending row with that id existed in the tenant.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn cancel(&self, tenant_id: &str, id: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE scheduled_publishes SET status = 'CANCELLED' \
                 WHERE tenant_id = $1 AND id = $2 AND status = 'PENDING'",
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed > 0)
    }

    /// Marks a pending publish applied, recording the config version it produced. A ULID id is globally
    /// unique, so matching on id alone is safe; the `status = 'PENDING'` guard makes it apply-once.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn mark_applied(&self, id: &str, version_id: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE scheduled_publishes SET status = 'APPLIED', applied_version_id = $2, \
                 failure = NULL \
                 WHERE id = $1 AND status = 'PENDING'",
                &[&id, &version_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Records why a pending publish last failed. The row stays pending and the activator retries it;
    /// this is what a release's report reads to say which pairs are struggling
    /// ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).
    ///
    /// Only a pending row is written, so a late failure from a pass that raced a successful one
    /// cannot put a reason on a publish that has already landed — and `mark_applied` clears the
    /// column, so a row that failed once and then succeeded does not keep explaining itself.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn mark_failed(&self, id: &str, failure: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE scheduled_publishes SET failure = $2 \
                 WHERE id = $1 AND status = 'PENDING'",
                &[&id, &failure],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Every pair of one release, for the node × store report — in every status, because a report
    /// that hid the applied ones could not tell an operator how far a release had got.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_for_release(
        &self,
        tenant_id: &str,
        release_id: &str,
    ) -> Result<Vec<ScheduledPublishRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let query = format!(
            "SELECT {SELECT_COLUMNS} FROM scheduled_publishes \
             WHERE tenant_id = $1 AND release_id = $2 \
             ORDER BY store_id ASC, node_key ASC"
        );
        let rows = connection
            .query(&query, &[&tenant_id, &release_id])
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_from).collect())
    }
}
