// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/tabs`: every open bar tab, which every till lists
//! ([ADR-0221](../../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)
//! decision 3). A tab opens with `POST /api/orders` and `tab: true`, takes its rounds as any order
//! does, and closes with the bill and payment of any order.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::ids::{BillId, OrderId};
use pos_proto::money::Money;
use pos_proto::time::Timestamp;

use crate::app::TabLabel;
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
    /// The guest's label staff typed, where the store keeps labels (`tabs.guest_label`). Kept on
    /// the store only. Absent without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    label: Option<String>,
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
                                      number: its order, `tab_number`, any `label`, `total_due`, its \
                                      last round's time and any bill open on it"),
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
    S: EventStore + SubjectStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let tabs = match deps.edge.open_tabs() {
        Ok(tabs) => tabs,
        Err(error) => return error_response(&error),
    };
    let order_ids: Vec<OrderId> = tabs.iter().map(|tab| tab.order_id).collect();
    match deps.edge.tab_labels(&order_ids).await {
        Ok(mut labels) => {
            let body: Vec<TabResponse> = tabs
                .into_iter()
                .map(|tab| TabResponse {
                    order_id: tab.order_id,
                    tab_number: tab.tab_number,
                    label: labels.remove(&tab.order_id),
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

/// The tab labelled.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct LabelledTabResponse {
    order_id: OrderId,
}

/// The label staff typed on a tab.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(super) struct TabLabelRequest {
    /// Up to 32 characters; empty or absent clears the label.
    #[serde(default)]
    label: Option<String>,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/tabs/{id}/label",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.line.add"))),
    params(
        ("id" = String, Path, description = "The tab's order id, a ULID"),
    ),
    request_body(
        content = TabLabelRequest, description = "`label`, up to 32 characters with no control character; \
                       empty or absent clears it. Kept on the store only, never in an event, a log or the cloud",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = LabelledTabResponse, description = "Written, or cleared: the tab's `order_id`"),
        (status = 400, description = "The id is not a ULID, or the label is longer than 32 characters or has a \
                                      control character"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, the store keeps no tabs \
                                      (`CAPABILITY_DISABLED`) or no labels (`GUEST_LABEL_OFF`), or the order \
                                      is no open tab (`UNKNOWN_ORDER`)"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "orders",
))]
/// `POST /api/tabs/{id}/label` — write or clear a guest's label on an open tab, where the store
/// keeps labels (ADR-0221 decision 2).
pub(super) async fn label_tab<S, Q>(
    State(deps): State<Arc<CounterDeps<S, Q>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<TabLabelRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    let label = match TabLabel::parse(request.label.as_deref().unwrap_or_default()) {
        Ok(label) => label,
        Err(refused) => return bad_request(refused),
    };
    match deps.edge.label_tab(actor, order_id, label).await {
        Ok(()) => (StatusCode::OK, Json(LabelledTabResponse { order_id })).into_response(),
        Err(error) => error_response(&error),
    }
}
