// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cash-shift routes: open with a float, pay cash in and out, enter the blind count, close (P5,
//! §6/§11.1), and open the drawer outside a sale
//! ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)).
//!
//! The count is **blind** — the count request carries only the physical amount, and the response to
//! counting reveals no expectation or variance. Only the close response does. `expected_amount` and
//! `variance` are therefore absent from the JSON until the shift closes. What was paid in and out is
//! in every response, because the cashier entered it: it tells them nothing the close keeps blind.
//!
//! A store that turns `shift.blind_close` off
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2) is sent `expected_amount` in every response, so the Shift screen can show it beside
//! the count. `variance` is still the close's alone.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{EventId, ReasonCodeId, ShiftId};
use pos_proto::money::Money;

use crate::app::{Approval, CashMovement, Edge, ShiftView};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{DrawerOutcome, PrintOutcome, Printers};

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

/// A paid in or a paid out: how much, in minor units, and why (ADR-0165 decision 1).
#[derive(Debug, Deserialize)]
pub(crate) struct MovementRequest {
    amount_minor: i64,
    reason_code_id: ReasonCodeId,
}

/// Opening the drawer outside a sale: why, and the manager whose PIN allows it (ADR-0165 decision 3).
#[derive(Debug, Deserialize)]
pub(crate) struct OpenDrawerRequest {
    reason_code_id: ReasonCodeId,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

impl OpenDrawerRequest {
    fn approval(&self) -> Option<Approval> {
        let code = self.approver_code.clone()?;
        let pin = self.approver_pin.clone()?;
        Some(Approval { code, pin })
    }
}

/// What a no-sale opening reports: the opening is recorded, and this is whether the drawer sprang.
#[derive(Debug, Serialize)]
struct OpenDrawerResponse {
    /// `OPENED`, `NO_DRAWER` or `DRAWER_UNAVAILABLE`.
    drawer_open: &'static str,
}

/// A shift as returned to a device after a command.
#[derive(Debug, Serialize)]
pub(crate) struct ShiftResponse {
    shift_id: String,
    /// The shift's state (`SHIFT_STATE_OPEN`, `SHIFT_STATE_COUNTED`, `SHIFT_STATE_CLOSED`).
    state: String,
    /// Revealed at close (§11.1), and before it only where the store's count is not blind.
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_amount: Option<Money>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counted_amount: Option<Money>,
    /// Revealed only at close. Negative means short.
    #[serde(skip_serializing_if = "Option::is_none")]
    variance: Option<Money>,
    /// Cash paid into the drawer outside a sale so far (ADR-0165).
    paid_in_amount: Money,
    /// Cash paid out of it outside a sale so far.
    paid_out_amount: Money,
    print_shift_report: bool,
    /// What came of printing the shift report, on the close that asked for one — the same wire
    /// tokens a settle reports its receipt with. Absent on every other command.
    #[serde(skip_serializing_if = "Option::is_none")]
    shift_report_print: Option<String>,
    /// What came of opening the drawer, on a paid in or a paid out: `OPENED`, `NO_DRAWER` or
    /// `DRAWER_UNAVAILABLE`. Absent on every other command.
    #[serde(skip_serializing_if = "Option::is_none")]
    drawer_open: Option<String>,
}

impl From<ShiftView> for ShiftResponse {
    fn from(view: ShiftView) -> Self {
        Self {
            shift_id: view.shift_id.to_string(),
            state: view.state.as_wire().to_owned(),
            expected_amount: view.expected_amount,
            counted_amount: view.counted_amount,
            variance: view.variance,
            paid_in_amount: view.paid_in,
            paid_out_amount: view.paid_out,
            print_shift_report: view.print_shift_report,
            shift_report_print: None,
            drawer_open: None,
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
/// close's — except the expectation, where the store has turned the blind close off.
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

/// `POST /api/shifts/{id}/paid-in` — cash put into the drawer outside a sale (ADR-0165).
pub(crate) async fn paid_in<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<MovementRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    movement(&edge, actor, printers, &id, CashMovement::PaidIn, request).await
}

/// `POST /api/shifts/{id}/paid-out` — cash taken out of the drawer outside a sale (ADR-0165).
pub(crate) async fn paid_out<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<MovementRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    movement(&edge, actor, printers, &id, CashMovement::PaidOut, request).await
}

/// Records a paid in or a paid out, then opens the drawer for the cash to go in or come out.
///
/// The drawer after the commit, never before, for the reason a receipt prints after one: a movement
/// that failed must never have sprung a drawer full of cash.
async fn movement<S>(
    edge: &Arc<Edge<S>>,
    actor: Actor,
    printers: Option<Extension<Arc<Printers>>>,
    id: &str,
    movement: CashMovement,
    request: MovementRequest,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(shift_id) = parse_ulid(id).map(ShiftId::new) else {
        return bad_request("a shift id is a ULID");
    };
    if request.amount_minor <= 0 {
        return bad_request("an amount paid in or out is greater than zero");
    }
    let amount = Money::new(edge.session().currency, request.amount_minor);
    let view = match edge
        .record_cash_movement(actor, shift_id, movement, amount, request.reason_code_id)
        .await
    {
        Ok(view) => view,
        Err(error) => return error_response(&error),
    };
    let opened = open_drawer_through(printers.as_deref(), edge).await;
    let mut response = ShiftResponse::from(view);
    response.drawer_open = Some(opened.as_wire().to_owned());
    Json(response).into_response()
}

/// `POST /api/drawer/open` — open the drawer outside a sale, with a manager's PIN and a reason
/// (ADR-0165 decision 3). The opening is recorded first, and the drawer opens after.
pub(crate) async fn open_drawer<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Json(request): Json<OpenDrawerRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let approval = request.approval();
    if let Err(error) = edge
        .open_drawer_no_sale(actor, request.reason_code_id, approval.as_ref())
        .await
    {
        return error_response(&error);
    }
    let opened = open_drawer_through(printers.as_deref(), &edge).await;
    Json(OpenDrawerResponse {
        drawer_open: opened.as_wire(),
    })
    .into_response()
}

/// Opens the drawer through the composition's dispatcher. A composition with none layered in — the
/// fakes-backed example, a route test that does not care — has no drawer to open, and says so.
async fn open_drawer_through<S>(
    printers: Option<&Arc<Printers>>,
    edge: &Arc<Edge<S>>,
) -> DrawerOutcome
where
    S: EventStore + Send + Sync + 'static,
{
    match printers {
        Some(printers) => printers.open_drawer(&edge.session()).await,
        None => DrawerOutcome::NoDrawer,
    }
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
