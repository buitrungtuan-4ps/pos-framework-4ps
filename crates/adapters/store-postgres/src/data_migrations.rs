// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's record of the one-time data changes it has made, over PostgreSQL.
//!
//! `data_migrations` (migration 0073) holds one row per change, named after it, with the time it
//! was made. A change written in SQL claims its row in the statement that makes the change, as
//! 0073, 0074 and 0076 do. A change the cloud makes through its own code, such as publishing a new
//! configuration version to every store, cannot share a statement with its row, so it reads the
//! row first and writes it once the change is done:
//! [`recorded_time`](PostgresDataMigrations::recorded_time) and
//! [`record`](PostgresDataMigrations::record). Such a change must be safe to run again, because a
//! crash between the change and its row runs it again at the next boot.
//!
//! The table is the cloud's own bookkeeping, not tenant data, so it has no row-level security and
//! no grant to `app_tenant`; the cloud reads and writes it as the trusted pool owner that runs the
//! migrations.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The record of one-time data changes over a shared pool. Built by
/// [`PostgresStore::data_migrations`](crate::PostgresStore::data_migrations).
#[derive(Clone, Debug)]
pub struct PostgresDataMigrations {
    pool: Pool,
}

impl PostgresDataMigrations {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// When the change named `name` was recorded as made, in Unix milliseconds, or `None` if it has
    /// not been.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn recorded_time(&self, name: &str) -> Result<Option<i64>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT (EXTRACT(EPOCH FROM applied_time) * 1000)::bigint \
                 FROM data_migrations WHERE name = $1",
                &[&name],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Records the change named `name` as made at `applied_time_ms`, Unix milliseconds. Recording
    /// one already recorded keeps the first row, and its time.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn record(&self, name: &str, applied_time_ms: i64) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO data_migrations (name, applied_time) \
                 VALUES ($1, to_timestamp($2::bigint / 1000.0)) \
                 ON CONFLICT (name) DO NOTHING",
                &[&name, &applied_time_ms],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}
