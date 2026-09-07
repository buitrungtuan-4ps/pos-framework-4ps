// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The staff-confirmation queue: list the guest orders waiting, then confirm or refuse one
//! ([ADR-0116](../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)).
//!
//! ADR-0012 and [ADR-0057](../../../docs/adr/0057-qr-ordering.md) made staff confirmation the
//! protection on a printed QR code — *"what stops a passer-by ordering forty pizzas to a table they
//! are not sitting at"*. Until these routes existed there was nothing for a member of staff to press:
//! the hold was reported to the cloud and written to the idempotency ledger, and no code read it
//! back. These three routes are the missing half, and `POST /api/lines/{id}/fire` is where the hold
//! is now enforced.
//!
//! A rejection cites a reason from the managed list
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md)), validated for
//! `REASON_ACTION_REJECT_ORDER` before the event is written. `GET /api/orders/awaiting-confirmation`
//! carries the reasons the picker may offer, in the store's display language, so the till needs no
//! second read and no hardcoded list.

use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use axum::{Json, http::StatusCode};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{OrderId, ReasonCodeId};
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::ReasonAction;

use crate::app::Edge;
use crate::http::{bad_request, error_response, parse_ulid};

/// One line of a waiting order, so staff can see what the guest asked for before agreeing to it.
#[derive(Debug, Serialize)]
struct WaitingLineResponse {
    display_name: String,
    quantity: Quantity,
    line_total: Money,
}

/// One reason the till may offer for a refusal — id to send back, name to show.
#[derive(Debug, Serialize)]
struct RejectReasonResponse {
    reason_code_id: String,
    code: String,
    display_name: String,
}

/// One guest order waiting for a member of staff.
#[derive(Debug, Serialize)]
struct WaitingOrderResponse {
    order_id: String,
    /// The table the guest scanned. Always present: a held order is a tabled QR order by
    /// definition (ADR-0116), and a screen that could not say which table would be useless.
    table_id: String,
    items: Vec<WaitingLineResponse>,
    total: Money,
}

/// The queue, plus the reasons a refusal may cite.
///
/// One response rather than two reads: the picker is only ever opened from this screen, and a
/// till that had to fetch the reasons separately could show a refusal dialog with nothing in it.
#[derive(Debug, Serialize)]
struct WaitingResponse {
    orders: Vec<WaitingOrderResponse>,
    reject_reasons: Vec<RejectReasonResponse>,
}

/// The reason a refusal cites.
#[derive(Debug, Deserialize)]
pub(crate) struct RejectRequest {
    reason_code_id: ReasonCodeId,
}

/// `GET /api/orders/awaiting-confirmation` — the guest orders waiting, and the refusal reasons.
pub(crate) async fn awaiting<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    let language = session.display_language.clone().unwrap_or_default();
    let orders = edge
        .orders_awaiting_staff_confirmation()
        .into_iter()
        .map(|order_id| {
            let (lines, table_id) = edge.waiting_order_view(order_id);
            let total = lines
                .iter()
                .try_fold(Money::zero(session.currency), |sum, line| {
                    sum.checked_add(line.line_total)
                })
                .unwrap_or_else(|_| Money::zero(session.currency));
            WaitingOrderResponse {
                order_id: order_id.to_string(),
                table_id: table_id.map(|id| id.to_string()).unwrap_or_default(),
                items: lines
                    .into_iter()
                    .map(|line| WaitingLineResponse {
                        display_name: line.display_name,
                        quantity: line.quantity,
                        line_total: line.line_total,
                    })
                    .collect(),
                total,
            }
        })
        .collect();

    let reject_reasons = session
        .reason_codes
        .for_action(ReasonAction::RejectOrder)
        .map(|code| RejectReasonResponse {
            reason_code_id: code.id.to_string(),
            code: code.code.as_str().to_owned(),
            display_name: code.localized_name(&language).as_str().to_owned(),
        })
        .collect();

    Json(WaitingResponse {
        orders,
        reject_reasons,
    })
    .into_response()
}

/// `POST /api/orders/{id}/confirm` — release a guest's order to the kitchen.
pub(crate) async fn confirm<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    match edge.confirm_inbound_order(actor, order_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => error_response(&error),
    }
}

/// `POST /api/orders/{id}/reject` — refuse a guest's order, with a reason, and close it.
pub(crate) async fn reject<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<RejectRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    match edge
        .reject_inbound_order(actor, order_id, request.reason_code_id)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => error_response(&error),
    }
}
