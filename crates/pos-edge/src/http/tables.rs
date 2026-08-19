// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Table floor routes: seat, clean, read (P5).
//!
//! Each is a thin shell over [`Edge`](crate::app::Edge): parse the table id, call the application
//! loop, map the outcome to a status. The loop is what actually loads state, decides, writes and
//! publishes ([`crate::app`]); nothing domain-shaped happens here.
//!
//! The acting employee and device come from a fixed development actor for now; resolving them from
//! the paired device token and the signed-in employee is the auth-integration follow-up
//! ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)).

use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{DeviceId, EmployeeId, TableId};
use pos_proto::ulid::Ulid;

use crate::app::{AppError, Edge, TableView};

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

/// `POST /api/tables/{id}/seat` — seat guests and open an order.
pub(crate) async fn seat<S>(State(edge): State<Arc<Edge<S>>>, Path(id): Path<String>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_table(&id) else {
        return bad_request();
    };
    respond(edge.seat_table(dev_actor(), table_id).await)
}

/// `POST /api/tables/{id}/clean` — clean the table down and release it.
pub(crate) async fn clean<S>(State(edge): State<Arc<Edge<S>>>, Path(id): Path<String>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_table(&id) else {
        return bad_request();
    };
    respond(edge.clean_table(dev_actor(), table_id).await)
}

/// `GET /api/tables/{id}` — the table's current projected state.
pub(crate) async fn get<S>(State(edge): State<Arc<Edge<S>>>, Path(id): Path<String>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(table_id) = parse_table(&id) else {
        return bad_request();
    };
    Json(TableResponse {
        table_id: table_id.to_string(),
        state: edge.table_state(table_id).as_wire().to_owned(),
    })
    .into_response()
}

/// Parses a table id from the path. `None` if it is not a ULID.
fn parse_table(id: &str) -> Option<TableId> {
    Ulid::from_str(id).ok().map(TableId::new)
}

/// The `400` for a path segment that is not a ULID.
fn bad_request() -> Response {
    (StatusCode::BAD_REQUEST, "a table id is a ULID").into_response()
}

/// Maps a command outcome to a response: the table on success, a status that names the failure kind.
fn respond(outcome: Result<TableView, AppError>) -> Response {
    match outcome {
        Ok(view) => Json(TableResponse::from(view)).into_response(),
        // A refused command (illegal transition, missing permission, disabled capability, or a
        // table/line that is not in a state the command applies to) is the caller's fault, not the
        // server's — 409 Conflict rather than 500.
        Err(AppError::Domain(error)) => (StatusCode::CONFLICT, error.to_string()).into_response(),
        Err(error @ (AppError::NoOpenOrder | AppError::UnknownLine | AppError::UnknownBill)) => {
            (StatusCode::CONFLICT, error.to_string()).into_response()
        }
        Err(AppError::Port(_)) => {
            (StatusCode::SERVICE_UNAVAILABLE, "the store is unavailable").into_response()
        }
        Err(AppError::Clock | AppError::Encode(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the edge could not apply the command",
        )
            .into_response(),
    }
}

/// A fixed development actor, until the paired device token and signed-in employee are resolved.
fn dev_actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(1)),
        device_id: DeviceId::new(Ulid::from_u128(1)),
    }
}
