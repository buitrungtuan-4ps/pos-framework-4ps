// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Public order intake — `POST /v1/orders` over the `OrderIn` port
//! ([ADR-0056](../../../docs/adr/0056-public-order-intake.md)).
//!
//! The shared entry the marketplaces, the QR ordering module
//! ([ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)) and external integrators all reuse.
//! `OrderIn` is a driving port ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §5): the store's
//! edge implements it (it reprices, routes to the kitchen, and accepts offline), and this endpoint is
//! a caller. In the binary the `OrderIn` it holds is the cloud→store relay; in tests it is
//! `FakeIntake`.
//!
//! # Two things the endpoint supplies that the port does not
//!
//! The port keys idempotency on `(sales_channel, external_reference)` and carries a `store_id` but no
//! tenant. A `/v1` key is tenant-scoped ([ADR-0037](../../../docs/adr/0037-api-keys.md)), so the
//! handler binds the two through [`StoreDirectory`] before submitting — a store the caller's tenant
//! does not own is a generic `404`, no oracle. And because `pos-proto` carries no `utoipa`, the wire
//! shape is an explicit [`OrderRequest`]/[`OrderResponse`] pair rather than the domain types.

use core::future::Future;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use pos_ports::PortError;
use pos_ports::order_in::{
    ExternalReference, InboundOrder, InboundOrderLine, OrderAcceptance, OrderIn,
};
use pos_proto::determinism::ClockSource;
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{MenuItemId, StoreId, TableId, TenantId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::GuestNote;
use pos_proto::{Open, Quantity, SalesChannel, Timestamp};

use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope};

/// Maps a request's store to the tenant that owns it, so the endpoint can refuse a cross-tenant
/// submission ([ADR-0056](../../../docs/adr/0056-public-order-intake.md)).
///
/// Backed by the config tree — which already holds the Tenant→Brand→Store hierarchy — in the binary,
/// and a fake in tests.
pub trait StoreDirectory: Send + Sync {
    /// The tenant that owns `store_id`, or `None` if no store by that id is known.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the directory itself could not be read — distinct from an unknown store, which
    /// is `Ok(None)`.
    fn tenant_of(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<TenantId>, PortError>> + Send;
}

/// An amount of money on the wire: `{"currency_code": "VND", "amount_minor": 150000}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct MoneyDto {
    currency_code: String,
    amount_minor: i64,
}

/// One requested line.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct OrderLineRequest {
    menu_item_id: String,
    quantity_milli: i64,
    #[serde(default)]
    modifier_menu_item_ids: Vec<String>,
    #[serde(default)]
    quoted_unit_price: Option<MoneyDto>,
    #[serde(default)]
    note: Option<String>,
}

/// An order arriving through the public API.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct OrderRequest {
    external_reference: String,
    sales_channel: String,
    store_id: String,
    #[serde(default)]
    table_id: Option<String>,
    lines: Vec<OrderLineRequest>,
    placed_at_ms: i64,
}

/// What became of a submitted order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct OrderResponse {
    order_id: String,
    created: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_number: Option<u32>,
    total: MoneyDto,
    repriced: bool,
    awaiting_staff_confirmation: bool,
}

/// The collaborators the intake route composes: the [`OrderIn`] it relays to, the [`ApiKeyStore`] and
/// [`ClockSource`] that authenticate the bearer key, and the [`StoreDirectory`] that binds the store
/// to a tenant.
struct OrdersState<X, K, C, D> {
    intake: X,
    keys: K,
    clock: C,
    directory: D,
}

impl<X: Clone, K: Clone, C: Clone, D: Clone> Clone for OrdersState<X, K, C, D> {
    fn clone(&self) -> Self {
        Self {
            intake: self.intake.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
            directory: self.directory.clone(),
        }
    }
}

/// Builds the public order-intake sub-router: `POST /v1/orders`.
///
/// Carries its own state and is merged into the app router, so the `CloudApp` generics do not grow
/// (the same shape the device, activation, and translation sub-routers take).
pub fn orders_router<X, K, C, D>(intake: X, keys: K, clock: C, directory: D) -> Router
where
    X: OrderIn + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    D: StoreDirectory + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/v1/orders", post(submit_order::<X, K, C, D>))
        .with_state(OrdersState {
            intake,
            keys,
            clock,
            directory,
        })
}

/// `POST /v1/orders` — authenticate, bind the store to the caller's tenant, and submit.
async fn submit_order<X, K, C, D>(
    State(state): State<OrdersState<X, K, C, D>>,
    headers: HeaderMap,
    Json(request): Json<OrderRequest>,
) -> Response
where
    X: OrderIn + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    D: StoreDirectory + Clone + Send + Sync + 'static,
{
    // Identity, then authorisation, then the resource — the order the whole `/v1` surface takes.
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::PlaceOrders) {
        return forbidden.into_response();
    }

    let order = match to_inbound_order(&request) {
        Ok(order) => order,
        Err(reason) => return bad_request(reason),
    };

    // The isolation boundary: the store must belong to the key's tenant. Unknown and not-yours
    // collapse to one `404`, so a prober cannot map another tenant's stores.
    match state.directory.tenant_of(order.store_id).await {
        Ok(Some(owner)) if owner == grant.tenant() => {}
        Ok(_) => return (StatusCode::NOT_FOUND, "no such store").into_response(),
        Err(_error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "the store directory is unavailable",
            )
                .into_response();
        }
    }

    match state.intake.submit(&order).await {
        Ok(acceptance) => order_response(&acceptance),
        Err(error) => intake_error(&error),
    }
}

/// Maps the wire request to an [`InboundOrder`], or the reason its first malformed field is a `400`.
///
/// The error is a `&'static str` rather than a built [`Response`], so a bad field does not carry a
/// kilobyte-sized `Err` variant through every call (`clippy::result_large_err`); the handler turns the
/// reason into the response.
fn to_inbound_order(request: &OrderRequest) -> Result<InboundOrder, &'static str> {
    let external_reference = ExternalReference::parse(&request.external_reference)
        .map_err(|_ignored| "external_reference must be non-empty and at most 128 bytes")?;
    let store_id = request
        .store_id
        .parse::<StoreId>()
        .map_err(|_ignored| "store_id must be a ULID")?;
    let table_id = match &request.table_id {
        Some(text) => Some(
            text.parse::<TableId>()
                .map_err(|_ignored| "table_id must be a ULID")?,
        ),
        None => None,
    };
    let placed_at = Timestamp::from_milliseconds_since_epoch(request.placed_at_ms)
        .map_err(|_ignored| "placed_at_ms is out of range")?;

    let mut lines = Vec::with_capacity(request.lines.len());
    for line in &request.lines {
        lines.push(to_inbound_line(line)?);
    }

    Ok(InboundOrder {
        external_reference,
        // The wire token is tolerated as an open enum: an unrecognised channel deserialises rather
        // than failing, exactly as the envelope does elsewhere.
        sales_channel: Open::<SalesChannel>::parse(&request.sales_channel),
        store_id,
        table_id,
        // The buyer side-table id is not part of the public submission shape yet; a corporate buyer
        // (P10) attaches it downstream.
        subject_id: None,
        lines,
        placed_at,
    })
}

/// Maps one wire line to an [`InboundOrderLine`], or the reason it is a `400`.
fn to_inbound_line(line: &OrderLineRequest) -> Result<InboundOrderLine, &'static str> {
    let menu_item_id = line
        .menu_item_id
        .parse::<MenuItemId>()
        .map_err(|_ignored| "menu_item_id must be a ULID")?;
    let mut modifier_menu_item_ids = Vec::with_capacity(line.modifier_menu_item_ids.len());
    for modifier in &line.modifier_menu_item_ids {
        modifier_menu_item_ids.push(
            modifier
                .parse::<MenuItemId>()
                .map_err(|_ignored| "a modifier id must be a ULID")?,
        );
    }
    let quoted_unit_price = match &line.quoted_unit_price {
        Some(money) => Some(to_money(money)?),
        None => None,
    };
    Ok(InboundOrderLine {
        menu_item_id,
        quantity: Quantity::from_milli(line.quantity_milli),
        modifier_menu_item_ids,
        quoted_unit_price,
        // Free text stays at the store and never enters the event log — that is what `GuestNote` is.
        note: line.note.clone().map(GuestNote::new),
    })
}

/// Maps a wire money value to [`Money`], or the reason a malformed currency code is a `400`.
fn to_money(money: &MoneyDto) -> Result<Money, &'static str> {
    let currency_code = CurrencyCode::parse(&money.currency_code)
        .map_err(|_ignored| "currency_code must be three uppercase letters")?;
    Ok(Money {
        currency_code,
        amount_minor: money.amount_minor,
    })
}

/// Renders an [`OrderAcceptance`]: `201` when this call created the order, `200` for an idempotent
/// repeat.
fn order_response(acceptance: &OrderAcceptance) -> Response {
    let status = if acceptance.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let body = OrderResponse {
        order_id: acceptance.order_id.to_string(),
        created: acceptance.created,
        queue_number: acceptance.queue_number,
        total: MoneyDto {
            currency_code: acceptance.total.currency_code.as_str().to_owned(),
            amount_minor: acceptance.total.amount_minor,
        },
        repriced: acceptance.repriced,
        awaiting_staff_confirmation: acceptance.awaiting_staff_confirmation,
    };
    (status, Json(body)).into_response()
}

/// Maps a submission [`PortError`] to the status the endpoint answers with.
fn intake_error(error: &PortError) -> Response {
    let status = match error.status() {
        ErrorStatus::InvalidArgument => StatusCode::BAD_REQUEST,
        ErrorStatus::FailedPrecondition | ErrorStatus::AlreadyExists => StatusCode::CONFLICT,
        ErrorStatus::ResourceExhausted => StatusCode::TOO_MANY_REQUESTS,
        ErrorStatus::NotFound => StatusCode::NOT_FOUND,
        ErrorStatus::PermissionDenied => StatusCode::FORBIDDEN,
        ErrorStatus::Unauthenticated => StatusCode::UNAUTHORIZED,
        ErrorStatus::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorStatus::Internal | ErrorStatus::Unspecified => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string()).into_response()
}

/// A `400` carrying a one-line reason.
fn bad_request(reason: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, reason).into_response()
}
