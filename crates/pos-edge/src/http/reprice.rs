// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/lines/{id}/price`: overriding a line's price before its bill opens, with a reason and
//! an approver
//! ([ADR-0219](../../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)
//! decision 2).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{OrderId, OrderLineId, ReasonCodeId};
use pos_proto::money::Money;

use crate::app::{Approval, Edge};
use crate::http::{bad_request, error_response, parse_ulid};

/// A price override as a till asks for it: the new price of one, why, and the badge and PIN of
/// somebody who holds `billing.price.override` where the person acting needs one.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct RepriceRequest {
    /// The new price of one, in minor units, above zero.
    unit_price: Money,
    reason_code_id: ReasonCodeId,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

impl RepriceRequest {
    fn approval(&self) -> Option<Approval> {
        let code = self.approver_code.clone()?;
        let pin = self.approver_pin.clone()?;
        Some(Approval { code, pin })
    }
}

/// The line as the override left it: its new price of one and the total the edge computed from it.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct RepriceResponse {
    order_id: OrderId,
    order_line_id: OrderLineId,
    unit_price: Money,
    previous_unit_price: Money,
    line_total: Money,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/lines/{id}/price",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("billing.price.override"))),
    params(
        ("id" = String, Path, description = "The order line's id, a ULID"),
    ),
    request_body(
        content = RepriceRequest, description = "`unit_price`, in minor units and above zero, and `reason_code_id`, published \
                       for `REASON_ACTION_PRICE_OVERRIDE`, with `approver_code` and `approver_pin` of \
                       somebody who may override a price where the person acting may not. Both are \
                       secrets and are never logged",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = RepriceResponse, description = "The line's new price of one and its total, from the edge's own arithmetic"),
        (status = 400, description = "The id is not a ULID, or the price is not above zero"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; the \
                                      override needs an approval that was missing or refused, or the \
                                      approver is locked out; or this box was superseded by a replacement"),
        (status = 409, description = "Refused: no such line or it was voided (`UNKNOWN_LINE`), its order \
                                      has a bill (`BILL_ALREADY_OPEN`), the reason is not one published \
                                      for an override (`REASON_CODE_NOT_VALID`), the price is above the \
                                      store's published multiple of the price the line was added at \
                                      (`PRICE_OUT_OF_RANGE`), or the person lacks the permission. \
                                      `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "orders",
))]
/// `POST /api/lines/{id}/price` — override a line's price before its bill opens (ADR-0219
/// decision 2).
pub(crate) async fn reprice_line<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<RepriceRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(order_line_id) = parse_ulid(&id).map(OrderLineId::new) else {
        return bad_request("an order line id is a ULID");
    };
    if request.unit_price.amount_minor <= 0 {
        // Giving an item away is a comp, and a negative price sells nothing: both malformed.
        return bad_request("a price override is above zero");
    }
    let approval = request.approval();
    match edge
        .reprice_line(
            actor,
            order_line_id,
            request.unit_price,
            request.reason_code_id,
            approval.as_ref(),
        )
        .await
    {
        Ok(repriced) => Json(RepriceResponse {
            order_id: repriced.order_id,
            order_line_id: repriced.order_line_id,
            unit_price: repriced.unit_price,
            previous_unit_price: repriced.previous_unit_price,
            line_total: repriced.line_total,
        })
        .into_response(),
        Err(error) => error_response(&error),
    }
}
