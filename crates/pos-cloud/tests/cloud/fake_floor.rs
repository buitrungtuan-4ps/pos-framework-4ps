// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The in-memory floor store the cloud's tests run on, moved out of `cloud.rs` so it can learn the
//! area tree (ADR-0211) without that file growing.

use std::sync::{Arc, Mutex};

use pos_cloud::floorplan::{
    Area, AreaStore, AreaUpdate, AreaWriteOutcome, FloorStoreError, NewArea, NewRoutingRule,
    NewStation, NewTable, RoutingRule, RoutingRuleId, RoutingRuleStore, Station, StationStore,
    StationUpdate, Table, TableStore, TableUpdate,
};
use pos_cloud::registry::EntityStatus;
use pos_cloud::version::{UpdateOutcome, Version, Versioned};
use pos_core::floor::area_tree::{AreaLink, area_change_refusal};
use pos_proto::ids::{AreaId, StationId, StoreId, TableId, TenantId};
use pos_proto::wire_enum::WireEnum;

/// The floor master-data store as two in-memory lists — the same `AreaStore`/`TableStore` seams the
/// binary implements over the tenant-and-store-scoped `floor_areas`/`floor_tables` tables. Areas and
/// tables are archived, never removed (Track M2, ADR-0072).
#[derive(Clone, Default)]
pub(crate) struct FakeFloor {
    areas: Arc<Mutex<Vec<Versioned<Area>>>>,
    tables: Arc<Mutex<Vec<Versioned<Table>>>>,
    stations: Arc<Mutex<Vec<Versioned<Station>>>>,
    rules: Arc<Mutex<Vec<RoutingRule>>>,
    next_version: Arc<Mutex<u64>>,
}

impl FakeFloor {
    /// The next version, as the store-postgres adapter's `xmin::text` is: a token, not a number the
    /// caller may reason about. One counter across all three entities — a version is compared only
    /// against the row it came from, so sharing the sequence proves nothing depends on it.
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(next.to_string())
    }
}

impl FakeFloor {
    /// The tree rule over the store's areas as they stand, as the adapter runs it inside its write.
    fn tree_refusal(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        change: AreaLink,
    ) -> Option<AreaWriteOutcome> {
        let links: Vec<AreaLink> = self
            .areas
            .lock()
            .expect("lock")
            .iter()
            .filter(|area| area.record.tenant_id == tenant && area.record.store_id == store_id)
            .map(|area| AreaLink {
                area_id: area.record.area_id,
                parent_area_id: area.record.parent_area_id,
                active: area.record.status == EntityStatus::Active,
            })
            .collect();
        area_change_refusal(&links, &change).map(AreaWriteOutcome::Refused)
    }
}

impl AreaStore for FakeFloor {
    async fn create(&self, area: &NewArea) -> Result<AreaWriteOutcome, FloorStoreError> {
        let change = AreaLink {
            area_id: area.area_id,
            parent_area_id: area.parent_area_id,
            active: true,
        };
        if let Some(refused) = self.tree_refusal(area.tenant_id, area.store_id, change) {
            return Ok(refused);
        }
        let version = self.mint();
        self.areas.lock().expect("lock").push(Versioned::new(
            Area {
                area_id: area.area_id,
                tenant_id: area.tenant_id,
                store_id: area.store_id,
                name: area.name.clone(),
                status: EntityStatus::Active,
                parent_area_id: area.parent_area_id,
            },
            version.clone(),
        ));
        Ok(AreaWriteOutcome::Written(version))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Area>>, FloorStoreError> {
        let mut rows: Vec<Versioned<Area>> = self
            .areas
            .lock()
            .expect("lock")
            .iter()
            .filter(|area| area.record.tenant_id == tenant && area.record.store_id == store_id)
            .cloned()
            .collect();
        rows.reverse(); // stored oldest-first; the read is newest-first.
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        area_id: AreaId,
    ) -> Result<Option<Versioned<Area>>, FloorStoreError> {
        Ok(self
            .areas
            .lock()
            .expect("lock")
            .iter()
            .find(|area| area.record.tenant_id == tenant && area.record.area_id == area_id)
            .cloned())
    }

    async fn update(
        &self,
        update: &AreaUpdate,
        expected: &Version,
    ) -> Result<AreaWriteOutcome, FloorStoreError> {
        let Some(current) = AreaStore::get(self, update.tenant_id, update.area_id).await? else {
            return Ok(AreaWriteOutcome::NotFound);
        };
        if &current.etag != expected {
            return Ok(AreaWriteOutcome::VersionMismatch);
        }
        let change = AreaLink {
            area_id: update.area_id,
            parent_area_id: update.parent_area_id,
            active: update.status == EntityStatus::Active,
        };
        if let Some(refused) = self.tree_refusal(update.tenant_id, current.record.store_id, change)
        {
            return Ok(refused);
        }
        let version = self.mint();
        let mut rows = self.areas.lock().expect("lock");
        if let Some(row) = rows.iter_mut().find(|area| {
            area.record.tenant_id == update.tenant_id && area.record.area_id == update.area_id
        }) {
            row.record.name.clone_from(&update.name);
            row.record.status = update.status;
            row.record.parent_area_id = update.parent_area_id;
            row.etag = version.clone();
        }
        Ok(AreaWriteOutcome::Written(version))
    }
}

impl TableStore for FakeFloor {
    async fn create(&self, table: &NewTable) -> Result<Version, FloorStoreError> {
        let version = self.mint();
        self.tables.lock().expect("lock").push(Versioned::new(
            Table {
                table_id: table.table_id,
                tenant_id: table.tenant_id,
                store_id: table.store_id,
                area_id: table.area_id,
                label: table.label.clone(),
                seats: table.seats,
                position: table.position,
                status: EntityStatus::Active,
            },
            version.clone(),
        ));
        Ok(version)
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Table>>, FloorStoreError> {
        let mut rows: Vec<Versioned<Table>> = self
            .tables
            .lock()
            .expect("lock")
            .iter()
            .filter(|table| table.record.tenant_id == tenant && table.record.store_id == store_id)
            .cloned()
            .collect();
        rows.reverse();
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        table_id: TableId,
    ) -> Result<Option<Versioned<Table>>, FloorStoreError> {
        Ok(self
            .tables
            .lock()
            .expect("lock")
            .iter()
            .find(|table| table.record.tenant_id == tenant && table.record.table_id == table_id)
            .cloned())
    }

    async fn update(
        &self,
        update: &TableUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        let version = self.mint();
        let mut rows = self.tables.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|table| {
            table.record.tenant_id == update.tenant_id && table.record.table_id == update.table_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record.area_id = update.area_id;
        row.record.label.clone_from(&update.label);
        row.record.seats = update.seats;
        row.record.position = update.position;
        row.record.status = update.status;
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }
}

impl StationStore for FakeFloor {
    async fn create(&self, station: &NewStation) -> Result<Version, FloorStoreError> {
        let version = self.mint();
        self.stations.lock().expect("lock").push(Versioned::new(
            Station {
                station_id: station.station_id,
                tenant_id: station.tenant_id,
                store_id: station.store_id,
                name: station.name.clone(),
                backup_station_id: station.backup_station_id,
                is_default: station.is_default,
                late_after_seconds: station.late_after_seconds,
                ticket_language: station
                    .ticket_language
                    .map(|language| language.as_wire().to_owned()),
                status: EntityStatus::Active,
            },
            version.clone(),
        ));
        Ok(version)
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Station>>, FloorStoreError> {
        let mut rows: Vec<Versioned<Station>> = self
            .stations
            .lock()
            .expect("lock")
            .iter()
            .filter(|station| {
                station.record.tenant_id == tenant && station.record.store_id == store_id
            })
            .cloned()
            .collect();
        rows.reverse();
        Ok(rows)
    }

    async fn get(
        &self,
        tenant: TenantId,
        station_id: StationId,
    ) -> Result<Option<Versioned<Station>>, FloorStoreError> {
        Ok(self
            .stations
            .lock()
            .expect("lock")
            .iter()
            .find(|station| {
                station.record.tenant_id == tenant && station.record.station_id == station_id
            })
            .cloned())
    }

    async fn update(
        &self,
        update: &StationUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        let version = self.mint();
        let mut rows = self.stations.lock().expect("lock");
        let Some(row) = rows.iter_mut().find(|station| {
            station.record.tenant_id == update.tenant_id
                && station.record.station_id == update.station_id
        }) else {
            return Ok(UpdateOutcome::NotFound);
        };
        if &row.etag != expected {
            return Ok(UpdateOutcome::VersionMismatch);
        }
        row.record.name.clone_from(&update.name);
        row.record.backup_station_id = update.backup_station_id;
        row.record.is_default = update.is_default;
        row.record.late_after_seconds = update.late_after_seconds;
        row.record.ticket_language = update
            .ticket_language
            .map(|language| language.as_wire().to_owned());
        row.record.status = update.status;
        row.etag = version.clone();
        Ok(UpdateOutcome::Updated(version))
    }
}

impl RoutingRuleStore for FakeFloor {
    async fn create(&self, rule: &NewRoutingRule) -> Result<(), FloorStoreError> {
        self.rules.lock().expect("lock").push(RoutingRule {
            rule_id: rule.rule_id,
            tenant_id: rule.tenant_id,
            store_id: rule.store_id,
            station_id: rule.station_id,
            menu_item_id: rule.menu_item_id,
            course_id: rule.course_id,
            sort: rule.sort,
        });
        Ok(())
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<RoutingRule>, FloorStoreError> {
        let mut rows: Vec<RoutingRule> = self
            .rules
            .lock()
            .expect("lock")
            .iter()
            .filter(|rule| rule.tenant_id == tenant && rule.store_id == store_id)
            .cloned()
            .collect();
        rows.sort_by_key(|rule| rule.sort); // the seam reads rules in `sort` order.
        Ok(rows)
    }

    async fn remove(
        &self,
        tenant: TenantId,
        rule_id: RoutingRuleId,
    ) -> Result<bool, FloorStoreError> {
        let mut rows = self.rules.lock().expect("lock");
        let before = rows.len();
        rows.retain(|rule| !(rule.tenant_id == tenant && rule.rule_id == rule_id));
        Ok(rows.len() != before)
    }
}
