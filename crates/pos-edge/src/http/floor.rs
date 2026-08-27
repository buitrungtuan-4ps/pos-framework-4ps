// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The floor & kitchen read route (Track M2, [ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
//!
//! `GET /api/floor` serves the store's published floor plan and kitchen stations from the live
//! [`EdgeSession`](crate::app::EdgeSession) the config-pull rebuilds — so the in-store UI renders the
//! store's *real* areas and tables (not a hardcoded eight) and routes fires by the published rules.
//! Empty until the console publishes a floor; the UI keeps its own fallback while it is.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::floor::{FloorPlan, StationPlan};

use crate::app::Edge;

/// The store's floor and kitchen plans, as the in-store UI reads them.
#[derive(Debug, Serialize)]
pub(crate) struct FloorResponse {
    /// The areas and their tables.
    floor: FloorPlan,
    /// The kitchen stations and item→station routing.
    stations: StationPlan,
}

/// `GET /api/floor` — the store's published floor plan and kitchen stations, read from the live
/// session (ADR-0072).
pub(crate) async fn plan<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    (
        StatusCode::OK,
        Json(FloorResponse {
            floor: session.floor.clone(),
            stations: session.stations.clone(),
        }),
    )
        .into_response()
}
