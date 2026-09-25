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
use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::time::Timestamp;

use crate::app::Edge;

/// The store's floor and kitchen plans, as the in-store UI reads them.
#[derive(Debug, Serialize)]
pub(crate) struct FloorResponse {
    /// The areas and their tables.
    floor: FloorPlan,
    /// The kitchen stations and item→station routing.
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
            stations: session.stations.clone(),
            table_states,
            seated_times,
        }),
    )
        .into_response()
}
