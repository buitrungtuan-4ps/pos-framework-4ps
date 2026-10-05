// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Binding a terminal's print agent over HTTP ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! The claim carries **two** gates, and the tests below are about what each one is for. The paired
//! gate says a box the store admitted is asking. The signed-in-manager gate says a person with
//! standing decided — because binding a terminal is a managerial act performed in front of the
//! machine, and a waiter's tap must not be able to move where the kitchen's tickets print.
//!
//! What neither gate proves is which physical machine is on the other end: the framework has no
//! device attestation, so a manager who signs in on a phone and claims a terminal gets a phone as
//! the agent. These tests pin what the gates *do* buy — that it cannot happen casually, and that a
//! second box cannot quietly take a live binding over.
//!
//! The read the till's **This device** card is drawn from sits behind the same two gates, and says
//! of each published terminal whether this device, another device or nobody holds it — never which
//! other device. A terminal this device holds that the node does not list is named apart, so the
//! card can release it; another device's never is.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::devices::PublishedDevices;
use pos_proto::ids::{DeviceId, EmployeeId, StoreId};
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

/// A manager (may manage devices) and a waiter (may not).
const MANAGER_CODE: &str = "M01";
const WAITER_CODE: &str = "W01";
const PIN: &str = "2468";

/// The terminal being claimed — a `TERMINAL` entry the console created, as it reaches the store in
/// the published `devices` node.
const TILL: &str = "0000000000000000000000000E";

/// Two more terminals and a printer, for the read: one terminal for each answer it gives.
const BAR: &str = "0000000000000000000000000F";
const SPARE: &str = "0000000000000000000000000G";
const PRINTER: &str = "0000000000000000000000000H";

/// A terminal no node here lists. A claim does not look its id up, so a call to the route binds a
/// device to it all the same.
const UNLISTED: &str = "0000000000000000000000000J";

/// A paired device's bearer token and the id the pairing minted for it.
type Paired = (String, DeviceId);

/// One published terminal, as the console's devices publish writes it.
fn terminal(id: &str, name: &str) -> Value {
    json!({
        "device_id": id,
        "kind": "DEVICE_KIND_TERMINAL",
        "connection": "DEVICE_CONNECTION_UNSPECIFIED",
        "address": "",
        "name": name,
    })
}

/// A receipt printer, which is not a till.
fn printer() -> Value {
    json!({
        "device_id": PRINTER,
        "kind": "DEVICE_KIND_PRINTER",
        "connection": "DEVICE_CONNECTION_NETWORK",
        "address": "192.0.2.10:9100",
        "name": "Counter printer",
    })
}

/// A `devices` node listing `devices`, parsed as the edge parses the published one.
fn node(devices: &[Value]) -> PublishedDevices {
    serde_json::from_value(json!({ "devices": devices })).expect("the node parses")
}

/// Three terminals with the printer between them: the read lists the terminals in this order.
fn three_tills() -> PublishedDevices {
    node(&[
        terminal(TILL, "Counter till"),
        printer(),
        terminal(BAR, "Bar till"),
        terminal(SPARE, "Spare till"),
    ])
}

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

/// The domain router and **two** paired device tokens: the binding is exclusive, and one token
/// cannot prove that.
async fn paired_pair() -> (Router, String, String) {
    let (router, [(holder, _), (other, _)]) =
        paired_pair_publishing(PublishedDevices::default()).await;
    (router, holder, other)
}

/// The same, over a store that publishes `devices`, with each device's minted id.
async fn paired_pair_publishing(devices: PublishedDevices) -> (Router, [Paired; 2]) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3)));
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: [Permission::ManageDevices].into_iter().collect(),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(PIN)),
        },
    );
    staff.insert(
        WAITER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(12))),
            permissions: PermissionSet::default(),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(PIN)),
        },
    );
    let mut session = EdgeSession::bootstrap().with_staff(staff);
    session.devices = devices;
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let mut paired = Vec::new();
    for _ in 0..2 {
        let (code, _) = pairing
            .mint(now, Minter::Boot)
            .expect("mint a pairing code");
        let token = pairing
            .redeem(&code, now)
            .await
            .expect("redeem")
            .token()
            .expect("a fresh code pairs a device");
        let device = pairing
            .device_for(&token)
            .expect("a freshly issued token resolves");
        paired.push((token.as_str().to_owned(), device));
    }
    let router = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let other = paired.pop().expect("two devices paired");
    let holder = paired.pop().expect("two devices paired");
    (router, [holder, other])
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

/// Posts to one of the two agent routes, returning the status and body.
async fn post_agent(app: &Router, token: &str, path: &str, agent: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "agent_device_id": agent }).to_string()))
                .expect("request builds"),
        )
        .await
        .expect("route the request");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Reads the store's terminals as `token`'s device: the status, the refusal's `pos-error-reason`,
/// and the body as JSON (`null` when it is not).
async fn get_terminals(app: &Router, token: &str) -> (StatusCode, Option<String>, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/print/agent")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("route the request");
    let status = response.status();
    let reason = response
        .headers()
        .get("pos-error-reason")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    (
        status,
        reason,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    )
}

/// What the read says of each terminal, in its order.
fn held(body: &Value) -> Vec<&str> {
    body["terminals"]
        .as_array()
        .expect("a list of terminals")
        .iter()
        .map(|terminal| terminal["held"].as_str().expect("a held token"))
        .collect()
}

/// A manager at the box binds a terminal, and a second box cannot take it over.
#[tokio::test]
async fn a_manager_binds_a_terminal_and_a_second_device_is_refused() {
    let (app, first, second) = paired_pair().await;
    sign_in(&app, &first, MANAGER_CODE).await;
    sign_in(&app, &second, MANAGER_CODE).await;

    let (status, body) = post_agent(&app, &first, "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("BOUND"), "the first claim binds: {body}");

    // Take-over-by-latest is the tempting simplification and it is wrong: two boxes holding one
    // identity both claim from the same queue, so each ticket prints once — on whichever grabbed it.
    let (status, body) = post_agent(&app, &second, "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("HELD_BY_ANOTHER_DEVICE"),
        "the second box is refused, not silently promoted: {body}"
    );

    // The holder releases it, and then the second box may take it — how a dead terminal is replaced.
    let (status, _) = post_agent(&app, &first, "/api/print/agent/revoke", TILL).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = post_agent(&app, &second, "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("BOUND"),
        "the replacement machine binds: {body}"
    );
}

/// A signed-in waiter cannot move where the kitchen's tickets print.
///
/// The sharper of the two gates. A paired device is any box the store admitted, and every waiter's
/// tablet is one; without this check a tap on the wrong screen would redirect a station's tickets to
/// a machine in another room.
#[tokio::test]
async fn a_signed_in_person_without_manage_devices_is_refused() {
    let (app, token, _second) = paired_pair().await;
    sign_in(&app, &token, WAITER_CODE).await;

    let (status, _) = post_agent(&app, &token, "/api/print/agent", TILL).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "binding a terminal takes a manager, not merely a signed-in person"
    );
    let (status, _) = post_agent(&app, &token, "/api/print/agent/revoke", TILL).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "and releasing one takes the same standing as binding it"
    );
}

/// A paired device with nobody signed in is refused before the permission is even considered.
#[tokio::test]
async fn a_paired_device_with_nobody_signed_in_is_refused() {
    let (app, token, _second) = paired_pair().await;
    let (status, _) = post_agent(&app, &token, "/api/print/agent", TILL).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the signed-in gate answers 403 so the till shows the sign-in screen"
    );
}

/// An unpaired caller does not reach the routes at all.
#[tokio::test]
async fn an_unpaired_caller_is_refused() {
    let (app, _first, _second) = paired_pair().await;
    let (status, _) = post_agent(&app, "not-a-token", "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An id that is not a ULID is a request fault, not a store fault.
#[tokio::test]
async fn an_agent_id_that_is_not_a_ulid_is_refused() {
    let (app, token, _second) = paired_pair().await;
    sign_in(&app, &token, MANAGER_CODE).await;
    let (status, _) = post_agent(&app, &token, "/api/print/agent", "not-a-ulid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The read lists the published terminals in the node's order, the printer left out, and says of
/// each whether this device, another device or nobody holds it: never which other device.
#[tokio::test]
async fn the_read_says_which_terminal_this_device_another_device_or_nobody_holds() {
    let (app, [(holder, holder_device), (other, other_device)]) =
        paired_pair_publishing(three_tills()).await;
    sign_in(&app, &holder, MANAGER_CODE).await;
    sign_in(&app, &other, MANAGER_CODE).await;
    assert_eq!(
        post_agent(&app, &holder, "/api/print/agent", TILL).await.0,
        StatusCode::OK
    );
    assert_eq!(
        post_agent(&app, &other, "/api/print/agent", BAR).await.0,
        StatusCode::OK
    );

    let (status, _, body) = get_terminals(&app, &holder).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "terminals": [
            { "agent_device_id": TILL, "name": "Counter till", "held": "THIS_DEVICE" },
            { "agent_device_id": BAR, "name": "Bar till", "held": "ANOTHER_DEVICE" },
            { "agent_device_id": SPARE, "name": "Spare till", "held": "NONE" },
        ]}),
        "three fields a terminal, and nothing about who holds one or when it last printed"
    );
    let (_, _, seen_by_other) = get_terminals(&app, &other).await;
    assert_eq!(
        held(&seen_by_other),
        ["ANOTHER_DEVICE", "THIS_DEVICE", "NONE"]
    );
    for answer in [body.to_string(), seen_by_other.to_string()] {
        for device in [holder_device, other_device] {
            assert!(
                !answer.contains(&device.to_string()),
                "a paired device's id would enumerate the store's pairings: {answer}"
            );
        }
    }
}

/// The read follows the record: a bind shows at once, and so does a release.
#[tokio::test]
async fn the_read_follows_a_bind_and_a_release() {
    let (app, [(token, _), _]) = paired_pair_publishing(three_tills()).await;
    sign_in(&app, &token, MANAGER_CODE).await;
    assert_eq!(
        held(&get_terminals(&app, &token).await.2),
        ["NONE", "NONE", "NONE"]
    );

    assert_eq!(
        post_agent(&app, &token, "/api/print/agent", BAR).await.0,
        StatusCode::OK
    );
    assert_eq!(
        held(&get_terminals(&app, &token).await.2),
        ["NONE", "THIS_DEVICE", "NONE"]
    );

    assert_eq!(
        post_agent(&app, &token, "/api/print/agent/revoke", BAR)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        held(&get_terminals(&app, &token).await.2),
        ["NONE", "NONE", "NONE"]
    );
}

/// A device bound by hand to a terminal the node does not list reads that terminal's id apart from
/// the list, and the device beside it does not: the read names this device's own binding and never
/// another's. A binding the node does list, or none, adds nothing.
#[tokio::test]
async fn the_read_names_this_devices_binding_to_a_terminal_the_node_does_not_list() {
    let (app, [(holder, holder_device), (other, other_device)]) =
        paired_pair_publishing(three_tills()).await;
    sign_in(&app, &holder, MANAGER_CODE).await;
    sign_in(&app, &other, MANAGER_CODE).await;
    let nobody_holds_a_listed_one = json!([
        { "agent_device_id": TILL, "name": "Counter till", "held": "NONE" },
        { "agent_device_id": BAR, "name": "Bar till", "held": "NONE" },
        { "agent_device_id": SPARE, "name": "Spare till", "held": "NONE" },
    ]);
    assert_eq!(
        get_terminals(&app, &holder).await.2,
        json!({ "terminals": nobody_holds_a_listed_one }),
        "a device that holds nothing reads the list alone"
    );

    let (status, body) = post_agent(&app, &holder, "/api/print/agent", UNLISTED).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("BOUND"),
        "a claim does not look the id up: {body}"
    );

    let (status, _, body) = get_terminals(&app, &holder).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "terminals": nobody_holds_a_listed_one,
            "unlisted_agent_device_id": UNLISTED,
        }),
        "the holder reads the terminal it holds, which a release names"
    );
    let (_, _, seen_by_other) = get_terminals(&app, &other).await;
    assert_eq!(
        seen_by_other,
        json!({ "terminals": nobody_holds_a_listed_one }),
        "another device's binding is never this device's to read"
    );
    for answer in [body.to_string(), seen_by_other.to_string()] {
        for device in [holder_device, other_device] {
            assert!(
                !answer.contains(&device.to_string()),
                "a paired device's id would enumerate the store's pairings: {answer}"
            );
        }
    }

    // Released, it is gone; bound to a listed terminal instead, the list says so and nothing else.
    assert_eq!(
        post_agent(&app, &holder, "/api/print/agent/revoke", UNLISTED)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        get_terminals(&app, &holder).await.2,
        json!({ "terminals": nobody_holds_a_listed_one })
    );
    assert_eq!(
        post_agent(&app, &holder, "/api/print/agent", BAR).await.0,
        StatusCode::OK
    );
    let (_, _, body) = get_terminals(&app, &holder).await;
    assert_eq!(held(&body), ["NONE", "THIS_DEVICE", "NONE"]);
    assert_eq!(
        body.get("unlisted_agent_device_id"),
        None,
        "a listed terminal is the list's to say: {body}"
    );
}

/// A store that publishes no terminal still names the one this device holds: the card has no list
/// there to release it from.
#[tokio::test]
async fn a_store_that_publishes_no_terminal_still_names_the_one_this_device_holds() {
    let (app, [(token, _), _]) = paired_pair_publishing(node(&[printer()])).await;
    sign_in(&app, &token, MANAGER_CODE).await;
    assert_eq!(
        post_agent(&app, &token, "/api/print/agent", UNLISTED)
            .await
            .0,
        StatusCode::OK
    );
    let (status, _, body) = get_terminals(&app, &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "terminals": [], "unlisted_agent_device_id": UNLISTED })
    );
}

/// A store that publishes no terminal, whether it publishes no node or only a printer, reads an
/// empty list: the till then says where terminals are created.
#[tokio::test]
async fn a_store_that_publishes_no_terminal_reads_an_empty_list() {
    for devices in [PublishedDevices::default(), node(&[printer()])] {
        let (app, [(token, _), _]) = paired_pair_publishing(devices).await;
        sign_in(&app, &token, MANAGER_CODE).await;
        let (status, _, body) = get_terminals(&app, &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "terminals": [] }));
    }
}

/// The read is refused where the claim is: to an unpaired caller, to a paired device with nobody
/// signed in, and to a signed-in person who may not manage devices.
#[tokio::test]
async fn the_read_is_refused_to_whoever_may_not_bind() {
    let (app, [(token, _), _]) = paired_pair_publishing(three_tills()).await;
    assert_eq!(
        get_terminals(&app, "not-a-token").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get_terminals(&app, &token).await.0,
        StatusCode::FORBIDDEN,
        "the signed-in gate answers before the permission is considered"
    );

    sign_in(&app, &token, WAITER_CODE).await;
    let (status, reason, body) = get_terminals(&app, &token).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(reason.as_deref(), Some("PERMISSION_DENIED"));
    assert_eq!(body, Value::Null, "no terminal is listed to a waiter");
}
