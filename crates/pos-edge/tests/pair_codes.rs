// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Minting the pairing code for the next device
//! ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §3).
//!
//! Before this route, a code was minted once per process start and nothing minted another, so
//! commissioning a second till on a trading store meant stopping the service — which drops every
//! till's and kitchen display's `/ws` session and forces the outbox to drain
//! ([ADR-0117](../../../docs/adr/0117-a-headless-store-keeps-a-log.md) §70).
//!
//! What the tests below pin is the *shape* of the concession, because the route hands out a
//! credential and the gates are the whole of what makes that acceptable:
//!
//! * both gates are on it — a paired device **and** a signed-in manager;
//! * a waiter's tap cannot admit hardware, even from a paired till;
//! * the code it returns actually pairs a device, once;
//! * minting **replaces** the live code rather than adding beside it, so the store never has two
//!   codes a guess could hit and an operator is never holding one of two.

use std::num::NonZeroU32;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::admission_events::AdmissionEvents;
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ClockSource;
use pos_proto::ids::{EmployeeId, StoreId};
use pos_proto::ulid::Ulid;
use serde_json::json;
use tower::ServiceExt;

/// A manager (may manage devices) and a waiter (may not) — the same split ADR-0112's binding uses,
/// because admitting a device and binding a terminal are the same kind of act.
const MANAGER_CODE: &str = "M01";
const WAITER_CODE: &str = "W01";
const PIN: &str = "2468";

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(b"fixed-test-salt!").expect("salt");
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .expect("hash")
        .to_string()
}

/// The domain router, the shared `Pairing`, the composed `Edge`, and one paired device's token.
///
/// The `Pairing` and the `Edge` are handed back as well as wired in, because the assertions are
/// about what the *store* holds after a call — the live code table and the durable event log — not
/// only about what the response said.
async fn paired() -> (Router, Arc<Pairing>, Arc<Edge<FakeStore>>, String) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7)));
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(21))),
            permissions: [Permission::ManageDevices].into_iter().collect(),
            pin_phc: Some(hash_of(PIN)),
        },
    );
    staff.insert(
        WAITER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(22))),
            permissions: PermissionSet::default(),
            pin_phc: Some(hash_of(PIN)),
        },
    );
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            EdgeSession::bootstrap().with_staff(staff),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    // Wired to the real reporter, so the mint route's end-to-end test can read what reached the
    // cloud-bound log rather than trusting the handler to have named the right device (ADR-0118 §4).
    let pairing = Arc::new(Pairing::new().with_admission_events(Some(
        Arc::<Edge<FakeStore>>::clone(&edge) as Arc<dyn AdmissionEvents>,
    )));
    let now = SystemClock.now();
    // The boot code and the first device — the floor this route deliberately does not replace,
    // because the first tablet on a virgin box has nothing to authenticate with.
    let (boot_code, _) = pairing.mint(now, Minter::Boot).expect("mint the boot code");
    let token = pairing
        .redeem(&boot_code, now)
        .await
        .expect("redeem")
        .token()
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    let router = pos_edge::http::domain_router(
        Arc::<Edge<FakeStore>>::clone(&edge),
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        Arc::clone(&pairing),
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    (router, pairing, edge, token)
}

async fn sign_in(app: &Router, token: &str, code: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/sign-in")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "code": code, "pin": PIN }).to_string()))
                .expect("request builds"),
        )
        .await
        .expect("route the sign-in");
    assert_eq!(response.status(), StatusCode::OK, "the seeded PIN signs in");
}

/// Posts the mint, returning the status and the body.
async fn mint(app: &Router, token: Option<&str>) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/pair/codes")
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).expect("request builds"))
        .await
        .expect("route the mint");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn code_from(body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body).expect("the reply is JSON");
    parsed["code"]
        .as_str()
        .expect("the reply carries a code")
        .to_owned()
}

#[tokio::test]
async fn a_signed_in_manager_mints_a_code_that_pairs_the_next_device() {
    let (app, pairing, _edge, token) = paired().await;
    sign_in(&app, &token, MANAGER_CODE).await;

    let (status, body) = mint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let code = code_from(&body);
    assert_eq!(code.len(), 6, "a pairing code is six digits: {code}");
    assert!(code.chars().all(|character| character.is_ascii_digit()));

    let parsed: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    let expires_at_ms = parsed["expires_at_ms"]
        .as_i64()
        .expect("the reply says when the code dies");
    assert!(
        expires_at_ms > SystemClock.now().as_milliseconds_since_epoch(),
        "the code expires in the future"
    );

    // The whole point: this code admits the second till, with no restart in between.
    let parsed_code = pos_edge::Code::parse(&code).expect("the reply's code is well formed");
    let second = pairing
        .redeem(&parsed_code, SystemClock.now())
        .await
        .expect("redeem")
        .token();
    assert!(second.is_some(), "the minted code paired a device");
    assert_eq!(
        pairing.issued_count(),
        2,
        "the store now holds two devices — the boot one and the minted one"
    );
}

#[tokio::test]
async fn minting_replaces_the_live_code_rather_than_adding_beside_it() {
    // The invariant the pairing surface is reasoned about under: one live code. Before ADR-0118 it
    // held only because one caller existed. Two live codes would double the window a guess has to
    // hit, and leave an operator unable to say which of the two they were told to use.
    let (app, pairing, _edge, token) = paired().await;
    sign_in(&app, &token, MANAGER_CODE).await;

    let (_, first_body) = mint(&app, Some(&token)).await;
    let first = pos_edge::Code::parse(&code_from(&first_body)).expect("well formed");
    let (_, second_body) = mint(&app, Some(&token)).await;
    let second = pos_edge::Code::parse(&code_from(&second_body)).expect("well formed");
    assert_ne!(
        first.as_str(),
        second.as_str(),
        "two mints produced the same code, so entropy is not reaching this path"
    );

    let now = SystemClock.now();
    assert!(
        pairing
            .redeem(&first, now)
            .await
            .expect("redeem")
            .token()
            .is_none(),
        "the superseded code still pairs a device"
    );
    assert!(
        pairing
            .redeem(&second, now)
            .await
            .expect("redeem")
            .token()
            .is_some(),
        "the code the manager is holding does not pair"
    );
}

#[tokio::test]
async fn a_waiter_on_a_paired_till_cannot_mint() {
    // Standing, not paired-ness, is what this route is gated on. A waiter's tap must not be able to
    // admit hardware to the store — and the refusal must not read as "sign in again", because
    // somebody *is* signed in.
    let (app, pairing, _edge, token) = paired().await;
    sign_in(&app, &token, WAITER_CODE).await;

    let (status, body) = mint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("manager"),
        "the refusal should say what is missing: {body}"
    );
    assert_eq!(
        pairing.code_count(),
        0,
        "a refused mint left a code behind anyway"
    );
}

#[tokio::test]
async fn an_unpaired_caller_is_refused_before_the_handler() {
    // The outer gate. A laptop plugged into the store switch gets `401` from the paired-device
    // middleware and never reaches the permission check.
    let (app, pairing, _edge, _token) = paired().await;

    let (status, _body) = mint(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _body) = mint(&app, Some("not-a-real-device-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(pairing.code_count(), 0, "an unpaired caller minted a code");
}

#[tokio::test]
async fn a_paired_device_with_nobody_signed_in_is_refused() {
    // The inner gate. Paired but nobody signed in — which is what an unattended till looks like —
    // gets `403 sign in to act on this device` from the signed-in middleware, not a code.
    //
    // `403` rather than `401` on purpose, and it is the middleware's choice rather than this
    // route's: the device *is* admitted, so the UI's answer is the sign-in screen and not the
    // pairing screen, and `require_signed_in` answers the same way whether nobody signed in or the
    // session went idle (ADR-0091) so the till does not have to tell those apart.
    let (app, pairing, _edge, token) = paired().await;

    let (status, body) = mint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("sign in"),
        "the refusal should send the till to the sign-in screen: {body}"
    );
    assert_eq!(
        pairing.code_count(),
        0,
        "a till with nobody signed in minted a code"
    );
}

/// The device the code was minted **from** is what reaches the cloud — not the store, not the
/// manager, not the device that redeems.
///
/// The unit tests prove `Minter::Manager { device_id }` lands on `admitted_by_device_id`. What this
/// pins is the one line between them: that the handler builds the minter from the `Actor` the
/// signed-in gate assembled, whose `device_id` came from the paired gate's extension. Getting that
/// wrong would publish a plausible, wrong answer to "which till admitted this one", which is worse
/// than publishing none.
#[tokio::test]
async fn the_admission_names_the_till_the_code_was_minted_from() {
    let (app, pairing, edge, token) = paired().await;
    sign_in(&app, &token, MANAGER_CODE).await;
    // The till doing the minting is the device the boot code paired, which is what `token`
    // authenticates as.
    let minting_till = pairing
        .paired_devices()
        .first()
        .map(|(device_id, _)| *device_id)
        .expect("the boot code paired one device");

    let (status, body) = mint(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "the manager mints: {body}");
    let code = pos_edge::Code::parse(&code_from(&body)).expect("well formed");
    assert!(
        pairing
            .redeem(&code, SystemClock.now())
            .await
            .expect("redeem")
            .token()
            .is_some(),
        "the minted code pairs the next device"
    );

    let query = EventQuery::first(
        StoreId::new(Ulid::from_u128(7)),
        NonZeroU32::new(64).expect("non-zero"),
    );
    let admissions: Vec<serde_json::Value> = edge
        .store()
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "device.admission.granted")
        .map(|envelope| {
            serde_json::from_str(envelope.data.as_json()).unwrap_or(serde_json::Value::Null)
        })
        .collect();
    // Two admissions: the boot code's, then this one.
    assert_eq!(
        admissions.len(),
        2,
        "one event per admission: {admissions:?}"
    );
    let second = admissions.get(1).expect("the second admission");
    assert_eq!(
        second.get("admitted_by_device_id").and_then(|v| v.as_str()),
        Some(minting_till.to_string().as_str()),
        "the authorising device is the till the manager was signed in on: {second}"
    );
    let first = admissions.first().expect("the first admission");
    assert!(
        first
            .get("admitted_by_device_id")
            .is_some_and(serde_json::Value::is_null),
        "and the boot code's admission still names nobody: {first}"
    );
}
