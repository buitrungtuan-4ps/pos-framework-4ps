// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cash-shift routes: open with a float, enter the blind count, close (P5, §6/§11.1).
//!
//! The count is **blind** — the count request carries only the physical amount, and the response to
//! counting reveals no expectation or variance. Only the close response does. `expected_amount` and
//! `variance` are therefore absent from the JSON until the shift closes.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{EventId, ShiftId};
use pos_proto::money::Money;

use crate::app::{Edge, ShiftView};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{PrintOutcome, Printers};

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
    /// What came of printing the shift report, on the close that asked for one — the same wire
    /// tokens a settle reports its receipt with. Absent on every other command.
    #[serde(skip_serializing_if = "Option::is_none")]
    shift_report_print: Option<String>,
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
            shift_report_print: None,
        }
    }
}

/// `POST /api/shifts` — open a shift with a starting float.
pub(crate) async fn open<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<OpenRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    respond(edge.open_shift(actor, request.opening_float).await)
}

/// `GET /api/shifts/current` — the shift trading now, or `null` when none is open.
///
/// Until this read existed a shift lived in the browser that opened it: a reload, a second till or a
/// tablet that restarted mid-shift offered to *open* one, the edge refused because one was open, and
/// nothing could count or close it. `null` rather than `404` because no open shift is the ordinary
/// state before the first open of the day, not a missing resource.
///
/// Blind like the count: the expectation and the variance are never in this answer, only in the
/// close's.
pub(crate) async fn current<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    Json(edge.current_shift().map(ShiftResponse::from)).into_response()
}

/// `POST /api/shifts/{id}/count` — enter the blind count.
pub(crate) async fn count<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
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
        edge.count_shift(actor, shift_id, request.counted_minor)
            .await,
    )
}

/// `POST /api/shifts/{id}/close` — close a counted shift, revealing the variance, and print the
/// shift report when the domain asks for one.
///
/// The report prints after the commit, never before, for the reason a receipt does: a printer that
/// is down must not reopen a shift, and a close that failed must never have printed.
pub(crate) async fn close<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(shift_id) = parse_ulid(&id).map(ShiftId::new) else {
        return bad_request("a shift id is a ULID");
    };
    let view = match edge.close_shift(actor, shift_id).await {
        Ok(view) => view,
        Err(error) => return error_response(&error),
    };
    let printed = match (view.print_shift_report, view.report.as_ref()) {
        (true, Some(report)) => Some(match printers.as_deref() {
            // The shift's own id as the idempotency key: a close retried after an ambiguous failure
            // prints one report, not two.
            Some(printers) => {
                printers
                    .print_shift_report(
                        &edge.session(),
                        edge.store_id(),
                        EventId::new(report.shift_id.as_ulid()),
                        report,
                    )
                    .await
            }
            None => PrintOutcome::NoPrinter,
        }),
        _ => None,
    };
    let mut response = ShiftResponse::from(view);
    response.shift_report_print = printed.map(|outcome| outcome.as_wire().to_owned());
    Json(response).into_response()
}

/// Maps a shift command outcome to a response.
fn respond(outcome: Result<ShiftView, crate::app::AppError>) -> Response {
    match outcome {
        Ok(view) => Json(ShiftResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
