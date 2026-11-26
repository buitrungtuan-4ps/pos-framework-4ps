// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/bills/{id}/correction_request`: a store manager asks HQ to correct a paid bill's
//! issued e-invoice
//! ([ADR-0250](../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;
use pos_proto::ids::{BillId, ReasonCodeId};
use pos_proto::money::Money;
use pos_proto::time::Timestamp;

use crate::app::{Approval, CorrectionRequested, Edge};
use crate::http::{bad_request, error_response, parse_ulid};

/// A correction request as a till sends it: the reason, and the approver's code and PIN where the
/// person acting does not hold `billing.bill.void_settled` directly. Both are secrets and are
/// never logged.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct CorrectionRequestBody {
    /// A reason from the store's `VOID_BILL` list; required while
    /// `einvoice.correction_reason_required` is on.
    #[serde(default)]
    reason_code_id: Option<ReasonCodeId>,
    #[serde(default)]
    approver_code: Option<String>,
    #[serde(default)]
    approver_pin: Option<String>,
}

/// The request as it was recorded, for the till to show while it waits for HQ.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct CorrectionRequestResponse {
    bill_id: BillId,
    /// The amount asked back: the bill's total.
    amount: Money,
    /// The kind the cloud will issue once HQ approves, such as `EINVOICE_KIND_ADJUSTMENT`.
    kind: &'static str,
    requested_time: Timestamp,
}

impl From<CorrectionRequested> for CorrectionRequestResponse {
    fn from(asked: CorrectionRequested) -> Self {
        Self {
            bill_id: asked.bill_id,
            amount: asked.amount,
            kind: asked.kind.as_wire(),
            requested_time: asked.requested_time,
        }
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/bills/{id}/correction_request",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("billing.bill.void_settled"))),
    params(
        ("id" = String, Path, description = "The bill's id, a ULID"),
    ),
    request_body(
        content = CorrectionRequestBody, description = "`reason_code_id`, a void reason from the managed list, with `approver_code` and \
                       `approver_pin` of somebody who holds `billing.bill.void_settled` where the person acting \
                       does not hold it directly",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = CorrectionRequestResponse, description = "Recorded as `fiscal.correction.requested`: HQ decides in \
                                      the console, and the store may void the bill once it approves. Nothing is issued \
                                      and no money moves"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; the request \
                                      needs an approval that was missing or refused, or the approver is locked out"),
        (status = 409, description = "Refused: the bill is not settled (`TRANSITION_REFUSED`), it needs no request \
                                      (`CORRECTION_NOT_NEEDED`: no invoice, already approved, or the till may void \
                                      it), HQ corrects it in the console (`E_INVOICE_ISSUED`), the reason is missing \
                                      or not one for voiding a bill (`VOID_REASON_NOT_VALID`), or the person lacks \
                                      the permission. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "bills",
))]
/// `POST /api/bills/{id}/correction_request` — ask HQ to correct a paid bill's issued e-invoice
/// (ADR-0245 route b, ADR-0250).
pub(crate) async fn request_correction<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<CorrectionRequestBody>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(bill_id) = parse_ulid(&id).map(BillId::new) else {
        return bad_request("a bill id is a ULID");
    };
    let approval = request
        .approver_code
        .zip(request.approver_pin)
        .map(|(code, pin)| Approval { code, pin });
    match edge
        .request_invoice_correction(actor, bill_id, request.reason_code_id, approval.as_ref())
        .await
    {
        Ok(asked) => Json(CorrectionRequestResponse::from(asked)).into_response(),
        Err(error) => error_response(&error),
    }
}
