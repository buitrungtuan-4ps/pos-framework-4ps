// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Releases over PostgreSQL ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).
//!
//! One row per release (`releases`, migration 0064) holding the identity, the moment and the roll-up.
//! The **writes** are not here: they are `scheduled_publishes` rows carrying this release's id, read
//! through [`crate::scheduling`]. That split is what keeps a release from becoming a second publish
//! path — this table cannot produce a config version and has no statement that tries.
//!
//! The moment is stored as three nullable columns rather than one, because the two kinds are not the
//! same shape: a wall-clock release is a local date and time whose *instants* live per store on the
//! pairs, and an instant release is one `timestamptz` for the whole fleet. Collapsing them into one
//! column would mean a release that says "Monday 04:00" and a release that says "this exact moment"
//! were indistinguishable on read, and only the second is safe to apply at a store whose timezone
//! nobody recorded.
//!
//! `in_flight` spans all tenants as the trusted pool owner (RLS bypassed, like the activator's `due`
//! read it follows); the per-tenant reads and every write name the tenant explicitly. Timestamps
//! cross the boundary as Unix milliseconds, the same conversion the rest of this crate uses.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// A release to insert.
#[derive(Clone, Debug)]
pub struct NewReleaseRow<'a> {
    /// The row id (a ULID string).
    pub id: &'a str,
    /// The tenant id (a ULID string).
    pub tenant_id: &'a str,
    /// The operator's name for it.
    pub name: &'a str,
    /// Its starting status token.
    pub status: &'a str,
    /// The cohort it was aimed at, if any.
    pub target_group_id: Option<&'a str>,
    /// The local date, `YYYY-MM-DD`, for a wall-clock release.
    pub wall_clock_date: Option<&'a str>,
    /// The local time, `HH:MM`, for a wall-clock release.
    pub wall_clock_time: Option<&'a str>,
    /// The single instant, Unix milliseconds, for an instant release.
    pub instant_at_ms: Option<i64>,
    /// The admin who created it.
    pub created_by: &'a str,
}

/// One stored release row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseRow {
    /// The row id.
    pub id: String,
    /// The tenant id (a ULID string).
    pub tenant_id: String,
    /// The operator's name for it.
    pub name: String,
    /// The status token.
    pub status: String,
    /// The cohort it was aimed at, if any.
    pub target_group_id: Option<String>,
    /// The local date, for a wall-clock release.
    pub wall_clock_date: Option<String>,
    /// The local time, for a wall-clock release.
    pub wall_clock_time: Option<String>,
    /// The single instant, Unix milliseconds, for an instant release.
    pub instant_at_ms: Option<i64>,
    /// The admin who created it.
    pub created_by: String,
    /// When it was created, Unix milliseconds.
    pub created_at_ms: i64,
    /// When the row last changed, Unix milliseconds.
    pub updated_at_ms: i64,
}

/// The config-release store over a shared pool. Built by
/// [`PostgresStore::config_releases`](crate::PostgresStore::config_releases).
///
/// **Config** release, not OTA release. This crate already has a `PostgresReleases` for the OTA side
/// — a signed edge binary and its artifact ([ADR-0048](../../../docs/adr/0048-ota-rollouts.md)) — and
/// the word now means two unrelated things: a version of the software, and a set of config nodes
/// going out to a set of shops. Both names say which, rather than one of them winning and the reader
/// having to remember. Renaming the OTA type to match would touch every call site it has and belongs
/// in its own change.
#[derive(Clone, Debug)]
pub struct PostgresConfigReleases {
    pool: Pool,
}

const SELECT_COLUMNS: &str = "id, tenant_id, name, status, target_group_id, \
     wall_clock_date, wall_clock_time, \
     (extract(epoch from instant_at) * 1000)::bigint, created_by, \
     (extract(epoch from created_at) * 1000)::bigint, \
     (extract(epoch from updated_at) * 1000)::bigint";

fn row_from(row: &tokio_postgres::Row) -> ReleaseRow {
    ReleaseRow {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        status: row.get(3),
        target_group_id: row.get(4),
        wall_clock_date: row.get(5),
        wall_clock_time: row.get(6),
        instant_at_ms: row.get(7),
        created_by: row.get(8),
        created_at_ms: row.get(9),
        updated_at_ms: row.get(10),
    }
}

impl PostgresConfigReleases {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts a release.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn create(&self, row: &NewReleaseRow<'_>) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO releases \
                 (id, tenant_id, name, status, target_group_id, wall_clock_date, wall_clock_time, \
                 instant_at, created_by) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, \
                 CASE WHEN $8::bigint IS NULL THEN NULL \
                      ELSE to_timestamp($8::bigint::double precision / 1000.0) END, $9)",
                &[
                    &row.id,
                    &row.tenant_id,
                    &row.name,
                    &row.status,
                    &row.target_group_id,
                    &row.wall_clock_date,
                    &row.wall_clock_time,
                    &row.instant_at_ms,
                    &row.created_by,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// A tenant's releases, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_for_tenant(&self, tenant_id: &str) -> Result<Vec<ReleaseRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let query = format!(
            "SELECT {SELECT_COLUMNS} FROM releases \
             WHERE tenant_id = $1 ORDER BY created_at DESC, id DESC"
        );
        let rows = connection
            .query(&query, &[&tenant_id])
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(row_from).collect())
    }

    /// One release, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<Option<ReleaseRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let query =
            format!("SELECT {SELECT_COLUMNS} FROM releases WHERE tenant_id = $1 AND id = $2");
        let rows = connection
            .query(&query, &[&tenant_id, &id])
            .await
            .map_err(unavailable)?;
        Ok(rows.first().map(row_from))
    }

    /// Moves a release to `status`, restamping `updated_at`. Returns whether the row existed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_status(
        &self,
        tenant_id: &str,
        id: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE releases SET status = $3, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed > 0)
    }

    /// Every release still scheduled or applying, across all tenants — what the activator re-tallies.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn in_flight(&self) -> Result<Vec<ReleaseRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let query = format!(
            "SELECT {SELECT_COLUMNS} FROM releases \
             WHERE status IN ('SCHEDULED', 'APPLYING') ORDER BY created_at ASC, id ASC"
        );
        let rows = connection.query(&query, &[]).await.map_err(unavailable)?;
        Ok(rows.iter().map(row_from).collect())
    }
}
