// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Table floor routes: seat, clean, read (P5).
//!
//! Each is a thin shell over [`Edge`](crate::app::Edge): parse the table id, call the application
//! loop, map the outcome to a status. The loop is what actually loads state, decides, writes and
//! publishes ([`crate::app`]); nothing domain-shaped happens here.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::TableId;

use crate::app::{AppError, Edge, TableView};
use crate::http::{bad_request, error_response, parse_ulid};

/// A table as returned to a device.
#[derive(Debug, Serialize)]
pub(crate) struct TableResponse {
    /// The table id, as a ULID string.
    table_id: String,
    /// The table's state (`TABLE_STATE_FREE`, `TABLE_STATE_OCCUPIED`, …).
    state: String,
}

impl From<TableView> for TableResponse {
    fn from(view: TableView) -> Self {
        Self {
            table_id: view.table_id.to_string(),
            state: view.state.as_wire().to_owned(),
        }
    }
}

/// How many guests were seated, if whoever seated them said.
///
/// Reporting data (average check per cover), never a gate: a seat with no body at all is the
/// request every device sent before B1.2 and still means "seat the table, count unknown".
#[derive(Debug, Deserialize)]
pub(crate) struct SeatRequest {
    #[serde(default)]
    guest_count: Option<u16>,
}

/// `POST /api/tables/{id}/seat` — seat guests and open an order.
///
/// The body is optional, and read as raw [`Bytes`] rather than through `Json` so that it is
/// optional in the way devices actually behave: `Option<Json<_>>` treats a body as absent only when
/// the `content-type` header is missing, and plenty of clients send `application/json` with nothing
/// after it. Both spellings of "no count" are accepted; a body that is present but malformed is
/// refused rather than silently ignored, because a device that meant to send a count and got the
/// shape wrong should be told.
///
/// No screen prompts for the count yet — whether tap-to-seat should grow a step is roadmap **Q7**'s
/// call, not this slice's; the route carries it so that decision does not also need a wire change.
pub(crate) async fn seat<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_table(&id) else {
        return bad_request("a table id is a ULID");
    };
    let guest_count = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        match serde_json::from_slice::<SeatRequest>(&body) {
            Ok(request) => request.guest_count,
            Err(_ignored) => {
                return bad_request("a seat body is {\"guest_count\": <number>}, or empty");
            }
        }
    };
    respond(edge.seat_table(actor, table_id, guest_count).await)
}

/// `POST /api/tables/{id}/clean` — clean the table down and release it.
pub(crate) async fn clean<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_table(&id) else {
        return bad_request("a table id is a ULID");
    };
    respond(edge.clean_table(actor, table_id).await)
}

/// `GET /api/tables/{id}` — the table's current projected state.
pub(crate) async fn get<S>(State(edge): State<Arc<Edge<S>>>, Path(id): Path<String>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_table(&id) else {
        return bad_request("a table id is a ULID");
    };
    Json(TableResponse {
        table_id: table_id.to_string(),
        state: edge.table_state(table_id).as_wire().to_owned(),
    })
    .into_response()
}

/// Parses a table id from the path. `None` if it is not a ULID.
fn parse_table(id: &str) -> Option<TableId> {
    parse_ulid(id).map(TableId::new)
}

/// Maps a command outcome to a response: the table on success, a status that names the failure kind.
fn respond(outcome: Result<TableView, AppError>) -> Response {
    match outcome {
        Ok(view) => Json(TableResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
