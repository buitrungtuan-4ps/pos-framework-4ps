// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The floor's persistence seams over their `store-postgres` tables: the `AreaStore`,
//! `TableStore`, `StationStore` and `RoutingRuleStore` impls, moved out of `pos-cloud`'s
//! `persistence.rs` with their traits. A trait can be implemented for another crate's type only in
//! the trait's crate, so the glue lives here; all SQL stays in the adapter.

use store_postgres::{AreaRow, PostgresFloor, RoutingRuleRow, StationRow, TableRow};

use pos_proto::display::GridPosition;
use pos_proto::ids::{AreaId, CourseId, MenuItemId, StationId, StoreId, TableId, TenantId};
use pos_proto::printing::ReceiptLanguage;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;

use crate::floorplan::{
    Area, AreaStore, AreaUpdate, FloorStoreError, NewArea, NewRoutingRule, NewStation, NewTable,
    RoutingRule, RoutingRuleId, RoutingRuleStore, Station, StationStore, StationUpdate, Table,
    TableStore, TableUpdate,
};
use crate::registry::EntityStatus;
use crate::version::{UpdateOutcome, Version, Versioned};
use pos_cloud_config::persistence::update_outcome;

// --- floor (Track M2, ADR-0072): store-postgres rows converted to the AreaStore/TableStore domain ---

/// Parses a stored ULID string, naming the field in the error for the server log.
fn parse_floor_ulid(text: &str, what: &str) -> Result<Ulid, FloorStoreError> {
    text.parse::<Ulid>()
        .map_err(|error| FloorStoreError::new(format!("stored {what} id is not a ULID: {error}")))
}

/// Reads one queried row into an [`Area`].
fn area_record(row: AreaRow) -> Result<Versioned<Area>, FloorStoreError> {
    let record = Area {
        area_id: AreaId::new(parse_floor_ulid(&row.id, "area")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    };
    Ok(Versioned::new(record, Version::new(row.version)))
}

/// Reads one queried row into a [`Table`], folding the two nullable grid columns into an optional
/// [`GridPosition`] (a table is placed only when both are set).
fn table_record(row: TableRow) -> Result<Versioned<Table>, FloorStoreError> {
    let position = match (row.grid_column, row.grid_row) {
        (Some(column), Some(grid_row)) => Some(GridPosition {
            column: u16::try_from(column).unwrap_or(0),
            row: u16::try_from(grid_row).unwrap_or(0),
        }),
        _ => None,
    };
    let record = Table {
        table_id: TableId::new(parse_floor_ulid(&row.id, "table")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        area_id: AreaId::new(parse_floor_ulid(&row.area_id, "area")?),
        label: row.label,
        seats: u16::try_from(row.seats).unwrap_or(0),
        position,
        status: EntityStatus::from_db(&row.status),
    };
    Ok(Versioned::new(record, Version::new(row.version)))
}

impl AreaStore for PostgresFloor {
    async fn create(&self, area: &NewArea) -> Result<Version, FloorStoreError> {
        self.insert_area(
            &area.area_id.to_string(),
            &area.tenant_id.to_string(),
            &area.store_id.to_string(),
            &area.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Area>>, FloorStoreError> {
        let rows = self
            .fetch_areas(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.into_iter().map(area_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        area_id: AreaId,
    ) -> Result<Option<Versioned<Area>>, FloorStoreError> {
        let row = self
            .fetch_area(&tenant.to_string(), &area_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        row.map(area_record).transpose()
    }

    async fn update(
        &self,
        area: &AreaUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        self.set_area(
            &area.tenant_id.to_string(),
            &area.area_id.to_string(),
            &area.name,
            area.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}

impl TableStore for PostgresFloor {
    async fn create(&self, table: &NewTable) -> Result<Version, FloorStoreError> {
        self.insert_table(
            &table.table_id.to_string(),
            &table.tenant_id.to_string(),
            &table.store_id.to_string(),
            &table.area_id.to_string(),
            &table.label,
            i32::from(table.seats),
            table.position.map(|position| i32::from(position.column)),
            table.position.map(|position| i32::from(position.row)),
        )
        .await
        .map(Version::new)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Table>>, FloorStoreError> {
        let rows = self
            .fetch_tables(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.into_iter().map(table_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        table_id: TableId,
    ) -> Result<Option<Versioned<Table>>, FloorStoreError> {
        let row = self
            .fetch_table(&tenant.to_string(), &table_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        row.map(table_record).transpose()
    }

    async fn update(
        &self,
        table: &TableUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        self.set_table(
            &table.tenant_id.to_string(),
            &table.table_id.to_string(),
            &table.area_id.to_string(),
            &table.label,
            i32::from(table.seats),
            table.position.map(|position| i32::from(position.column)),
            table.position.map(|position| i32::from(position.row)),
            table.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}

/// A station's late threshold as its `integer` column holds it.
///
/// # Errors
///
/// [`FloorStoreError`] for a threshold no `integer` holds, which the route's bounds never let
/// through.
fn late_after_column(seconds: Option<u32>) -> Result<Option<i32>, FloorStoreError> {
    seconds
        .map(|seconds| {
            i32::try_from(seconds).map_err(|_| {
                FloorStoreError::new("a station's late threshold does not fit its column")
            })
        })
        .transpose()
}

/// Reads one queried row into a [`Station`].
fn station_record(row: StationRow) -> Result<Versioned<Station>, FloorStoreError> {
    let backup_station_id = row
        .backup_station_id
        .as_deref()
        .map(|text| parse_floor_ulid(text, "backup station").map(StationId::new))
        .transpose()?;
    let record = Station {
        station_id: StationId::new(parse_floor_ulid(&row.id, "station")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        name: row.name,
        backup_station_id,
        is_default: row.is_default,
        // A column the route only ever writes within its bounds; a value that is not a `u32` reads
        // as unset, the default, rather than refusing the whole list.
        late_after_seconds: row
            .late_after_seconds
            .and_then(|seconds| u32::try_from(seconds).ok()),
        ticket_language: row.ticket_language,
        status: EntityStatus::from_db(&row.status),
    };
    Ok(Versioned::new(record, Version::new(row.version)))
}

/// Reads one queried row into a [`RoutingRule`].
fn routing_rule_record(row: &RoutingRuleRow) -> Result<RoutingRule, FloorStoreError> {
    let menu_item_id = row
        .menu_item_id
        .as_deref()
        .map(|text| parse_floor_ulid(text, "menu item").map(MenuItemId::new))
        .transpose()?;
    let course_id = row
        .course_id
        .as_deref()
        .map(|text| parse_floor_ulid(text, "course").map(CourseId::new))
        .transpose()?;
    Ok(RoutingRule {
        rule_id: RoutingRuleId::new(parse_floor_ulid(&row.id, "routing rule")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        station_id: StationId::new(parse_floor_ulid(&row.station_id, "station")?),
        menu_item_id,
        course_id,
        sort: u16::try_from(row.sort).unwrap_or(0),
    })
}

impl StationStore for PostgresFloor {
    async fn create(&self, station: &NewStation) -> Result<Version, FloorStoreError> {
        let backup = station.backup_station_id.map(|id| id.to_string());
        self.insert_station(
            &station.station_id.to_string(),
            &station.tenant_id.to_string(),
            &station.store_id.to_string(),
            &station.name,
            backup.as_deref(),
            station.is_default,
            late_after_column(station.late_after_seconds)?,
            station.ticket_language.map(ReceiptLanguage::as_wire),
        )
        .await
        .map(Version::new)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Station>>, FloorStoreError> {
        let rows = self
            .fetch_stations(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.into_iter().map(station_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        station_id: StationId,
    ) -> Result<Option<Versioned<Station>>, FloorStoreError> {
        let row = self
            .fetch_station(&tenant.to_string(), &station_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        row.map(station_record).transpose()
    }

    async fn update(
        &self,
        station: &StationUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        let backup = station.backup_station_id.map(|id| id.to_string());
        self.set_station(
            &station.tenant_id.to_string(),
            &station.station_id.to_string(),
            &station.name,
            backup.as_deref(),
            station.is_default,
            late_after_column(station.late_after_seconds)?,
            station.ticket_language.map(ReceiptLanguage::as_wire),
            station.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}

impl RoutingRuleStore for PostgresFloor {
    async fn create(&self, rule: &NewRoutingRule) -> Result<(), FloorStoreError> {
        let menu_item = rule.menu_item_id.map(|id| id.to_string());
        let course = rule.course_id.map(|id| id.to_string());
        self.insert_rule(
            &rule.rule_id.to_string(),
            &rule.tenant_id.to_string(),
            &rule.store_id.to_string(),
            &rule.station_id.to_string(),
            menu_item.as_deref(),
            course.as_deref(),
            i32::from(rule.sort),
        )
        .await
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<RoutingRule>, FloorStoreError> {
        let rows = self
            .fetch_rules(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.iter().map(routing_rule_record).collect()
    }

    async fn remove(
        &self,
        tenant: TenantId,
        rule_id: RoutingRuleId,
    ) -> Result<bool, FloorStoreError> {
        self.delete_rule(&tenant.to_string(), &rule_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}
