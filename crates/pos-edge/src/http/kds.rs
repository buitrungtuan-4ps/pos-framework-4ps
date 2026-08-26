// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Kitchen-display routes: bump a ticket (mark lines prepared) — P6 residual / #44.
//!
//! A bump was UI-local: each KDS held its own "done" set, so a second screen never agreed. This route
//! records the durable `kitchen.ticket.bumped` event and fans it out, so every KDS folds the same
//! truth. A bump is orthogonal to a line's order state, so it is not a state-machine command; the edge
//! writes the event and marks its projection.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_ports::event_store::EventStore;
use pos_proto::ids::{OrderId, OrderLineId, StationId};

use crate::app::{BumpView, Edge};
use crate::http::{bad_request, dev_actor, error_response, parse_ulid};

/// A bump as a KDS asks for it: the order, the station, and the lines it made.
#[derive(Debug, Deserialize)]
pub(crate) struct BumpRequest {
    order_id: String,
    station_id: String,
    order_line_ids: Vec<String>,
}

/// What a bump returns: the order, station, and the lines now marked prepared.
#[derive(Debug, Serialize)]
pub(crate) struct BumpResponse {
    order_id: String,
    station_id: String,
    order_line_ids: Vec<String>,
}

impl From<BumpView> for BumpResponse {
    fn from(view: BumpView) -> Self {
        Self {
            order_id: view.order_id.to_string(),
            station_id: view.station_id.to_string(),
            order_line_ids: view
                .order_line_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

/// `POST /api/kds/bump` — a station marks a ticket's lines prepared.
pub(crate) async fn bump<S>(
    State(edge): State<Arc<Edge<S>>>,
    Json(request): Json<BumpRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_id) = parse_ulid(&request.order_id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    let Some(station_id) = parse_ulid(&request.station_id).map(StationId::new) else {
        return bad_request("a station id is a ULID");
    };
    let mut order_line_ids = Vec::with_capacity(request.order_line_ids.len());
    for id in &request.order_line_ids {
        let Some(line_id) = parse_ulid(id).map(OrderLineId::new) else {
            return bad_request("an order line id is a ULID");
        };
        order_line_ids.push(line_id);
    }
    match edge
        .bump_ticket(dev_actor(), order_id, station_id, order_line_ids)
        .await
    {
        Ok(view) => Json(BumpResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
