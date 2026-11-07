// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A personal value never reaches an event or a log: roadmap-v4 phase 3's exit criterion, proven
//! end to end through the till's routes
//! ([ADR-0194](../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 4, [ADR-0206](../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)).
//!
//! One made-up value goes through every path a personal value takes on a store: entered behind its
//! consent, changed, listed, read back by someone holding `data.personal.read`, refused to someone
//! who does not, its slip asked for, and erased. Every event the store logged, every answer but the
//! subject read, and every line the edge wrote to its log at any level are then searched for it. The
//! subject read and the subject store are searched too, and must hold it, so a search that can find
//! nothing proves nothing.

use std::io::Write;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

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
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::subject_store::SubjectStore;
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, EmployeeId, StoreId, SubjectId};
use pos_proto::package::PackageContent;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The made-up personal value. Nothing like it is a real person's.
const VALUE: &str = "DEMO-ONLY-PERSONAL-0000";
/// The value it is changed to.
const CHANGED: &str = "DEMO-ONLY-PERSONAL-1111";

const READER_CODE: &str = "R01";
const WRITER_CODE: &str = "W01";
const PIN: &str = "2468";
const PARTY: &str = "01J0000000000000000000CF02";
const CONTACT: &str = "01J0000000000000000000CF03";
const RESERVATION: &str = "01J00000000000000000000B01";
const CONSENT: &str = "data.consent.reservation";

/// Everything the edge logs, at every level, from every target.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A reservation type with a required party size and a personal contact note kept 30 days.
fn package() -> PackageContent {
    serde_json::from_value(json!({
        "package_id": "01J00000000000000000000P01",
        "package_version_id": "01J00000000000000000000V01",
        "version_label": "Reservations",
        "min_edge_version": "0.0.0",
        "data": { "object_types": [{ "object_type_id": RESERVATION, "label_key": "data.reservation",
            "fields": [
                { "custom_field_id": PARTY, "label_key": "data.reservation.party",
                  "field_type": "CUSTOM_FIELD_TYPE_INT", "required": true, "personal": false },
                { "custom_field_id": CONTACT, "label_key": "data.reservation.contact",
                  "field_type": "CUSTOM_FIELD_TYPE_TEXT", "personal": true, "retention_days": 30,
                  "consent_purpose_key": CONSENT }
            ] }] }
    }))
    .expect("the package parses")
}

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), b"a-fixed-test-slt")
        .expect("hash")
        .to_string()
}

fn staff(id: u128, permissions: PermissionSet) -> StaffAuth {
    StaffAuth {
        employee_id: Some(EmployeeId::new(Ulid::from_u128(id))),
        permissions,
        permissions_with_approval: PermissionSet::EMPTY,
        discount_ceiling: None,
        pin_phc: Some(hash_of(PIN)),
    }
}

/// The domain router over an edge running the package, the edge, and two paired devices' tokens.
async fn store() -> (Router, Arc<Edge<FakeStore>>, String, String) {
    let mut roster = StaffRoster::new();
    roster.insert(
        READER_CODE,
        staff(
            21,
            [Permission::WriteDataRecord, Permission::ReadPersonalData]
                .into_iter()
                .collect(),
        ),
    );
    roster.insert(
        WRITER_CODE,
        staff(22, [Permission::WriteDataRecord].into_iter().collect()),
    );
    let mut session = EdgeSession::bootstrap().with_staff(roster);
    session.package = Some(Arc::new(package()));
    // Each person decides with their own permissions, so the writer cannot read a value back.
    session.permissions_enforced = true;
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let mut tokens = Vec::new();
    for _ in 0..2 {
        let now = SystemClock.now();
        let (code, _) = pairing.mint(now, Minter::Boot).expect("mint a code");
        let token = pairing
            .redeem(&code, now)
            .await
            .expect("redeem")
            .token()
            .expect("a fresh code pairs a device")
            .as_str()
            .to_owned();
        tokens.push(token);
    }
    let app = pos_edge::http::domain_router(
        Arc::clone(&edge),
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let writer = tokens.pop().expect("a token");
    let reader = tokens.pop().expect("a token");
    (app, edge, reader, writer)
}

/// Sends one request on a paired device, answering the status and the body as text.
async fn send(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("routed");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn values(party: i64, note: &str) -> Value {
    json!([
        { "custom_field_id": PARTY, "integer": party },
        { "custom_field_id": CONTACT, "text": note },
    ])
}

/// Captures every line the edge logs from here on, at every level.
fn capture_logs() -> Captured {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();
    captured
}

/// Signs both people in, and walks the value through every path but the erasure. Answers every
/// route's answer but the subject read's, the subject, and the subject read's answer.
async fn walk(app: &Router, reader: &str, writer: &str) -> (Vec<String>, SubjectId, String) {
    let mut answers = Vec::new();
    for (token, code) in [(reader, READER_CODE), (writer, WRITER_CODE)] {
        let (status, body) = send(
            app,
            token,
            "POST",
            "/api/session/sign-in",
            Some(json!({ "code": code, "pin": PIN })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // Without its consent the value is refused, and nothing is written.
    let (status, body) = send(
        app,
        writer,
        "POST",
        "/api/custom-records",
        Some(json!({ "object_type_id": RESERVATION, "values": values(4, VALUE) })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    answers.push(body);

    // Entered behind its consent.
    let (status, body) = send(
        app,
        writer,
        "POST",
        "/api/custom-records",
        Some(
            json!({ "object_type_id": RESERVATION, "values": values(4, VALUE),
                     "consented_purpose_keys": [CONSENT] }),
        ),
    )
    .await;
    assert!(status.is_success(), "{body}");
    let saved: Value = serde_json::from_str(&body).expect("json");
    let record = saved["custom_record_id"]
        .as_str()
        .expect("a record")
        .to_owned();
    let subject: SubjectId = saved["subject_id"]
        .as_str()
        .expect("a subject holds the note")
        .parse()
        .expect("a subject id");
    answers.push(body);

    // Changed, listed, read, and its slip asked for.
    let (status, body) = send(
        app,
        writer,
        "POST",
        &format!("/api/custom-records/{record}/values"),
        Some(json!({ "object_type_id": RESERVATION,
                     "values": [{ "custom_field_id": CONTACT, "text": CHANGED }],
                     "consented_purpose_keys": [CONSENT] })),
    )
    .await;
    assert!(status.is_success(), "{body}");
    answers.push(body);
    for uri in [
        format!("/api/custom-records?object_type_id={RESERVATION}"),
        format!("/api/custom-records/{record}"),
    ] {
        let (status, body) = send(app, writer, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        answers.push(body);
    }
    let slip = format!("/api/custom-records/{record}/slip");
    answers.push(send(app, writer, "POST", &slip, None).await.1);

    // Read back: refused without `data.personal.read`, answered with it.
    let subject_uri = format!("/api/subjects/{subject}");
    let (status, body) = send(app, writer, "GET", &subject_uri, None).await;
    assert!(
        status.is_client_error() && body.contains("data.personal.read"),
        "{status} {body}"
    );
    answers.push(body);
    let (status, read_back) = send(app, reader, "GET", &subject_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    (answers, subject, read_back)
}

#[tokio::test]
async fn a_personal_value_reaches_no_event_and_no_log() {
    let captured = capture_logs();
    let (app, edge, reader, writer) = store().await;
    let (answers, subject, read_back) = walk(&app, &reader, &writer).await;
    assert!(
        read_back.contains(CHANGED),
        "the read answers the value, so the searches below can find one: {read_back}"
    );

    // Erased on the cloud's instruction.
    let box_device = DeviceId::new(Ulid::from_u128(7));
    assert_eq!(
        edge.erase_subjects(box_device, &[subject])
            .await
            .expect("erases"),
        1
    );

    let events = edge
        .store()
        .read(&EventQuery::first(
            StoreId::new(Ulid::from_u128(7)),
            NonZeroU32::new(1_000).expect("non-zero"),
        ))
        .await
        .expect("reads the log");
    assert!(events.len() >= 3, "the record, its change and the erasure");
    let logged = serde_json::to_string(&events).expect("the events encode");
    let log = captured.text();
    assert!(
        log.contains("data.subject.read"),
        "the capture holds the edge's log, its audit line too"
    );
    for value in [VALUE, CHANGED] {
        assert!(!logged.contains(value), "no event carries {value}");
        assert!(!log.contains(value), "no log line carries {value}");
        for answer in &answers {
            assert!(
                !answer.contains(value),
                "no answer carries {value}: {answer}"
            );
        }
    }
    let held = edge
        .store()
        .fetch(StoreId::new(Ulid::from_u128(7)), subject)
        .await
        .expect("reads")
        .expect("the subject is kept, masked");
    assert!(held.is_masked(), "the erasure masked it");
}
