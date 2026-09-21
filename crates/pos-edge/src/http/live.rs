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
    /// The modifiers chosen for the line (ADR-0127), as ids — the caller names them from the price
    /// book it already holds. Always present, empty for a line that carries none, so a reader never
    /// has to tell "no modifiers" apart from "an older edge that did not say".
    modifier_menu_item_ids: Vec<String>,
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
                    modifier_menu_item_ids: line
                        .modifier_menu_item_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                })
                .collect(),
        })
        .collect();
    (StatusCode::OK, Json(body)).into_response()
}
