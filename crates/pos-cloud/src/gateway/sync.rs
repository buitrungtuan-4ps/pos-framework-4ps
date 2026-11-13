// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /sync/stores/{store_id}/gateway/calls`: a store's external call, made by the cloud on its
//! behalf ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
//! decisions 1 and 4 to 9).
//!
//! In order: the store's credential, with the `call_gateway` scope and bound to the store; its
//! idempotency key; the endpoint, the tenant's and enabled; the values against its request fields;
//! the key's kept answer, if any (decision 6); the limits, ten calls a second per store and fifty per
//! endpoint, and the endpoint's breaker (decision 7); then the call itself
//! ([`super::call::send_within`]) within the endpoint's deadline. Every refusal before the call
//! answers at once.
//!
//! The log line of a call names its id, the endpoint's id, the outcome, the target's status and how
//! long it took. Never a value, a header or a body (decision 9).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::mint_ulid;
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::call::{
    CallOutcome, GatewayTransport, build_request, check_values, is_idempotency_key, send_within,
};
use super::calls::{GatewayCallAnswer, GatewayCallStore, GatewayOutcome, KeptCall};
use super::endpoints::{GatewayEndpoint, GatewayEndpointStore};
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::auth::rate_limit::SlidingRateLimiter;
use crate::connections::SealedFor;
use crate::http::too_many_requests;
use crate::keys::{Keys, TenantSealer};
use crate::webhook::breaker::{BreakerConfig, CircuitBreaker};

/// Calls a second one store may make (decision 7).
pub const PER_STORE_PER_SECOND: usize = 10;
/// Calls a second all of a tenant's stores may make to one endpoint (decision 7).
pub const PER_ENDPOINT_PER_SECOND: usize = 50;
/// The most endpoints whose breaker the cloud holds at once. Past it a new endpoint's calls go
/// unguarded by a breaker until one is forgotten, rather than the map growing without bound.
pub const MAX_BREAKERS: usize = 4_096;

/// A store's call.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct GatewayCallRequest {
    /// The endpoint, a ULID of the store's tenant.
    gateway_endpoint_id: String,
    /// The store's key for this act: 1 to 64 letters, digits, `-` and `_`. A key repeated within 24
    /// hours gets the first answer back.
    idempotency_key: String,
    /// The endpoint's request fields by name, each of its kind.
    #[serde(default)]
    values: Map<String, Value>,
}

/// What the route shares: the stores, the credential check, the clock, the transport, the sealing
/// key, and the limits and breakers every call of this process counts against.
pub(crate) struct GatewayCallState<E, R, K, C, T> {
    endpoints: E,
    calls: R,
    keys: K,
    clock: C,
    transport: Arc<T>,
    sealing: Option<Keys>,
    per_store: SlidingRateLimiter,
    per_endpoint: SlidingRateLimiter,
    breakers: Arc<Mutex<BTreeMap<String, CircuitBreaker>>>,
}

impl<E: Clone, R: Clone, K: Clone, C: Clone, T> Clone for GatewayCallState<E, R, K, C, T> {
    fn clone(&self) -> Self {
        Self {
            endpoints: self.endpoints.clone(),
            calls: self.calls.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
            transport: Arc::clone(&self.transport),
            sealing: self.sealing.clone(),
            per_store: self.per_store.clone(),
            per_endpoint: self.per_endpoint.clone(),
            breakers: Arc::clone(&self.breakers),
        }
    }
}

/// A gateway breaker: five failures in a row open it for a minute; it never disables an endpoint,
/// which only its tenant does.
fn breaker() -> CircuitBreaker {
    CircuitBreaker::new(BreakerConfig {
        auto_disable_after: i64::MAX,
        ..BreakerConfig::default()
    })
}

/// Builds the store-facing gateway router.
pub fn gateway_call_router<E, R, K, C, T>(
    endpoints: E,
    calls: R,
    keys: K,
    clock: C,
    transport: Arc<T>,
    sealing: Option<Keys>,
) -> Router
where
    E: GatewayEndpointStore + Clone + Send + Sync + 'static,
    R: GatewayCallStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/gateway/calls",
            post(store_gateway_call::<E, R, K, C, T>),
        )
        .with_state(GatewayCallState {
            endpoints,
            calls,
            keys,
            clock,
            transport,
            sealing,
            per_store: SlidingRateLimiter::new(PER_STORE_PER_SECOND, 1),
            per_endpoint: SlidingRateLimiter::new(PER_ENDPOINT_PER_SECOND, 1),
            breakers: Arc::new(Mutex::new(BTreeMap::new())),
        })
}

/// [`gateway_call_router`] over the cloud's outbound HTTPS client: what the binary serves.
pub fn https_gateway_call_router<E, R, K, C>(
    endpoints: E,
    calls: R,
    keys: K,
    clock: C,
    sealing: Option<Keys>,
) -> Router
where
    E: GatewayEndpointStore + Clone + Send + Sync + 'static,
    R: GatewayCallStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    gateway_call_router(
        endpoints,
        calls,
        keys,
        clock,
        Arc::new(crate::outbound::HttpsGet::new()),
        sealing,
    )
}

/// The `400` for a request field.
fn invalid(message: &str, field: &str, reason: &str) -> Response {
    api_error_with_details(ErrorStatus::InvalidArgument, message, &[(field, reason)])
}

#[utoipa::path(
    post,
    path = "/sync/stores/{store_id}/gateway/calls",
    security(("device_credential" = ["call_gateway"])),
    params(("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own")),
    request_body(
        content = GatewayCallRequest, description = "The endpoint's id, the store's `idempotency_key` for the act, and the \
                       endpoint's request fields as `values`",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = GatewayCallAnswer, description = "The call was made, or its key's first answer is replayed: \
                                      `GATEWAY_OUTCOME_ANSWERED` with the declared `result`, or \
                                      `GATEWAY_OUTCOME_REFUSED` with the `refusal` (timeout, \
                                      unreachable, the target's error status, or an answer without \
                                      the declared fields)"),
        (status = 400, description = "store_id or the endpoint id is not a ULID, the idempotency key is \
                                      malformed, or a value is missing, undeclared or of the wrong \
                                      kind, named in `details` as `values.<field>`", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `call_gateway` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no enabled endpoint with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 409, description = "The idempotency key was used for another endpoint in the last 24 \
                                      hours", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Over ten calls a second from this store, over fifty to this \
                                      endpoint, or the endpoint's breaker is open: `Retry-After` \
                                      says when to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store, the endpoint store or the call record is \
                                      unavailable, or the endpoint's secret headers cannot be opened", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "gateway",
)]
/// `POST /sync/stores/{store_id}/gateway/calls` — the cloud makes a store's external call.
pub(crate) async fn store_gateway_call<E, R, K, C, T>(
    State(state): State<GatewayCallState<E, R, K, C, T>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<GatewayCallRequest>,
) -> Response
where
    E: GatewayEndpointStore + Clone + Send + Sync + 'static,
    R: GatewayCallStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
{
    let (tenant_id, store_id, endpoint) = match admit(&state, &headers, &store_id, &request).await {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
    };
    let now = state.clock.now();
    if let Some(answered) =
        replay_or_hold(&state, tenant_id, store_id, &endpoint, &request, now).await
    {
        return answered;
    }
    call_and_keep(&state, tenant_id, store_id, &endpoint, &request, now).await
}

/// The credential, the store, the key's shape, the endpoint and the values: everything checked
/// before the record or the limits are read.
async fn admit<E, R, K, C, T>(
    state: &GatewayCallState<E, R, K, C, T>,
    headers: &HeaderMap,
    store_id: &str,
    request: &GatewayCallRequest,
) -> Result<(TenantId, StoreId, GatewayEndpoint), Response>
where
    E: GatewayEndpointStore + Clone + Send + Sync + 'static,
    R: GatewayCallStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
{
    let grant = authenticate(&state.keys, &state.clock, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    require_scope(&grant, Scope::CallGateway).map_err(IntoResponse::into_response)?;
    let [store_id, endpoint_id] = parse_ulid_fields([
        ("store_id", store_id),
        ("gateway_endpoint_id", &request.gateway_endpoint_id),
    ])?;
    let store_id = StoreId::new(store_id);
    require_store(&grant, store_id).map_err(IntoResponse::into_response)?;
    if !is_idempotency_key(&request.idempotency_key) {
        return Err(invalid(
            "an idempotency key is 1 to 64 letters, digits, - and _",
            "idempotency_key",
            "INVALID_FORMAT",
        ));
    }
    let tenant_id = grant.tenant();
    let endpoint = match state
        .endpoints
        .get(tenant_id, &endpoint_id.to_string())
        .await
    {
        Ok(Some(row)) if row.record.enabled => row.record,
        Ok(_) => {
            return Err(api_error(
                ErrorStatus::NotFound,
                "the tenant has no enabled gateway endpoint with that id",
            ));
        }
        Err(error) => {
            tracing::error!(%error, "the gateway endpoint store failed");
            return Err(unavailable());
        }
    };
    if let Err(refusal) = check_values(&endpoint, &request.values) {
        let field = format!("values.{}", refusal.field);
        return Err(invalid(
            "a value is missing, undeclared or of the wrong kind",
            &field,
            refusal.reason,
        ));
    }
    Ok((tenant_id, store_id, endpoint))
}

/// The key's kept answer, a key used for another endpoint, or a limit or an open breaker: an
/// answer without a call. `None` lets the call go.
async fn replay_or_hold<E, R, K, C, T>(
    state: &GatewayCallState<E, R, K, C, T>,
    tenant_id: TenantId,
    store_id: StoreId,
    endpoint: &GatewayEndpoint,
    request: &GatewayCallRequest,
    now: Timestamp,
) -> Option<Response>
where
    E: GatewayEndpointStore + Clone + Send + Sync + 'static,
    R: GatewayCallStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
{
    let endpoint_id = &endpoint.gateway_endpoint_id;
    match state
        .calls
        .recall(tenant_id, store_id, &request.idempotency_key, now)
        .await
    {
        Ok(Some(kept)) if &kept.gateway_endpoint_id == endpoint_id => {
            return Some((StatusCode::OK, Json(kept.answer)).into_response());
        }
        Ok(Some(_)) => {
            return Some(api_error(
                ErrorStatus::AlreadyExists,
                "that idempotency key was used for another endpoint in the last 24 hours",
            ));
        }
        Ok(None) => {}
        Err(error) => {
            tracing::error!(%error, "the gateway call record failed");
            return Some(unavailable());
        }
    }
    if let Err(retry) = state
        .per_store
        .check_and_record(&[format!("{tenant_id}/{store_id}")], now)
        .and_then(|()| {
            state
                .per_endpoint
                .check_and_record(&[format!("{tenant_id}/{endpoint_id}")], now)
        })
    {
        return Some(too_many_requests(
            "too many gateway calls: try again shortly",
            retry,
        ));
    }
    if !breaker_allows(&state.breakers, endpoint_id, now) {
        return Some(too_many_requests(
            "the endpoint keeps failing, so calls to it are paused: try again in a minute",
            60,
        ));
    }
    None
}

/// Makes the call within the endpoint's deadline, counts it against the breaker, logs it, and keeps
/// an answer the target gave for the key.
async fn call_and_keep<E, R, K, C, T>(
    state: &GatewayCallState<E, R, K, C, T>,
    tenant_id: TenantId,
    store_id: StoreId,
    endpoint: &GatewayEndpoint,
    request: &GatewayCallRequest,
    now: Timestamp,
) -> Response
where
    E: GatewayEndpointStore + Clone + Send + Sync + 'static,
    R: GatewayCallStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    T: GatewayTransport + Send + Sync + 'static,
{
    let tenant_keys = match (&state.sealing, endpoint.sealed_headers.is_empty()) {
        (Some(sealing), false) => sealing.for_tenant(tenant_id).await.ok(),
        _ => None,
    };
    let Some(secret_headers) = opened_headers(endpoint, tenant_id, tenant_keys.as_ref()) else {
        return api_error(
            ErrorStatus::Unavailable,
            "the endpoint's credentials cannot be opened: check the cloud's keys and \
             integration_secret in cloud.toml",
        );
    };
    let Some(call_id) = mint_ulid(now.as_milliseconds_since_epoch()) else {
        return unavailable();
    };
    let Ok(outbound) = build_request(
        endpoint,
        &request.values,
        &request.idempotency_key,
        secret_headers,
    ) else {
        return unavailable();
    };
    let deadline = Duration::from_millis(u64::from(endpoint.deadline_ms));
    let outcome = send_within(state.transport.as_ref(), endpoint, &outbound, deadline).await;
    let answer = answer_of(call_id.to_string(), outcome);
    let endpoint_id = &endpoint.gateway_endpoint_id;
    record_breaker(&state.breakers, endpoint_id, &answer, now);

    let took_ms = state
        .clock
        .now()
        .as_milliseconds_since_epoch()
        .saturating_sub(now.as_milliseconds_since_epoch());
    tracing::info!(
        call_id = %answer.call_id,
        gateway_endpoint_id = %endpoint_id,
        refusal = ?answer.refusal,
        status = answer.status,
        took_ms,
        "gateway call"
    );

    if answer.from_target() {
        let kept = KeptCall {
            gateway_endpoint_id: endpoint_id.clone(),
            answer: answer.clone(),
        };
        if let Err(error) = state
            .calls
            .keep(tenant_id, store_id, &request.idempotency_key, &kept, now)
            .await
        {
            // The answer still goes back: losing the record costs a retry its replay, not the call.
            tracing::error!(%error, call_id = %answer.call_id, "keeping a gateway answer failed");
        }
    }
    (StatusCode::OK, Json(answer)).into_response()
}

/// The `503` the gateway answers when one of its own stores fails.
fn unavailable() -> Response {
    api_error(ErrorStatus::Unavailable, "the gateway is unavailable")
}

/// The answer a call's outcome becomes.
fn answer_of(call_id: String, outcome: CallOutcome) -> GatewayCallAnswer {
    match outcome {
        CallOutcome::Answered { status, result } => GatewayCallAnswer {
            call_id,
            outcome: GatewayOutcome::Answered,
            refusal: None,
            status: Some(status),
            result,
        },
        CallOutcome::Refused { refusal, status } => GatewayCallAnswer {
            call_id,
            outcome: GatewayOutcome::Refused,
            refusal: Some(refusal),
            status,
            result: Map::new(),
        },
    }
}

/// The endpoint's secret headers, opened, or `None` when one is set and cannot be: no keys, or
/// not the key it was sealed under. An endpoint with none needs no key.
fn opened_headers(
    endpoint: &GatewayEndpoint,
    tenant_id: TenantId,
    tenant_keys: Option<&TenantSealer>,
) -> Option<Vec<(String, String)>> {
    if endpoint.sealed_headers.is_empty() {
        return Some(Vec::new());
    }
    let tenant_keys = tenant_keys?;
    let mut opened = Vec::with_capacity(endpoint.sealed_headers.len());
    for (name, sealed) in &endpoint.sealed_headers {
        let field_key = format!("header:{name}");
        let value = tenant_keys
            .open(
                SealedFor {
                    tenant_id,
                    connection_id: &endpoint.gateway_endpoint_id,
                    field_key: &field_key,
                },
                sealed,
            )
            .ok()?;
        opened.push((name.clone(), value.as_str().to_owned()));
    }
    Some(opened)
}

/// Whether the endpoint's breaker lets a call through. An endpoint past [`MAX_BREAKERS`] has none.
fn breaker_allows(
    breakers: &Mutex<BTreeMap<String, CircuitBreaker>>,
    endpoint_id: &str,
    now: Timestamp,
) -> bool {
    let mut breakers = breakers.lock().unwrap_or_else(PoisonError::into_inner);
    breakers
        .get_mut(endpoint_id)
        .is_none_or(|breaker| breaker.allow(now))
}

/// Counts the call against the endpoint's breaker: a failure is a call the target did not answer in
/// time or at all, or answered with a `5xx`.
fn record_breaker(
    breakers: &Mutex<BTreeMap<String, CircuitBreaker>>,
    endpoint_id: &str,
    answer: &GatewayCallAnswer,
    now: Timestamp,
) {
    use super::call::CallRefusal;

    let failed = match answer.refusal {
        Some(CallRefusal::Timeout | CallRefusal::Unreachable) => true,
        Some(CallRefusal::TargetError) => answer.status.is_some_and(|status| status >= 500),
        _ => false,
    };
    let mut breakers = breakers.lock().unwrap_or_else(PoisonError::into_inner);
    if !breakers.contains_key(endpoint_id) {
        if !failed || breakers.len() >= MAX_BREAKERS {
            return;
        }
        breakers.insert(endpoint_id.to_owned(), breaker());
    }
    if let Some(breaker) = breakers.get_mut(endpoint_id) {
        if failed {
            breaker.record_failure(now);
        } else {
            breaker.record_success();
        }
    }
}
