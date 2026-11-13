// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/gateway/calls`: a till's lookup, made by the cloud on the store's behalf
//! ([ADR-0214](../../../../docs/adr/0214-a-tills-external-lookup-is-one-catalogue-act.md)
//! decision 3, [ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)).
//!
//! The person needs `integrations.lookup.run`, which no role holds until a tenant grants it. The
//! edge names the act's `idempotency_key` when the till sends none, passes the endpoint and the
//! values to the cloud's `POST /sync/stores/{store_id}/gateway/calls`, and waits for the endpoint's
//! longest deadline plus a second. It answers the declared result, or one refusal the till
//! translates:
//!
//! - `GATEWAY_OFFLINE` (`503`), at once, when the box has no link to the cloud or the cloud cannot
//!   be reached or does not take the call;
//! - `GATEWAY_TIMEOUT` (`504`), when the target or the cloud did not answer in time;
//! - `GATEWAY_REFUSED` (`502`), when the target answered with its own error, an answer without the
//!   declared fields, or could not be reached by the cloud;
//! - `GATEWAY_BUSY` (`429`), when a limit or the endpoint's breaker held the call back.
//!
//! A lookup writes no event and moves no money, and nothing it carries is kept or logged: the log
//! line names the endpoint, the outcome and the cloud's status, never a value or a result.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;

use crate::app::Edge;
use crate::cloud_http::CloudHttpError;
use crate::http::{ERROR_REASON_HEADER, bad_request, parse_ulid};

/// How long the edge waits for the cloud: the longest deadline an endpoint may declare, ten
/// seconds, plus one (ADR-0198 decision 5, ADR-0214 decision 3).
pub const GATEWAY_WAIT: Duration = Duration::from_secs(11);

/// The most values one call carries: an endpoint declares at most 32 request fields.
const MAX_VALUES: usize = 32;

/// One call's answer from the cloud: its status and its body.
pub type GatewayFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), CloudHttpError>> + Send + 'a>>;

/// The edge's half of the cloud gateway: sends one call's JSON body to the store's own gateway
/// route. The box's link implements it over HTTPS; a test, over a fake.
pub trait GatewayCalls: Send + Sync {
    /// Sends `body` and returns the cloud's status and body, or why the cloud was not reached.
    fn call(&self, body: Vec<u8>) -> GatewayFuture<'_>;
}

/// The gateway link a route reaches through, layered on the application once the box has a link to
/// its cloud. A box with none answers every lookup `GATEWAY_OFFLINE`.
#[derive(Clone)]
pub struct Gateway {
    calls: Arc<dyn GatewayCalls>,
    wait: Duration,
}

impl core::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Gateway")
            .field("wait", &self.wait)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// A link over `calls`, waiting [`GATEWAY_WAIT`] for each answer.
    #[must_use]
    pub fn new(calls: Arc<dyn GatewayCalls>) -> Self {
        Self {
            calls,
            wait: GATEWAY_WAIT,
        }
    }

    /// The same link, waiting `wait` instead: for a test that cannot wait eleven seconds.
    #[must_use]
    pub const fn waiting(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }
}

/// A till's lookup.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct LookupRequest {
    /// The endpoint, a ULID the package names.
    gateway_endpoint_id: String,
    /// The act's key, 1 to 64 letters, digits, `-` and `_`: sent again on a retry of the same act,
    /// which gets the first answer back. Absent, the edge names one.
    #[serde(default)]
    idempotency_key: Option<String>,
    /// The endpoint's request fields by name, each of its declared kind.
    #[serde(default)]
    values: Map<String, Value>,
}

/// A lookup's result, drawn by the till and never kept.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct LookupResponse {
    /// The cloud's id for the call, a ULID: what its log names it by.
    call_id: String,
    /// The key the call was made under, the till's or the one the edge named.
    idempotency_key: String,
    /// The endpoint's declared response fields, by name.
    result: Map<String, Value>,
}

/// The cloud's answer, as far as the edge reads it.
#[derive(Deserialize)]
struct CloudAnswer {
    call_id: String,
    outcome: String,
    #[serde(default)]
    refusal: Option<String>,
    #[serde(default)]
    result: Map<String, Value>,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/gateway/calls",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("integrations.lookup.run"))),
    request_body(content = LookupRequest, content_type = "application/json",
        description = "The endpoint, the act's key on a retry, and the endpoint's request fields as `values`. \
                       No value is logged"),
    responses(
        (status = 200, body = LookupResponse, description = "The endpoint's declared result, the call's id and its key. \
                                      Nothing is kept and no event is written"),
        (status = 400, description = "The endpoint id is not a ULID, the key is malformed, there are more than \
                                      32 values, or a value is missing, undeclared or of the wrong \
                                      kind (`INVALID_ARGUMENT`)"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in, or the person's role lacks \
                                      integrations.lookup.run (`PERMISSION_DENIED`)"),
        (status = 404, description = "The tenant has no enabled endpoint with that id \
                                      (`UNKNOWN_GATEWAY_ENDPOINT`)"),
        (status = 409, description = "The key was used for another endpoint in the last 24 hours \
                                      (`INVALID_ARGUMENT`)"),
        (status = 429, description = "A limit or the endpoint's breaker held the call back (`GATEWAY_BUSY`)"),
        (status = 502, description = "The target answered with its own error or without the declared \
                                      fields, or the cloud could not reach it (`GATEWAY_REFUSED`)"),
        (status = 503, description = "The cloud cannot be reached or did not take the call (`GATEWAY_OFFLINE`)"),
        (status = 504, description = "No answer within the endpoint's deadline (`GATEWAY_TIMEOUT`)"),
    ),
    tag = "integrations",
))]
/// `POST /api/gateway/calls` — runs one lookup through the cloud gateway.
pub(crate) async fn gateway_call<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    gateway: Option<Extension<Gateway>>,
    request: Result<Json<LookupRequest>, JsonRejection>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    if !edge
        .session()
        .staff
        .grants(actor.employee_id, Permission::RunLookup)
    {
        return refusal(
            StatusCode::FORBIDDEN,
            "PERMISSION_DENIED",
            "a lookup needs integrations.lookup.run",
        );
    }
    let Ok(Json(request)) = request else {
        return bad_request("the body is not a lookup");
    };
    if parse_ulid(&request.gateway_endpoint_id).is_none() {
        return bad_request("gateway_endpoint_id is not a ULID");
    }
    if request.values.len() > MAX_VALUES {
        return bad_request("a lookup carries at most 32 values");
    }
    let idempotency_key = match request.idempotency_key {
        Some(key) if is_idempotency_key(&key) => key,
        Some(_) => return bad_request("idempotency_key is 1 to 64 letters, digits, - and _"),
        None => edge.gateway_idempotency_key(),
    };
    let Some(Extension(gateway)) = gateway else {
        return offline("this box has no link to its cloud");
    };
    let body = serde_json::json!({
        "gateway_endpoint_id": request.gateway_endpoint_id,
        "idempotency_key": idempotency_key,
        "values": request.values,
    });
    let Ok(body) = serde_json::to_vec(&body) else {
        return offline("the call could not be written");
    };
    let answer = tokio::time::timeout(gateway.wait, gateway.calls.call(body)).await;
    let endpoint = request.gateway_endpoint_id.as_str();
    match answer {
        Err(_elapsed) => {
            tracing::info!(
                gateway_endpoint_id = endpoint,
                "a lookup timed out at the edge"
            );
            refusal(
                StatusCode::GATEWAY_TIMEOUT,
                "GATEWAY_TIMEOUT",
                "the lookup had no answer in time",
            )
        }
        Ok(Err(error)) => {
            tracing::warn!(gateway_endpoint_id = endpoint, %error, "a lookup could not reach the cloud");
            offline("the cloud cannot be reached")
        }
        Ok(Ok((status, body))) => answered(endpoint, idempotency_key, status, &body),
    }
}

/// The till's answer for the cloud's.
fn answered(endpoint: &str, idempotency_key: String, status: u16, body: &[u8]) -> Response {
    tracing::info!(
        gateway_endpoint_id = endpoint,
        status,
        "a lookup was answered by the cloud"
    );
    match status {
        200 => {}
        400 | 409 => {
            return refusal(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
                "INVALID_ARGUMENT",
                "the endpoint does not take these values or this key",
            );
        }
        404 => {
            return refusal(
                StatusCode::NOT_FOUND,
                "UNKNOWN_GATEWAY_ENDPOINT",
                "the tenant has no enabled endpoint with that id",
            );
        }
        429 => return busy(),
        _ => return offline("the cloud did not take the call"),
    }
    let Ok(answer) = serde_json::from_slice::<CloudAnswer>(body) else {
        return offline("the cloud's answer could not be read");
    };
    if answer.outcome == "GATEWAY_OUTCOME_ANSWERED" {
        return Json(LookupResponse {
            call_id: answer.call_id,
            idempotency_key,
            result: answer.result,
        })
        .into_response();
    }
    match answer.refusal.as_deref() {
        Some("GATEWAY_REFUSAL_TIMEOUT") => refusal(
            StatusCode::GATEWAY_TIMEOUT,
            "GATEWAY_TIMEOUT",
            "the target did not answer in time",
        ),
        Some("GATEWAY_REFUSAL_INVALID_VALUES") => refusal(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "the endpoint does not take these values",
        ),
        _ => refusal(
            StatusCode::BAD_GATEWAY,
            "GATEWAY_REFUSED",
            "the target refused the lookup or could not be reached",
        ),
    }
}

/// Whether `key` is 1 to 64 of `[A-Za-z0-9_-]`, the cloud's rule.
fn is_idempotency_key(key: &str) -> bool {
    (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn offline(message: &'static str) -> Response {
    refusal(StatusCode::SERVICE_UNAVAILABLE, "GATEWAY_OFFLINE", message)
}

fn busy() -> Response {
    refusal(
        StatusCode::TOO_MANY_REQUESTS,
        "GATEWAY_BUSY",
        "the endpoint is busy; try again shortly",
    )
}

fn refusal(status: StatusCode, reason: &'static str, message: &'static str) -> Response {
    (
        status,
        [(ERROR_REASON_HEADER, HeaderValue::from_static(reason))],
        message,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::is_idempotency_key;

    #[test]
    fn a_key_follows_the_clouds_rule() {
        assert!(is_idempotency_key("01J9Z3-retry_2"));
        assert!(is_idempotency_key(&"a".repeat(64)));
        assert!(!is_idempotency_key(""));
        assert!(!is_idempotency_key(&"a".repeat(65)));
        assert!(!is_idempotency_key("has space"));
        assert!(!is_idempotency_key("ký"));
    }
}
