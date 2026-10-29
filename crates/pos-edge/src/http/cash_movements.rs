// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cash moved outside a sale: a paid in, a paid out, and opening the drawer with no sale
//! ([ADR-0165](../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)),
//! and the drawer a settle that took cash opens.
//!
//! Each is made at a till, in its own drawer, which is the one that opens, and each is recorded
//! before the drawer opens. Split out of [`super::shifts`], whose shift these act on, by meaning
//! (ADR-0170 decision 3): the shift's own life is there, the cash that moves through it is here.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{DeviceId, ReasonCodeId, ShiftId};
use pos_proto::money::Money;

use crate::app::{Approval, CashMovement, Edge};
use crate::http::shifts::{ShiftResponse, approval, till_scope};
use crate::http::{bad_request, error_response, parse_ulid};
use crate::printing::{DrawerOutcome, Printers};

/// A paid in or a paid out: how much, in minor units, and why (ADR-0165 decision 1), and the
/// approver for a drawer assigned to somebody else.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct MovementRequest {
    amount_minor: i64,
    reason_code_id: ReasonCodeId,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

/// Opening the drawer outside a sale: why, and the manager whose PIN allows it (ADR-0165 decision 3).
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct OpenDrawerResponse {
    /// `OPENED`, `NO_DRAWER` or `DRAWER_UNAVAILABLE`.
    #[cfg_attr(feature = "openapi", schema(value_type = DrawerOutcomeSchema))]
    drawer_open: &'static str,
}

// A schema only: a `drawer_open` is `DrawerOutcome::as_wire`'s string on the wire, and this names its
// three tokens so the till's type is them rather than any string. `drawer_outcome_schema_tests`
// holds the two to one another.
/// What came of opening the cash drawer (ADR-0165). `NO_DRAWER` is a store with no drawer marked,
/// the ordinary case; `DRAWER_UNAVAILABLE` is a drawer that should have opened and did not.
#[cfg(feature = "openapi")]
#[derive(utoipa::ToSchema)]
#[expect(
    dead_code,
    reason = "a schema only: the wire value is `DrawerOutcome::as_wire`'s string"
)]
pub(crate) enum DrawerOutcomeSchema {
    /// The kick went to the drawer printer, or its print agent acknowledged it.
    #[schema(rename = "OPENED")]
    Opened,
    /// No printer this edge may open a drawer through: the ordinary state of a store with no drawer.
    #[schema(rename = "NO_DRAWER")]
    NoDrawer,
    /// The drawer printer was chosen and did not take the kick, which the cashier has to hear about.
    #[schema(rename = "DRAWER_UNAVAILABLE")]
    Unavailable,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/shifts/{id}/paid-in",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["cash.movement.record", "cash.drawer.override_assignment"]))),
    params(
        ("id" = String, Path, description = "The shift's id, a ULID"),
    ),
    request_body(
        content = MovementRequest, description = "`amount_minor`, greater than zero, and `reason_code_id`, with `approver_code` and \
                       `approver_pin` for a drawer assigned to somebody else. Both are secrets and are never \
                       logged",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = ShiftResponse, description = "Recorded: the shift, with `drawer_open` saying whether the drawer \
                                      opened for the cash"),
        (status = 400, description = "The id is not a ULID, or the amount is not greater than zero"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; a drawer \
                                      assigned to somebody else needs an approval that was missing or \
                                      refused, or the approver is locked out; or this box was superseded by \
                                      a replacement"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, or the till this device is could not be \
                                      read; nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "shifts",
))]
/// `POST /api/shifts/{id}/paid-in` — cash put into the drawer outside a sale (ADR-0165).
pub(crate) async fn record_paid_in<S>(
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

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/shifts/{id}/paid-out",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["cash.movement.record", "cash.drawer.override_assignment"]))),
    params(
        ("id" = String, Path, description = "The shift's id, a ULID"),
    ),
    request_body(
        content = MovementRequest, description = "`amount_minor`, greater than zero, and `reason_code_id`, with `approver_code` and \
                       `approver_pin` for a drawer assigned to somebody else. Both are secrets and are never \
                       logged",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = ShiftResponse, description = "Recorded: the shift, with `drawer_open` saying whether the drawer \
                                      opened for the cash"),
        (status = 400, description = "The id is not a ULID, or the amount is not greater than zero"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; a drawer \
                                      assigned to somebody else needs an approval that was missing or \
                                      refused, or the approver is locked out; or this box was superseded by \
                                      a replacement"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, or the till this device is could not be \
                                      read; nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "shifts",
))]
/// `POST /api/shifts/{id}/paid-out` — cash taken out of the drawer outside a sale (ADR-0165).
pub(crate) async fn record_paid_out<S>(
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
    let approval = approval(
        request.approver_code.as_ref(),
        request.approver_pin.as_ref(),
    );
    let view = match edge
        .record_cash_movement_at(
            actor,
            scope,
            shift_id,
            movement,
            amount,
            request.reason_code_id,
            approval.as_ref(),
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

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/drawer/open",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["cash.drawer.open_no_sale", "cash.drawer.override_assignment"]))),
    request_body(
        content = OpenDrawerRequest, description = "`reason_code_id`, with `approver_code` and `approver_pin` of the manager allowing the \
                       opening. Both are secrets and are never logged",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = OpenDrawerResponse, description = "Recorded: `drawer_open` says whether the drawer opened (`OPENED`, \
                                      `NO_DRAWER` or `DRAWER_UNAVAILABLE`)"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; a no-sale \
                                      opening needs a manager's approval that was missing or refused, or the \
                                      approver is locked out; or this box was superseded by a replacement"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, or the till this device is could not be \
                                      read; nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "shifts",
))]
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

#[cfg(all(test, feature = "openapi"))]
mod drawer_outcome_schema_tests {
    //! [`DrawerOutcomeSchema`] names exactly the tokens [`DrawerOutcome::as_wire`] sends.

    use utoipa::PartialSchema as _;

    use super::DrawerOutcomeSchema;
    use crate::printing::DrawerOutcome;

    #[test]
    fn the_schema_names_every_wire_token_and_no_other() {
        let schema = serde_json::to_value(DrawerOutcomeSchema::schema()).expect("schema");
        let named: Vec<&str> = schema["enum"]
            .as_array()
            .expect("an enum of tokens")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let sent: Vec<&str> = [
            DrawerOutcome::Opened,
            DrawerOutcome::NoDrawer,
            DrawerOutcome::Unavailable,
        ]
        .into_iter()
        .map(DrawerOutcome::as_wire)
        .collect();
        assert_eq!(named, sent);
    }
}
