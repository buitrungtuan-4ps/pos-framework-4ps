// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Table floor routes: seat, clean, move the guests to another table, read (P5).
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

use crate::app::{AppError, Edge, TableView, TransferView};
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

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/tables/{id}/seat",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.table.manage"))),
    params(
        ("id" = String, Path, description = "The table's id, a ULID"),
    ),
    request_body(
        description = "Optional `guest_count`. An empty body seats the table with the count unknown",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Seated, with an order opened: its id and its new `state`, a \
                                      `TABLE_STATE_*` token"),
        (status = 400, description = "The id is not a ULID, or a body is present that is not `guest_count`"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "floor",
))]
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
pub(crate) async fn seat_table<S>(
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

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/tables/{id}/clean",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.table.manage"))),
    params(
        ("id" = String, Path, description = "The table's id, a ULID"),
    ),
    responses(
        (status = 200, description = "The table: its id and its new `state`, a `TABLE_STATE_*` token"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "floor",
))]
/// `POST /api/tables/{id}/clean` — clean the table down and release it.
pub(crate) async fn clean_table<S>(
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

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/tables/{id}/release",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.table.manage"))),
    params(
        ("id" = String, Path, description = "The table's id, a ULID"),
    ),
    responses(
        (status = 200, description = "Released while nothing is sold on it: its id and its new `state`, a \
                                      `TABLE_STATE_*` token"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "floor",
))]
/// `POST /api/tables/{id}/release` — give a table seated by mistake straight back to the floor,
/// while nothing is sold on it
/// ([ADR-0163](../../../../docs/adr/0163-a-table-seated-by-mistake-is-released.md)).
pub(crate) async fn release_table<S>(
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
    respond(edge.release_table(actor, table_id).await)
}

/// Where the guests are moving to.
#[derive(Debug, Deserialize)]
pub(crate) struct TransferRequest {
    /// The free table they move to, as a ULID string.
    to_table_id: String,
}

/// What a move did, as returned to a device: the order that moved, and both tables after it.
#[derive(Debug, Serialize)]
pub(crate) struct TransferResponse {
    /// The order that moved, as a ULID string.
    order_id: String,
    /// The table the guests left, now waiting to be cleared.
    from_table: TableResponse,
    /// The table they moved to, now seated with their order.
    to_table: TableResponse,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/tables/{id}/transfer",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["sales.order.transfer", "sales.table.manage"]))),
    params(
        ("id" = String, Path, description = "The table's id, a ULID"),
    ),
    request_body(
        description = "`to_table_id`, the free table the guests and their order move to",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Moved: the `order_id` that moved, and both tables after it, \
                                      `from_table` and `to_table`"),
        (status = 400, description = "An id is not a ULID, or the body has no `to_table_id`"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "floor",
))]
/// `POST /api/tables/{id}/transfer` — move the guests at this table, and their order, to a free
/// one (`sales.table.transferred`). The body is `{"to_table_id": "<ulid>"}`.
pub(crate) async fn transfer_table<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(from_table_id) = parse_table(&id) else {
        return bad_request("a table id is a ULID");
    };
    let Ok(request) = serde_json::from_slice::<TransferRequest>(&body) else {
        return bad_request("a transfer body is {\"to_table_id\": \"<ulid>\"}");
    };
    let Some(to_table_id) = parse_table(&request.to_table_id) else {
        return bad_request("a table id is a ULID");
    };
    match edge.transfer_table(actor, from_table_id, to_table_id).await {
        Ok(view) => Json(TransferResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}

impl From<TransferView> for TransferResponse {
    fn from(view: TransferView) -> Self {
        Self {
            order_id: view.order_id.to_string(),
            from_table: TableResponse::from(view.from),
            to_table: TableResponse::from(view.to),
        }
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/tables/{id}",
    security(("till_session" = [])),
    params(
        ("id" = String, Path, description = "The table's id, a ULID"),
    ),
    responses(
        (status = 200, description = "The table's id and its projected `state`, a `TABLE_STATE_*` token"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
    ),
    tag = "floor",
))]
/// `GET /api/tables/{id}` — the table's current projected state.
pub(crate) async fn table_state<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(id): Path<String>,
) -> Response
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
