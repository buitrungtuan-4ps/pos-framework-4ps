// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An agent claiming and acknowledging over HTTP
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! These two routes carry the **paired gate and no second one**, which is the thing worth pinning:
//! an agent is an unattended process, and requiring a sign-in would mean a manager's PIN before
//! every kitchen ticket. What stands in for the second gate is the *binding* — nothing in the
//! request names a terminal, so a paired device cannot claim to answer for one it does not hold.
//!
//! The park and the wake are exercised here rather than in a unit test because the property that
//! matters is end to end: a job enqueued while an agent is parked reaches that agent without
//! waiting out the park, and a second concurrent request is answered rather than parked.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_edge::print_agent::PrintAgents;
use pos_edge::print_queue::{InMemoryPrintQueue, PrintQueue};
use pos_edge::print_wake::{PrintWake, SharedPrintWake};
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StoreIdentity,
    SystemClock,
};
use pos_fakes::FakeStore;
use pos_ports::printer::{PrintBlock, PrintDocument, PrintJob, TextStyle};
use pos_proto::ClockSource;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::{DeviceId, EventId, StoreId};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// The `TERMINAL` entry the console created, as it reaches the store in the published `devices`
/// node — the id an agent is bound to, not the locally minted id its pairing gave it.
const TERMINAL: u128 = 0x7E_2117;
/// A second terminal, so "the other agent's job" is a real thing rather than a hypothetical.
const OTHER_TERMINAL: u128 = 0x7E_2118;
/// The printer whose transport the terminal owns.
const PRINTER: u128 = 0x9127;

fn id(seed: u128) -> DeviceId {
    DeviceId::new(Ulid::from_u128(seed))
}

/// A rendered kitchen ticket, of the kind the edge puts on the queue.
fn ticket(job_id: u128) -> PrintJob {
    PrintJob {
        job_id: EventId::new(Ulid::from_u128(job_id)),
        store_id: StoreId::new(Ulid::from_u128(3)),
        station_id: None,
        document: PrintDocument {
            blocks: vec![
                PrintBlock::Text {
                    line: "Phở bò tái".to_owned(),
                    style: TextStyle::default(),
                },
                PrintBlock::Cut,
            ],
        },
    }
}

/// Everything a test needs to stand on both sides of the queue at once.
struct Harness {
    router: Router,
    agents: Arc<pos_edge::print_agent::InMemoryPrintAgents>,
    queue: Arc<InMemoryPrintQueue>,
    wake: SharedPrintWake,
    /// The paired device bound to [`TERMINAL`].
    agent_token: String,
    /// A second paired device, bound to nothing until a test binds it.
    stranger_token: String,
    /// That second device's locally minted id, so a test can bind it to another terminal.
    stranger_device: DeviceId,
}

impl Harness {
    /// Mints two paired devices, binds the first to [`TERMINAL`], and composes the router over the
    /// same three seams the test holds.
    async fn new() -> Self {
        // The store publishes one printer, and that printer names the terminal as its agent — the
        // node the console publishes, which is where the address and the connection come from. A
        // job whose printer is not in this node has nowhere to go and is never handed over.
        let session = EdgeSession {
            devices: PublishedDevices::new(vec![PublishedDevice {
                device_id: id(PRINTER),
                kind: DeviceKind::Printer.into(),
                connection: DeviceConnection::Usb.into(),
                address: "/dev/usb/lp0".to_owned(),
                name: DisplayName::new("Counter"),
                station_id: None,
                agent_device_id: Some(id(TERMINAL)),
            }]),
            ..EdgeSession::bootstrap()
        };
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
        let mut tokens = Vec::new();
        let mut devices = Vec::new();
        for _ in 0..2 {
            let code = pairing.mint(now).expect("mint a pairing code");
            let token = pairing
                .redeem(&code, now)
                .await
                .expect("redeem")
                .token()
                .expect("a fresh code pairs a device");
            devices.push(
                pairing
                    .device_for(&token)
                    .expect("a freshly issued token resolves"),
            );
            tokens.push(token.as_str().to_owned());
        }
        // The binding a manager makes at the till (ADR-0112), already in place: what these routes
        // are about is what an agent does once one exists.
        let agents = Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new());
        agents
            .claim(id(TERMINAL), devices[0], now.as_milliseconds_since_epoch())
            .await
            .expect("the manager's claim is recorded");
        let queue = Arc::new(InMemoryPrintQueue::new());
        let wake = SharedPrintWake::new();
        let router = pos_edge::http::domain_router(
            edge,
            InMemoryQueueNumbers::new(),
            Arc::clone(&agents),
            Arc::clone(&queue),
            wake.clone(),
            pairing,
            Arc::new(Sessions::new()),
            &Arc::new(pos_edge::origins::Origins::new()),
        );
        Self {
            router,
            agents,
            queue,
            wake,
            agent_token: tokens.remove(0),
            stranger_token: tokens.remove(0),
            stranger_device: devices.remove(1),
        }
    }

    /// Puts a job on the bound agent's queue, the way the dispatch does.
    async fn enqueue(&self, job_id: u128) {
        self.enqueue_for(job_id, id(PRINTER)).await;
    }

    /// The same, for a printer of the caller's choosing — including one this store does not
    /// publish.
    async fn enqueue_for(&self, job_id: u128, printer: DeviceId) {
        let now = SystemClock.now().as_milliseconds_since_epoch();
        self.queue
            .enqueue(
                id(TERMINAL),
                printer,
                ticket(job_id),
                now,
                now + 600_000,
                200,
            )
            .await
            .expect("the queue takes it");
        self.wake.queued(id(TERMINAL));
    }

    async fn send(&self, method: &str, uri: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = self
            .router
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
            String::from_utf8(bytes.to_vec()).unwrap_or_default(),
        )
    }
}

/// The job ids in a `GET /api/print/jobs` body.
fn job_ids(body: &str) -> Vec<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).expect("json");
    parsed["jobs"]
        .as_array()
        .expect("jobs is an array")
        .iter()
        .map(|job| job["job"]["job_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn an_unpaired_request_reaches_neither_route() {
    // The one gate these routes do carry. Weaker than the domain surface, and not absent.
    let harness = Harness::new().await;
    for (method, uri) in [
        ("GET", "/api/print/jobs"),
        ("POST", "/api/print/jobs/00000000000000000000000001/ack"),
    ] {
        let (status, _) = harness.send(method, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn a_paired_device_that_answers_for_nothing_is_refused_without_parking() {
    // The binding is what says which terminal a caller answers for, and a device holding none has
    // no queue to read. Refused rather than parked: parking a caller that can never be signalled
    // would hold a socket for the whole park and answer empty at the end of it.
    let harness = Harness::new().await;
    let stranger = harness.stranger_token.clone();
    let refused = tokio::time::timeout(Duration::from_secs(5), async {
        harness
            .send("GET", "/api/print/jobs", Some(&stranger))
            .await
    })
    .await
    .expect("a device with no binding is answered, not parked");
    assert_eq!(refused.0, StatusCode::CONFLICT);
    assert!(
        refused.1.contains("manager"),
        "the refusal says what fixes it: {}",
        refused.1
    );

    let (status, _) = harness
        .send(
            "POST",
            "/api/print/jobs/00000000000000000000000001/ack",
            Some(&stranger),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "and the same on the ack");
}

#[tokio::test]
async fn a_bound_agent_claims_a_queued_job_and_acknowledging_it_deletes_it() {
    let harness = Harness::new().await;
    harness.enqueue(0x30B).await;
    let token = harness.agent_token.clone();

    let (status, body) = harness.send("GET", "/api/print/jobs", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let claimed = job_ids(&body);
    assert_eq!(claimed.len(), 1, "one job, for one printer: {body}");
    assert!(
        body.contains("Phở bò tái"),
        "the agent receives the rendered document: {body}"
    );

    let (status, _) = harness
        .send(
            "POST",
            &format!("/api/print/jobs/{}/ack", claimed[0]),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Gone from the queue, not merely unclaimed: acknowledging is what deletes it.
    let now = SystemClock.now().as_milliseconds_since_epoch();
    assert!(
        harness
            .queue
            .claim(id(TERMINAL), now + 60_000, now + 90_000)
            .await
            .expect("the queue answers")
            .is_empty(),
        "an acknowledged job does not come back when its lease lapses"
    );
}

#[tokio::test]
async fn one_agent_cannot_acknowledge_another_agent_s_job() {
    // The sharp end of the scoping: an acknowledgement *deletes* a document that exists nowhere
    // else, so a device answering for one terminal must not be able to delete a ticket queued for
    // another — which would be a dish the kitchen never sees, on a machine nobody is looking at.
    let harness = Harness::new().await;
    harness.enqueue(0x30C).await;
    let now = SystemClock.now().as_milliseconds_since_epoch();
    harness
        .agents
        .claim(id(OTHER_TERMINAL), harness.stranger_device, now)
        .await
        .expect("the second device is a legitimate agent for its own terminal");

    let (status, _) = harness
        .send(
            "POST",
            &format!(
                "/api/print/jobs/{}/ack",
                EventId::new(Ulid::from_u128(0x30C))
            ),
            Some(&harness.stranger_token.clone()),
        )
        .await;
    // `204`, the same answer a late or repeated acknowledgement gets — telling the caller it missed
    // would enumerate what other agents hold. What matters is what did *not* happen.
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = harness
        .send("GET", "/api/print/jobs", Some(&harness.agent_token.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        job_ids(&body).len(),
        1,
        "the job the other agent tried to acknowledge is still there: {body}"
    );
}

#[tokio::test]
async fn a_job_queued_while_an_agent_is_parked_reaches_it_without_waiting_out_the_park() {
    // ADR-0062's shape, one tier down: the process that writes the row is the process the waiter is
    // parked in, so nothing polls to discover a write it performed itself. Without the wake this
    // test would take the full `AGENT_PARK`.
    let harness = Arc::new(Harness::new().await);
    let parked = tokio::spawn({
        let harness = Arc::clone(&harness);
        let token = harness.agent_token.clone();
        async move { harness.send("GET", "/api/print/jobs", Some(&token)).await }
    });

    // Long enough for the handler to have subscribed, read an empty queue and parked.
    tokio::time::sleep(Duration::from_millis(100)).await;
    harness.enqueue(0x30D).await;

    let (status, body) = tokio::time::timeout(Duration::from_secs(5), parked)
        .await
        .expect("the wake arrives well inside the park")
        .expect("the parked request finishes");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job_ids(&body).len(), 1, "the enqueued job: {body}");
}

#[tokio::test]
async fn a_second_request_on_one_binding_is_answered_rather_than_parked() {
    // One agent parks once, so an agent cannot accumulate held connections against the edge.
    let harness = Arc::new(Harness::new().await);
    let parked = tokio::spawn({
        let harness = Arc::clone(&harness);
        let token = harness.agent_token.clone();
        async move { harness.send("GET", "/api/print/jobs", Some(&token)).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let token = harness.agent_token.clone();
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(5),
        harness.send("GET", "/api/print/jobs", Some(&token)),
    )
    .await
    .expect("the second request is answered rather than parked behind the first");
    assert_eq!(status, StatusCode::OK);
    assert!(job_ids(&body).is_empty(), "and it answers empty: {body}");

    parked.abort();
}

#[tokio::test]
async fn a_leased_job_carries_the_address_the_connection_and_the_edge_s_capabilities() {
    // ADR-0112's contract for the agent is *open this address, write these bytes, report this id*,
    // so all three travel with the job. The capabilities are the edge's own: an agent that computed
    // its own would be a second opinion about a printer's column width, and one that disagreed
    // about `prints_bitmaps` would refuse a raster the edge had just drawn for it.
    let harness = Harness::new().await;
    harness.enqueue(0x30E).await;
    let (status, body) = harness
        .send("GET", "/api/print/jobs", Some(&harness.agent_token.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);

    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    let job = &parsed["jobs"][0];
    assert_eq!(
        job["address"], "/dev/usb/lp0",
        "the published address: {body}"
    );
    assert_eq!(job["connection"], "Usb", "and how to open it: {body}");
    assert_eq!(
        job["capabilities"]["columns"], 42,
        "the width the edge rendered against: {body}"
    );
    assert_eq!(
        job["capabilities"]["kicks_drawer"], false,
        "no drawer is opened from anywhere yet (ADR-0100, ADR-0112)"
    );
}

#[tokio::test]
async fn a_job_whose_printer_is_no_longer_published_is_not_handed_over() {
    // Unpublished between the enqueue and the claim. There is nowhere to send it, and inventing an
    // address here would dial something at random; it expires at its TTL instead. The agent's other
    // printers still get their work, which is why this is a skip and not a failed claim.
    let harness = Harness::new().await;
    harness.enqueue_for(0x30F, id(0xDEAD)).await;
    harness.enqueue(0x310).await;

    let (status, body) = harness
        .send("GET", "/api/print/jobs", Some(&harness.agent_token.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        job_ids(&body),
        vec![EventId::new(Ulid::from_u128(0x310)).to_string()],
        "only the job whose printer this store still publishes: {body}"
    );
}
