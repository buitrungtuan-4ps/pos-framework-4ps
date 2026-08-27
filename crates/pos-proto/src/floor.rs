// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's floor plan and kitchen station plan ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
//!
//! # Why these types are here
//!
//! Same reason the [`crate::MenuBook`] and [`crate::locale::TaxRateTable`] are: they cross the wire —
//! the cloud publishes them to a store inside the configuration tree
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md)) — so they need the same forward-compatible
//! serialisation as every other configuration shape, and both the cloud (which authors and validates
//! them) and the edge (which reads them) read a published plan through *one* type, so the two cannot
//! disagree on what a floor or a station plan *means*. The *logic* over them — referential validation
//! and the fired-line→station routing — is domain and lives in `pos_core::floor`.
//!
//! Both plans are **lists, not maps** (`FloorPlan` of areas, each of tables; `StationPlan` of stations
//! and rules), for the reason every configuration shape is: a list survives JSON round-tripping
//! through the tree and a person can read it in a diff ([ADR-0010](../../../docs/adr/0010-naming-standard.md)).

use serde::{Deserialize, Serialize};

use crate::display::GridPosition;
use crate::ids::{AreaId, CourseId, MenuItemId, StationId, TableId};
use crate::text::DisplayName;

/// One table on a floor plan: its identity, the label a host reads, how many it seats, and where it
/// sits in the visual editor's grid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FloorTable {
    /// The table's identity — the id a `sales.order.opened` event carries and a QR token binds.
    pub table_id: TableId,
    /// The label a host and guest read: "T1", "Bar 3". Presentation text, carried here so the UI
    /// never re-reads anything to draw the floor.
    pub label: DisplayName,
    /// How many covers the table seats. Zero means unspecified — a table still seats guests, the
    /// capacity is just not recorded.
    #[serde(default)]
    pub seats: u16,
    /// Where the table sits in the floor editor's grid. `None` for a table not yet placed — it exists
    /// and can be seated, it just has no pinned position. Omitted from the wire when absent, the same
    /// optional-field shape [`crate::display::DisplayButton`] uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<GridPosition>,
}

/// A named region of the floor — a terrace, the main hall — with the tables it contains, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FloorArea {
    /// The area's identity.
    pub area_id: AreaId,
    /// The name to show: "Terrace", "Main hall".
    pub name: DisplayName,
    /// The tables in this area, in display order.
    #[serde(default)]
    pub tables: Vec<FloorTable>,
}

/// The store's floor plan: its areas and the tables in each.
///
/// An empty plan is a store whose floor has not been published yet — the edge keeps whatever it holds
/// rather than blanking the room (the never-blank config contract, ADR-0072).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FloorPlan {
    #[serde(default)]
    areas: Vec<FloorArea>,
}

impl FloorPlan {
    /// An empty floor plan.
    #[must_use]
    pub const fn new() -> Self {
        Self { areas: Vec::new() }
    }

    /// A plan from its areas.
    #[must_use]
    pub const fn from_areas(areas: Vec<FloorArea>) -> Self {
        Self { areas }
    }

    /// Adds an area, for building a plan in code or a test.
    #[must_use]
    pub fn with(mut self, area: FloorArea) -> Self {
        self.areas.push(area);
        self
    }

    /// Every area, in order.
    #[must_use]
    pub fn areas(&self) -> &[FloorArea] {
        &self.areas
    }

    /// Whether the plan names no areas at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }

    /// Every table across every area, in area-then-table order.
    pub fn tables(&self) -> impl Iterator<Item = &FloorTable> {
        self.areas.iter().flat_map(|area| area.tables.iter())
    }

    /// The table with this id, if the plan names it.
    #[must_use]
    pub fn table(&self, table_id: TableId) -> Option<&FloorTable> {
        self.tables().find(|table| table.table_id == table_id)
    }
}

/// One kitchen station a fired line can route to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KitchenStation {
    /// The station's identity — the `station_id` a `sales.order_line.fired` event carries and a
    /// kitchen `PrintJob` is addressed by (`pos-ports`, ADR-0026).
    pub station_id: StationId,
    /// The name to show: "Oven", "Bar", "Cold line".
    pub name: DisplayName,
    /// The station a ticket falls to when this station's printer is down — the backup-printer failover
    /// target a dispatcher consults. `None` means no failover: a ticket for this station stays here.
    /// A backup must name a *different* station in the same plan (enforced by `pos_core::floor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_station_id: Option<StationId>,
}

/// One item→station routing rule: a fired line matching it routes to `station_id`.
///
/// A rule matches by a specific item or by a course; an item match takes precedence over a course
/// match (`pos_core::floor::route_station`). A rule that names neither is meaningless — the validator
/// rejects it rather than let it sit as a silent no-op.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RoutingRule {
    /// The station a matching line routes to.
    pub station_id: StationId,
    /// Match a specific item. Takes precedence over a course match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_item_id: Option<MenuItemId>,
    /// Match any line on a course.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course_id: Option<CourseId>,
}

/// The store's kitchen plan: its stations, the routing rules that place a fired line on one, and the
/// default station a line with no matching rule falls to.
///
/// An empty plan is a store whose kitchen has not been published — the edge keeps whatever it holds.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StationPlan {
    #[serde(default)]
    stations: Vec<KitchenStation>,
    #[serde(default)]
    routing: Vec<RoutingRule>,
    /// The station a fired line falls to when no routing rule matches. `None` means the store has no
    /// catch-all — a line with no matching rule routes nowhere and the caller keeps its own fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_station_id: Option<StationId>,
}

impl StationPlan {
    /// An empty station plan.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stations: Vec::new(),
            routing: Vec::new(),
            default_station_id: None,
        }
    }

    /// A plan from its parts.
    #[must_use]
    pub const fn from_parts(
        stations: Vec<KitchenStation>,
        routing: Vec<RoutingRule>,
        default_station_id: Option<StationId>,
    ) -> Self {
        Self {
            stations,
            routing,
            default_station_id,
        }
    }

    /// Adds a station, for building a plan in code or a test.
    #[must_use]
    pub fn with_station(mut self, station: KitchenStation) -> Self {
        self.stations.push(station);
        self
    }

    /// Adds a routing rule.
    #[must_use]
    pub fn with_rule(mut self, rule: RoutingRule) -> Self {
        self.routing.push(rule);
        self
    }

    /// Sets the default (catch-all) station.
    #[must_use]
    pub const fn with_default(mut self, station_id: StationId) -> Self {
        self.default_station_id = Some(station_id);
        self
    }

    /// Every station, in order.
    #[must_use]
    pub fn stations(&self) -> &[KitchenStation] {
        &self.stations
    }

    /// Every routing rule, in order.
    #[must_use]
    pub fn routing(&self) -> &[RoutingRule] {
        &self.routing
    }

    /// The default (catch-all) station, if the plan names one.
    #[must_use]
    pub const fn default_station_id(&self) -> Option<StationId> {
        self.default_station_id
    }

    /// Whether the plan names no stations at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }

    /// The station with this id, if the plan names it.
    #[must_use]
    pub fn station(&self, station_id: StationId) -> Option<&KitchenStation> {
        self.stations
            .iter()
            .find(|station| station.station_id == station_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{FloorArea, FloorPlan, FloorTable, KitchenStation, RoutingRule, StationPlan};
    use crate::display::GridPosition;
    use crate::ids::{AreaId, CourseId, MenuItemId, StationId, TableId};
    use crate::text::DisplayName;
    use crate::ulid::Ulid;

    fn area_id(n: u128) -> AreaId {
        AreaId::new(Ulid::from_u128(n))
    }
    fn table_id(n: u128) -> TableId {
        TableId::new(Ulid::from_u128(n))
    }
    fn station_id(n: u128) -> StationId {
        StationId::new(Ulid::from_u128(n))
    }
    fn item_id(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }
    fn course_id(n: u128) -> CourseId {
        CourseId::new(Ulid::from_u128(n))
    }

    fn sample_floor() -> FloorPlan {
        FloorPlan::new().with(FloorArea {
            area_id: area_id(1),
            name: DisplayName::new("Terrace"),
            tables: vec![
                FloorTable {
                    table_id: table_id(10),
                    label: DisplayName::new("T1"),
                    seats: 4,
                    position: Some(GridPosition { column: 0, row: 0 }),
                },
                FloorTable {
                    table_id: table_id(11),
                    label: DisplayName::new("T2"),
                    seats: 2,
                    position: None,
                },
            ],
        })
    }

    #[test]
    fn floor_plan_round_trips_through_json() {
        let plan = sample_floor();
        let json = serde_json::to_string(&plan).expect("serialises");
        let back: FloorPlan = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(plan, back);
    }

    #[test]
    fn floor_plan_iterates_and_looks_up_tables() {
        let plan = sample_floor();
        assert_eq!(plan.tables().count(), 2);
        assert_eq!(plan.table(table_id(11)).map(|table| table.seats), Some(2));
        assert!(plan.table(table_id(99)).is_none());
    }

    #[test]
    fn an_unplaced_table_omits_its_position_from_the_wire() {
        let plan = sample_floor();
        let json = serde_json::to_string(&plan).expect("serialises");
        // T1 has a position; T2 does not — exactly one `position` key survives.
        assert_eq!(json.matches("\"position\"").count(), 1);
    }

    fn sample_stations() -> StationPlan {
        StationPlan::new()
            .with_station(KitchenStation {
                station_id: station_id(1),
                name: DisplayName::new("Oven"),
                backup_station_id: Some(station_id(2)),
            })
            .with_station(KitchenStation {
                station_id: station_id(2),
                name: DisplayName::new("Bar"),
                backup_station_id: None,
            })
            .with_rule(RoutingRule {
                station_id: station_id(1),
                menu_item_id: Some(item_id(100)),
                course_id: None,
            })
            .with_rule(RoutingRule {
                station_id: station_id(2),
                menu_item_id: None,
                course_id: Some(course_id(200)),
            })
            .with_default(station_id(2))
    }

    #[test]
    fn station_plan_round_trips_through_json() {
        let plan = sample_stations();
        let json = serde_json::to_string(&plan).expect("serialises");
        let back: StationPlan = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(plan, back);
    }

    #[test]
    fn station_plan_exposes_stations_rules_and_default() {
        let plan = sample_stations();
        assert_eq!(plan.stations().len(), 2);
        assert_eq!(plan.routing().len(), 2);
        assert_eq!(plan.default_station_id(), Some(station_id(2)));
        assert_eq!(
            plan.station(station_id(1))
                .and_then(|s| s.backup_station_id),
            Some(station_id(2))
        );
    }

    #[test]
    fn empty_plans_are_empty() {
        assert!(FloorPlan::new().is_empty());
        assert!(StationPlan::new().is_empty());
    }
}
