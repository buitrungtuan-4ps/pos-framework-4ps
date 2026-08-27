// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Floor master data over PostgreSQL (Track M2, [ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
//!
//! A store's areas and the tables in each, one tenant-and-store-scoped row apiece. This adapter keeps
//! only the SQL and returns plain rows; `pos-cloud` implements its `AreaStore`/`TableStore` seams over
//! this type. Tenant scoping is the explicit `WHERE tenant_id = $1` filter every cloud adapter carries;
//! a floor is per-store, so reads also filter `store_id`. Its methods use distinct verbs from the seam
//! (`insert`/`fetch`/`set`) so the seam impl calls the SQL and never itself. Both entities are archived,
//! never deleted.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// An area as listed — identity, its store, name, and status.
#[derive(Clone, Debug)]
pub struct AreaRow {
    /// The area id (a ULID string).
    pub id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The store this area belongs to.
    pub store_id: String,
    /// The area name.
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
}

/// The area columns a read returns, in a stable order matching [`area_row`].
const AREA_COLUMNS: &str = "id, tenant_id, store_id, name, status";

/// A table as listed — identity, its store and area, label, seat count, optional grid position, status.
#[derive(Clone, Debug)]
pub struct TableRow {
    /// The table id (a ULID string).
    pub id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The store this table belongs to.
    pub store_id: String,
    /// The area this table sits in.
    pub area_id: String,
    /// The label a host reads ("T1").
    pub label: String,
    /// How many covers the table seats.
    pub seats: i32,
    /// The grid column, or `None` if the table is unplaced.
    pub grid_column: Option<i32>,
    /// The grid row, or `None` if the table is unplaced.
    pub grid_row: Option<i32>,
    /// `active` or `archived`.
    pub status: String,
}

/// The table columns a read returns, in a stable order matching [`table_row`].
const TABLE_COLUMNS: &str =
    "id, tenant_id, store_id, area_id, label, seats, grid_column, grid_row, status";

/// The floor master-data store over a shared pool. Built by
/// [`PostgresStore::floor`](crate::PostgresStore::floor).
#[derive(Clone, Debug)]
pub struct PostgresFloor {
    pool: Pool,
}

impl PostgresFloor {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts an area.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_area(
        &self,
        id: &str,
        tenant_id: &str,
        store_id: &str,
        name: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO floor_areas (id, tenant_id, store_id, name) VALUES ($1, $2, $3, $4)",
                &[&id, &tenant_id, &store_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a store's areas, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_areas(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Vec<AreaRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {AREA_COLUMNS} FROM floor_areas \
                     WHERE tenant_id = $1 AND store_id = $2 ORDER BY created_at DESC"
                ),
                &[&tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(area_row).collect())
    }

    /// Reads one area within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_area(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<Option<AreaRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!("SELECT {AREA_COLUMNS} FROM floor_areas WHERE tenant_id = $1 AND id = $2"),
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(area_row))
    }

    /// Renames an area and sets its status, within its tenant. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_area(
        &self,
        tenant_id: &str,
        id: &str,
        name: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE floor_areas SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id, &name, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    /// Inserts a table.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "a table row is a flat record of primitive columns; a params struct would only \
                  re-list them"
    )]
    pub async fn insert_table(
        &self,
        id: &str,
        tenant_id: &str,
        store_id: &str,
        area_id: &str,
        label: &str,
        seats: i32,
        grid_column: Option<i32>,
        grid_row: Option<i32>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO floor_tables \
                 (id, tenant_id, store_id, area_id, label, seats, grid_column, grid_row) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[
                    &id,
                    &tenant_id,
                    &store_id,
                    &area_id,
                    &label,
                    &seats,
                    &grid_column,
                    &grid_row,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a store's tables, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_tables(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Vec<TableRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {TABLE_COLUMNS} FROM floor_tables \
                     WHERE tenant_id = $1 AND store_id = $2 ORDER BY created_at DESC"
                ),
                &[&tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(table_row).collect())
    }

    /// Reads one table within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_table(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<Option<TableRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {TABLE_COLUMNS} FROM floor_tables WHERE tenant_id = $1 AND id = $2"
                ),
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(table_row))
    }

    /// Updates a table's area, label, seats, position, and status. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    #[expect(
        clippy::too_many_arguments,
        reason = "a table row is a flat record of primitive columns; a params struct would only \
                  re-list them"
    )]
    pub async fn set_table(
        &self,
        tenant_id: &str,
        id: &str,
        area_id: &str,
        label: &str,
        seats: i32,
        grid_column: Option<i32>,
        grid_row: Option<i32>,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE floor_tables \
                 SET area_id = $3, label = $4, seats = $5, grid_column = $6, grid_row = $7, \
                     status = $8, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2",
                &[
                    &tenant_id,
                    &id,
                    &area_id,
                    &label,
                    &seats,
                    &grid_column,
                    &grid_row,
                    &status,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }
}

/// Reads one queried row into an [`AreaRow`]. The column order matches [`AREA_COLUMNS`].
fn area_row(row: &tokio_postgres::Row) -> AreaRow {
    AreaRow {
        id: row.get(0),
        tenant_id: row.get(1),
        store_id: row.get(2),
        name: row.get(3),
        status: row.get(4),
    }
}

/// Reads one queried row into a [`TableRow`]. The column order matches [`TABLE_COLUMNS`].
fn table_row(row: &tokio_postgres::Row) -> TableRow {
    TableRow {
        id: row.get(0),
        tenant_id: row.get(1),
        store_id: row.get(2),
        area_id: row.get(3),
        label: row.get(4),
        seats: row.get(5),
        grid_column: row.get(6),
        grid_row: row.get(7),
        status: row.get(8),
    }
}
