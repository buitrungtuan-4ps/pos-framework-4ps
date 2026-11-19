// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/orders/{id}/weigh`, the act `sales.line.weigh`, and `GET /api/scale`
//! ([ADR-0223](../../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md) decision 2):
//! a weighed item's line from the scale beside the till or a weight typed by hand, and what the
//! scale says now. The edge reads the scale and prices the line; the till names the item and the
//! source, and for a typed weight its grams.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_ports::scale::ScaleReading;
use pos_proto::ids::{MenuItemId, OrderId};
use pos_proto::weighing::{ScaleState, WeightSource};
use pos_proto::wire_enum::Open;

use crate::app::WeighFrom;
use crate::http::counter::CounterDeps;
use crate::http::lines::LineResponse;
use crate::http::{bad_request, error_response, parse_ulid};
use crate::queue::QueueNumberAuthority;

/// What to weigh, and from where.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(super) struct WeighRequest {
    /// The weighed item, a ULID.
    menu_item_id: String,
    /// `WEIGHT_SOURCE_SCALE` or `WEIGHT_SOURCE_MANUAL`; a label is scanned instead.
    source: Open<WeightSource>,
    /// For a typed weight, its net grams.
    #[serde(default)]
    net_grams: Option<u32>,
}

/// What the scale says now.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(super) struct ScaleResponse {
    /// Settled, moving, over its capacity or below zero.
    state: Open<ScaleState>,
    /// The settled net weight in grams, before the item's tare; absent unless settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    net_grams: Option<u32>,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/orders/{id}/weigh",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("sales.line.add"))),
    params(("id" = String, Path, description = "The order's id, a ULID")),
    request_body(
        content = WeighRequest, description = "The weighed item, and the scale or a typed weight",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = LineResponse, description = "The weighed line: its net grams at the item's price per \
                                      kilogram, rounded once, its weight's source recorded"),
        (status = 400, description = "An id is not a ULID, the source is not the scale or a hand, or a typed \
                                      weight names no grams"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "Refused: the item is sold by the piece (`NOT_SOLD_BY_WEIGHT`), a typed weight \
                                      where the store takes none (`MANUAL_WEIGHT_NOT_ALLOWED`), a scale that has \
                                      not settled (`SCALE_NOT_SETTLED`) or does not answer (`SCALE_ABSENT`), a \
                                      net weight out of bounds (`WEIGHT_BELOW_MINIMUM`, `WEIGHT_ABOVE_MAXIMUM`), \
                                      or what adding a line refuses"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
        (status = 500, description = "The edge could not apply the command"),
    ),
    tag = "orders",
))]
/// `POST /api/orders/{id}/weigh` — add a weighed item's line.
pub(super) async fn weigh_line<S, Q>(
    State(deps): State<Arc<CounterDeps<S, Q>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<WeighRequest>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let Some(order_id) = parse_ulid(&id).map(OrderId::new) else {
        return bad_request("an order id is a ULID");
    };
    let Some(menu_item_id) = parse_ulid(&request.menu_item_id).map(MenuItemId::new) else {
        return bad_request("a menu item id is a ULID");
    };
    let from = match (request.source.known(), request.net_grams) {
        (WeightSource::Scale, _) => WeighFrom::Scale,
        (WeightSource::Manual, Some(net_grams)) => WeighFrom::Manual { net_grams },
        (WeightSource::Manual, None) => return bad_request("a typed weight names its net_grams"),
        _ => return bad_request("a weight comes from the scale or a hand; a label is scanned"),
    };
    match deps
        .edge
        .add_weighed_line(actor, order_id, menu_item_id, from)
        .await
    {
        Ok(view) => Json(LineResponse::from(view)).into_response(),
        Err(error) => error_response(&error),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/scale",
    security(("till_session" = [])),
    responses(
        (status = 200, body = ScaleResponse, description = "What the scale beside the till says now"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "No scale answers (`SCALE_ABSENT`)"),
    ),
    tag = "orders",
))]
/// `GET /api/scale` — what the scale says now.
pub(super) async fn read_scale<S, Q>(State(deps): State<Arc<CounterDeps<S, Q>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
    Q: QueueNumberAuthority + 'static,
{
    let reading = match deps.edge.read_scale().await {
        Ok(reading) => reading,
        Err(error) => return error_response(&error),
    };
    let (state, net_grams) = match reading {
        ScaleReading::Stable { net_grams } => (ScaleState::Stable, Some(net_grams)),
        ScaleReading::Unstable => (ScaleState::Unstable, None),
        ScaleReading::Overloaded => (ScaleState::Overloaded, None),
        ScaleReading::UnderZero => (ScaleState::UnderZero, None),
    };
    Json(ScaleResponse {
        state: Open::from_known(state),
        net_grams,
    })
    .into_response()
}
