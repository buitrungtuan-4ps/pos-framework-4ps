// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Order-line routes: add a line to a table, fire a line to the kitchen (P5).
//!
//! A fire prints the station's ticket after the commit, over the
//! [`Printers`](crate::printing::Printers) dispatcher the composition layers in
//! ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)). The kitchen display sees the
//! order either way — the paper is the part that can be missing, and the response says so rather than
//! letting the till imply a ticket came out.
//!
//! The edge does not invent prices: the device sends the amounts it captured from the menu it holds
//! (`sales.order_line.added` §14.2), and this shell records them. A line's guest note is a boolean
//! (`note_present`) and never its text — the text is PII and stays out of the event log
//! ([`pos_proto::pii`]).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{CourseId, MenuItemId, OrderLineId, StationId, TableId, TaxClassId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;

use pos_proto::ids::EventId;

use crate::app::{Edge, LineDraft, LineView};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{PrintOutcome, Printers, ticket_line};

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
    /// The modifiers chosen for this line. Optional, so a device that sends none — or an older one
    /// that does not know the field — adds a bare line exactly as before. What it changes is the
    /// kitchen: a fired line consumes the base recipe **plus** one recipe per modifier (§8), so a
    /// line that omits them is a line whose extras are never taken off the shelf.
    #[serde(default)]
    modifier_menu_item_ids: Vec<MenuItemId>,
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
            modifier_menu_item_ids: request.modifier_menu_item_ids,
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
    /// The station the published routing sent this fire to, absent on an add (ADR-0072).
    #[serde(skip_serializing_if = "Option::is_none")]
    station_id: Option<String>,
    /// What came of the kitchen ticket: `PRINTED`, `NO_PRINTER`, `PRINTER_UNAVAILABLE` or
    /// `UNPRINTABLE_TEXT`. Absent on an add, which prints nothing (ADR-0100).
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket_print: Option<String>,
}

impl From<LineView> for LineResponse {
    fn from(view: LineView) -> Self {
        Self {
            order_id: view.order_id.to_string(),
            order_line_id: view.order_line_id.to_string(),
            state: view.state.as_wire().to_owned(),
            station_id: view
                .fired
                .as_ref()
                .map(|fired| fired.station_id.to_string()),
            ticket_print: None,
        }
    }
}

/// `POST /api/tables/{id}/lines` — add a line to the order the table holds.
pub(crate) async fn add<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_ulid(&id).map(TableId::new) else {
        return bad_request("a table id is a ULID");
    };
    respond(edge.add_line(actor, table_id, request.into()).await)
}

/// `POST /api/lines/{id}/fire` — fire a line to its station.
pub(crate) async fn fire<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<FireRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_line_id) = parse_ulid(&id).map(OrderLineId::new) else {
        return bad_request("an order line id is a ULID");
    };
    let outcome = edge
        .fire_line(actor, order_line_id, request.station_id)
        .await;
    let Ok(view) = outcome else {
        return respond(outcome);
    };

    // After the commit: a printer that is down must not un-fire a line the kitchen is already
    // making, and a rolled-back fire must never have printed.
    let mut response = LineResponse::from(view.clone());
    if let Some(fired) = view.fired.as_ref() {
        let printed = print_ticket_for(printers.as_deref(), &edge, &view, fired).await;
        response.ticket_print = Some(printed.as_wire().to_owned());
    }
    Json(response).into_response()
}

/// Runs the kitchen-ticket effect and says what came of it.
///
/// A composition with no dispatcher layered in reports `NO_PRINTER` — the truth for it.
async fn print_ticket_for<S>(
    printers: Option<&Arc<Printers>>,
    edge: &Arc<Edge<S>>,
    view: &LineView,
    fired: &crate::app::FiredLine,
) -> PrintOutcome
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(printers) = printers else {
        return PrintOutcome::NoPrinter;
    };
    let session = edge.session();
    printers
        .print_ticket(
            &session,
            edge.store_id(),
            // The line's own id as the idempotency key: a fire retried after an ambiguous failure
            // reuses it, and the kitchen gets one ticket rather than making the dish twice.
            EventId::new(view.order_line_id.as_ulid()),
            fired.station_id,
            &crate::printing::short_reference(&view.order_id.to_string()),
            &ticket_line(&session, fired),
        )
        .await
}

/// Maps a line command outcome to a response.
fn respond(outcome: Result<LineView, crate::app::AppError>) -> Response {
    match outcome {
        Ok(view) => Json(LineResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
