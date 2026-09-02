// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The counter's order list: `GET /api/orders/open` ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)).
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

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;

use crate::app::Edge;
use crate::http::error_response;
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

/// The counter's sub-router, to be merged behind the same gates as the rest of the domain surface.
pub(crate) fn router<S, Q>(edge: Arc<Edge<S>>, queue: Q) -> Router
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    Router::new()
        .route("/api/orders/open", get(open_orders::<S, Q>))
        .with_state(Arc::new(CounterDeps { edge, queue }))
}
