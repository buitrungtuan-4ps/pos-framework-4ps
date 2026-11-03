// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The X at the till: a shift's, and the store's across its drawers, read and printed without
//! resetting anything
//! ([ADR-0187](../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
//! decisions 4 and 5).
//!
//! `GET /api/shifts/{id}/x` reads one shift, `GET /api/shifts:x` every open drawer's, or once none
//! is open the business date's closed shifts, which is the store's end of day, and
//! `POST /api/shifts:print_x` prints either as "X - not a close". A person reads an X by their own
//! role's `reports.takings.view` or `reports.shift.read_x`, as the takings read is asked, and
//! anybody else is answered `403 PERMISSION_DENIED`. In a blind-close store the expected cash is
//! there only for a holder of `reports.shift.read_x`. No person is named.
//!
//! A holder of `reports.shift.read_x` also prints a handover's slip again,
//! `POST /api/shifts/{id}/handover-slip/print`, from the drawer list (decision 3).

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::ShiftId;
use pos_proto::money::Money;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};

use crate::app::{Edge, ShiftX, StoreX, XFigures};
use crate::http::shifts::{printed_at, till_scope};
use crate::http::{ERROR_REASON_HEADER, bad_request, error_response, parse_ulid};
use crate::printing::{PrintOutcome, Printers, XPaper};

/// What each tender took.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct TendersResponse {
    cash: Money,
    card: Money,
    qr: Money,
    other: Money,
}

/// One shift's figures, or several shifts' together.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct XFiguresResponse {
    /// What the settled bills came to: what their guests paid.
    sales_amount: Money,
    /// How many bills settled.
    bill_count: u32,
    /// What each tender applied to them, change and tips apart.
    tenders: TendersResponse,
    /// Their discounts and comps.
    discount_amount: Money,
    /// How many paid bills were voided at the drawer's till (ADR-0184).
    voided_bill_count: u32,
    /// The cash those voids handed back.
    voided_cash_amount: Money,
    paid_in_amount: Money,
    paid_out_amount: Money,
    /// Cash dropped to the safe (ADR-0186).
    dropped_amount: Money,
    /// The cash the drawer should hold. Absent in a blind-close store for a reader without
    /// `reports.shift.read_x`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    expected_amount: Option<Money>,
}

impl From<&XFigures> for XFiguresResponse {
    fn from(figures: &XFigures) -> Self {
        Self {
            sales_amount: figures.sales,
            bill_count: figures.bills,
            tenders: TendersResponse {
                cash: figures.tenders.cash,
                card: figures.tenders.card,
                qr: figures.tenders.qr,
                other: figures.tenders.other,
            },
            discount_amount: figures.discounts,
            voided_bill_count: figures.voided_bills,
            voided_cash_amount: figures.voided_cash,
            paid_in_amount: figures.paid_in,
            paid_out_amount: figures.paid_out,
            dropped_amount: figures.dropped,
            expected_amount: figures.expected_cash,
        }
    }
}

/// One shift's X.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct ShiftXResponse {
    shift_id: String,
    /// The till whose drawer it is, absent for the store's one drawer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    terminal_device_id: Option<String>,
    /// That till's name, as the store's devices give it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    name: Option<DisplayName>,
    /// `SHIFT_STATE_OPEN`, `SHIFT_STATE_COUNTED` or `SHIFT_STATE_CLOSED`.
    state: String,
    business_date: BusinessDate,
    opened_time: Timestamp,
    figures: XFiguresResponse,
}

impl From<&ShiftX> for ShiftXResponse {
    fn from(x: &ShiftX) -> Self {
        Self {
            shift_id: x.shift_id.to_string(),
            terminal_device_id: x.till.map(|till| till.to_string()),
            name: x.till_name.clone(),
            state: x.state.as_wire().to_owned(),
            business_date: x.business_date,
            opened_time: x.opened_time,
            figures: XFiguresResponse::from(&x.figures),
        }
    }
}

/// The store's X across its drawers.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct StoreXResponse {
    business_date: BusinessDate,
    /// `true` once no drawer is open and every shift of the day has closed: "All tills closed for
    /// the day" (ADR-0187 decision 5).
    all_closed: bool,
    /// Every open drawer's shift, or the day's closed shifts once none is open, oldest first.
    shifts: Vec<ShiftXResponse>,
    /// Their figures together, absent where there is no shift.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    totals: Option<XFiguresResponse>,
}

impl From<&StoreX> for StoreXResponse {
    fn from(x: &StoreX) -> Self {
        Self {
            business_date: x.business_date,
            all_closed: x.all_closed,
            shifts: x.shifts.iter().map(ShiftXResponse::from).collect(),
            totals: x.totals.as_ref().map(XFiguresResponse::from),
        }
    }
}

/// Which X to print: one shift's, or the store's where it names none.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct PrintXRequest {
    #[serde(default)]
    shift_id: Option<ShiftId>,
}

/// What printing an X answers.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct PrintXResponse {
    /// What came of printing it, the tokens a settle reports its receipt with.
    x_print: String,
}

/// `403 PERMISSION_DENIED` for somebody who may not read an X.
fn not_a_reader() -> Response {
    (
        StatusCode::FORBIDDEN,
        [(ERROR_REASON_HEADER, "PERMISSION_DENIED")],
        "reading an X needs reports.takings.view or reports.shift.read_x",
    )
        .into_response()
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/shifts/{id}/x",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["reports.takings.view", "reports.shift.read_x"]))),
    params(("id" = String, Path, description = "The shift's id, a ULID")),
    responses(
        (status = 200, body = ShiftXResponse, description = "The shift's X: sales, bills, tenders, discounts, voids after \
                                      payment, paid in and out, dropped to safe, and the expected cash \
                                      except in a blind-close store for a reader without reports.shift.read_x. \
                                      Resets nothing"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in, or the person's own role grants neither \
                                      reports.takings.view nor reports.shift.read_x (`PERMISSION_DENIED`)"),
        (status = 409, description = "`UNKNOWN_SHIFT`: the edge knows no such shift"),
    ),
    tag = "shifts",
))]
/// `GET /api/shifts/{id}/x` — a shift's X, which resets nothing.
pub(crate) async fn shift_x<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(shift_id) = parse_ulid(&id).map(ShiftId::new) else {
        return bad_request("a shift id is a ULID");
    };
    if !edge.reads_x(actor) {
        return not_a_reader();
    }
    match edge.shift_x(actor, shift_id) {
        Ok(x) => Json(ShiftXResponse::from(&x)).into_response(),
        Err(error) => error_response(&error),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/shifts:x",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["reports.takings.view", "reports.shift.read_x"]))),
    responses(
        (status = 200, body = StoreXResponse, description = "Every open drawer's X with their totals; once none is open, the \
                                      business date's closed shifts with `all_closed`. Resets nothing"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in, or the person's own role grants neither \
                                      reports.takings.view nor reports.shift.read_x (`PERMISSION_DENIED`)"),
        (status = 500, description = "The edge could not read its clock"),
    ),
    tag = "shifts",
))]
/// `GET /api/shifts:x` — the store's X across its drawers, which resets nothing.
pub(crate) async fn store_x<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    if !edge.reads_x(actor) {
        return not_a_reader();
    }
    match edge.store_x(actor) {
        Ok(x) => Json(StoreXResponse::from(&x)).into_response(),
        Err(error) => error_response(&error),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/shifts:print_x",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["reports.takings.view", "reports.shift.read_x"]))),
    request_body(
        content = PrintXRequest, description = "Optional `shift_id`: that shift's X; absent, the store's. The body may be empty",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = PrintXResponse, description = "Printed as \"X - not a close\" in the paper's language where this \
                                      device's till prints its cash-up, and what came of it"),
        (status = 400, description = "The body is not a shift id"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in, or the person's own role grants neither \
                                      reports.takings.view nor reports.shift.read_x (`PERMISSION_DENIED`)"),
        (status = 409, description = "`UNKNOWN_SHIFT`: the edge knows no such shift"),
        (status = 503, description = "The till this device is could not be read"),
    ),
    tag = "shifts",
))]
/// `POST /api/shifts:print_x` — print a shift's X or the store's, recording nothing.
pub(crate) async fn print_x<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    body: Bytes,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let request: PrintXRequest = if body.is_empty() {
        PrintXRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(_) => {
                return bad_request("an X to print names a shift, or nothing for the store's");
            }
        }
    };
    if !edge.reads_x(actor) {
        return not_a_reader();
    }
    let read = match request.shift_id {
        Some(shift_id) => edge.shift_x(actor, shift_id).map(Read::Shift),
        None => edge.store_x(actor).map(Read::Store),
    };
    let read = match read {
        Ok(read) => read,
        Err(error) => return error_response(&error),
    };
    let scope = match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let outcome = match printers.as_deref() {
        Some(printers) => {
            let session = edge.session();
            let at = printed_at(&session, scope);
            let paper = match &read {
                Read::Shift(x) => XPaper::Shift(x),
                Read::Store(x) => XPaper::Store(x),
            };
            printers
                .print_x(&session, &at, edge.store_id(), edge.print_job_id(), paper)
                .await
        }
        None => PrintOutcome::NoPrinter,
    };
    Json(PrintXResponse {
        x_print: outcome.as_wire().to_owned(),
    })
    .into_response()
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/shifts/{id}/handover-slip/print",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("reports.shift.read_x"))),
    params(("id" = String, Path, description = "The shift that took the drawer over, a ULID")),
    responses(
        (status = 200, body = HandoverSlipPrintResponse, description = "The handover slip printed again where this device's till prints \
                                      its cash-up, and the difference it shows"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in, or the person's own role lacks \
                                      reports.shift.read_x (`PERMISSION_DENIED`)"),
        (status = 409, description = "`UNKNOWN_SHIFT`, or `NOTHING_TO_PRINT` for a shift that took no drawer over"),
        (status = 503, description = "The till this device is could not be read"),
    ),
    tag = "shifts",
))]
/// `POST /api/shifts/{id}/handover-slip/print` — print a handover's slip again, from the drawer list
/// (ADR-0187 decision 3). It records nothing; the slip names no person.
pub(crate) async fn reprint_handover_slip<S>(
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
    if !edge
        .session()
        .staff
        .grants(actor.employee_id, Permission::ReadShiftX)
    {
        return (
            StatusCode::FORBIDDEN,
            [(ERROR_REASON_HEADER, "PERMISSION_DENIED")],
            "printing a handover slip again needs reports.shift.read_x",
        )
            .into_response();
    }
    let slip = match edge.handover_slip_of(shift_id) {
        Ok(slip) => slip,
        Err(error) => return error_response(&error),
    };
    let scope = match till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let outcome = match printers.as_deref() {
        Some(printers) => {
            let session = edge.session();
            let at = printed_at(&session, scope);
            // A job of its own each time: a reprint asked twice prints twice.
            printers
                .print_handover_slip(&session, &at, edge.store_id(), edge.print_job_id(), &slip)
                .await
        }
        None => PrintOutcome::NoPrinter,
    };
    Json(HandoverSlipPrintResponse {
        handover_difference: slip.difference,
        handover_slip_print: outcome.as_wire().to_owned(),
    })
    .into_response()
}

/// What printing a handover slip again answers.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct HandoverSlipPrintResponse {
    /// The incoming float less the outgoing count.
    handover_difference: Money,
    /// What came of printing it.
    handover_slip_print: String,
}

/// The X a print request read.
enum Read {
    Shift(ShiftX),
    Store(StoreX),
}
