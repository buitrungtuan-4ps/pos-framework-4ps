// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store's answer to a delivery vendor, through the cloud
//! ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 4).
//!
//! `POST /sync/stores/{store_id}/delivery/answers`, with a credential bound to the store that holds
//! `relay_orders`, names the vendor (the opaque `vendor_id`, the tenant's delivery connection), the
//! order's `external_reference` as the store received it, and one generic answer: accept with a
//! preparation time, reject with a reason code, ready, or busy. The cloud finds the connection,
//! which must be enabled and serve this store, asks its connector for the vendor's request, signed
//! under the connection's sealed `webhook_secret`, and sends it to the connection's `answer_url`
//! over the outbound client behind the webhook guard, within [`ANSWER_DEADLINE`], with an
//! `Idempotency-Key` that is the same for the same answer. No vendor is special: a vendor whose
//! protocol these answers cannot express needs a new ADR. Nothing personal is sent or logged.

use core::future::Future;
use core::time::Duration;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pos_cloud_api::response::{api_error, parse_ulid_fields};
use pos_ports::PortError;
use pos_proto::ErrorStatus;
use pos_proto::SalesChannel;
use pos_proto::WireEnum as _;
use pos_proto::ids::{OrderId, StoreId, TenantId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::OrderConnector;
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::connections::{ConnectionStore, ScopeLevel, SealedFor};
use crate::gateway::call::{GatewayTransport, OutboundRequest};
use crate::gateway::endpoints::GatewayMethod;
use crate::keys::Keys;

/// How long the cloud waits for the vendor.
pub const ANSWER_DEADLINE: Duration = Duration::from_secs(5);

/// The connection setting a connector sends its answers to.
pub const ANSWER_URL: &str = "answer_url";

/// What a store tells a vendor. Generic: the same for every vendor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum AnswerKind {
    /// The store takes the order and will have it ready in `prep_minutes`.
    #[serde(rename = "DELIVERY_ANSWER_ACCEPT")]
    Accept,
    /// The store will not make it, for `reason_code_id`.
    #[serde(rename = "DELIVERY_ANSWER_REJECT")]
    Reject,
    /// The order is ready for the rider.
    #[serde(rename = "DELIVERY_ANSWER_READY")]
    Ready,
    /// The store is busy: new orders should expect `prep_minutes`, or none when it is `0`.
    #[serde(rename = "DELIVERY_ANSWER_BUSY")]
    Busy,
}

/// One answer, as a connector turns it into the vendor's request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryAnswer {
    /// Which answer.
    pub kind: AnswerKind,
    /// Minutes to ready, for accept and busy.
    pub prep_minutes: Option<u16>,
    /// The reason code, for reject.
    pub reason_code_id: Option<String>,
}

/// The request a connector builds for its vendor: headers after `Host`, and a body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerCall {
    /// The vendor's own headers, such as its signature.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: Vec<u8>,
}

/// A store's answer, as it posts it.
#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct AnswerRequest {
    /// The vendor, the opaque id the order carried (`vendor_id`).
    pub vendor_id: String,
    /// The order's reference as the store received it. A store that names the order by
    /// `order_id` instead leaves it out; absent for busy.
    #[serde(default)]
    pub external_reference: Option<String>,
    /// The id the store gave the order, a ULID: the cloud finds the vendor's reference from the
    /// acceptance the store reported for it. Used when `external_reference` is absent.
    #[serde(default)]
    pub order_id: Option<String>,
    /// The answer.
    pub answer: AnswerKind,
    /// Minutes to ready: 1 to 240 for accept, 0 to 240 for busy.
    #[serde(default)]
    pub prep_minutes: Option<u16>,
    /// The reason code a reject cites.
    #[serde(default)]
    pub reason_code_id: Option<String>,
}

/// The orders a store took, by the id it gave each: where a store that names an order by its own id
/// has its vendor's reference looked up.
pub trait DeliveryOrders: Send + Sync {
    /// The reference `store_id` accepted as `order_id` on the delivery channel, or `None`.
    fn reference_of(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        order_id: OrderId,
    ) -> impl Future<Output = Result<Option<String>, PortError>> + Send;
}

impl DeliveryOrders for store_postgres::PostgresOrderQueue {
    async fn reference_of(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        order_id: OrderId,
    ) -> Result<Option<String>, PortError> {
        self.reference_of_order(
            &tenant_id.to_string(),
            &store_id.to_string(),
            SalesChannel::Delivery.as_wire(),
            &order_id.to_string(),
        )
        .await
    }
}

/// What the route shares.
pub(crate) struct AnswerState<S, K, C, T, O> {
    orders: O,
    connections: S,
    keys: K,
    clock: C,
    transport: Arc<T>,
    connectors: Arc<[Arc<dyn OrderConnector>]>,
    sealing: Option<Keys>,
}

impl<S: Clone, K: Clone, C: Clone, T, O: Clone> Clone for AnswerState<S, K, C, T, O> {
    fn clone(&self) -> Self {
        Self {
            orders: self.orders.clone(),
            connections: self.connections.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
            transport: Arc::clone(&self.transport),
            connectors: Arc::clone(&self.connectors),
            sealing: self.sealing.clone(),
        }
    }
}

/// The store-facing answers route, finding an order a store names by its own id in `orders`.
pub fn delivery_answer_router<S, K, C, T, O>(
    (connections, orders): (S, O),
    keys: K,
    clock: C,
    transport: Arc<T>,
    connectors: Vec<Arc<dyn OrderConnector>>,
    sealing: Option<Keys>,
) -> Router
where
    S: ConnectionStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: pos_proto::ClockSource + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
    O: DeliveryOrders + Clone + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/delivery/answers",
            post(store_answer::<S, K, C, T, O>),
        )
        .with_state(AnswerState {
            orders,
            connections,
            keys,
            clock,
            transport,
            connectors: connectors.into(),
            sealing,
        })
}

/// [`delivery_answer_router`] over the outbound HTTPS client, which vets the URL before every
/// attempt and dials only what it vetted.
pub fn https_delivery_answer_router<S, K, C, O>(
    (connections, orders): (S, O),
    keys: K,
    clock: C,
    connectors: Vec<Arc<dyn OrderConnector>>,
    sealing: Option<Keys>,
) -> Router
where
    S: ConnectionStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: pos_proto::ClockSource + Clone + Send + Sync + 'static,
    O: DeliveryOrders + Clone + 'static,
{
    delivery_answer_router(
        (connections, orders),
        keys,
        clock,
        Arc::new(crate::outbound::HttpsGet::new()),
        connectors,
        sealing,
    )
}

/// Checks an answer's own shape; `Err` names the field at fault.
fn checked(request: &AnswerRequest) -> Result<DeliveryAnswer, &'static str> {
    let minutes = request.prep_minutes;
    let needs_reference = request.answer != AnswerKind::Busy;
    if needs_reference
        && request
            .external_reference
            .as_deref()
            .is_none_or(str::is_empty)
        && request.order_id.is_none()
    {
        return Err("external_reference");
    }
    let fits = match request.answer {
        AnswerKind::Accept => minutes.is_some_and(|minutes| (1..=240).contains(&minutes)),
        AnswerKind::Busy => minutes.is_some_and(|minutes| minutes <= 240),
        AnswerKind::Reject => request.reason_code_id.is_some(),
        AnswerKind::Ready => true,
    };
    if !fits {
        return Err(if request.answer == AnswerKind::Reject {
            "reason_code_id"
        } else {
            "prep_minutes"
        });
    }
    Ok(DeliveryAnswer {
        kind: request.answer,
        prep_minutes: minutes,
        reason_code_id: request.reason_code_id.clone(),
    })
}

/// The same key for the same answer about the same order, so a store's retry is one answer.
fn idempotency_key(connection_id: &str, reference: &str, answer: &DeliveryAnswer) -> String {
    let mut hash = Sha256::new();
    for part in [connection_id, reference, &format!("{answer:?}")] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    crate::archive::to_hex(&hash.finalize())
}

fn refused(status: ErrorStatus, message: &str) -> Response {
    api_error(status, message)
}

/// `POST /sync/stores/{store_id}/delivery/answers` — a store's answer, sent on to its vendor.
async fn store_answer<S, K, C, T, O>(
    State(state): State<AnswerState<S, K, C, T, O>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<AnswerRequest>,
) -> Response
where
    S: ConnectionStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: pos_proto::ClockSource + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
    O: DeliveryOrders + Clone + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::RelayOrders) {
        return forbidden.into_response();
    }
    let (store_id, connection_id) =
        match parse_ulid_fields([("store_id", &store_id), ("vendor_id", &request.vendor_id)]) {
            Ok([store, vendor]) => (StoreId::new(store), vendor.to_string()),
            Err(refusal) => return refusal,
        };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let answer = match checked(&request) {
        Ok(answer) => answer,
        Err(field) => {
            return pos_cloud_api::response::api_error_with_details(
                ErrorStatus::InvalidArgument,
                "the answer is not one a vendor can be told",
                &[(field, "INVALID")],
            );
        }
    };
    let held = match held_reference(&state.orders, grant.tenant(), store_id, &request).await {
        Ok(held) => held,
        Err(refusal) => return refusal,
    };
    send(
        &state,
        (grant.tenant(), store_id),
        &connection_id,
        held.as_deref(),
        &answer,
    )
    .await
}

/// The order's reference as the store holds it: the one it sent, or the one it accepted as the
/// order it names by `order_id`. `404` for an order the store reported no acceptance of.
async fn held_reference<O: DeliveryOrders>(
    orders: &O,
    tenant_id: TenantId,
    store_id: StoreId,
    request: &AnswerRequest,
) -> Result<Option<String>, Response> {
    if let Some(reference) = request
        .external_reference
        .clone()
        .filter(|reference| !reference.is_empty())
    {
        return Ok(Some(reference));
    }
    let Some(order_id) = request.order_id.as_deref() else {
        return Ok(None);
    };
    let order_id = match parse_ulid_fields([("order_id", order_id)]) {
        Ok([order_id]) => OrderId::new(order_id),
        Err(refusal) => return Err(refusal),
    };
    match orders.reference_of(tenant_id, store_id, order_id).await {
        Ok(Some(reference)) => Ok(Some(reference)),
        Ok(None) => Err(refused(
            ErrorStatus::NotFound,
            "this store took no such order",
        )),
        Err(error) => {
            tracing::error!(%store_id, status = ?error.status(), "the order queue failed");
            Err(refused(
                ErrorStatus::Unavailable,
                "the vendor cannot be reached now",
            ))
        }
    }
}

/// The connection's webhook secret, opened under its tenant's keys, or `None`.
async fn opened_secret(
    sealing: Option<&Keys>,
    connection: &crate::connections::Connection,
    tenant_id: TenantId,
    connection_id: &str,
) -> Option<zeroize::Zeroizing<String>> {
    let tenant_keys = sealing?.for_tenant(tenant_id).await.ok()?;
    let sealed = connection
        .sealed_secrets
        .get(super::route::WEBHOOK_SECRET)?;
    let bound = SealedFor {
        tenant_id,
        connection_id,
        field_key: super::route::WEBHOOK_SECRET,
    };
    tenant_keys.open(bound, sealed).ok()
}

async fn send<S, K, C, T, O>(
    state: &AnswerState<S, K, C, T, O>,
    (tenant_id, store_id): (TenantId, StoreId),
    connection_id: &str,
    held: Option<&str>,
    answer: &DeliveryAnswer,
) -> Response
where
    S: ConnectionStore + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
{
    let unknown = || refused(ErrorStatus::NotFound, "this store has no such vendor");
    let connection = match state.connections.get(tenant_id, connection_id).await {
        Ok(Some(row)) => row.record,
        Ok(None) => return unknown(),
        Err(error) => {
            tracing::error!(%error, "the connection store failed");
            return refused(ErrorStatus::Unavailable, "the vendor cannot be reached now");
        }
    };
    let serves = connection.enabled
        && connection.scope_level == ScopeLevel::Store
        && connection.scope_id.as_deref() == Some(store_id.to_string().as_str());
    let connector = state
        .connectors
        .iter()
        .find(|connector| connector.provider_id() == connection.provider_id);
    let url = match connection.settings.get(ANSWER_URL) {
        Some(pos_providers::SettingValue::Text(url)) => Some(url.clone()),
        _ => None,
    };
    let (true, Some(connector), Some(url)) = (serves, connector, url) else {
        return unknown();
    };
    // The vendor's own reference: the store holds it prefixed by the connection.
    let reference = held
        .map(|held| {
            held.strip_prefix(&format!("{connection_id}:"))
                .unwrap_or(held)
        })
        .unwrap_or_default();
    let secret = opened_secret(
        state.sealing.as_ref(),
        &connection,
        tenant_id,
        connection_id,
    )
    .await;
    let Some(secret) = secret else {
        tracing::error!(%connection_id, "a vendor connection's secret cannot be opened");
        return refused(ErrorStatus::Unavailable, "the vendor cannot be reached now");
    };
    let Some(call) = connector.answer(reference, answer, secret.as_str()) else {
        return refused(
            ErrorStatus::FailedPrecondition,
            "this vendor takes no answers",
        );
    };
    let mut headers = call.headers;
    headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
    headers.push((
        "Idempotency-Key".to_owned(),
        idempotency_key(connection_id, reference, answer),
    ));
    let outbound = OutboundRequest {
        method: GatewayMethod::Post,
        url,
        headers,
        body: Some(call.body),
    };
    let sent = tokio::time::timeout(ANSWER_DEADLINE, state.transport.send(&outbound)).await;
    let outcome = match &sent {
        Ok(Ok(answered)) if (200..300).contains(&answered.status) => "delivered",
        Ok(Ok(_)) => "refused",
        Ok(Err(_)) => "unreachable",
        Err(_) => "timeout",
    };
    tracing::info!(%connection_id, answer = ?answer.kind, outcome, "answered a delivery vendor");
    match outcome {
        "delivered" => StatusCode::NO_CONTENT.into_response(),
        "timeout" => refused(
            ErrorStatus::Unavailable,
            "the vendor did not answer in time",
        ),
        _ => api_error(
            ErrorStatus::Unavailable,
            "the vendor did not take the answer",
        ),
    }
}
