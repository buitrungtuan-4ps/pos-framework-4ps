// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The floor & kitchen read route (Track M2, [ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
//!
//! `GET /api/floor` serves the store's published floor plan and kitchen stations from the live
//! [`EdgeSession`](crate::app::EdgeSession) the config-pull rebuilds — so the in-store UI renders the
//! store's *real* areas and tables (not a hardcoded eight) and routes fires by the published rules.
//! Empty until the console publishes a floor; the UI keeps its own fallback while it is.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::floor::{FloorPlan, KitchenStation, StationPlan};
use pos_proto::time::Timestamp;

use crate::app::Edge;

/// The store's floor and kitchen plans, as the in-store UI reads them.
#[derive(Debug, Serialize)]
pub(crate) struct FloorResponse {
    /// The areas and their tables.
    floor: FloorPlan,
    /// The kitchen stations and item→station routing, each station with the threshold its board
    /// marks a ticket late after ([`resolved_thresholds`]).
    stations: StationPlan,
    /// What each table is doing right now, keyed by table id: free, occupied, awaiting payment,
    /// needs cleaning.
    ///
    /// Beside the plan rather than inside it, because the two are different kinds of fact. The plan
    /// is *configuration* — what the console published, the same for every device, changing when
    /// somebody edits the floor. A table's state is *live*, changes every time a guest sits down,
    /// and belongs to this store's projection. Folding it into `FloorPlan` would have put a running
    /// value into `pos-proto`'s published shape and made a device's reload rewrite config.
    ///
    /// Sent because a device that has just started has **no other way to learn it**. The states
    /// reach a running till on the fan-out, and a till that was not running when the guests sat down
    /// never saw those events — so before this, a reload drew every occupied table as free, on the
    /// home screen, and a server could seat a table that already had people at it.
    table_states: BTreeMap<String, String>,
    /// When the guests at each seated table sat down, keyed by table id, for the tables somebody is
    /// sitting at — how a floor plan says "seated 25 min", which a host reads to know who is due a
    /// check and who is about to leave.
    ///
    /// Here rather than on the live orders, because it is a fact about the table and a table is
    /// seated before anything is ordered: the live read lists orders with lines, so a till that
    /// reloaded over a table sat down a minute ago would have had no time for it. Read from the
    /// table's order id ([`Edge::seated_since`]), so every device shows the same figure however long
    /// it has been running.
    seated_times: BTreeMap<String, Timestamp>,
}

/// `GET /api/floor` — the store's published floor plan and kitchen stations, read from the live
/// session (ADR-0072).
pub(crate) async fn plan<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    // Every table the store published, so a reader never has to tell "free" apart from "this edge
    // did not say". A table the projection has never heard of reads free, which is what it is.
    let table_states = session
        .floor
        .tables()
        .map(|table| {
            (
                table.table_id.to_string(),
                edge.table_state(table.table_id).as_wire().to_owned(),
            )
        })
        .collect();
    let seated_times = session
        .floor
        .tables()
        .filter_map(|table| {
            edge.seated_since(table.table_id)
                .map(|since| (table.table_id.to_string(), since))
        })
        .collect();
    (
        StatusCode::OK,
        Json(FloorResponse {
            floor: session.floor.clone(),
            stations: resolved_thresholds(&session.stations),
            table_states,
            seated_times,
        }),
    )
        .into_response()
}

/// The station plan with every station's `late_after_seconds` as its board reads it
/// ([`KitchenStation::late_after_seconds`], ADR-0160 decision 2): the station's own threshold, or
/// ten minutes where it sets none or one out of bounds.
///
/// Resolved here so the kitchen display applies the bounds and the default exactly as the published
/// type defines them, rather than keeping a copy of the rule that could drift. Only the read is
/// resolved: the session keeps the plan as the cloud published it.
fn resolved_thresholds(plan: &StationPlan) -> StationPlan {
    StationPlan::from_parts(
        plan.stations()
            .iter()
            .map(|station| KitchenStation {
                late_after_seconds: Some(station.late_after_seconds()),
                ..station.clone()
            })
            .collect(),
        plan.routing().to_vec(),
        plan.default_station_id(),
    )
}

#[cfg(test)]
mod tests {
    use super::resolved_thresholds;
    use pos_proto::floor::{KitchenStation, RoutingRule, StationPlan};
    use pos_proto::ids::{MenuItemId, StationId};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    fn station(seed: u128, late_after_seconds: Option<u32>) -> KitchenStation {
        KitchenStation {
            station_id: StationId::new(Ulid::from_u128(seed)),
            name: DisplayName::new("Station"),
            backup_station_id: None,
            late_after_seconds,
            ticket_language: Open::default(),
        }
    }

    #[test]
    fn each_station_is_read_with_the_threshold_its_board_marks_a_ticket_late_after() {
        let rule = RoutingRule {
            station_id: StationId::new(Ulid::from_u128(1)),
            menu_item_id: Some(MenuItemId::new(Ulid::from_u128(9))),
            course_id: None,
        };
        let published = StationPlan::from_parts(
            vec![
                station(1, Some(240)),
                station(2, None),
                station(3, Some(30)),
            ],
            vec![rule.clone()],
            Some(StationId::new(Ulid::from_u128(2))),
        );
        let read = resolved_thresholds(&published);
        let thresholds: Vec<Option<u32>> = read
            .stations()
            .iter()
            .map(|station| station.late_after_seconds)
            .collect();
        assert_eq!(thresholds, [Some(240), Some(600), Some(600)]);
        assert_eq!(read.routing(), [rule]);
        assert_eq!(read.default_station_id(), published.default_station_id());
    }
}
