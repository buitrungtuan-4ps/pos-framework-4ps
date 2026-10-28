// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The counter: its order list, `GET /api/orders/open` ([ADR-0093](../../../../docs/adr/0093-bill-keyed-on-order.md)),
//! and the two routes that let it start its own orders, `POST /api/orders` and
//! `POST /api/orders/{id}/lines` ([ADR-0146](../../../../docs/adr/0146-a-counter-store-starts-its-own-orders.md)),
//! and today's settled bills, `GET /api/bills/settled`, which lists a counter order by its queue
//! number ([ADR-0164](../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
//!
//! The counter's equivalent of `GET /api/floor`. A relayed or QR-counter order is tableless by
//! design ([ADR-0064](../../../../docs/adr/0064-edge-order-in.md)), so it appears on no floor plan —
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

use pos_core::business_date::{StoreTimeZone, local_time};
use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{BillId, CourseId, MenuItemId, OrderId, TableId};
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;
use pos_proto::time::Timestamp;
use pos_proto::{Open, SalesChannel};

use crate::app::{Edge, OrderLineChoice};
use crate::http::lines::LineResponse;
use crate::http::{bad_request, error_response, parse_ulid};
use crate::line_notes::NoteText;
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct CounterLineResponse {
    display_name: String,
    quantity: Quantity,
}

/// One counter order awaiting payment.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct CounterOrderResponse {
    order_id: String,
    /// The daily number staff shouted. Absent for an order that was never given one, which is why
    /// the route reads the number rather than allocating it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    queue_number: Option<u64>,
    items: Vec<CounterLineResponse>,
    total_due: Money,
    /// A bill already open on this order. The screen settles **this** bill rather than opening a
    /// second one, which the domain refuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    bill_id: Option<String>,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/orders/open",
    security(("till_session" = [])),
    responses(
        (status = 200, body = Vec<CounterOrderResponse>, description = "Every counter order still owing money, with its queue number where it \
                                      was given one, its lines, `total_due`, and any bill open on it"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "The figures cannot be assembled, for instance a line whose tax class \
                                      the store has published no rate for. `pos-error-reason` names why"),
        (status = 503, description = "The queue numbers could not be read"),
    ),
    tag = "orders",
))]
/// `GET /api/orders/open` — every counter order still owing money.
pub(super) async fn open_orders<S, Q>(State(deps): State<Arc<CounterDeps<S, Q>>>) -> Response
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

/// One of today's settled bills, as the till lists it to print a copy from
/// ([ADR-0164](../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct SettledBillResponse {
    bill_id: BillId,
    order_id: OrderId,
    /// The table it was paid at, which the till shows by the floor's label. Absent for a counter
    /// order, which shows its queue number instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    table_id: Option<TableId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    queue_number: Option<u64>,
    receipt_number: u64,
    total_due: Money,
    settle_time: Timestamp,
    /// The shop's wall clock when it settled, `HH:MM` in the store's timezone, for the list to show
    /// as it is: a till's own clock may keep another zone, and the list is today's, so the date
    /// would say nothing. Absent in the one case the calendar cannot read the instant.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    settle_clock: Option<String>,
    /// How many copies have been printed.
    copies: u32,
}

/// The wall clock an instant reads in the store's timezone, `HH:MM`.
pub(super) fn wall_clock(instant: Timestamp, zone: &StoreTimeZone) -> Option<String> {
    let local = local_time(instant, zone).ok()?.to_string();
    local.split_once(' ').map(|(_date, clock)| clock.to_owned())
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/bills/settled",
    security(("till_session" = [])),
    responses(
        (status = 200, body = Vec<SettledBillResponse>, description = "Today's settled bills, newest first and capped: each with its order, \
                                      table or queue number, receipt number, total, settle time and the \
                                      shop's clock at it, and how many copies were printed"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 500, description = "The business date could not be derived from the clock"),
    ),
    tag = "orders",
))]
/// `GET /api/bills/settled` — the bills this store settled today, newest first, at most
/// [`crate::MAX_SETTLED_LISTED`] (ADR-0164). Here rather than beside the other bill routes because a
/// counter order is listed by the queue number staff shouted, which only this router's authority
/// holds.
pub(super) async fn settled_bills<S, Q>(State(deps): State<Arc<CounterDeps<S, Q>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let bills = match deps.edge.settled_bills() {
        Ok(bills) => bills,
        Err(error) => return error_response(&error),
    };
    let session = deps.edge.session();
    let mut body = Vec::with_capacity(bills.len());
    for bill in bills {
        // A table's order has no queue number, and a number that cannot be read leaves the row
        // with its receipt number alone rather than failing the list.
        let queue_number = match bill.table_id {
            Some(_) => None,
            None => deps
                .queue
                .queue_number_for(deps.edge.store_id(), bill.order_id)
                .await
                .ok()
                .flatten(),
        };
        body.push(SettledBillResponse {
            bill_id: bill.bill_id,
            order_id: bill.order_id,
            table_id: bill.table_id,
            queue_number,
            receipt_number: bill.receipt_number,
            total_due: bill.total_due,
            settle_time: bill.settle_time,
            settle_clock: wall_clock(bill.settle_time, &session.timezone),
            copies: bill.copies,
        });
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// A walk-in order as the counter asks for it (ADR-0146). Without a channel it takes the store's
/// walk-in channel, `counter.walk_in_channel`: takeaway, the ordinary counter order, unless the store
/// sets its walk-ins to be eaten in (ADR-0160 decision 2).
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(super) struct OpenOrderRequest {
    #[serde(default)]
    channel: Option<Open<SalesChannel>>,
}

/// A walk-in order the counter has just opened, with the number the guest will be called by.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
struct OpenedOrderResponse {
    order_id: String,
    queue_number: u64,
    business_date: String,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/orders",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.line.add"))),
    request_body(
        content = OpenOrderRequest, description = "Optional `channel`, a `SALES_CHANNEL_*` token. Without one the order takes the \
                       store's walk-in channel. The body may be left out",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = OpenedOrderResponse, description = "Opened: the `order_id`, the day's `queue_number` it is called by, and \
                                      its `business_date`"),
        (status = 400, description = "The channel is not a sales channel this edge knows"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "orders",
))]
/// `POST /api/orders` — the counter opens a tableless order and gives it the day's next queue
/// number, as a relayed takeaway order gets one (ADR-0146).
///
/// The channel the till names wins. A till that names none gets the store's walk-in channel, which
/// is takeaway under `WALK_IN_CHANNEL_ASK` too: there the till names the guest's answer, so only a
/// till from before the setting names none, and it gets the takeaway order it always opened.
pub(super) async fn open_order<S, Q>(
    State(deps): State<Arc<CounterDeps<S, Q>>>,
    Extension(actor): Extension<Actor>,
    body: Option<Json<OpenOrderRequest>>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let request = body.map(|Json(request)| request).unwrap_or_default();
    let walk_in = deps.edge.session().counter.walk_in_channel();
    let channel = match request.channel.map(|channel| channel.require()) {
        None => walk_in.sales_channel(),
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(super) struct OrderLineRequest {
    menu_item_id: MenuItemId,
    quantity: Quantity,
    #[serde(default)]
    modifier_menu_item_ids: Vec<MenuItemId>,
    #[serde(default)]
    seat: Option<u16>,
    #[serde(default)]
    course_id: Option<CourseId>,
    /// Deprecated by `note`: a device that sends only the flag still records that a note exists.
    #[serde(default)]
    note_present: bool,
    /// The guest's note for the kitchen, held in memory and never logged (ADR-0157).
    #[serde(default)]
    note: Option<String>,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/orders/{id}/lines",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.line.add"))),
    params(
        ("id" = String, Path, description = "The order's id, a ULID"),
    ),
    request_body(
        content = OrderLineRequest, description = "What the guest chose: `menu_item_id` and `quantity` (in thousandths), with optional \
                       `modifier_menu_item_ids`, `seat`, `course_id` and `note`. No price: the edge prices \
                       the line at the order's channel",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = LineResponse, description = "The line: `order_id`, `order_line_id`, its `state`, and `station_id` \
                                      once fired"),
        (status = 400, description = "The id is not a ULID, or a note longer than 200 characters or with a \
                                      control character"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement that holds the store's lease \
                                      (`SUPERSEDED`)"),
        (status = 409, description = "Refused: the person lacks the permission, or the record is not in a \
                                      state the command applies to. `pos-error-reason` names which"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "orders",
))]
/// `POST /api/orders/{id}/lines` — add a line to an order by its id, priced by the edge.
pub(super) async fn add_order_line<S, Q>(
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
    let note = match NoteText::parse(request.note.as_deref()) {
        Ok(note) => note,
        Err(refused) => return bad_request(refused.message()),
    };
    let choice = OrderLineChoice {
        menu_item_id: request.menu_item_id,
        quantity: request.quantity,
        modifier_menu_item_ids: request.modifier_menu_item_ids,
        seat: request.seat,
        course_id: request.course_id,
        note_present: request.note_present,
    };
    match deps
        .edge
        .add_noted_line_to_order(actor, order_id, choice, note)
        .await
    {
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
        .route("/api/bills/settled", get(settled_bills::<S, Q>))
        .route("/api/orders", post(open_order::<S, Q>))
        .route("/api/orders/{id}/lines", post(add_order_line::<S, Q>))
        .with_state(Arc::new(CounterDeps { edge, queue }))
}

#[cfg(test)]
mod tests {
    use pos_core::business_date::StoreTimeZone;

    use super::wall_clock;

    /// Today's list shows the shop's clock, not the instant's own text (ADR-0164): a bill paid at
    /// half past seven in the evening in Ho Chi Minh City settled at half past twelve UTC.
    #[test]
    fn a_settled_bill_is_listed_at_the_shop_s_time() {
        let saigon = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("a real zone");
        let settled = "2026-09-30T12:30:00Z".parse().expect("an instant");
        assert_eq!(wall_clock(settled, &saigon).as_deref(), Some("19:30"));
    }
}
