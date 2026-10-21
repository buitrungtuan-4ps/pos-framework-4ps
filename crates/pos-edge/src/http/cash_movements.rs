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
