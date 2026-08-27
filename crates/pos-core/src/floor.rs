// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Domain logic over the floor and kitchen plans ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
//!
//! The *shapes* — [`FloorPlan`], [`StationPlan`] — are data and live in `pos-proto`. The *logic* over
//! them is domain and lives here, the same split the menu keeps (`pos_proto::MenuBook` vs
//! `pos_core::menu`). Two concerns:
//!
//! - **Referential validation** ([`floor_violations`], [`station_violations`]): the checks the cloud
//!   runs before publishing a `floor`/`stations` config version, exactly as [`crate::capability::conflicts`]
//!   is run for the capability flags. Pure over the plan, returns *all* problems (the console shows an
//!   admin every one at once), each a human-readable line the cloud surfaces as a config violation.
//! - **Routing** ([`route_station`]): the pure function that turns a fired line into the station it
//!   belongs to, so the edge derives the station from the published plan instead of trusting the
//!   caller. An item rule beats a course rule beats the default.

use std::collections::BTreeSet;

use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::ids::{CourseId, MenuItemId, StationId};

/// The station a fired line routes to under `plan`, or `None` when nothing matches and the plan names
/// no default.
///
/// Precedence, first match wins within each tier: a rule naming this **item**, then a rule naming this
/// line's **course** (when it has one), then the plan's **default** station. The rules are scanned in
/// authoring order, so an operator can order specific rules before general ones.
#[must_use]
pub fn route_station(
    plan: &StationPlan,
    menu_item_id: MenuItemId,
    course_id: Option<CourseId>,
) -> Option<StationId> {
    if let Some(rule) = plan
        .routing()
        .iter()
        .find(|rule| rule.menu_item_id == Some(menu_item_id))
    {
        return Some(rule.station_id);
    }
    if let Some(course_id) = course_id
        && let Some(rule) = plan
            .routing()
            .iter()
            .find(|rule| rule.course_id == Some(course_id))
    {
        return Some(rule.station_id);
    }
    plan.default_station_id()
}

/// The referential problems in a floor plan, in reading order. Empty means the plan is valid.
///
/// A table id must be unique across the whole floor — events key a table by its id, so the same id in
/// two areas would be one table pretending to be two. Area ids must likewise be distinct.
#[must_use]
pub fn floor_violations(plan: &FloorPlan) -> Vec<String> {
    let mut violations = Vec::new();
    let mut seen_areas = BTreeSet::new();
    let mut seen_tables = BTreeSet::new();
    for area in plan.areas() {
        if !seen_areas.insert(area.area_id) {
            violations.push(format!("area {} appears more than once", area.area_id));
        }
        for table in &area.tables {
            if !seen_tables.insert(table.table_id) {
                violations.push(format!(
                    "table {} appears more than once across the floor",
                    table.table_id
                ));
            }
        }
    }
    violations
}

/// The referential problems in a station plan, in reading order. Empty means the plan is valid.
///
/// Every station id is distinct; a routing rule names a known station and matches exactly one of an
/// item or a course (a rule that matches neither is a silent no-op, one that matches both is
/// ambiguous); a backup names a known, *different* station; the default station is a known one.
#[must_use]
pub fn station_violations(plan: &StationPlan) -> Vec<String> {
    let mut violations = Vec::new();
    let mut known: BTreeSet<StationId> = BTreeSet::new();
    for station in plan.stations() {
        if !known.insert(station.station_id) {
            violations.push(format!(
                "station {} appears more than once",
                station.station_id
            ));
        }
    }
    for station in plan.stations() {
        if let Some(backup) = station.backup_station_id {
            if backup == station.station_id {
                violations.push(format!(
                    "station {} names itself as its backup",
                    station.station_id
                ));
            } else if !known.contains(&backup) {
                violations.push(format!(
                    "station {} names an unknown backup station {backup}",
                    station.station_id
                ));
            }
        }
    }
    for (index, rule) in plan.routing().iter().enumerate() {
        if !known.contains(&rule.station_id) {
            violations.push(format!(
                "routing rule {index} names an unknown station {}",
                rule.station_id
            ));
        }
        match (rule.menu_item_id.is_some(), rule.course_id.is_some()) {
            (false, false) => {
                violations.push(format!(
                    "routing rule {index} matches neither an item nor a course"
                ));
            }
            (true, true) => {
                violations.push(format!(
                    "routing rule {index} matches both an item and a course"
                ));
            }
            _ => {}
        }
    }
    if let Some(default) = plan.default_station_id()
        && !known.contains(&default)
    {
        violations.push(format!(
            "the default station {default} is not a known station"
        ));
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::{floor_violations, route_station, station_violations};
    use pos_proto::floor::{
        FloorArea, FloorPlan, FloorTable, KitchenStation, RoutingRule, StationPlan,
    };
    use pos_proto::ids::{AreaId, CourseId, MenuItemId, StationId, TableId};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    fn area(n: u128) -> AreaId {
        AreaId::new(Ulid::from_u128(n))
    }
    fn table(n: u128) -> TableId {
        TableId::new(Ulid::from_u128(n))
    }
    fn station(n: u128) -> StationId {
        StationId::new(Ulid::from_u128(n))
    }
    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }
    fn course(n: u128) -> CourseId {
        CourseId::new(Ulid::from_u128(n))
    }

    fn one_table(area_id: AreaId, table_id: TableId) -> FloorArea {
        FloorArea {
            area_id,
            name: DisplayName::new("Area"),
            tables: vec![FloorTable {
                table_id,
                label: DisplayName::new("T"),
                seats: 2,
                position: None,
            }],
        }
    }

    #[test]
    fn a_clean_floor_has_no_violations() {
        let plan = FloorPlan::new()
            .with(one_table(area(1), table(10)))
            .with(one_table(area(2), table(11)));
        assert!(floor_violations(&plan).is_empty());
    }

    #[test]
    fn a_table_repeated_across_areas_is_flagged() {
        let plan = FloorPlan::new()
            .with(one_table(area(1), table(10)))
            .with(one_table(area(2), table(10)));
        assert_eq!(floor_violations(&plan).len(), 1);
    }

    #[test]
    fn a_repeated_area_is_flagged() {
        let plan = FloorPlan::new()
            .with(one_table(area(1), table(10)))
            .with(one_table(area(1), table(11)));
        assert_eq!(floor_violations(&plan).len(), 1);
    }

    fn oven_and_bar() -> StationPlan {
        StationPlan::new()
            .with_station(KitchenStation {
                station_id: station(1),
                name: DisplayName::new("Oven"),
                backup_station_id: Some(station(2)),
            })
            .with_station(KitchenStation {
                station_id: station(2),
                name: DisplayName::new("Bar"),
                backup_station_id: None,
            })
    }

    #[test]
    fn a_clean_station_plan_has_no_violations() {
        let plan = oven_and_bar()
            .with_rule(RoutingRule {
                station_id: station(1),
                menu_item_id: Some(item(100)),
                course_id: None,
            })
            .with_default(station(2));
        assert!(
            station_violations(&plan).is_empty(),
            "{:?}",
            station_violations(&plan)
        );
    }

    #[test]
    fn an_unknown_backup_is_flagged() {
        let plan = StationPlan::new().with_station(KitchenStation {
            station_id: station(1),
            name: DisplayName::new("Oven"),
            backup_station_id: Some(station(9)),
        });
        assert_eq!(station_violations(&plan).len(), 1);
    }

    #[test]
    fn a_self_backup_is_flagged() {
        let plan = StationPlan::new().with_station(KitchenStation {
            station_id: station(1),
            name: DisplayName::new("Oven"),
            backup_station_id: Some(station(1)),
        });
        assert_eq!(station_violations(&plan).len(), 1);
    }

    #[test]
    fn a_rule_to_an_unknown_station_and_a_rule_matching_nothing_are_both_flagged() {
        let plan = oven_and_bar()
            .with_rule(RoutingRule {
                station_id: station(9),
                menu_item_id: Some(item(100)),
                course_id: None,
            })
            .with_rule(RoutingRule {
                station_id: station(1),
                menu_item_id: None,
                course_id: None,
            });
        assert_eq!(station_violations(&plan).len(), 2);
    }

    #[test]
    fn a_rule_matching_both_an_item_and_a_course_is_flagged() {
        let plan = oven_and_bar().with_rule(RoutingRule {
            station_id: station(1),
            menu_item_id: Some(item(100)),
            course_id: Some(course(200)),
        });
        assert_eq!(station_violations(&plan).len(), 1);
    }

    #[test]
    fn an_unknown_default_is_flagged() {
        let plan = oven_and_bar().with_default(station(9));
        assert_eq!(station_violations(&plan).len(), 1);
    }

    #[test]
    fn routing_prefers_item_then_course_then_default() {
        let plan = oven_and_bar()
            .with_rule(RoutingRule {
                station_id: station(1),
                menu_item_id: Some(item(100)),
                course_id: None,
            })
            .with_rule(RoutingRule {
                station_id: station(2),
                menu_item_id: None,
                course_id: Some(course(200)),
            })
            .with_default(station(1));

        // Item rule wins even when the line also carries a matching course.
        assert_eq!(
            route_station(&plan, item(100), Some(course(200))),
            Some(station(1))
        );
        // No item rule → the course rule.
        assert_eq!(
            route_station(&plan, item(999), Some(course(200))),
            Some(station(2))
        );
        // Neither → the default.
        assert_eq!(route_station(&plan, item(999), None), Some(station(1)));
    }

    #[test]
    fn routing_with_no_default_and_no_match_is_none() {
        let plan = oven_and_bar();
        assert_eq!(route_station(&plan, item(999), None), None);
    }
}
