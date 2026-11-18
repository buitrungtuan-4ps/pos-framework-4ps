// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/tabs`: every open bar tab, which every till lists
//! ([ADR-0221](../../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)
//! decision 3). A tab opens with `POST /api/orders` and `tab: true`, takes its rounds as any order
//! does, and closes with the bill and payment of any order; `POST /api/orders/{id}/close` ends one
//! nothing was sold on, as it ends any empty tableless order.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{BillId, OrderId, ReasonCodeId};
use pos_proto::money::Money;
use pos_proto::time::Timestamp;

use crate::http::counter::CounterDeps;
use crate::http::{bad_request, error_response, parse_ulid};
use crate::queue::QueueNumberAuthority;

/// One open tab.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct TabResponse {
    order_id: OrderId,
    /// The business day's number it opened with.
    tab_number: u64,
    total_due: Money,
    /// When its last round went to the kitchen. Absent before its first.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    last_round_time: Option<Timestamp>,
    /// A bill already open on it, which the till settles rather than opening a second.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    bill_id: Option<BillId>,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/tabs",
    security(("till_session" = [])),
    responses(
        (status = 200, body = Vec<TabResponse>, description = "Every open tab not moved to a table, by \
                                      number: its order, `tab_number`, `total_due`, its last round's \
                                      time and any bill open on it"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "The figures cannot be assembled, for instance a line whose tax class \
                                      the store has published no rate for. `pos-error-reason` names why"),
    ),
    tag = "orders",
))]
/// `GET /api/tabs` — every open tab.
pub(super) async fn open_tabs<S, Q>(State(deps): State<Arc<CounterDeps<S, Q>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    match deps.edge.open_tabs() {
        Ok(tabs) => {
            let body: Vec<TabResponse> = tabs
                .into_iter()
                .map(|tab| TabResponse {
                    order_id: tab.order_id,
                    tab_number: tab.tab_number,
                    total_due: tab.total_due,
                    last_round_time: tab.last_round_time,
                    bill_id: tab.bill_id,
                })
                .collect();
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(error) => error_response(&error),
    }
}

/// Why an empty tableless order is closed.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(super) struct CloseOrderRequest {
    /// A reason from the store's list for `REASON_ACTION_CLOSE_ORDER`, owed where the list gives
    /// one.
    #[serde(default)]
    reason_code_id: Option<ReasonCodeId>,
}

/// The order closed.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct ClosedOrderResponse {
    order_id: OrderId,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/orders/{id}/close",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.line.add"))),
    params(
        ("id" = String, Path, description = "The order's id, a ULID"),
    ),
    request_body(
        content = CloseOrderRequest, description = "An optional `reason_code_id`, from the store's reasons for \
                       `REASON_ACTION_CLOSE_ORDER`; owed where the list gives one. The body may be left out",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = ClosedOrderResponse, description = "Closed: the `order_id`, gone from the counter's and the tabs' lists"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, the order is not an open one \
                                      without a table (`UNKNOWN_ORDER`), something is sold on it or a bill is \
                                      open (`ORDER_NOT_EMPTY`), or the reason is not one the store lists for \
                                      closing, or is missing where it lists one (`CLOSE_REASON_NOT_VALID`)"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "orders",
))]
/// `POST /api/orders/{id}/close` — close an open tableless order nothing was sold on, such as a
/// tab opened by mistake, with the reason given (ADR-0221).
pub(super) async fn close_order<S, Q>(
    State(deps): State<Arc<CounterDeps<S, Q>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    body: Option<Json<CloseOrderRequest>>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    let request = body.map(|Json(request)| request).unwrap_or_default();
    match deps
        .edge
        .close_empty_order(actor, order_id, request.reason_code_id)
        .await
    {
        Ok(()) => (StatusCode::OK, Json(ClosedOrderResponse { order_id })).into_response(),
        Err(error) => error_response(&error),
    }
}
