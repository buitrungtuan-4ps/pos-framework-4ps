// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Background-task health over PostgreSQL ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 4).
//!
//! One row per named background loop (`task_health`), upserted at the end of every tick. This adapter
//! keeps only the SQL and returns plain rows; `pos-cloud` implements its `TaskHealthStore` seam over
//! this type. Not tenant-scoped — the loops run once per cloud, so the table has no `tenant_id` and no
//! RLS (administered only by the trusted pool-owner connection), the same posture as `super_admin`.
//! `detail` is bound and read as text around a `jsonb` column (the `$N::text::jsonb` cast on the way
//! in, `detail::text` on the way out), exactly as `order_queue` handles its payloads.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// A background loop's health as recorded: its name, the instant of its most recent tick (Unix ms),
/// and the tick's self-describing detail (a JSON document, as text).
#[derive(Clone, Debug)]
pub struct TaskHealthRow {
    /// The stable loop name (e.g. `rollup_projector`).
    pub task: String,
    /// Unix ms of the loop's most recent completed tick.
    pub last_tick_at_ms: i64,
    /// The tick's summary as a JSON document (`{"ok":bool,"interval_secs":N,…}`), still as text.
    pub detail_json: String,
}

/// The background-task health store over a shared pool. Built by
/// [`PostgresStore::task_health`](crate::PostgresStore::task_health).
#[derive(Clone, Debug)]
pub struct PostgresTaskHealth {
    pool: Pool,
}

impl PostgresTaskHealth {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Upserts a loop's tick: records the instant and detail, replacing the loop's prior row.
    ///
    /// The `$3::text::jsonb` cast pins the bound detail's inference to `text` before jsonb, the same
    /// reason `order_queue` casts its bound payloads. `at_ms` is Unix milliseconds.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record(&self, task: &str, at_ms: i64, detail_json: &str) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO task_health (task, last_tick_at, detail) \
                 VALUES ($1, $2, $3::text::jsonb) \
                 ON CONFLICT (task) DO UPDATE SET \
                 last_tick_at = EXCLUDED.last_tick_at, \
                 detail = EXCLUDED.detail, \
                 updated_at = now()",
                &[&task, &at_ms, &detail_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists every recorded loop's health, most-recently-ticked first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_all(&self) -> Result<Vec<TaskHealthRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT task, last_tick_at, detail::text FROM task_health ORDER BY last_tick_at DESC",
                &[],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| TaskHealthRow {
                task: row.get(0),
                last_tick_at_ms: row.get(1),
                detail_json: row.get(2),
            })
            .collect())
    }
}
