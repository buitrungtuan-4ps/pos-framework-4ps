// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/orders/{id}/scan`: the line a scanned code names
//! ([ADR-0222](../../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decisions 3 and 5), the act `sales.line.scan`. The edge reads a variable-measure label by the
//! store's layout and prices every line itself.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::OrderId;

use crate::http::counter::CounterDeps;
use crate::http::lines::LineResponse;
use crate::http::{bad_request, error_response, parse_ulid};
use crate::queue::QueueNumberAuthority;

/// The most digits a code carries: a GTIN-14.
const CODE_DIGITS_MAX: usize = 14;

/// What the scanner read.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(super) struct ScanRequest {
    /// The code's digits, as the scanner typed them.
    code: String,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/orders/{id}/scan",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.line.add"))),
    params(
        ("id" = String, Path, description = "The order's id, a ULID"),
    ),
    request_body(
        content = ScanRequest, description = "`code`: the digits a scanner read, a barcode, a PLU or a \
                       variable-measure label",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = LineResponse, description = "The line the code names, one of the item, priced by the \
                                      edge: at the menu's price, or at a price label's within an \
                                      open-price item's bounds"),
        (status = 400, description = "The id is not a ULID, or the code is not 1 to 14 digits"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "Refused: no item carries the code (`UNKNOWN_CODE`), a price label names a \
                                      fixed-price item (`PRICE_LABEL_NOT_OPEN_PRICE`), a weight label \
                                      (`WEIGHT_LABEL_NOT_SOLD`), a price label outside the item's bounds \
                                      (`PRICE_OUT_OF_RANGE`), or what adding a line refuses"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "orders",
))]
/// `POST /api/orders/{id}/scan` — add the line a scanned code names.
pub(super) async fn scan_line<S, Q>(
    State(deps): State<Arc<CounterDeps<S, Q>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<ScanRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    let code = request.code.trim();
    if code.is_empty()
        || code.len() > CODE_DIGITS_MAX
        || !code.bytes().all(|byte| byte.is_ascii_digit())
    {
        return bad_request("a code is 1 to 14 digits");
    }
    match deps.edge.add_scanned_line(actor, order_id, code).await {
        Ok(view) => Json(LineResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}
