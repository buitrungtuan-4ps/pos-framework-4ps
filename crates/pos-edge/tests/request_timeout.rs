// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A request has a deadline, and the fan-out does not.
//!
//! Before [`pos_edge::http::time_out`] the edge would hold a connection open for as long as the
//! caller cared to keep it. A socket that sent a request line, declared a body and then sent nothing
//! held a connection and a task with no way back: the measurement below, run against the router
//! without the layer, gave up after five seconds rather than the server doing so.
//!
//! That is a real exposure rather than a tidy-up. A store's edge sits on a shop network, and
//! [ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md) puts a second origin
//! — a guest's phone, ordering from a QR code — on the other side of it. Nothing there should be
//! able to take a till's connections away from it by doing nothing at all.
//!
//! The second test is the more important one. The layer is applied to the whole application, which
//! includes `/ws`, and a timeout that severed every kitchen display after thirty seconds would be a
//! far worse fault than the one being fixed. A WebSocket upgrade answers `101` at once and the
//! socket lives on outside the request being timed, so it is unaffected — asserted here rather than
//! reasoned about.

use std::time::Duration;

use futures_util::StreamExt as _;
use pos_edge::pairing::Minter;
use pos_edge::{
    AppState, Edge, EdgeConfig, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing,
    ServerMessage, Sessions, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource as _;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use std::sync::Arc;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};

/// The subprotocol the edge selects; the device token rides beside it (see `tests/ws.rs`).
const SUBPROTOCOL: &str = "pos-edge.v1";

/// Short enough to assert against without holding the suite up.
///
/// The shipped budget is [`pos_edge::http::REQUEST_TIMEOUT`], thirty seconds, which is a figure
/// about a slow *caller* and not about how fast the store is. Waiting that long here would prove
/// nothing the layer's own arithmetic does not: what is under test is that a deadline exists and
/// where it is applied, so these build the layer themselves with a budget a test can wait out.
const TEST_BUDGET: Duration = Duration::from_millis(300);

/// A state whose pairing table already holds one issued device token, as `tests/ws.rs` builds.
async fn state_with_paired_device(store: u128) -> (AppState, String) {
    let store = StoreId::new(Ulid::from_u128(store));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("a valid address"), store);
    let state = AppState::new(config);
    let now = state.clock.now();
    let (code, _) = state
        .pairing
        .mint(now, Minter::Boot)
        .expect("mint a pairing code");
    let token = state
        .pairing
        .redeem(&code, now)
        .await
        .expect("redeem does not fail")
        .token()
        .expect("a live code yields a token");
    (state, token.as_str().to_owned())
}

/// Serves `app` on an ephemeral port, returning the address and the task to abort.
async fn serve(app: axum::Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (addr, server)
}

/// Opens a connection that declares a body and never sends one, and reads the status line — or
/// `None` if the server never answered within `patience`.
async fn stall(
    addr: std::net::SocketAddr,
    path: &str,
    token: Option<&str>,
    patience: Duration,
) -> Option<String> {
    let mut socket = TcpStream::connect(addr).await.expect("connect");
    let authorization = token.map_or_else(String::new, |token| {
        format!("Authorization: Bearer {token}\r\n")
    });
    let head = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         {authorization}\
         Content-Length: 40\r\n\r\n"
    );
    socket
        .write_all(head.as_bytes())
        .await
        .expect("write the head");
    let mut buffer = vec![0_u8; 256];
    match tokio::time::timeout(patience, socket.read(&mut buffer)).await {
        Ok(Ok(read)) if read > 0 => Some(
            String::from_utf8_lossy(&buffer[..read])
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
        ),
        _ => None,
    }
}

#[tokio::test]
async fn a_caller_that_stops_sending_is_answered_rather_than_held() {
    let (state, _) = state_with_paired_device(1).await;
    let app = pos_edge::http::time_out_for(pos_edge::http::router(state), TEST_BUDGET);
    let (addr, server) = serve(app).await;

    let status = stall(addr, "/api/pair", None, Duration::from_secs(5)).await;
    assert_eq!(
        status.as_deref(),
        Some("HTTP/1.1 408 Request Timeout"),
        "a caller that declared a body and sent none is told so, rather than holding the socket"
    );

    server.abort();
}

#[tokio::test]
async fn without_the_deadline_the_same_caller_holds_the_socket() {
    // The other half of the measurement, so the test above is known to be testing something. Five
    // seconds is not "forever", but it is sixteen times the budget the first test answers within —
    // and the connection is still open when this gives up.
    let (state, _) = state_with_paired_device(2).await;
    let (addr, server) = serve(pos_edge::http::router(state)).await;

    let status = stall(addr, "/api/pair", None, Duration::from_secs(5)).await;
    assert_eq!(
        status, None,
        "without the layer nothing answers, which is the exposure it closes"
    );

    server.abort();
}

#[tokio::test]
async fn a_kitchen_display_outlives_the_deadline() {
    let (state, token) = state_with_paired_device(3).await;
    let fanout = state.fanout.clone();
    let app = pos_edge::http::time_out_for(pos_edge::http::router(state), TEST_BUDGET);
    let (addr, server) = serve(app).await;

    let mut request = format!("ws://{addr}/ws")
        .into_client_request()
        .expect("a valid ws url");
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_str(&format!("{SUBPROTOCOL}, {token}")).expect("a valid header value"),
    );
    let (mut client, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("a paired websocket connects");

    // The handshake completing does not mean the server's socket task has subscribed yet, and a
    // broadcast reaches only current subscribers (`tests/ws.rs` waits the same way).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fanout.subscriber_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server never subscribed the socket"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Four times the budget. A board is connected for a whole service; this is the shortest wait
    // that means anything.
    tokio::time::sleep(TEST_BUDGET * 4).await;
    assert_eq!(
        fanout.subscriber_count(),
        1,
        "the display is still subscribed well past the request deadline"
    );

    let reached = fanout.publish(&ServerMessage::Event {
        event_type: "table.opened".to_owned(),
        payload: serde_json::json!({ "table_id": "T-7" }),
    });
    assert_eq!(reached, 1, "and is still reached by the fan-out");

    let frame = tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .expect("a frame arrives")
        .expect("the stream is open")
        .expect("a valid frame");
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let value: serde_json::Value = serde_json::from_str(&text).expect("json frame");
    assert_eq!(
        value["event_type"], "table.opened",
        "the food still reaches the board"
    );

    server.abort();
}

/// The deadline reaches a route merged in **after** it, which is the whole point of applying it to
/// the composed application.
///
/// This is the shape of the bug [ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)'s
/// version header had and nobody saw: `Router::layer` covers what is registered when it is called,
/// so a layer applied inside `http::router` reached three pairing routes and the asset fallback and
/// left the forty domain routes bare. Those are the routes a till sells through. A deadline with the
/// same gap would look present and protect nothing that matters.
///
/// `/api/session/sign-in` is the domain route used because it needs a paired device and nothing
/// else — a guarded route would refuse on the *signed-in* gate before the body was ever read, and
/// the socket would be answered promptly for the wrong reason. Here the gate passes, the handler
/// waits for a body that never comes, and the deadline is the only thing that ends it.
#[tokio::test]
async fn the_deadline_reaches_a_route_merged_after_it() {
    let store = StoreId::new(Ulid::from_u128(4));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("a valid address"), store);
    let pairing = Arc::new(Pairing::new());
    let state = AppState::new(config).with_pairing(Arc::clone(&pairing));
    let now = state.clock.now();
    let (code, _) = pairing
        .mint(now, Minter::Boot)
        .expect("mint a pairing code");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem does not fail")
        .token()
        .expect("a live code yields a token")
        .as_str()
        .to_owned();
    let origins = Arc::clone(&state.origins);
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("edge composes"),
    );

    // Composed the way `serve` composes: the infra router, the domain router merged into it, and
    // the deadline applied last.
    let app = pos_edge::http::router(state).merge(pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &origins,
    ));
    let (addr, server) = serve(pos_edge::http::time_out_for(app, TEST_BUDGET)).await;

    let status = stall(
        addr,
        "/api/session/sign-in",
        Some(&token),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        status.as_deref(),
        Some("HTTP/1.1 408 Request Timeout"),
        "a domain route merged after the layer is covered by it too"
    );

    server.abort();
}
