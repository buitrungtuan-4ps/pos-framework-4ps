// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/sync` reports what the box last measured of its own clock (roadmap-v3 **PF4**).
//!
//! The probe itself is exercised in `sntp.rs` against a time server on loopback. What is proved
//! here is the other half: that a reading the probe loop records is the one a signed-in device reads,
//! that an alarm is named as one, that a failed probe does not blank the last good word, and that
//! the fields are additive — the outbox half of the answer is untouched.
//!
//! Driven with `tower::ServiceExt::oneshot`, like the other route suites: no socket, no race, and no
//! time server — the loop is `main`'s, so nothing here sends a datagram.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::PermissionSet;
use pos_edge::pairing::Minter;
use pos_edge::sntp::{ClockReading, ProbeError};
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::{EmployeeId, StoreId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
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

/// The domain router with the seeded staff signed in, and the edge behind it — kept, so a test can
/// record a reading the way the probe loop would.
async fn app() -> (Router, String, Arc<Edge<FakeStore>>) {
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(4))),
            EdgeSession::bootstrap().with_staff(roster),
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
        Arc::clone(&edge),
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
    (served, token, edge)
}

/// `GET /api/sync` as `token`, or with no credential at all.
async fn sync(app: &Router, token: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder().uri("/api/sync");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn at(ms: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(ms).expect("a valid instant")
}

#[tokio::test]
async fn a_clock_nobody_has_measured_says_so_and_the_outbox_reads_as_before() {
    let (app, token, _edge) = app().await;
    let (status, body) = sync(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);

    // Unmeasured is not "fine": no probe has answered, so there is no offset to report and none is
    // invented. The threshold is there regardless, so a screen can say what it would alarm at.
    assert_eq!(body["clock_drift"], "CLOCK_DRIFT_UNSPECIFIED");
    assert_eq!(body["clock_drift_alarm_ms"], 2_000);
    assert!(body.get("clock_offset_ms").is_none(), "got {body}");
    assert!(body.get("clock_measure_time").is_none(), "got {body}");

    // Additive: the fields the status bar reads are exactly what they were.
    assert_eq!(body["outbox_depth"], 0);
    assert_eq!(body["outbox_level"], "OUTBOX_LEVEL_NORMAL");
    assert_eq!(body["cloud_link"], "CLOUD_LINK_UNSPECIFIED");
}

#[tokio::test]
async fn a_measured_clock_is_reported_and_an_alarm_is_named_as_one() {
    let (app, token, edge) = app().await;

    let close = ClockReading {
        offset_ms: -350,
        measure_time: at(1_790_640_000_000),
    };
    edge.clock_status().record(Ok(close));
    let (_, body) = sync(&app, Some(&token)).await;
    assert_eq!(body["clock_drift"], "CLOCK_DRIFT_OK");
    assert_eq!(body["clock_offset_ms"], -350);
    assert_eq!(body["clock_measure_time"], json!(close.measure_time));

    // Two and a half seconds fast: past the alarm, which is what the Devices screen turns red on.
    let fast = ClockReading {
        offset_ms: 2_500,
        measure_time: at(1_790_640_900_000),
    };
    edge.clock_status().record(Ok(fast));
    let (_, body) = sync(&app, Some(&token)).await;
    assert_eq!(body["clock_drift"], "CLOCK_DRIFT_ALARM");
    assert_eq!(body["clock_offset_ms"], 2_500);
    assert_eq!(body["clock_measure_time"], json!(fast.measure_time));
}

#[tokio::test]
async fn a_failed_probe_keeps_the_last_reading_rather_than_blanking_it() {
    // A store whose uplink drops between two probes has learned nothing new about its clock. The
    // last reading stands, and its measure time is what says how old it is.
    let (app, token, edge) = app().await;
    let fast = ClockReading {
        offset_ms: 4_000,
        measure_time: at(1_790_640_000_000),
    };
    edge.clock_status().record(Ok(fast));
    edge.clock_status().record(Err(ProbeError::TimedOut));

    let (_, body) = sync(&app, Some(&token)).await;
    assert_eq!(body["clock_drift"], "CLOCK_DRIFT_ALARM");
    assert_eq!(body["clock_offset_ms"], 4_000);
    assert_eq!(body["clock_measure_time"], json!(fast.measure_time));
}

#[tokio::test]
async fn a_request_with_no_credential_learns_nothing_about_the_box() {
    let (app, _token, edge) = app().await;
    edge.clock_status().record(Ok(ClockReading {
        offset_ms: 2_500,
        measure_time: at(1_790_640_000_000),
    }));
    let (status, body) = sync(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.get("clock_offset_ms").is_none());
}
