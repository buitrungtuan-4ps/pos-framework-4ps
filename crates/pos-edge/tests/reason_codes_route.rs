// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/reason-codes` — the read the till's pickers are built from
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md)).
//!
//! One read serves every picker, so what this route leaves out is as load-bearing as what it
//! includes: a retired entry is offered to nobody, and an action token this release does not know
//! is dropped rather than shown. Both are the same argument — the edge validates a void against
//! `is_valid_for`, so anything this route offers that the edge would refuse is a button whose only
//! outcome is a `403` the operator cannot act on.
//!
//! Driven with `tower::ServiceExt::oneshot`, like the other route suites: no socket, no race.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::PermissionSet;
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::{EmployeeId, ReasonCodeId, StoreId};
use pos_proto::reason_codes::{
    PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode,
};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use serde_json::json;
use tower::ServiceExt;

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";

/// A real Argon2id PHC hash of `pin`, with a fixed salt so the test needs no RNG.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

/// The domain router over a session built by `seed`, with the seeded staff signed in.
async fn app(seed: impl FnOnce(&mut EdgeSession)) -> (Router, String) {
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let mut session = EdgeSession::bootstrap().with_staff(roster);
    seed(&mut session);
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3))),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let (code, _) = pairing
        .mint(now, Minter::Boot)
        .expect("mint a pairing code");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .token()
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    let served = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let signed_in = served
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/sign-in")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "code": STAFF_CODE, "pin": STAFF_PIN }).to_string(),
                ))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        signed_in.status(),
        StatusCode::OK,
        "the seeded staff signs in"
    );
    (served, token)
}

/// The route's answer as JSON.
async fn read(app: &Router, token: &str) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/reason-codes")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json body")
}

/// The entry with this code, or `None`.
fn entry<'a>(body: &'a serde_json::Value, code: &str) -> Option<&'a serde_json::Value> {
    body["reasons"]
        .as_array()
        .expect("an array of reasons")
        .iter()
        .find(|reason| reason["code"] == code)
}

/// The `applies_to` tokens of the entry with this code.
fn actions(body: &serde_json::Value, code: &str) -> Vec<String> {
    entry(body, code).expect("the entry is offered")["applies_to"]
        .as_array()
        .expect("an array of actions")
        .iter()
        .map(|action| action.as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn a_store_that_has_published_nothing_still_offers_the_framework_reasons() {
    // ADR-0115's "absence is not a brick": a store must be able to void a mis-keyed line on its
    // first day, before anyone has opened the console. The till's picker is built from this read,
    // so if it answered empty here the control would be unusable exactly when it is most needed.
    let (app, token) = app(|_| {}).await;
    let body = read(&app, &token).await;

    assert!(
        entry(&body, "WASTE").is_some(),
        "the framework default set is what an unpublished store offers, got {body}"
    );
    assert!(
        actions(&body, "WASTE").contains(&"REASON_ACTION_VOID_LINE".to_owned()),
        "a reason carries the actions it covers, so one read serves every picker"
    );
    // The near miss that makes `applies_to` load-bearing rather than decorative: a real, active,
    // published reason that the void picker must not offer, because the edge would refuse it.
    assert!(
        !actions(&body, "OUT_OF_STOCK").contains(&"REASON_ACTION_VOID_BILL".to_owned()),
        "a reason for refusing an order is not a reason for voiding a bill"
    );
}

#[tokio::test]
async fn a_retired_reason_is_offered_to_nobody_and_an_unknown_action_is_dropped() {
    let (app, token) = app(|session| {
        session.reason_codes = PublishedReasonCodes::from_parts(vec![
            PublishedReasonCode::new(
                ReasonCodeId::new(Ulid::from_u128(700)),
                ReasonCode::new("STILL_USED"),
                DisplayName::new("Still used"),
                vec![ReasonAction::VoidLine],
            ),
            PublishedReasonCode::new(
                ReasonCodeId::new(Ulid::from_u128(701)),
                ReasonCode::new("NO_LONGER_USED"),
                DisplayName::new("No longer used"),
                vec![ReasonAction::VoidLine],
            )
            .retired(),
            // An entry from a newer cloud: one action this release knows, one it does not. The node
            // round-trips the unknown token rather than failing, and this route drops it — a picker
            // offering it would send a refusal nobody at the till can act on.
            PublishedReasonCode {
                id: ReasonCodeId::new(Ulid::from_u128(702)),
                code: ReasonCode::new("FROM_A_NEWER_CLOUD"),
                display_name: DisplayName::new("From a newer cloud"),
                display_name_translations: std::collections::BTreeMap::new(),
                applies_to: vec![
                    Open::from(ReasonAction::VoidLine),
                    Open::parse("REASON_ACTION_SOMETHING_LATER"),
                ],
                active: true,
            },
        ]);
    })
    .await;
    let body = read(&app, &token).await;

    assert!(
        entry(&body, "STILL_USED").is_some(),
        "an active entry is offered, got {body}"
    );
    assert!(
        entry(&body, "NO_LONGER_USED").is_none(),
        "a retired entry stays in the node so history resolves, and shows on no picker"
    );
    assert_eq!(
        actions(&body, "FROM_A_NEWER_CLOUD"),
        vec!["REASON_ACTION_VOID_LINE".to_owned()],
        "an action token this release does not know is dropped, not offered"
    );
}

#[tokio::test]
async fn the_name_arrives_in_the_stores_own_language() {
    // The till never translates a reason: the text is tenant content (`docs/pos-spec.md` §12), so
    // it is resolved here against the store's display language, with `display_name` as the fallback
    // §12 requires. A device that had to pick would need the store's language and the whole
    // per-locale map, neither of which it holds.
    let (app, token) = app(|session| {
        session.display_language = Some("vi".to_owned());
    })
    .await;
    let body = read(&app, &token).await;

    assert_eq!(
        entry(&body, "WASTE").expect("the entry is offered")["display_name"],
        "Hàng hỏng hoặc bỏ đi",
        "the framework set carries a Vietnamese name, and the store asked for Vietnamese"
    );
}
