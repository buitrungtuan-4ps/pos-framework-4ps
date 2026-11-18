// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/bills/{id}/comp`: giving one line of an open bill away, with a reason and an approver
//! ([ADR-0219](../../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)
//! decision 1).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{BillId, OrderLineId, ReasonCodeId};

use crate::app::{Approval, Edge};
use crate::http::check::CheckResponse;
use crate::http::{bad_request, error_response, parse_ulid};

/// A comp as a till asks for it: which line, why, and the badge and PIN of somebody who holds
/// `billing.comp.apply` where the person acting needs one. No amount: what a comp takes off is the
/// line's whole total, and that is the edge's to say.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct CompRequest {
    order_line_id: OrderLineId,
    reason_code_id: ReasonCodeId,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

impl CompRequest {
    fn approval(&self) -> Option<Approval> {
        let code = self.approver_code.clone()?;
        let pin = self.approver_pin.clone()?;
        Some(Approval { code, pin })
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/bills/{id}/comp",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("billing.comp.apply"))),
    params(
        ("id" = String, Path, description = "The bill's id, a ULID"),
    ),
    request_body(
        content = CompRequest, description = "`order_line_id`, a line the bill covers, and `reason_code_id`, published for \
                       `REASON_ACTION_COMP`, with `approver_code` and `approver_pin` of somebody who may \
                       comp where the person acting may not. Both are secrets and are never logged",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = CheckResponse, description = "What the bill now owes, in the shape `GET /api/orders/{id}/check` \
                                      answers with, from the edge's own arithmetic"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; the comp \
                                      needs an approval that was missing or refused, or the approver is \
                                      locked out; or this box was superseded by a replacement"),
        (status = 409, description = "Refused: the bill does not cover the line or it was voided \
                                      (`UNKNOWN_LINE`), the line is already comped (`ALREADY_COMPED`), the \
                                      reason is not one published for a comp (`REASON_CODE_NOT_VALID`), the \
                                      bill is not open, or the person lacks the permission. \
                                      `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "bills",
))]
/// `POST /api/bills/{id}/comp` — give one line of an open bill away (ADR-0219 decision 1).
///
/// Records `billing.comp.applied` for the line's whole total and, where somebody approved it,
/// `security.permission.overridden`, then answers with the bill as it now stands, so the till
/// redraws it from the edge's arithmetic: the comp lowers the taxable base as a discount does.
pub(crate) async fn comp_line<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<CompRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    let approval = request.approval();
    match edge
        .comp_line(
            actor,
            bill_id,
            request.order_line_id,
            request.reason_code_id,
            approval.as_ref(),
        )
        .await
    {
        Ok(totals) => Json(CheckResponse::of(&totals, &edge.session())).into_response(),
        Err(error) => error_response(&error),
    }
}
