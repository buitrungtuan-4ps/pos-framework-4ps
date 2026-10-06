// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The stores whose people the cloud publishes again by itself, over PostgreSQL.
//!
//! One row per store (`people_republishes`, migration 0083), naming the migration that queued it
//! and when. A migration that changes what roles grant queues the stores in SQL; this adapter is
//! what the cloud's drain reads and clears, and keeps only the SQL, returning plain rows:
//! `pos-cloud` implements its `PeopleRepublishStore` seam over this type. The reads and the clear
//! span every tenant as the trusted pool owner (RLS bypassed), as the scheduled-publish
//! activator's `due` read does, because the queue is fleet-wide.
//!
//! Times cross the boundary as Unix milliseconds: written with `to_timestamp(... / 1000.0)` and
//! read with `EXTRACT(EPOCH ...) * 1000`, binding the parameter as `bigint`. The clear compares
//! the time in the form the read returned it, so a row is matched exactly as it was read. Ids are
//! ordered byte by byte (`COLLATE "C"`), which for the canonical ULID text the cloud writes is the
//! ids' own order, whatever the database's collation.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// One queued store, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeopleRepublishRow {
    /// The tenant id (a ULID string).
    pub tenant_id: String,
    /// The store id (a ULID string).
    pub store_id: String,
    /// The migration that queued the store, by its file's name.
    pub reason: String,
    /// When it was queued, Unix milliseconds.
    pub enqueued_time_ms: i64,
}

/// The queue over a shared pool. Built by
/// [`PostgresStore::people_republishes`](crate::PostgresStore::people_republishes).
#[derive(Clone, Debug)]
pub struct PostgresPeopleRepublishes {
    pool: Pool,
}

impl PostgresPeopleRepublishes {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Queues a store, or moves the reason and time of its row forward if it is queued already.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn enqueue(&self, row: &PeopleRepublishRow) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO people_republishes (tenant_id, store_id, reason, enqueued_time) \
                 VALUES ($1, $2, $3, to_timestamp($4::bigint / 1000.0)) \
                 ON CONFLICT (tenant_id, store_id) DO UPDATE \
                 SET reason = EXCLUDED.reason, enqueued_time = EXCLUDED.enqueued_time",
                &[
                    &row.tenant_id,
                    &row.store_id,
                    &row.reason,
                    &row.enqueued_time_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Up to `limit` queued stores across every tenant, in tenant then store order, starting past
    /// `after`, a `(tenant_id, store_id)` pair, when it is given.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn pending(
        &self,
        after: Option<(&str, &str)>,
        limit: i64,
    ) -> Result<Vec<PeopleRepublishRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let (after_tenant, after_store) = after.unzip();
        let rows = connection
            .query(
                "SELECT tenant_id, store_id, reason, \
                        (EXTRACT(EPOCH FROM enqueued_time) * 1000)::bigint \
                   FROM people_republishes \
                  WHERE $1::text IS NULL \
                     OR tenant_id COLLATE \"C\" > $1::text \
                     OR (tenant_id = $1::text AND store_id COLLATE \"C\" > $2::text) \
                  ORDER BY tenant_id COLLATE \"C\", store_id COLLATE \"C\" \
                  LIMIT $3",
                &[&after_tenant, &after_store, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| PeopleRepublishRow {
                tenant_id: row.get(0),
                store_id: row.get(1),
                reason: row.get(2),
                enqueued_time_ms: row.get(3),
            })
            .collect())
    }

    /// Removes a store's row while it still carries `enqueued_time_ms`, the time a read returned,
    /// and says whether it removed one: a store queued again since keeps its row.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn clear(
        &self,
        tenant_id: &str,
        store_id: &str,
        enqueued_time_ms: i64,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let removed = connection
            .execute(
                "DELETE FROM people_republishes \
                  WHERE tenant_id = $1 AND store_id = $2 \
                    AND (EXTRACT(EPOCH FROM enqueued_time) * 1000)::bigint = $3::bigint",
                &[&tenant_id, &store_id, &enqueued_time_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(removed > 0)
    }
}
