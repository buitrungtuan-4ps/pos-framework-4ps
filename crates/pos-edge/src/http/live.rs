// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What is open right now: `GET /api/orders/live`.
//!
//! Every other read a device makes at boot describes what the store *sells* — the floor plan, the
//! price book, the button plan, the money settings. None of them describes what the store is in the
//! middle of. A device learns that from the fan-out, and the fan-out carries what happens next, so
//! a till that reloaded, a tablet that woke from sleep and a kitchen display switched on mid-service
//! all drew an empty order and an empty board while the food existed.
//!
//! This is the read that answers them, and one read rather than three: it is keyed on the order,
//! carries the table when there is one, and carries every line with the id a fire, a void or a bump
//! is addressed to. The floor's orders and the counter's both appear — the kitchen works from both,
//! and a counter order sits on no table ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)).
//!
//! It is distinct from `GET /api/orders/open`, which is the cashier's list: that one is counter
//! orders only, carries the queue number staff shouted and the money owed, and deliberately leaves
//! the floor out so nobody charges one meal from two screens. This one is a device's memory.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;
use pos_proto::time::Timestamp;

use crate::app::Edge;

/// One line of an open order, with everything a screen needs to draw it and act on it.
#[derive(Debug, Serialize)]
struct LiveLineResponse {
    /// The id a fire, a void or a bump is addressed to.
    order_line_id: String,
    display_name: String,
    quantity: Quantity,
    /// The extended total captured when the line was added — the figure the guest was quoted.
    line_total: Money,
    /// `ORDER_LINE_STATE_ADDED`, `_FIRED`, `_VOIDED`.
    state: String,
    /// Whether a station has marked it prepared. Orthogonal to `state`: a fired line is still fired
    /// once it is made, and this is what stops a kitchen display re-showing a ticket it has bumped.
    bumped: bool,
    /// When the line went to the kitchen. Absent while it is still on the pad, and omitted from the
    /// wire when absent, the way every other optional field on this route is.
    ///
    /// The one number a kitchen board is for: which of eight tickets has been waiting longest. Taken
    /// from the firing event's envelope rather than from any clock a screen owns, so a board that
    /// reloads mid-service still counts from when the food was actually ordered — and two boards
    /// side by side agree.
    #[serde(skip_serializing_if = "Option::is_none")]
    fired_time: Option<Timestamp>,
    /// The modifiers chosen for the line (ADR-0127), as ids — the caller names them from the price
    /// book it already holds. Always present, empty for a line that carries none, so a reader never
    /// has to tell "no modifiers" apart from "an older edge that did not say".
    modifier_menu_item_ids: Vec<String>,
    /// The seat the line was ordered for, absent for the table's. Omitted from the wire when absent,
    /// the way every other optional field on this route is: a `seat` of zero is not "no seat", and a
    /// null would be a second spelling of the same nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    seat: Option<u16>,
    /// The course the line goes out on, absent for a line on none (ADR-0130). Omitted when absent,
    /// for the reason `seat` is.
    ///
    /// Without it a till that reloaded mid-service drew every line uncoursed, so the course controls
    /// offered to send starters the screen no longer knew were starters — the same loss `seat` was
    /// added here to stop, one field over.
    #[serde(skip_serializing_if = "Option::is_none")]
    course_id: Option<String>,
    /// The station the line was fired to, absent while it is still on the pad. Omitted when absent,
    /// for the reason `seat` is. What a kitchen board filters on, so a bar screen that reloads shows
    /// the bar's tickets and not the grill's.
    #[serde(skip_serializing_if = "Option::is_none")]
    station_id: Option<String>,
}

/// One open order.
#[derive(Debug, Serialize)]
struct LiveOrderResponse {
    order_id: String,
    /// The table it sits on. Absent for a counter order, which has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    table_id: Option<String>,
    /// A bill already open on it, so a device that reloads on the payment screen settles **that**
    /// bill rather than asking for a second one the domain would refuse.
    #[serde(skip_serializing_if = "Option::is_none")]
    bill_id: Option<String>,
    lines: Vec<LiveLineResponse>,
}

/// `GET /api/orders/live` — every open order with its lines.
///
/// A pure read of the projection: no decision, no write, and nothing that can fail. An order the
/// store has settled is gone from it, which is what keeps the answer the size of the store's live
/// trading rather than of its history.
pub(crate) async fn read<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let body: Vec<LiveOrderResponse> = edge
        .live_orders()
        .into_iter()
        .map(|order| LiveOrderResponse {
            order_id: order.order_id.to_string(),
            table_id: order.table_id.map(|id| id.to_string()),
            bill_id: order.bill_id.map(|id| id.to_string()),
            lines: order
                .lines
                .into_iter()
                .map(|line| LiveLineResponse {
                    order_line_id: line.order_line_id.to_string(),
                    display_name: line.display_name.as_str().to_owned(),
                    quantity: line.quantity,
                    line_total: line.line_total,
                    state: line.state.as_wire().to_owned(),
                    bumped: line.bumped,
                    fired_time: line.fired_time,
                    modifier_menu_item_ids: line
                        .modifier_menu_item_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    seat: line.seat,
                    course_id: line.course_id.map(|id| id.to_string()),
                    station_id: line.station_id.map(|id| id.to_string()),
                })
                .collect(),
        })
        .collect();
    (StatusCode::OK, Json(body)).into_response()
}
