// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Compile a store's authored floor & kitchen master data into the `FloorPlan`/`StationPlan` config
//! nodes the edge reads ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md), Track M2 slice 4).
//!
//! Pure: the caller loads the store's areas, tables, stations, and routing rules (from the CRUD seams)
//! and this turns them into the two structured nodes that ride the config tree to the store like every
//! other config change ([ADR-0033](../../../docs/adr/0033-config-tree.md)) — no new channel. Unlike the
//! people compiler, the target shapes are the shared `pos-proto` types themselves, so this is a
//! transformation, not a bespoke document.
//!
//! **Forgiving by construction** (the same posture the people compiler keeps): only **active** areas,
//! tables, and stations are emitted (an archived one is dropped from the published plan); a table under
//! an archived area is dropped with it; a station's backup that names an archived/removed station (or
//! itself) is cleared; a routing rule whose target station is not in the active set is dropped. So the
//! compiled plan references only things that exist — the §10 referential validation the publish runs
//! (`pos_core::floor`) then passes on a plan authored consistently, and only a genuine inconsistency
//! surfaces as a `422`. Output is sorted (areas/tables/stations by id, rules by `sort`) so two publishes
//! of the same state produce byte-identical JSON.

use std::collections::BTreeSet;

use pos_proto::floor::{
    FloorArea, FloorPlan, FloorTable, KitchenStation, RoutingRule as PlanRoutingRule, StationPlan,
};
use pos_proto::text::DisplayName;

use crate::floorplan::{Area, RoutingRule, Station, Table};
use crate::registry::EntityStatus;

/// Compiles a store's areas and tables into its `floor` node — active areas, each with its active
/// tables, in id order.
#[must_use]
pub fn compile_floor(areas: &[Area], tables: &[Table]) -> FloorPlan {
    let mut plan_areas: Vec<FloorArea> = areas
        .iter()
        .filter(|area| area.status == EntityStatus::Active)
        .map(|area| {
            let mut floor_tables: Vec<FloorTable> = tables
                .iter()
                .filter(|table| {
                    table.status == EntityStatus::Active && table.area_id == area.area_id
                })
                .map(|table| FloorTable {
                    table_id: table.table_id,
                    label: DisplayName::new(table.label.clone()),
                    seats: table.seats,
                    position: table.position,
                })
                .collect();
            floor_tables.sort_by_key(|table| table.table_id.to_string());
            FloorArea {
                area_id: area.area_id,
                name: DisplayName::new(area.name.clone()),
                tables: floor_tables,
            }
        })
        .collect();
    plan_areas.sort_by_key(|area| area.area_id.to_string());
    FloorPlan::from_areas(plan_areas)
}

/// Compiles a store's stations and routing rules into its `stations` node — active stations (their
/// backup cleared if it does not name another active station), the routing rules that target an active
/// station in `sort` order, and the default (catch-all) station if one is flagged.
#[must_use]
pub fn compile_stations(stations: &[Station], rules: &[RoutingRule]) -> StationPlan {
    let active: Vec<&Station> = stations
        .iter()
        .filter(|station| station.status == EntityStatus::Active)
        .collect();
    let active_ids: BTreeSet<_> = active.iter().map(|station| station.station_id).collect();

    let mut plan_stations: Vec<KitchenStation> = active
        .iter()
        .map(|station| KitchenStation {
            station_id: station.station_id,
            name: DisplayName::new(station.name.clone()),
            backup_station_id: station
                .backup_station_id
                .filter(|backup| *backup != station.station_id && active_ids.contains(backup)),
        })
        .collect();
    plan_stations.sort_by_key(|station| station.station_id.to_string());

    let mut kept: Vec<&RoutingRule> = rules
        .iter()
        .filter(|rule| active_ids.contains(&rule.station_id))
        .collect();
    kept.sort_by(|a, b| {
        a.sort
            .cmp(&b.sort)
            .then_with(|| a.rule_id.to_string().cmp(&b.rule_id.to_string()))
    });
    let routing: Vec<PlanRoutingRule> = kept
        .iter()
        .map(|rule| PlanRoutingRule {
            station_id: rule.station_id,
            menu_item_id: rule.menu_item_id,
            course_id: rule.course_id,
        })
        .collect();

    // The default is the lowest-id active station flagged `is_default` (deterministic when more than
    // one is flagged — the console offers a single choice, but the compile must not depend on order).
    let default_station_id = active
        .iter()
        .filter(|station| station.is_default)
        .map(|station| station.station_id)
        .min();

    StationPlan::from_parts(plan_stations, routing, default_station_id)
}

#[cfg(test)]
mod tests {
    use super::{compile_floor, compile_stations};

    use pos_proto::ids::{AreaId, CourseId, MenuItemId, StationId, StoreId, TableId, TenantId};
    use pos_proto::ulid::Ulid;

    use crate::floorplan::{Area, RoutingRule, RoutingRuleId, Station, Table};
    use crate::registry::EntityStatus;

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(0x7E))
    }
    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(0x5701))
    }

    fn area(id: u128, name: &str, status: EntityStatus) -> Area {
        Area {
            area_id: AreaId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            store_id: store(),
            name: name.to_owned(),
            status,
        }
    }

    fn table(id: u128, area: u128, label: &str, status: EntityStatus) -> Table {
        Table {
            table_id: TableId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            store_id: store(),
            area_id: AreaId::new(Ulid::from_u128(area)),
            label: label.to_owned(),
            seats: 2,
            position: None,
            status,
        }
    }

    fn station(
        id: u128,
        name: &str,
        backup: Option<u128>,
        is_default: bool,
        status: EntityStatus,
    ) -> Station {
        Station {
            station_id: StationId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            store_id: store(),
            name: name.to_owned(),
            backup_station_id: backup.map(|b| StationId::new(Ulid::from_u128(b))),
            is_default,
            status,
        }
    }

    fn rule(
        id: u128,
        station: u128,
        item: Option<u128>,
        course: Option<u128>,
        sort: u16,
    ) -> RoutingRule {
        RoutingRule {
            rule_id: RoutingRuleId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            store_id: store(),
            station_id: StationId::new(Ulid::from_u128(station)),
            menu_item_id: item.map(|i| MenuItemId::new(Ulid::from_u128(i))),
            course_id: course.map(|c| CourseId::new(Ulid::from_u128(c))),
            sort,
        }
    }

    #[test]
    fn floor_drops_archived_areas_and_tables_and_groups_the_rest() {
        let areas = vec![
            area(1, "Terrace", EntityStatus::Active),
            area(2, "Old room", EntityStatus::Archived),
        ];
        let tables = vec![
            table(10, 1, "T1", EntityStatus::Active),
            table(11, 1, "T2", EntityStatus::Archived), // archived table dropped
            table(12, 2, "T3", EntityStatus::Active),   // table under archived area dropped
        ];
        let plan = compile_floor(&areas, &tables);
        assert_eq!(plan.areas().len(), 1);
        assert_eq!(plan.areas()[0].tables.len(), 1);
        assert_eq!(plan.tables().count(), 1);
    }

    #[test]
    fn stations_clear_stale_backups_drop_orphan_rules_and_pick_the_default() {
        let stations = vec![
            station(1, "Oven", Some(9), true, EntityStatus::Active), // backup 9 is unknown -> cleared
            station(2, "Bar", Some(1), false, EntityStatus::Active),
            station(3, "Gone", None, false, EntityStatus::Archived),
        ];
        let rules = vec![
            rule(20, 1, Some(100), None, 1),
            rule(21, 3, None, Some(200), 0), // targets an archived station -> dropped
        ];
        let plan = compile_stations(&stations, &rules);
        assert_eq!(plan.stations().len(), 2);
        assert_eq!(
            plan.station(StationId::new(Ulid::from_u128(1)))
                .and_then(|s| s.backup_station_id),
            None,
            "the unknown backup was cleared"
        );
        assert_eq!(
            plan.station(StationId::new(Ulid::from_u128(2)))
                .and_then(|s| s.backup_station_id),
            Some(StationId::new(Ulid::from_u128(1))),
            "a backup naming an active station survives"
        );
        assert_eq!(
            plan.routing().len(),
            1,
            "the rule to the archived station was dropped"
        );
        assert_eq!(
            plan.default_station_id(),
            Some(StationId::new(Ulid::from_u128(1)))
        );
    }

    #[test]
    fn a_clean_plan_passes_pos_core_validation() {
        let areas = vec![area(1, "Terrace", EntityStatus::Active)];
        let tables = vec![table(10, 1, "T1", EntityStatus::Active)];
        let stations = vec![
            station(1, "Oven", Some(2), true, EntityStatus::Active),
            station(2, "Bar", None, false, EntityStatus::Active),
        ];
        let rules = vec![rule(20, 1, Some(100), None, 0)];
        assert!(pos_core::floor::floor_violations(&compile_floor(&areas, &tables)).is_empty());
        assert!(
            pos_core::floor::station_violations(&compile_stations(&stations, &rules)).is_empty()
        );
    }
}
