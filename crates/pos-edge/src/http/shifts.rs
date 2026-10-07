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
//!
//! A store that keeps a drawer per till ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)) runs a shift
//! on each. The till a device is comes from its print-agent binding, read only then; a request may
//! name another till's drawer, which also needs `cash.shift.manage_other_till`, and `GET
//! /api/shifts` lists every drawer. A paid in, a paid out and a no-sale opening are made at a till,
//! in its own drawer, and that drawer is the one that opens.
//!
//! What prints at a close is the store's `shift.close_report`: each drawer's report where it is
//! closed, one slip for several closed at once by `POST /api/shifts:batch_close`, or nothing.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{DeviceId, EventId, ReasonCodeId, ShiftId};
use pos_proto::money::Money;
use pos_proto::shift::DrawerModel;
use pos_proto::text::DisplayName;
use pos_proto::time::Timestamp;

use crate::app::{
    AppError, Approval, CashMovement, DrawersView, Edge, EdgeSession, ShiftView, TillScope,
};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{DrawerOutcome, PrintOutcome, Printers, TillPrinting, published_terminals};

/// The code and PIN of somebody approving an act the person acting holds only with approval: both
/// halves, or no approval at all.
fn approval(code: Option<&String>, pin: Option<&String>) -> Option<Approval> {
    Some(Approval {
        code: code?.clone(),
        pin: pin?.clone(),
    })
}

/// Opening a shift with a starting float, on the device's own till's drawer or the one it names.
/// Another till's also takes an approver where the person holds that only with approval
/// (ADR-0167 decision 3).
#[derive(Debug, Deserialize)]
pub(crate) struct OpenRequest {
    opening_float: Money,
    /// Another till's drawer to open, where the store keeps one per till. Absent is the device's
    /// own, and where the store keeps one drawer it is ignored.
    #[serde(default)]
    terminal_device_id: Option<DeviceId>,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

/// The blind count: the physical cash counted, in minor units. Nothing about what was expected.
#[derive(Debug, Deserialize)]
pub(crate) struct CountRequest {
    counted_minor: i64,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

/// A close's optional body: the approver for another till's drawer.
#[derive(Debug, Default, Deserialize)]
struct CloseRequest {
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

/// Closing several counted drawers at once: their shifts, and one approver for those that are
/// other tills' (ADR-0167 decision 10).
#[derive(Debug, Deserialize)]
pub(crate) struct BatchCloseRequest {
    shift_ids: Vec<ShiftId>,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

/// What a batch close answers: each drawer closed, and what came of the one slip for all of them
/// where the store combines its close report.
#[derive(Debug, Serialize)]
struct BatchCloseResponse {
    shifts: Vec<ShiftResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shift_report_print: Option<String>,
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
        approval(self.approver_code.as_ref(), self.approver_pin.as_ref())
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
    /// The till whose drawer it is, absent for the store's one drawer (ADR-0167).
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_device_id: Option<String>,
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
    /// `true` while the shift is still open past the business day it opened on, which a till and
    /// the console flag (ADR-0167 decision 11). Absent otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    day_ended: bool,
}

impl From<ShiftView> for ShiftResponse {
    fn from(view: ShiftView) -> Self {
        Self {
            shift_id: view.shift_id.to_string(),
            terminal_device_id: view.till.map(|till| till.to_string()),
            state: view.state.as_wire().to_owned(),
            expected_amount: view.expected_amount,
            counted_amount: view.counted_amount,
            variance: view.variance,
            paid_in_amount: view.paid_in,
            paid_out_amount: view.paid_out,
            print_shift_report: view.print_shift_report,
            shift_report_print: None,
            drawer_open: None,
            day_ended: view.day_ended,
        }
    }
}

/// `POST /api/shifts` — open a shift with a starting float, on the device's own till's drawer or
/// the one the request names.
pub(crate) async fn open<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Json(request): Json<OpenRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let scope = match till_scope(
        &edge,
        printers.as_deref(),
        actor,
        request.terminal_device_id,
    )
    .await
    {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let approval = approval(
        request.approver_code.as_ref(),
        request.approver_pin.as_ref(),
    );
    respond(
        edge.open_shift_at(actor, scope, request.opening_float, approval.as_ref())
            .await,
    )
}

/// `GET /api/shifts` — every drawer the store keeps: its till, its default float, and its shift
/// while one is open, as blind as the shift's own read; and the model, with the one the store has
/// published while open shifts keep the edge on the other (ADR-0167 decisions 3 and 8). Another
/// till's expectation shows only to a person whose own role grants `cash.shift.manage_other_till`.
pub(crate) async fn drawers<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => Json(DrawersResponse::from(edge.drawers(actor, scope))).into_response(),
        Err(error) => error_response(&error),
    }
}

/// What `GET /api/shifts` answers.
#[derive(Debug, Serialize)]
struct DrawersResponse {
    /// The model the edge runs: `DRAWER_MODEL_PER_STORE` or `DRAWER_MODEL_PER_TERMINAL`.
    drawer_model: &'static str,
    /// The model the store has published, while open shifts keep the edge on the other.
    #[serde(skip_serializing_if = "Option::is_none")]
    waiting_drawer_model: Option<&'static str>,
    drawers: Vec<DrawerResponse>,
}

/// One drawer: absent `terminal_device_id` and `name` for the store's one drawer.
#[derive(Debug, Serialize)]
struct DrawerResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<DisplayName>,
    default_float: Money,
    #[serde(skip_serializing_if = "Option::is_none")]
    opened_time: Option<Timestamp>,
    /// Its shift, `null` while the drawer is closed.
    shift: Option<ShiftResponse>,
}

impl From<DrawersView> for DrawersResponse {
    fn from(view: DrawersView) -> Self {
        Self {
            drawer_model: view.model.as_wire(),
            waiting_drawer_model: view.waiting.map(DrawerModel::as_wire),
            drawers: view
                .drawers
                .into_iter()
                .map(|drawer| DrawerResponse {
                    terminal_device_id: drawer.till.map(|till| till.to_string()),
                    name: drawer.name,
                    default_float: drawer.default_float,
                    opened_time: drawer.opened_time,
                    shift: drawer.shift.map(ShiftResponse::from),
                })
                .collect(),
        }
    }
}

/// The till the device is, by its print-agent binding to a `TERMINAL` entry the store's `devices`
/// node lists, and the till the request names (ADR-0167 decisions 2 and 6). The binding is read
/// only where the edge keeps a drawer per till; where it keeps one, every device's drawer is the
/// store's. One that cannot be read refuses the act, `503`, rather than guess, as a till's paper is
/// refused.
pub(crate) async fn till_scope<S>(
    edge: &Edge<S>,
    printers: Option<&Arc<Printers>>,
    actor: Actor,
    named: Option<DeviceId>,
) -> Result<TillScope, AppError>
where
    S: EventStore + Send + Sync + 'static,
{
    if edge.drawer_model() != DrawerModel::PerTerminal {
        return Ok(TillScope { own: None, named });
    }
    let bound = match printers {
        Some(printers) => printers
            .terminal_of(actor.device_id)
            .await
            .map_err(AppError::Port)?,
        None => None,
    };
    let own = bound.filter(|terminal| {
        published_terminals(&edge.session().devices).any(|entry| entry.device_id == *terminal)
    });
    Ok(TillScope { own, named })
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
///
/// Where the store keeps a drawer per till, the drawer is the device's own till's, and a device that
/// is no till has none.
pub(crate) async fn current<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => Json(edge.current_shift_at(scope).map(ShiftResponse::from)).into_response(),
        Err(error) => error_response(&error),
    }
}

/// `POST /api/shifts/{id}/count` — enter the blind count.
pub(crate) async fn count<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<CountRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(shift_id) = parse_ulid(&id).map(ShiftId::new) else {
        return bad_request("a shift id is a ULID");
    };
    let scope = match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let approval = approval(
        request.approver_code.as_ref(),
        request.approver_pin.as_ref(),
    );
    respond(
        edge.count_shift_at(
            actor,
            scope,
            shift_id,
            request.counted_minor,
            approval.as_ref(),
        )
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
    let scope = match till_scope(edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let view = match edge
        .record_cash_movement_at(
            actor,
            scope,
            shift_id,
            movement,
            amount,
            request.reason_code_id,
        )
        .await
    {
        Ok(view) => view,
        Err(error) => return error_response(&error),
    };
    let opened = open_drawer_through(printers.as_deref(), edge, view.till).await;
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
    let scope = match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let till = match edge
        .open_drawer_no_sale_at(actor, scope, request.reason_code_id, approval.as_ref())
        .await
    {
        Ok(till) => till,
        Err(error) => return error_response(&error),
    };
    let opened = open_drawer_through(printers.as_deref(), &edge, till).await;
    Json(OpenDrawerResponse {
        drawer_open: opened.as_wire(),
    })
    .into_response()
}

/// Opens `till`'s drawer, or the store's one drawer for `None`, through the composition's
/// dispatcher. A composition with none layered in — the fakes-backed example, a route test that
/// does not care — has no drawer to open, and says so.
pub(crate) async fn open_drawer_through<S>(
    printers: Option<&Arc<Printers>>,
    edge: &Arc<Edge<S>>,
    till: Option<DeviceId>,
) -> DrawerOutcome
where
    S: EventStore + Send + Sync + 'static,
{
    match printers {
        Some(printers) => {
            printers
                .open_drawer(&edge.session(), edge.store_id(), edge.print_job_id(), till)
                .await
        }
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
    body: Bytes,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(shift_id) = parse_ulid(&id).map(ShiftId::new) else {
        return bad_request("a shift id is a ULID");
    };
    // No body, as every close sent before another till's drawer could be closed, is no approver.
    let request: CloseRequest = if body.is_empty() {
        CloseRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(_) => return bad_request("a close's body is an approver's code and PIN"),
        }
    };
    let scope = match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let approval = approval(
        request.approver_code.as_ref(),
        request.approver_pin.as_ref(),
    );
    let view = match edge
        .close_shift_at(actor, scope, shift_id, approval.as_ref())
        .await
    {
        Ok(view) => view,
        Err(error) => return error_response(&error),
    };
    let at = printed_at(&edge.session(), scope);
    let printed = print_report(printers.as_deref(), &edge, &at, &view).await;
    let mut response = ShiftResponse::from(view);
    response.shift_report_print = printed;
    Json(response).into_response()
}

/// `POST /api/shifts:batch_close` — close several counted drawers in one act, every one or none,
/// and print what the store's `shift.close_report` asks for: one slip for them all, each drawer's
/// report, or nothing (ADR-0167 decision 10). One approver covers every other till's drawer named.
pub(crate) async fn batch_close<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Json(request): Json<BatchCloseRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    if request.shift_ids.is_empty() {
        return bad_request("a batch close names the shifts it closes");
    }
    let scope = match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let approval = approval(
        request.approver_code.as_ref(),
        request.approver_pin.as_ref(),
    );
    let closed = match edge
        .close_shifts_at(actor, scope, &request.shift_ids, approval.as_ref())
        .await
    {
        Ok(closed) => closed,
        Err(error) => return error_response(&error),
    };
    let session = edge.session();
    let at = printed_at(&session, scope);
    let mut shifts = Vec::with_capacity(closed.shifts.len());
    for view in closed.shifts {
        let printed = print_report(printers.as_deref(), &edge, &at, &view).await;
        let mut response = ShiftResponse::from(view);
        response.shift_report_print = printed;
        shifts.push(response);
    }
    let mut shift_report_print = None;
    if let Some(combined) = closed.combined {
        // The first drawer's shift as the idempotency key: no close of its own prints under it.
        let job_id = combined.drawers.first().map_or_else(
            || edge.print_job_id(),
            |(_, report)| EventId::new(report.shift_id.as_ulid()),
        );
        let outcome = match printers.as_deref() {
            Some(printers) => {
                printers
                    .print_combined_report(&session, &at, edge.store_id(), job_id, &combined)
                    .await
            }
            None => PrintOutcome::NoPrinter,
        };
        shift_report_print = Some(outcome.as_wire().to_owned());
    }
    Json(BatchCloseResponse {
        shifts,
        shift_report_print,
    })
    .into_response()
}

/// Where a cash-up prints: the receipt printer of the till the device is, where the store keeps a
/// drawer per till, and the store's otherwise, as every report printed before (ADR-0167 decision
/// 10).
fn printed_at(session: &EdgeSession, scope: TillScope) -> TillPrinting {
    scope
        .own
        .and_then(|own| published_terminals(&session.devices).find(|entry| entry.device_id == own))
        .map_or(TillPrinting::STORE, TillPrinting::of)
}

/// Prints a closed drawer's report at `at` where its close asks for one, and says what came of it.
async fn print_report<S>(
    printers: Option<&Arc<Printers>>,
    edge: &Edge<S>,
    at: &TillPrinting,
    view: &ShiftView,
) -> Option<String>
where
    S: EventStore + Send + Sync + 'static,
{
    let report = view.report.as_ref().filter(|_| view.print_shift_report)?;
    let outcome = match printers {
        // The shift's own id as the idempotency key: a close retried after an ambiguous failure
        // prints one report, not two.
        Some(printers) => {
            printers
                .print_shift_report(
                    &edge.session(),
                    at,
                    edge.store_id(),
                    EventId::new(report.shift_id.as_ulid()),
                    report,
                )
                .await
        }
        None => PrintOutcome::NoPrinter,
    };
    Some(outcome.as_wire().to_owned())
}

/// Maps a shift command outcome to a response.
fn respond(outcome: Result<ShiftView, AppError>) -> Response {
    match outcome {
        Ok(view) => Json(ShiftResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
