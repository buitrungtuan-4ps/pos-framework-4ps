// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /sync/stores/{store_id}/gateway/calls` (ADR-0198): a store's external call, made by the
//! cloud. The target is a fake transport, so the route's own rules are what is proved: the scope and
//! the store, the endpoint and the values, the kept answer for a repeated key, the limits and the
//! breaker, and that the target sees the opened secret header and the store's idempotency key.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pos_cloud::auth::apikey::Scope;
use pos_cloud::connections::{ConnectionSecret, SealedFor, seal};
use pos_cloud::gateway::call::{GatewayTransport, OutboundRequest, TargetAnswer};
use pos_cloud::gateway::calls::{GatewayCallStore, GatewayCallStoreError, KEPT_FOR_MS, KeptCall};
use pos_cloud::gateway::endpoints::{
    GatewayEndpoint, GatewayEndpointStore, GatewayField, GatewayFieldKind, GatewayMethod,
};
use pos_cloud::gateway::sync::gateway_call_router;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::gateway_endpoints::FakeEndpoints;
use crate::{FakeKeys, clock, issue_store_key, json_body, post_json_bearer, store_id, tenant};

const ENDPOINT: &str = "01J00000000000000000000G01";
const DISABLED: &str = "01J00000000000000000000G02";
const PLAINTEXT: &str = "demo-key-do-not-leak";

fn key() -> ConnectionSecret {
    ConnectionSecret::new("ef".repeat(32))
}

/// Answers from a script and records every request it was sent.
/// One request as the target saw it: its URL, its headers and its body.
type Sent = (String, Vec<(String, String)>, Option<Vec<u8>>);

#[derive(Default)]
struct FakeTarget {
    answers: Mutex<Vec<TargetAnswer>>,
    sent: Mutex<Vec<Sent>>,
}

impl FakeTarget {
    fn answering(answers: Vec<TargetAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers),
            sent: Mutex::new(Vec::new()),
        })
    }

    fn sent(&self) -> usize {
        self.sent.lock().expect("lock").len()
    }
}

impl GatewayTransport for FakeTarget {
    async fn send(&self, request: &OutboundRequest) -> Result<TargetAnswer, String> {
        self.sent.lock().expect("lock").push((
            request.url.clone(),
            request.headers.clone(),
            request.body.clone(),
        ));
        let mut answers = self.answers.lock().expect("lock");
        if answers.is_empty() {
            Err("nothing scripted".to_owned())
        } else {
            Ok(answers.remove(0))
        }
    }
}

/// The idempotency record in memory, keyed and aged as the real one.
/// A kept call by its key, with when it was kept.
type Kept = BTreeMap<(TenantId, StoreId, String), (i64, KeptCall)>;

#[derive(Default, Clone)]
struct FakeCalls {
    kept: Arc<Mutex<Kept>>,
}

impl GatewayCallStore for FakeCalls {
    async fn recall(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        key: &str,
        now: Timestamp,
    ) -> Result<Option<KeptCall>, GatewayCallStoreError> {
        Ok(self
            .kept
            .lock()
            .expect("lock")
            .get(&(tenant_id, store_id, key.to_owned()))
            .filter(|(at, _)| *at > now.as_milliseconds_since_epoch() - KEPT_FOR_MS)
            .map(|(_, call)| call.clone()))
    }

    async fn keep(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        key: &str,
        call: &KeptCall,
        now: Timestamp,
    ) -> Result<(), GatewayCallStoreError> {
        self.kept.lock().expect("lock").insert(
            (tenant_id, store_id, key.to_owned()),
            (now.as_milliseconds_since_epoch(), call.clone()),
        );
        Ok(())
    }
}

fn field(name: &str, kind: GatewayFieldKind) -> GatewayField {
    GatewayField {
        name: name.to_owned(),
        kind,
        required: true,
    }
}

async fn endpoints(idempotent: bool) -> FakeEndpoints {
    let store = FakeEndpoints::default();
    // Sealed in the format before tenant keys, version 0, which the router still opens under
    // integration_secret (ADR-0217 decision 2).
    let header = seal(
        &key(),
        SealedFor {
            tenant_id: tenant(),
            connection_id: ENDPOINT,
            field_key: "header:x-api-key",
        },
        PLAINTEXT,
    )
    .expect("seal");
    for (id, enabled) in [(ENDPOINT, true), (DISABLED, false)] {
        let endpoint = GatewayEndpoint {
            gateway_endpoint_id: id.to_owned(),
            display_name: "Demo loyalty".to_owned(),
            url: "https://93.184.216.34/balance".to_owned(),
            method: GatewayMethod::Post,
            request_fields: vec![field("card_code", GatewayFieldKind::String)],
            response_fields: vec![field("points", GatewayFieldKind::Integer)],
            deadline_ms: 3_000,
            idempotent,
            enabled,
            sealed_headers: [("x-api-key".to_owned(), header.clone())].into(),
        };
        store
            .create(tenant(), &endpoint)
            .await
            .expect("seed an endpoint");
    }
    store
}

fn app(endpoints: FakeEndpoints, target: Arc<FakeTarget>, keys: FakeKeys) -> axum::Router {
    gateway_call_router(
        endpoints,
        FakeCalls::default(),
        keys,
        clock(),
        target,
        Some(crate::sealing_keys::file_keys("gateway_calls", &key())),
    )
}

fn uri() -> String {
    format!("/sync/stores/{}/gateway/calls", store_id().as_ulid())
}

fn call(endpoint: &str, idempotency_key: &str, values: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "gateway_endpoint_id": endpoint,
        "idempotency_key": idempotency_key,
        "values": values,
    })
}

fn ok(body: &str) -> TargetAnswer {
    TargetAnswer {
        status: 200,
        body: body.as_bytes().to_vec(),
    }
}

/// The call is made with the opened secret header and the store's key, its declared result comes
/// back, and the same key again replays the first answer without calling the target.
#[tokio::test]
async fn a_store_s_call_reaches_the_target_once_per_key() {
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::CallGateway]);
    let target = FakeTarget::answering(vec![ok(r#"{"points": 120, "phone": "0900"}"#)]);
    let router = app(endpoints(false).await, Arc::clone(&target), keys);
    let sent = call(
        ENDPOINT,
        "act-1",
        &serde_json::json!({ "card_code": "C-1" }),
    );

    let first = router
        .clone()
        .oneshot(post_json_bearer(&uri(), &sent, &token))
        .await
        .expect("route the call");
    assert_eq!(first.status(), StatusCode::OK);
    let first = json_body(first).await;
    assert_eq!(first["outcome"], "GATEWAY_OUTCOME_ANSWERED");
    assert_eq!(
        first["result"],
        serde_json::json!({ "points": 120 }),
        "only the declared field"
    );
    assert_eq!(first["status"], 200);
    {
        let sent = target.sent.lock().expect("lock");
        let (url, headers, body) = sent.first().expect("one request");
        assert_eq!(url, "https://93.184.216.34/balance");
        assert!(headers.contains(&("x-api-key".to_owned(), PLAINTEXT.to_owned())));
        assert!(headers.contains(&("idempotency-key".to_owned(), "act-1".to_owned())));
        assert_eq!(body.as_deref(), Some(br#"{"card_code":"C-1"}"#.as_slice()));
    }
    assert!(!first.to_string().contains(PLAINTEXT));

    let again = router
        .oneshot(post_json_bearer(&uri(), &sent, &token))
        .await
        .expect("route the repeat");
    assert_eq!(json_body(again).await, first, "the first answer, replayed");
    assert_eq!(target.sent(), 1, "the target was called once");
}

/// Everything refused before a call: no scope, another store, an unknown or disabled endpoint, a
/// value of the wrong kind, a malformed key, and a key already used for another endpoint.
#[tokio::test]
async fn what_is_refused_before_any_call() {
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::CallGateway]);
    let unscoped = issue_store_key(&keys, tenant(), store_id(), &[Scope::RelayOrders]);
    let elsewhere = issue_store_key(
        &keys,
        tenant(),
        StoreId::new(Ulid::from_u128(0x0BAD)),
        &[Scope::CallGateway],
    );
    let target = FakeTarget::answering(vec![ok(r#"{"points": 1}"#)]);
    let router = app(endpoints(false).await, Arc::clone(&target), keys);
    let good = || serde_json::json!({ "card_code": "C-1" });
    let status = |request: axum::http::Request<axum::body::Body>| {
        let router = router.clone();
        async move {
            router
                .oneshot(request)
                .await
                .expect("route the call")
                .status()
        }
    };

    for (body, token, expected) in [
        (
            call(ENDPOINT, "k1", &good()),
            &unscoped,
            StatusCode::FORBIDDEN,
        ),
        (
            call(ENDPOINT, "k1", &good()),
            &elsewhere,
            StatusCode::FORBIDDEN,
        ),
        (call(DISABLED, "k1", &good()), &token, StatusCode::NOT_FOUND),
        (
            call("01J00000000000000000000G09", "k1", &good()),
            &token,
            StatusCode::NOT_FOUND,
        ),
        (
            call(ENDPOINT, "k1", &serde_json::json!({ "card_code": 5 })),
            &token,
            StatusCode::BAD_REQUEST,
        ),
        (
            call(ENDPOINT, "k1", &serde_json::json!({})),
            &token,
            StatusCode::BAD_REQUEST,
        ),
        (
            call(ENDPOINT, "has space", &good()),
            &token,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(
            status(post_json_bearer(&uri(), &body, token)).await,
            expected,
            "{body}"
        );
    }
    assert_eq!(target.sent(), 0, "nothing reached the target");
}

/// A key kept for one endpoint is refused for another (`409`).
#[tokio::test]
async fn a_key_kept_for_one_endpoint_is_refused_for_another() {
    let good = || serde_json::json!({ "card_code": "C-1" });
    let calls = FakeCalls::default();
    calls
        .keep(
            tenant(),
            store_id(),
            "k3",
            &KeptCall {
                gateway_endpoint_id: DISABLED.to_owned(),
                answer: serde_json::from_value(serde_json::json!({
                    "call_id": "01J00000000000000000000C01",
                    "outcome": "GATEWAY_OUTCOME_ANSWERED",
                    "result": {},
                }))
                .expect("an answer"),
            },
            clock_now(),
        )
        .await
        .expect("keep");
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::CallGateway]);
    let reused = gateway_call_router(
        endpoints(false).await,
        calls,
        keys,
        clock(),
        FakeTarget::answering(Vec::new()),
        Some(crate::sealing_keys::file_keys("gateway_calls", &key())),
    )
    .oneshot(post_json_bearer(
        &uri(),
        &call(ENDPOINT, "k3", &good()),
        &token,
    ))
    .await
    .expect("route the reused key");
    assert_eq!(reused.status(), StatusCode::CONFLICT);
}

fn clock_now() -> Timestamp {
    use pos_proto::determinism::ClockSource as _;
    clock().now()
}

/// An endpoint that fails five times in a row is paused by its breaker; and ten calls a second from
/// one store go through, the eleventh is refused.
#[tokio::test]
async fn the_limits_and_the_breaker_hold_and_an_unanswered_call_is_not_kept() {
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::CallGateway]);
    let failing = vec![
        TargetAnswer {
            status: 503,
            body: Vec::new(),
        };
        5
    ];
    let target = FakeTarget::answering(failing);
    let router = app(endpoints(false).await, Arc::clone(&target), keys);
    let send = |key: String| {
        let router = router.clone();
        let token = token.clone();
        async move {
            router
                .oneshot(post_json_bearer(
                    &uri(),
                    &call(ENDPOINT, &key, &serde_json::json!({ "card_code": "C-1" })),
                    &token,
                ))
                .await
                .expect("route the call")
        }
    };

    for n in 0..5 {
        let answer = send(format!("fail-{n}")).await;
        assert_eq!(answer.status(), StatusCode::OK);
        let answer = json_body(answer).await;
        assert_eq!(answer["refusal"], "GATEWAY_REFUSAL_TARGET_ERROR");
        assert_eq!(answer["status"], 503);
    }
    let paused = send("fail-5".to_owned()).await;
    assert_eq!(
        paused.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the breaker is open"
    );
    assert_eq!(target.sent(), 5);

    // Another store's router, answering: within one second (the clock is fixed) ten calls go
    // through and the eleventh is refused before it reaches the target.
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::CallGateway]);
    let target = FakeTarget::answering(vec![ok(r#"{"points": 1}"#); 11]);
    let router = app(endpoints(false).await, Arc::clone(&target), keys);
    let mut statuses = Vec::new();
    for n in 0..11 {
        let response = router
            .clone()
            .oneshot(post_json_bearer(
                &uri(),
                &call(
                    ENDPOINT,
                    &format!("busy-{n}"),
                    &serde_json::json!({ "card_code": "C" }),
                ),
                &token,
            ))
            .await
            .expect("route the call");
        statuses.push(response.status());
    }
    assert!(
        statuses[..10]
            .iter()
            .all(|status| *status == StatusCode::OK)
    );
    assert_eq!(statuses[10], StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(target.sent(), 10);
}

/// A call the target never answered is not kept: the same key calls again.
#[tokio::test]
async fn a_call_that_was_not_answered_is_tried_again_under_its_key() {
    let keys = FakeKeys::default();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::CallGateway]);
    let target = FakeTarget::answering(Vec::new());
    let router = app(endpoints(false).await, Arc::clone(&target), keys);
    let sent = call(
        ENDPOINT,
        "retry-me",
        &serde_json::json!({ "card_code": "C-1" }),
    );
    for _ in 0..2 {
        let answer = router
            .clone()
            .oneshot(post_json_bearer(&uri(), &sent, &token))
            .await
            .expect("route the call");
        assert_eq!(
            json_body(answer).await["refusal"],
            "GATEWAY_REFUSAL_UNREACHABLE"
        );
    }
    assert_eq!(target.sent(), 2, "not kept, so tried again");
}
