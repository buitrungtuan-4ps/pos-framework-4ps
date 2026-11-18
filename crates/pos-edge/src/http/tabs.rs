// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/tabs`: every open bar tab, which every till lists
//! ([ADR-0221](../../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)
//! decision 3). A tab opens with `POST /api/orders` and `tab: true`, takes its rounds as any order
//! does, and closes with the bill and payment of any order.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::ids::{BillId, OrderId};
use pos_proto::money::Money;
use pos_proto::time::Timestamp;

use crate::http::counter::CounterDeps;
use crate::http::error_response;
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
