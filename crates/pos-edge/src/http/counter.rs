// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The counter: its order list, `GET /api/orders/open` ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)),
//! and the two routes that let it start its own orders, `POST /api/orders` and
//! `POST /api/orders/{id}/lines` ([ADR-0146](../../../docs/adr/0146-a-counter-store-starts-its-own-orders.md)).
//!
//! The counter's equivalent of `GET /api/floor`. A relayed or QR-counter order is tableless by
//! design ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)), so it appears on no floor plan —
//! without this route a cashier has no way to *find* the order they are being asked to charge, and
//! the order-keyed bill routes are unreachable in practice even though they exist.
//!
//! This module carries its own router and state because it needs the queue-number authority
//! alongside the edge, and `QueueNumberAuthority` returns `impl Future` — it is not dyn-compatible,
//! so the authority cannot be erased into the shared `Arc<Edge<S>>` state every other domain route
//! uses. A sibling sub-router is what `domain_router` already does for the sign-in routes, so this
//! costs no change to any existing handler's signature.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{CourseId, MenuItemId, OrderId};
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;
use pos_proto::{Open, SalesChannel};

use crate::app::{Edge, OrderLineChoice};
use crate::http::lines::LineResponse;
use crate::http::{bad_request, error_response, parse_ulid};
use crate::queue::QueueNumberAuthority;

/// The edge and the queue-number authority, together — this router's state.
///
/// One authority, shared with the intake path rather than a second one built here: a list that
/// disagreed with the numbers the counter actually shouted would be worse than no list.
pub(crate) struct CounterDeps<S, Q> {
    pub(crate) edge: Arc<Edge<S>>,
    pub(crate) queue: Q,
}

/// One line of an order, for a cashier to recognise what they are charging for.
#[derive(Debug, Serialize)]
struct CounterLineResponse {
    display_name: String,
    quantity: Quantity,
}

/// One counter order awaiting payment.
#[derive(Debug, Serialize)]
struct CounterOrderResponse {
    order_id: String,
    /// The daily number staff shouted. Absent for an order that was never given one, which is why
    /// the route reads the number rather than allocating it.
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_number: Option<u64>,
    items: Vec<CounterLineResponse>,
    total_due: Money,
    /// A bill already open on this order. The screen settles **this** bill rather than opening a
    /// second one, which the domain refuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    bill_id: Option<String>,
}

/// `GET /api/orders/open` — every counter order still owing money.
async fn open_orders<S, Q>(State(deps): State<Arc<CounterDeps<S, Q>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    match deps.edge.open_counter_orders(&deps.queue).await {
        Ok(orders) => {
            let body: Vec<CounterOrderResponse> = orders
                .into_iter()
                .map(|order| CounterOrderResponse {
                    order_id: order.order_id.to_string(),
                    queue_number: order.queue_number,
                    items: order
                        .items
                        .into_iter()
                        .map(|line| CounterLineResponse {
                            display_name: line.display_name.as_str().to_owned(),
                            quantity: line.quantity,
                        })
                        .collect(),
                    total_due: order.total_due,
                    bill_id: order.bill_id.map(|id| id.to_string()),
                })
                .collect();
            (StatusCode::OK, Json(body)).into_response()
        }
        // The one real failure is a line whose tax class the store has published no rate for — a
        // configuration error, and the counter showing it beats the counter inventing a number.
        Err(error) => error_response(&error),
    }
}

/// A walk-in order as the counter asks for it (ADR-0146). The channel defaults to takeaway, the
/// ordinary counter order.
#[derive(Debug, Default, Deserialize)]
struct OpenOrderRequest {
    #[serde(default)]
    channel: Option<Open<SalesChannel>>,
}

/// A walk-in order the counter has just opened, with the number the guest will be called by.
#[derive(Debug, Serialize)]
struct OpenedOrderResponse {
    order_id: String,
    queue_number: u64,
    business_date: String,
}

/// `POST /api/orders` — the counter opens a tableless order and gives it the day's next queue
/// number, as a relayed takeaway order gets one (ADR-0146).
async fn open_order<S, Q>(
    State(deps): State<Arc<CounterDeps<S, Q>>>,
    Extension(actor): Extension<Actor>,
    body: Option<Json<OpenOrderRequest>>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let request = body.map(|Json(request)| request).unwrap_or_default();
    let channel = match request.channel.map(|channel| channel.require()) {
        None => SalesChannel::Takeaway,
        Some(Ok(channel)) => channel,
        // A token this build does not know: refused, never guessed, since the channel sets the tax.
        Some(Err(_)) => return bad_request("channel is not a sales channel this edge knows"),
    };
    let opened = match deps.edge.open_counter_order(actor, channel).await {
        Ok(opened) => opened,
        Err(error) => return error_response(&error),
    };
    // After the order commits, as the intake allocates: a number for an order that failed to open
    // would be a gap in the day's calls for nothing.
    let queue_number = match deps
        .queue
        .allocate_queue_number(deps.edge.store_id(), opened.business_date, opened.order_id)
        .await
    {
        Ok(number) => number,
        Err(error) => return error_response(&crate::app::AppError::Port(error)),
    };
    (
        StatusCode::CREATED,
        Json(OpenedOrderResponse {
            order_id: opened.order_id.to_string(),
            queue_number,
            business_date: opened.business_date.to_string(),
        }),
    )
        .into_response()
}

/// A line as the counter adds it to an order by id: what the guest chose, and no price — the
/// edge prices it at the order's own channel (ADR-0146).
#[derive(Debug, Deserialize)]
struct OrderLineRequest {
    menu_item_id: MenuItemId,
    quantity: Quantity,
    #[serde(default)]
    modifier_menu_item_ids: Vec<MenuItemId>,
    #[serde(default)]
    seat: Option<u16>,
    #[serde(default)]
    course_id: Option<CourseId>,
    #[serde(default)]
    note_present: bool,
}

/// `POST /api/orders/{id}/lines` — add a line to an order by its id, priced by the edge.
async fn add_order_line<S, Q>(
    State(deps): State<Arc<CounterDeps<S, Q>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<OrderLineRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    let choice = OrderLineChoice {
        menu_item_id: request.menu_item_id,
        quantity: request.quantity,
        modifier_menu_item_ids: request.modifier_menu_item_ids,
        seat: request.seat,
        course_id: request.course_id,
        note_present: request.note_present,
    };
    match deps.edge.add_line_to_order(actor, order_id, choice).await {
        Ok(view) => Json(LineResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}

/// The counter's sub-router, to be merged behind the same gates as the rest of the domain surface.
pub(crate) fn router<S, Q>(edge: Arc<Edge<S>>, queue: Q) -> Router
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    Router::new()
        .route("/api/orders/open", get(open_orders::<S, Q>))
        .route("/api/orders", post(open_order::<S, Q>))
        .route("/api/orders/{id}/lines", post(add_order_line::<S, Q>))
        .with_state(Arc::new(CounterDeps { edge, queue }))
}
