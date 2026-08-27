// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The floor & kitchen master-data seam ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md), Track M2).
//!
//! A store's **floor**: its **areas** (a terrace, the main hall) and the **tables** in each. Unlike the
//! catalog (per-tenant), a floor is a physical room, so these are **per-store** master data — every
//! record carries both a `tenant_id` and a `store_id`. None of it is PII: an area is a name, a table is
//! a label, a seat count, and an optional grid position (where the visual editor pins it).
//!
//! Traits so they run against an in-memory fake in tests and the tenant-scoped, RLS-isolated
//! `floor_areas` / `floor_tables` tables in the cloud (the impl lives in [`crate::persistence`], the SQL
//! in `store-postgres`). Areas and tables are **archived, never hard-deleted**, so a published floor
//! plan (a later M2 slice) and any order history that names a table stay reconcilable.
//!
//! The identifiers ([`AreaId`](pos_proto::ids::AreaId), [`TableId`](pos_proto::ids::TableId)) are
//! `pos-proto` types, not cloud-local ones, because they cross the wire: the compiled `floor` config
//! node the edge reads names its tables by the very ids authored here.

use core::future::Future;

use pos_proto::display::GridPosition;
use pos_proto::ids::{AreaId, StoreId, TableId, TenantId};

use crate::registry::EntityStatus;

/// An area as the console reads it: a named region of one store's floor.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Area {
    /// The area's id.
    pub area_id: AreaId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store this area belongs to.
    pub store_id: StoreId,
    /// The name to show ("Terrace").
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A new area to create — identity + name; status starts active.
#[derive(Debug, Clone)]
pub struct NewArea {
    /// The minted id.
    pub area_id: AreaId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store.
    pub store_id: StoreId,
    /// The name.
    pub name: String,
}

/// An update to an area's name and/or status, addressed by id within its tenant.
#[derive(Debug, Clone)]
pub struct AreaUpdate {
    /// The area to change.
    pub area_id: AreaId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The new name.
    pub name: String,
    /// The new status (archiving retires the area without deleting it).
    pub status: EntityStatus,
}

/// A table as the console reads it: a table on one store's floor, in an area, optionally placed on the
/// editor's grid.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Table {
    /// The table's id.
    pub table_id: TableId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store this table belongs to.
    pub store_id: StoreId,
    /// The area it sits in.
    pub area_id: AreaId,
    /// The label a host reads ("T1").
    pub label: String,
    /// How many covers it seats (zero means unspecified).
    pub seats: u16,
    /// Where it sits in the floor editor's grid, or `None` if not yet placed.
    pub position: Option<GridPosition>,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A new table to create.
#[derive(Debug, Clone)]
pub struct NewTable {
    /// The minted id.
    pub table_id: TableId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store.
    pub store_id: StoreId,
    /// The area it belongs to.
    pub area_id: AreaId,
    /// The label.
    pub label: String,
    /// The seat count.
    pub seats: u16,
    /// The grid position, or `None` if unplaced.
    pub position: Option<GridPosition>,
}

/// An update to a table's area, label, seats, position, and/or status.
#[derive(Debug, Clone)]
pub struct TableUpdate {
    /// The table to change.
    pub table_id: TableId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The area it now belongs to.
    pub area_id: AreaId,
    /// The new label.
    pub label: String,
    /// The new seat count.
    pub seats: u16,
    /// The new grid position, or `None` to unplace it.
    pub position: Option<GridPosition>,
    /// The new status (archiving retires the table without deleting it).
    pub status: EntityStatus,
}

/// Persists and reads a store's floor areas. Archived, never deleted.
pub trait AreaStore {
    /// Inserts an area.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn create(&self, area: &NewArea) -> impl Future<Output = Result<(), FloorStoreError>> + Send;

    /// Lists a store's areas, newest first.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<Area>, FloorStoreError>> + Send;

    /// Reads one area within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        area_id: AreaId,
    ) -> impl Future<Output = Result<Option<Area>, FloorStoreError>> + Send;

    /// Renames an area and/or sets its status. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn update(
        &self,
        area: &AreaUpdate,
    ) -> impl Future<Output = Result<bool, FloorStoreError>> + Send;
}

/// Persists and reads a store's floor tables. Archived, never deleted.
pub trait TableStore {
    /// Inserts a table.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn create(&self, table: &NewTable) -> impl Future<Output = Result<(), FloorStoreError>> + Send;

    /// Lists a store's tables, newest first.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<Table>, FloorStoreError>> + Send;

    /// Reads one table within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        table_id: TableId,
    ) -> impl Future<Output = Result<Option<Table>, FloorStoreError>> + Send;

    /// Updates a table's area, label, seats, position, and status. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn update(
        &self,
        table: &TableUpdate,
    ) -> impl Future<Output = Result<bool, FloorStoreError>> + Send;
}

/// A failure of the floor store itself — the database is unreachable, or a write violated a constraint.
#[derive(Debug, thiserror::Error)]
#[error("the floor store failed: {0}")]
pub struct FloorStoreError(String);

impl FloorStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
