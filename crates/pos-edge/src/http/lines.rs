// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Order-line routes: add a line to a table, fire a line to the kitchen (P5).
//!
//! The edge does not invent prices: the device sends the amounts it captured from the menu it holds
//! (`sales.order_line.added` §14.2), and this shell records them. A line's guest note is a boolean
//! (`note_present`) and never its text — the text is PII and stays out of the event log
//! ([`pos_proto::pii`]).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{CourseId, MenuItemId, OrderLineId, StationId, TableId, TaxClassId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;

use crate::app::{Edge, LineDraft, LineView};
use crate::http::{bad_request, dev_actor, error_response, parse_ulid};

/// A line as a device asks for it to be added — the amounts captured from the menu it holds.
#[derive(Debug, Deserialize)]
pub(crate) struct LineRequest {
    menu_item_id: MenuItemId,
    display_name: DisplayName,
    quantity: Quantity,
    unit_price: Money,
    line_total: Money,
    tax_class_id: TaxClassId,
    tax_rate: Ratio,
    #[serde(default)]
    seat: Option<u16>,
    #[serde(default)]
    course_id: Option<CourseId>,
    #[serde(default)]
    note_present: bool,
}

impl From<LineRequest> for LineDraft {
    fn from(request: LineRequest) -> Self {
        Self {
            menu_item_id: request.menu_item_id,
            display_name: request.display_name,
            quantity: request.quantity,
            unit_price: request.unit_price,
            line_total: request.line_total,
            tax_class_id: request.tax_class_id,
            tax_rate: request.tax_rate,
            seat: request.seat,
            course_id: request.course_id,
            note_present: request.note_present,
        }
    }
}

/// Which station a fire goes to. Optional: the edge derives the station from the published routing
/// (ADR-0072), and this is only the fallback for a store with no station plan yet — an absent field
/// lets the plan decide entirely.
#[derive(Debug, Deserialize)]
pub(crate) struct FireRequest {
    #[serde(default)]
    station_id: Option<StationId>,
}

/// A line as returned to a device after a command.
#[derive(Debug, Serialize)]
pub(crate) struct LineResponse {
    order_id: String,
    order_line_id: String,
    /// The line's state (`ORDER_LINE_STATE_ADDED`, `ORDER_LINE_STATE_FIRED`, …).
    state: String,
}

impl From<LineView> for LineResponse {
    fn from(view: LineView) -> Self {
        Self {
            order_id: view.order_id.to_string(),
            order_line_id: view.order_line_id.to_string(),
            state: view.state.as_wire().to_owned(),
        }
    }
}

/// `POST /api/tables/{id}/lines` — add a line to the order the table holds.
pub(crate) async fn add<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_ulid(&id).map(TableId::new) else {
        return bad_request("a table id is a ULID");
    };
    respond(edge.add_line(dev_actor(), table_id, request.into()).await)
}

/// `POST /api/lines/{id}/fire` — fire a line to its station.
pub(crate) async fn fire<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(id): Path<String>,
    Json(request): Json<FireRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_line_id) = parse_ulid(&id).map(OrderLineId::new) else {
        return bad_request("an order line id is a ULID");
    };
    respond(
        edge.fire_line(dev_actor(), order_line_id, request.station_id)
            .await,
    )
}

/// Maps a line command outcome to a response.
fn respond(outcome: Result<LineView, crate::app::AppError>) -> Response {
    match outcome {
        Ok(view) => Json(LineResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
