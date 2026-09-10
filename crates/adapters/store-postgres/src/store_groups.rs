// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store groups and their config batches over PostgreSQL
//! ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md), migration 0061).
//!
//! Four tables and the SQL over them: the cohort (`store_groups`, `store_group_members`) and the
//! durable record of what a fan-out did at each of its members (`config_batches`,
//! `config_batch_results`). Like the rest of this crate the adapter carries only rows and strings —
//! `pos-cloud` implements its `StoreGroupStore` seam over this type and owns the domain shapes, so
//! no cloud type reaches the SQL.
//!
//! Tenant scoping is an explicit `WHERE tenant_id = $1`, matching every other table here: the cloud
//! connects as the trusted pool owner, which bypasses RLS, and the migration's policy is the second
//! line of defence rather than the first.
//!
//! # Why membership is replaced in a transaction and versioned by the group
//!
//! [`set_members_at`](PostgresStoreGroups::set_members_at) is a `DELETE` then an `INSERT` of the
//! whole set, and both run inside one transaction with the group's own version bump. Two things
//! come out of that, and both are the point rather than incidental:
//!
//!   * a reader never sees the half-second in which the cohort is empty, which — because the batch
//!     route reads the membership it is about to publish to — would otherwise be a fan-out to
//!     nobody that reported complete success;
//!   * the version the caller passed is checked *inside* that transaction, so a concurrent edit
//!     loses the race rather than interleaving with it (ADR-0095 shape C, and the reason the set has
//!     no version of its own: what the screen holds is the group).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{RowUpdate, pool_unavailable, unavailable};

/// One store group as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreGroupRow {
    /// The group's id (a ULID string).
    pub group_id: String,
    /// The human name.
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
    /// `xmin` as text — the row's version (ADR-0094), opaque above this adapter.
    pub version: String,
}

/// One batch as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigBatchRow {
    /// The batch's id (a ULID string).
    pub batch_id: String,
    /// The group it fanned out over (a ULID string).
    pub group_id: String,
    /// The node kind published.
    pub node: String,
    /// The node's arguments as JSON text, as stored in the `arguments` jsonb column.
    pub arguments_json: String,
    /// The console admin who started it.
    pub actor_email: String,
    /// Milliseconds since the epoch.
    pub started_at_ms: i64,
    /// Milliseconds since the epoch, or `None` while the fan-out is still running (or died).
    pub finished_at_ms: Option<i64>,
}

/// One `(batch, store)` result as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchResultRow {
    /// The store this row is about (a ULID string).
    pub store_id: String,
    /// `applied`, `skipped` or `failed`.
    pub outcome: String,
    /// The refusal's message, for `skipped` and `failed`.
    pub detail: Option<String>,
    /// The config version produced, for `applied`.
    pub version_id: Option<String>,
    /// Milliseconds since the epoch.
    pub at_ms: i64,
}

/// The columns a group read returns, in the order [`store_group_row`] expects.
const GROUP_COLUMNS: &str = "group_id, name, status, xmin::text";
/// The columns a batch read returns, in the order [`config_batch_row`] expects.
const BATCH_COLUMNS: &str =
    "batch_id, group_id, node, arguments::text, actor_email, started_at, finished_at";
/// The columns a result read returns, in the order [`batch_result_row`] expects.
const RESULT_COLUMNS: &str = "store_id, outcome, detail, version_id, at";

/// The store-group tables over a shared pool. Built by
/// [`PostgresStore::store_groups`](crate::PostgresStore::store_groups).
#[derive(Clone, Debug)]
pub struct PostgresStoreGroups {
    pool: Pool,
}

impl PostgresStoreGroups {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts a group, returning its starting version, or `None` if the id is taken.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn insert_group(
        &self,
        tenant_id: &str,
        group_id: &str,
        name: &str,
        status: &str,
    ) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let inserted = connection
            .query_opt(
                "INSERT INTO store_groups (tenant_id, group_id, name, status) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (tenant_id, group_id) DO NOTHING \
                 RETURNING xmin::text",
                &[&tenant_id, &group_id, &name, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(inserted.map(|row| row.get(0)))
    }

    /// A tenant's groups, id order — a ULID, so creation order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_groups(&self, tenant_id: &str) -> Result<Vec<StoreGroupRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {GROUP_COLUMNS} FROM store_groups \
                     WHERE tenant_id = $1 ORDER BY group_id"
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(store_group_row).collect())
    }

    /// Renames a group and/or sets its status, only at `expected`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn update_group_at(
        &self,
        tenant_id: &str,
        group_id: &str,
        name: &str,
        status: &str,
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE store_groups SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND group_id = $2 AND xmin::text = $5 \
                 RETURNING xmin::text",
                &[&tenant_id, &group_id, &name, &status, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        self.present_or_missing(tenant_id, group_id).await
    }

    /// A group's members, store-id order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_members(
        &self,
        tenant_id: &str,
        group_id: &str,
    ) -> Result<Vec<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT store_id FROM store_group_members \
                 WHERE tenant_id = $1 AND group_id = $2 ORDER BY store_id",
                &[&tenant_id, &group_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Replaces a group's membership wholesale, only at `expected`, in one transaction.
    ///
    /// See the module note: the delete and the insert are atomic together with the group's version
    /// bump, so no reader — and in particular no batch about to fan out — can observe the cohort
    /// empty, and a concurrent edit loses the race rather than interleaving with it.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn set_members_at(
        &self,
        tenant_id: &str,
        group_id: &str,
        members: &[String],
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let mut connection = self.pool.get().await.map_err(pool_unavailable)?;
        let transaction = connection.transaction().await.map_err(unavailable)?;
        // The version bump doubles as the lock: `UPDATE … WHERE xmin::text = $3` takes the row and
        // holds it for the rest of the transaction, so the membership rewrite below cannot
        // interleave with a competing one.
        let bumped = transaction
            .query_opt(
                "UPDATE store_groups SET updated_at = now() \
                 WHERE tenant_id = $1 AND group_id = $2 AND xmin::text = $3 \
                 RETURNING xmin::text",
                &[&tenant_id, &group_id, &expected],
            )
            .await
            .map_err(unavailable)?;
        let Some(bumped) = bumped else {
            transaction.rollback().await.map_err(unavailable)?;
            return self.present_or_missing(tenant_id, group_id).await;
        };
        transaction
            .execute(
                "DELETE FROM store_group_members WHERE tenant_id = $1 AND group_id = $2",
                &[&tenant_id, &group_id],
            )
            .await
            .map_err(unavailable)?;
        for store_id in members {
            transaction
                .execute(
                    "INSERT INTO store_group_members (tenant_id, group_id, store_id) \
                     VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                    &[&tenant_id, &group_id, store_id],
                )
                .await
                .map_err(unavailable)?;
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(RowUpdate::Updated(bumped.get(0)))
    }

    /// Records a fan-out before it touches its first store.
    ///
    /// `ON CONFLICT … DO NOTHING` so a retried request with the same batch id does not double the
    /// row; the results it writes are keyed on `(batch, store)` and behave the same way.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn insert_batch(
        &self,
        tenant_id: &str,
        batch_id: &str,
        group_id: &str,
        node: &str,
        arguments_json: &str,
        actor_email: &str,
        started_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO config_batches \
                     (tenant_id, batch_id, group_id, node, arguments, actor_email, started_at) \
                 VALUES ($1, $2, $3, $4, $5::text::jsonb, $6, $7) \
                 ON CONFLICT (tenant_id, batch_id) DO NOTHING",
                &[
                    &tenant_id,
                    &batch_id,
                    &group_id,
                    &node,
                    &arguments_json,
                    &actor_email,
                    &started_at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Records what happened at one store. Idempotent on `(batch, store)`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn upsert_result(
        &self,
        tenant_id: &str,
        batch_id: &str,
        store_id: &str,
        outcome: &str,
        detail: Option<&str>,
        version_id: Option<&str>,
        at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO config_batch_results \
                     (tenant_id, batch_id, store_id, outcome, detail, version_id, at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (tenant_id, batch_id, store_id) DO UPDATE SET \
                     outcome = EXCLUDED.outcome, detail = EXCLUDED.detail, \
                     version_id = EXCLUDED.version_id, at = EXCLUDED.at",
                &[
                    &tenant_id,
                    &batch_id,
                    &store_id,
                    &outcome,
                    &detail,
                    &version_id,
                    &at_ms,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Stamps a batch finished. Stamping one that does not exist is not an error.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn finish_batch(
        &self,
        tenant_id: &str,
        batch_id: &str,
        finished_at_ms: i64,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "UPDATE config_batches SET finished_at = $3 \
                 WHERE tenant_id = $1 AND batch_id = $2",
                &[&tenant_id, &batch_id, &finished_at_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// A group's batches, newest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_batches(
        &self,
        tenant_id: &str,
        group_id: &str,
        limit: i64,
    ) -> Result<Vec<ConfigBatchRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {BATCH_COLUMNS} FROM config_batches \
                     WHERE tenant_id = $1 AND group_id = $2 \
                     ORDER BY started_at DESC LIMIT $3"
                ),
                &[&tenant_id, &group_id, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(config_batch_row).collect())
    }

    /// One batch's per-store report, store-id order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_results(
        &self,
        tenant_id: &str,
        batch_id: &str,
    ) -> Result<Vec<BatchResultRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {RESULT_COLUMNS} FROM config_batch_results \
                     WHERE tenant_id = $1 AND batch_id = $2 ORDER BY store_id"
                ),
                &[&tenant_id, &batch_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(batch_result_row).collect())
    }

    /// Tells a failed conditional write apart from a missing row, which is the difference between
    /// a `412` and a `404` above this seam.
    async fn present_or_missing(
        &self,
        tenant_id: &str,
        group_id: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let present = connection
            .query_opt(
                "SELECT 1 FROM store_groups WHERE tenant_id = $1 AND group_id = $2",
                &[&tenant_id, &group_id],
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

/// Reads one queried row into a [`StoreGroupRow`]; the column order matches [`GROUP_COLUMNS`].
fn store_group_row(row: &tokio_postgres::Row) -> StoreGroupRow {
    StoreGroupRow {
        group_id: row.get(0),
        name: row.get(1),
        status: row.get(2),
        version: row.get(3),
    }
}

/// Reads one queried row into a [`ConfigBatchRow`]; the column order matches [`BATCH_COLUMNS`].
fn config_batch_row(row: &tokio_postgres::Row) -> ConfigBatchRow {
    ConfigBatchRow {
        batch_id: row.get(0),
        group_id: row.get(1),
        node: row.get(2),
        arguments_json: row.get(3),
        actor_email: row.get(4),
        started_at_ms: row.get(5),
        finished_at_ms: row.get(6),
    }
}

/// Reads one queried row into a [`BatchResultRow`]; the column order matches [`RESULT_COLUMNS`].
fn batch_result_row(row: &tokio_postgres::Row) -> BatchResultRow {
    BatchResultRow {
        store_id: row.get(0),
        outcome: row.get(1),
        detail: row.get(2),
        version_id: row.get(3),
        at_ms: row.get(4),
    }
}
