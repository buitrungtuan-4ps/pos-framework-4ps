// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/gateway/calls`, a till's lookup through the cloud gateway (ADR-0214 decision 3),
//! driven without a socket over a fake cloud. Fake data only.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::cloud_http::CloudHttpError;
use pos_edge::http::gateway::{Gateway, GatewayCalls, GatewayFuture};
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_ports::event_store::EventStore as _;
use pos_proto::ClockSource;
use pos_proto::ids::{EmployeeId, StoreId};
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

const SALT: &[u8] = b"a-fixed-test-slt";
const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";
const ENDPOINT: &str = "01J9Z3K8M2Q4R6T8V0W2Y4A6C8";

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

/// What the fake cloud answers, and every body it was sent.
enum Cloud {
    Answers(u16, Value),
    Unreachable,
    Silent,
}

struct FakeCloud {
    answer: Cloud,
    sent: Mutex<Vec<Value>>,
}

impl GatewayCalls for FakeCloud {
    fn call(&self, body: Vec<u8>) -> GatewayFuture<'_> {
        Box::pin(async move {
            let sent = serde_json::from_slice(&body).expect("the edge sends JSON");
            self.sent.lock().expect("the log").push(sent);
            match &self.answer {
                Cloud::Answers(status, body) => Ok((*status, body.to_string().into_bytes())),
                Cloud::Unreachable => Err(CloudHttpError::new("connecting to the cloud failed")),
                Cloud::Silent => {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Err(CloudHttpError::new("never"))
                }
            }
        })
    }
}

struct Till {
    app: Router,
    token: String,
    edge: Arc<Edge<FakeStore>>,
}

/// The guarded routes, a device signed in as somebody granted `permissions`, and the gateway link
/// over `cloud` when there is one.
async fn till(permissions: PermissionSet, cloud: Option<Arc<FakeCloud>>) -> Till {
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions,
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
            EdgeSession {
                ..EdgeSession::bootstrap().with_staff(roster)
            },
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let (code, _) = pairing.mint(now, Minter::Boot).expect("a pairing code");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .token()
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    let mut app = pos_edge::http::domain_router(
        Arc::clone(&edge),
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    if let Some(cloud) = cloud {
        let calls: Arc<dyn GatewayCalls> = cloud;
        app = app.layer(axum::Extension(
            Gateway::new(calls).waiting(Duration::from_millis(200)),
        ));
    }
    let (status, _, _) = send(
        &app,
        &token,
        "/api/session/sign-in",
        json!({ "code": STAFF_CODE, "pin": STAFF_PIN }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the seeded staff signs in");
    Till { app, token, edge }
}

async fn send(
    app: &Router,
    token: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Option<String>, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("router responds");
    let status = response.status();
    let reason = response
        .headers()
        .get("pos-error-reason")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        reason,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn look_up(till: &Till, body: Value) -> (StatusCode, Option<String>, Value) {
    send(&till.app, &till.token, "/api/gateway/calls", body).await
}

fn cloud(answer: Cloud) -> Arc<FakeCloud> {
    Arc::new(FakeCloud {
        answer,
        sent: Mutex::new(Vec::new()),
    })
}

fn granted() -> PermissionSet {
    PermissionSet::EMPTY.with(Permission::RunLookup)
}

fn answered() -> Value {
    json!({
        "call_id": "01J9Z3K8M2Q4R6T8V0W2Y4A6D0",
        "outcome": "GATEWAY_OUTCOME_ANSWERED",
        "status": 200,
        "result": { "points": 120, "tier": "demo-gold" },
    })
}

fn refused(refusal: &str) -> Value {
    json!({
        "call_id": "01J9Z3K8M2Q4R6T8V0W2Y4A6D0",
        "outcome": "GATEWAY_OUTCOME_REFUSED",
        "refusal": refusal,
        "result": {},
    })
}

/// No role holds the permission by default, and without it nothing reaches the cloud.
#[tokio::test]
async fn a_person_without_the_permission_is_refused_before_the_cloud_is_called() {
    let fake = cloud(Cloud::Answers(200, answered()));
    let till = till(PermissionSet::default(), Some(Arc::clone(&fake))).await;
    let (status, reason, _) = look_up(&till, json!({ "gateway_endpoint_id": ENDPOINT })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(reason.as_deref(), Some("PERMISSION_DENIED"));
    assert!(fake.sent.lock().expect("the log").is_empty());
}

/// The answer is the declared result, under a key the edge named, and nothing is written.
#[tokio::test]
async fn an_answered_lookup_returns_the_result_and_writes_no_event() {
    let fake = cloud(Cloud::Answers(200, answered()));
    let till = till(granted(), Some(Arc::clone(&fake))).await;
    let before = till
        .edge
        .store()
        .outbox_depth(till.edge.store_id())
        .await
        .expect("the outbox");
    let (status, _, body) = look_up(
        &till,
        json!({ "gateway_endpoint_id": ENDPOINT, "values": { "member_code": "DEMO-0001" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["result"],
        json!({ "points": 120, "tier": "demo-gold" })
    );
    assert_eq!(body["call_id"], "01J9Z3K8M2Q4R6T8V0W2Y4A6D0");
    let key = body["idempotency_key"].as_str().expect("a key").to_owned();
    assert_eq!(key.len(), 26, "the edge names a ULID");
    let sent = fake.sent.lock().expect("the log").clone();
    assert_eq!(
        sent,
        vec![json!({
            "gateway_endpoint_id": ENDPOINT,
            "idempotency_key": key,
            "values": { "member_code": "DEMO-0001" },
        })]
    );
    let after = till
        .edge
        .store()
        .outbox_depth(till.edge.store_id())
        .await
        .expect("the outbox");
    assert_eq!(before, after, "a lookup writes no event");
}

/// A retry of the same act carries the till's own key to the cloud.
#[tokio::test]
async fn the_tills_key_is_sent_as_it_is() {
    let fake = cloud(Cloud::Answers(200, answered()));
    let till = till(granted(), Some(Arc::clone(&fake))).await;
    let (status, _, body) = look_up(
        &till,
        json!({ "gateway_endpoint_id": ENDPOINT, "idempotency_key": "retry-01" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["idempotency_key"], "retry-01");
    assert_eq!(
        fake.sent.lock().expect("the log")[0]["idempotency_key"],
        "retry-01"
    );
}

/// A malformed request is refused at the edge and never sent.
#[tokio::test]
async fn a_malformed_lookup_never_leaves_the_box() {
    let fake = cloud(Cloud::Answers(200, answered()));
    let till = till(granted(), Some(Arc::clone(&fake))).await;
    let too_many: serde_json::Map<String, Value> =
        (0..33).map(|n| (format!("field_{n}"), json!(n))).collect();
    for body in [
        json!({ "gateway_endpoint_id": "not-a-ulid" }),
        json!({ "gateway_endpoint_id": ENDPOINT, "idempotency_key": "has space" }),
        json!({ "gateway_endpoint_id": ENDPOINT, "values": too_many }),
        json!({ "values": {} }),
    ] {
        let (status, reason, _) = look_up(&till, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(reason.as_deref(), Some("INVALID_ARGUMENT"), "{body}");
    }
    assert!(fake.sent.lock().expect("the log").is_empty());
}

/// Each way a call fails is one refusal the till translates.
#[tokio::test]
async fn every_failure_is_one_of_the_four_refusals() {
    let cases = [
        (
            Cloud::Unreachable,
            StatusCode::SERVICE_UNAVAILABLE,
            "GATEWAY_OFFLINE",
        ),
        (
            Cloud::Answers(503, json!({})),
            StatusCode::SERVICE_UNAVAILABLE,
            "GATEWAY_OFFLINE",
        ),
        (
            Cloud::Answers(403, json!({})),
            StatusCode::SERVICE_UNAVAILABLE,
            "GATEWAY_OFFLINE",
        ),
        (
            Cloud::Answers(200, json!("garbled")),
            StatusCode::SERVICE_UNAVAILABLE,
            "GATEWAY_OFFLINE",
        ),
        (
            Cloud::Silent,
            StatusCode::GATEWAY_TIMEOUT,
            "GATEWAY_TIMEOUT",
        ),
        (
            Cloud::Answers(200, refused("GATEWAY_REFUSAL_TIMEOUT")),
            StatusCode::GATEWAY_TIMEOUT,
            "GATEWAY_TIMEOUT",
        ),
        (
            Cloud::Answers(200, refused("GATEWAY_REFUSAL_TARGET_ERROR")),
            StatusCode::BAD_GATEWAY,
            "GATEWAY_REFUSED",
        ),
        (
            Cloud::Answers(200, refused("GATEWAY_REFUSAL_MALFORMED_ANSWER")),
            StatusCode::BAD_GATEWAY,
            "GATEWAY_REFUSED",
        ),
        (
            Cloud::Answers(200, refused("GATEWAY_REFUSAL_UNREACHABLE")),
            StatusCode::BAD_GATEWAY,
            "GATEWAY_REFUSED",
        ),
        (
            Cloud::Answers(429, json!({})),
            StatusCode::TOO_MANY_REQUESTS,
            "GATEWAY_BUSY",
        ),
        (
            Cloud::Answers(400, json!({})),
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
        ),
        (
            Cloud::Answers(409, json!({})),
            StatusCode::CONFLICT,
            "INVALID_ARGUMENT",
        ),
        (
            Cloud::Answers(404, json!({})),
            StatusCode::NOT_FOUND,
            "UNKNOWN_GATEWAY_ENDPOINT",
        ),
    ];
    for (answer, expected, token) in cases {
        let till = till(granted(), Some(cloud(answer))).await;
        let (status, reason, _) = look_up(&till, json!({ "gateway_endpoint_id": ENDPOINT })).await;
        assert_eq!((status, reason.as_deref()), (expected, Some(token)));
    }
}

/// A box with no link to its cloud says so at once.
#[tokio::test]
async fn a_box_without_a_link_answers_offline() {
    let till = till(granted(), None).await;
    let (status, reason, _) = look_up(&till, json!({ "gateway_endpoint_id": ENDPOINT })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reason.as_deref(), Some("GATEWAY_OFFLINE"));
}
