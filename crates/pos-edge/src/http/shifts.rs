// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cash-shift routes: open with a float, enter the blind count, close (P5, §6/§11.1).
//!
//! The count is **blind** — the count request carries only the physical amount, and the response to
//! counting reveals no expectation or variance. Only the close response does. `expected_amount` and
//! `variance` are therefore absent from the JSON until the shift closes.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::ShiftId;
use pos_proto::money::Money;

use crate::app::{Edge, ShiftView};
use crate::http::{bad_request, dev_actor, error_response, parse_ulid};

/// Opening a shift with a starting float.
#[derive(Debug, Deserialize)]
pub(crate) struct OpenRequest {
    opening_float: Money,
}

/// The blind count: the physical cash counted, in minor units. Nothing about what was expected.
#[derive(Debug, Deserialize)]
pub(crate) struct CountRequest {
    counted_minor: i64,
}

/// A shift as returned to a device after a command.
#[derive(Debug, Serialize)]
pub(crate) struct ShiftResponse {
    shift_id: String,
    /// The shift's state (`SHIFT_STATE_OPEN`, `SHIFT_STATE_COUNTED`, `SHIFT_STATE_CLOSED`).
    state: String,
    /// Revealed only at close (§11.1).
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_amount: Option<Money>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counted_amount: Option<Money>,
    /// Revealed only at close. Negative means short.
    #[serde(skip_serializing_if = "Option::is_none")]
    variance: Option<Money>,
    print_shift_report: bool,
}

impl From<ShiftView> for ShiftResponse {
    fn from(view: ShiftView) -> Self {
        Self {
            shift_id: view.shift_id.to_string(),
            state: view.state.as_wire().to_owned(),
            expected_amount: view.expected_amount,
            counted_amount: view.counted_amount,
            variance: view.variance,
            print_shift_report: view.print_shift_report,
        }
    }
}

/// `POST /api/shifts` — open a shift with a starting float.
pub(crate) async fn open<S>(
    State(edge): State<Arc<Edge<S>>>,
    Json(request): Json<OpenRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    respond(edge.open_shift(dev_actor(), request.opening_float).await)
}

/// `POST /api/shifts/{id}/count` — enter the blind count.
pub(crate) async fn count<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(id): Path<String>,
    Json(request): Json<CountRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(shift_id) = parse_ulid(&id).map(ShiftId::new) else {
        return bad_request("a shift id is a ULID");
    };
    respond(
        edge.count_shift(dev_actor(), shift_id, request.counted_minor)
            .await,
    )
}

/// `POST /api/shifts/{id}/close` — close a counted shift, revealing the variance.
pub(crate) async fn close<S>(State(edge): State<Arc<Edge<S>>>, Path(id): Path<String>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(shift_id) = parse_ulid(&id).map(ShiftId::new) else {
        return bad_request("a shift id is a ULID");
    };
    respond(edge.close_shift(dev_actor(), shift_id).await)
}

/// Maps a shift command outcome to a response.
fn respond(outcome: Result<ShiftView, crate::app::AppError>) -> Response {
    match outcome {
        Ok(view) => Json(ShiftResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
