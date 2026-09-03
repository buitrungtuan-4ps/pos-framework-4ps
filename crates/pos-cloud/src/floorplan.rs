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

use core::fmt;
use core::future::Future;

use pos_proto::display::GridPosition;
use pos_proto::ids::{AreaId, CourseId, MenuItemId, StationId, StoreId, TableId, TenantId};
use pos_proto::ulid::Ulid;

use crate::registry::EntityStatus;
use crate::version::{UpdateOutcome, Version, Versioned};

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
    fn create(
        &self,
        area: &NewArea,
    ) -> impl Future<Output = Result<Version, FloorStoreError>> + Send;

    /// Lists a store's areas, newest first.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<Versioned<Area>>, FloorStoreError>> + Send;

    /// Reads one area within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        area_id: AreaId,
    ) -> impl Future<Output = Result<Option<Versioned<Area>>, FloorStoreError>> + Send;

    /// Renames an area and/or sets its status. Applies only at `expected`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn update(
        &self,
        area: &AreaUpdate,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, FloorStoreError>> + Send;
}

/// Persists and reads a store's floor tables. Archived, never deleted.
pub trait TableStore {
    /// Inserts a table.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn create(
        &self,
        table: &NewTable,
    ) -> impl Future<Output = Result<Version, FloorStoreError>> + Send;

    /// Lists a store's tables, newest first.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<Versioned<Table>>, FloorStoreError>> + Send;

    /// Reads one table within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        table_id: TableId,
    ) -> impl Future<Output = Result<Option<Versioned<Table>>, FloorStoreError>> + Send;

    /// Updates a table's area, label, seats, position, and status. Applies only at `expected`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn update(
        &self,
        table: &TableUpdate,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, FloorStoreError>> + Send;
}

// --- kitchen: stations and item→station routing rules (ADR-0072) ---

/// A kitchen station as the console reads it: a station on one store's line, with an optional backup
/// (the printer failover) and a catch-all `is_default` flag.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Station {
    /// The station's id.
    pub station_id: StationId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store this station belongs to.
    pub store_id: StoreId,
    /// The name to show ("Oven").
    pub name: String,
    /// The failover target when this station's printer is down, or `None` for no backup.
    pub backup_station_id: Option<StationId>,
    /// Whether this is the store's catch-all station (a fired line with no matching rule).
    pub is_default: bool,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A new station to create.
#[derive(Debug, Clone)]
pub struct NewStation {
    /// The minted id.
    pub station_id: StationId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store.
    pub store_id: StoreId,
    /// The name.
    pub name: String,
    /// The backup station, or `None`.
    pub backup_station_id: Option<StationId>,
    /// Whether it is the catch-all.
    pub is_default: bool,
}

/// An update to a station's name, backup, default flag, and/or status.
#[derive(Debug, Clone)]
pub struct StationUpdate {
    /// The station to change.
    pub station_id: StationId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The new name.
    pub name: String,
    /// The new backup, or `None`.
    pub backup_station_id: Option<StationId>,
    /// Whether it is now the catch-all.
    pub is_default: bool,
    /// The new status (archiving retires the station without deleting it).
    pub status: EntityStatus,
}

/// A routing rule's identifier — a ULID minted at creation. Cloud-local (like
/// [`EmployeeId`](crate::people::EmployeeId)): a rule is authoring-only and compiles into the
/// `stations` node's flat routing list, which carries no rule id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct RoutingRuleId(Ulid);

impl RoutingRuleId {
    /// Wraps a ULID as a routing-rule id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for RoutingRuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An item→station routing rule as the console reads it. Matches a specific item or any line on a
/// course (the route layer enforces exactly one); `sort` orders rules within their tier.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingRule {
    /// The rule's id.
    pub rule_id: RoutingRuleId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store this rule belongs to.
    pub store_id: StoreId,
    /// The station a matching line routes to.
    pub station_id: StationId,
    /// The item this rule matches, if any.
    pub menu_item_id: Option<MenuItemId>,
    /// The course this rule matches, if any.
    pub course_id: Option<CourseId>,
    /// The author-controlled order within its tier.
    pub sort: u16,
}

/// A new routing rule to create.
#[derive(Debug, Clone)]
pub struct NewRoutingRule {
    /// The minted id.
    pub rule_id: RoutingRuleId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The store.
    pub store_id: StoreId,
    /// The target station.
    pub station_id: StationId,
    /// The item to match, if any.
    pub menu_item_id: Option<MenuItemId>,
    /// The course to match, if any.
    pub course_id: Option<CourseId>,
    /// The order within its tier.
    pub sort: u16,
}

/// Persists and reads a store's kitchen stations. Archived, never deleted.
pub trait StationStore {
    /// Inserts a station.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn create(
        &self,
        station: &NewStation,
    ) -> impl Future<Output = Result<Version, FloorStoreError>> + Send;

    /// Lists a store's stations, newest first.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<Versioned<Station>>, FloorStoreError>> + Send;

    /// Reads one station within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        station_id: StationId,
    ) -> impl Future<Output = Result<Option<Versioned<Station>>, FloorStoreError>> + Send;

    /// Updates a station's name, backup, default flag, and status. Applies only at `expected`.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn update(
        &self,
        station: &StationUpdate,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, FloorStoreError>> + Send;
}

/// Persists and reads a store's item→station routing rules. Unlike stations, a rule is a mapping that
/// is **removed** (not archived).
pub trait RoutingRuleStore {
    /// Inserts a routing rule.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn create(
        &self,
        rule: &NewRoutingRule,
    ) -> impl Future<Output = Result<(), FloorStoreError>> + Send;

    /// Lists a store's routing rules, in `sort` then insertion order.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<RoutingRule>, FloorStoreError>> + Send;

    /// Removes a routing rule. Returns whether a row was removed.
    ///
    /// # Errors
    ///
    /// [`FloorStoreError`] if the write fails.
    fn remove(
        &self,
        tenant: TenantId,
        rule_id: RoutingRuleId,
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
