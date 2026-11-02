// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/bills/{id}/void_settled`: voiding a paid bill, whole
//! ([ADR-0184](../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_core::business_date::local_time;
use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::BillId;
use pos_proto::money::Money;

use crate::app::{Edge, SettledVoidView, TillScope};
use crate::http::bills::VoidBillRequest;
use crate::http::{bad_request, cash_movements, error_response, parse_ulid, shifts};
use crate::printing::{PrintOutcome, Printers, VoidSlip};

/// What a void after payment gives back, for the till to show (ADR-0184 decision 6).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct VoidSettledResponse {
    bill_id: BillId,
    /// `BILL_STATE_VOIDED`.
    state: &'static str,
    /// The receipt's number, which stays used.
    receipt_number: u64,
    /// What the guest paid.
    total_due: Money,
    /// The cash handed back from this till's drawer.
    cash_returned: Money,
    /// Card and every other tender, recorded as reversed: the cashier gives it back on the card
    /// terminal or through the provider, which the till does not drive.
    other_reversed: Money,
    /// What came of opening the drawer for the cash, where there was cash: `OPENED`, `NO_DRAWER`,
    /// and so on. Absent where no cash is handed back.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, value_type = Option<cash_movements::DrawerOutcomeSchema>))]
    drawer_open: Option<String>,
    /// What came of printing the void slip, as a receipt's printing reports it: `PRINTED`,
    /// `NO_PRINTER`, and so on.
    void_slip_print: &'static str,
}

impl From<&SettledVoidView> for VoidSettledResponse {
    fn from(view: &SettledVoidView) -> Self {
        Self {
            bill_id: view.bill_id,
            state: view.state.as_wire(),
            receipt_number: view.receipt_number,
            total_due: view.total_due,
            cash_returned: view.cash_returned,
            other_reversed: view.other_reversed,
            drawer_open: None,
            void_slip_print: PrintOutcome::NoPrinter.as_wire(),
        }
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/bills/{id}/void_settled",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["billing.bill.void_settled", "cash.drawer.override_assignment"]))),
    params(
        ("id" = String, Path, description = "The bill's id, a ULID"),
    ),
    request_body(
        content = VoidBillRequest, description = "`reason_code_id`, a void reason from the managed list, with `approver_code` and \
                       `approver_pin` of somebody who holds `billing.bill.void_settled` where the person acting \
                       does not hold it directly. Both are secrets and are never logged",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = VoidSettledResponse, description = "Voided: what was given back, cash from this till's drawer and \
                                      every other tender recorded as reversed"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; the void \
                                      needs an approval that was missing or refused, or the approver is \
                                      locked out; or this box was superseded by a replacement"),
        (status = 409, description = "Refused: the bill is not settled (`TRANSITION_REFUSED`), it is past the \
                                      store's window (`SETTLED_VOID_WINDOW_CLOSED`), an e-invoice was issued \
                                      for it (`E_INVOICE_ISSUED`), the reason is not one for voiding a bill \
                                      (`VOID_REASON_NOT_VALID`), the person lacks the permission, or the cash \
                                      cannot leave this device's drawer. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "bills",
))]
/// `POST /api/bills/{id}/void_settled` — void a bill that was paid, whole, citing a reason from the
/// store's managed list (ADR-0184).
///
/// The bill's cash leaves the drawer of the till this device is, which opens for it once the void
/// has committed; every other tender is recorded as reversed. A device whose till cannot be read
/// gives back no cash rather than guess a drawer, as a settle takes none.
pub(crate) async fn void_settled_bill<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<VoidBillRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    let scope = match shifts::till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) => {
            tracing::warn!(%error, "the till a device is could not be read; it gives back no cash");
            TillScope::default()
        }
    };
    let approval = request.approval();
    let view = match edge
        .void_settled_bill(
            actor,
            scope,
            bill_id,
            request.reason_code_id,
            approval.as_ref(),
        )
        .await
    {
        Ok(view) => view,
        Err(error) => return error_response(&error),
    };
    let mut response = VoidSettledResponse::from(&view);
    if view.open_drawer {
        let opened =
            cash_movements::open_drawer_through(printers.as_deref(), &edge, view.drawer_till).await;
        response.drawer_open = Some(opened.as_wire().to_owned());
    }
    if view.print_void_slip
        && let Some(printers) = printers.as_deref()
    {
        response.void_slip_print = print_slip(printers, &edge, actor, &view).await.as_wire();
    }
    Json(response).into_response()
}

/// Prints the void slip at the receipt printer of the till the void was done at, in the store's
/// local time (ADR-0184 decision 4 and its amendment: no person on it). After the commit, never
/// before, so a printer that is down cannot unwind a void.
async fn print_slip<S>(
    printers: &Arc<Printers>,
    edge: &Arc<Edge<S>>,
    actor: Actor,
    view: &SettledVoidView,
) -> PrintOutcome
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    let till = printers.till_for(&session, actor.device_id).await;
    let slip = VoidSlip {
        receipt_number: view.receipt_number,
        total_due: view.total_due,
        cash_returned: view.cash_returned,
        other_reversed: view.other_reversed,
        reason: view.reason.as_ref().map(|name| name.as_str().to_owned()),
        till: view.till_name.as_ref().map(|name| name.as_str().to_owned()),
        voided_at: local_time(view.voided_time, &session.timezone)
            .map(|time| time.to_string())
            .unwrap_or_default(),
    };
    printers
        .print_void_slip(&session, &till, edge.store_id(), view.job_id, &slip)
        .await
}
