// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/bills/{id}/payments`: one payment towards a bill, recorded as it is taken; the last one
//! settles it ([ADR-0220](../../../../docs/adr/0220-a-bill-takes-its-payments-one-at-a-time.md)).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::PaymentMethod;
use pos_proto::ids::BillId;
use pos_proto::money::Money;

use crate::app::{Approval, Edge, TillScope};
use crate::http::bills::{BillResponse, PaymentRequest, print_receipt_for};
use crate::http::{bad_request, cash_movements, error_response, parse_ulid, shifts};
use crate::printing::Printers;

/// The most equal shares an even split records (ADR-0220 decision 3).
const SPLIT_WAYS: std::ops::RangeInclusive<u8> = 2..=20;

/// One payment towards a bill, as a till takes it.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct TakePaymentRequest {
    payment: PaymentRequest,
    /// How many equal shares an even split divided the bill into, 2 to 20, when this payment is
    /// one of them.
    #[serde(default)]
    split_ways: Option<u8>,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

/// What a payment came to: what is paid and what remains, and the settle when it was the last.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct TakePaymentResponse {
    bill_id: BillId,
    paid_total: Money,
    remaining: Money,
    /// What came of opening the drawer, where cash was taken.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    drawer_open: Option<String>,
    /// The settled bill, as `POST /api/bills/{id}/settle` answers, when this payment was the last.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    settled: Option<BillResponse>,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/bills/{id}/payments",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["billing.payment.take", "cash.drawer.override_assignment"]))),
    params(
        ("id" = String, Path, description = "The bill's id, a ULID"),
    ),
    request_body(
        content = TakePaymentRequest, description = "`payment`, with its `method`, `tendered`, `applied_to_bill` and optional `tip`; \
                       `split_ways` where it is one of an even split's shares; and `approver_code` and \
                       `approver_pin` where cash goes into a drawer assigned to somebody else. Both are \
                       secrets and are never logged",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = TakePaymentResponse, description = "Taken: what the bill's payments now come to and what remains, what came of \
                                      opening the drawer, and the settled bill when this payment covered \
                                      what remained"),
        (status = 400, description = "The id is not a ULID, the method is unknown or not accepted as tender \
                                      here, nothing is applied, or `split_ways` is not 2 to 20"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; cash into \
                                      somebody else's drawer needs an approval that was missing or refused, \
                                      or the approver is locked out; or this box was superseded by a \
                                      replacement"),
        (status = 409, description = "Refused: more than the bill still owes (`PAYMENT_EXCEEDS_REMAINING`), \
                                      a part payment at a store that takes a bill in one \
                                      (`PART_PAYMENTS_OFF`), the drawer or shift refusals a settle makes, or \
                                      the bill is not open. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "bills",
))]
/// `POST /api/bills/{id}/payments` — take one payment towards a bill (ADR-0220).
pub(crate) async fn take_payment<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
    Json(request): Json<TakePaymentRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    if request
        .split_ways
        .is_some_and(|ways| !SPLIT_WAYS.contains(&ways))
    {
        return bad_request("an even split is two to twenty shares");
    }
    let Ok(payment) = request.payment.into_payment() else {
        return bad_request("a payment method must be a known method");
    };
    if payment.applied_to_bill.amount_minor <= 0 {
        return bad_request("a payment applies something to the bill");
    }
    if !edge.session().tender_accepted(payment.method) {
        return bad_request("this store does not accept that payment method as tender");
    }
    let takes_cash = payment.method == PaymentMethod::Cash;
    let scope = match shifts::till_scope(&edge, printers.as_deref(), actor, None).await {
        Ok(scope) => scope,
        Err(error) if takes_cash => return error_response(&error),
        Err(error) => {
            tracing::warn!(%error, "the till a device is could not be read; its tender is no till's");
            TillScope::default()
        }
    };
    let approval = request
        .approver_code
        .zip(request.approver_pin)
        .map(|(code, pin)| Approval { code, pin });
    let taken = match edge
        .take_payment_at(
            actor,
            scope,
            bill_id,
            payment,
            request.split_ways,
            approval.as_ref(),
        )
        .await
    {
        Ok(taken) => taken,
        Err(error) => return error_response(&error),
    };

    // After the commit, as a settle: the drawer first, then the receipt of a payment that settled.
    let drawer_open = if taken.open_drawer {
        let opened =
            cash_movements::open_drawer_through(printers.as_deref(), &edge, taken.drawer_till)
                .await;
        Some(opened.as_wire().to_owned())
    } else {
        None
    };
    let mut settled = None;
    if let Some(view) = taken.settled {
        let mut response = BillResponse::from(view.clone());
        if view.print_receipt {
            let printed =
                print_receipt_for(printers.as_deref(), &edge, actor.device_id, &view, None).await;
            response.receipt_print = Some(printed.as_wire().to_owned());
        }
        response.drawer_open.clone_from(&drawer_open);
        settled = Some(response);
    }
    Json(TakePaymentResponse {
        bill_id,
        paid_total: taken.paid_total,
        remaining: taken.remaining,
        drawer_open,
        settled,
    })
    .into_response()
}
