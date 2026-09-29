// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The WebSocket fan-out: a committed change reaches every *paired* device, and nothing reaches an
//! unpaired one.
//!
//! Unlike `tests/http.rs`, this binds a real ephemeral port and connects a real WebSocket client,
//! because a WebSocket upgrade is exactly what `oneshot` cannot exercise. It proves the fan-out path
//! end to end: a device connects, the edge publishes, and the device receives the frame. The 50 ms
//! budget is a property of the in-process broadcast (ADR-0018), so it is not asserted as a
//! wall-clock — a timing assertion on a shared CI runner would flake; delivery is what is proven.
//!
//! Every connection here presents a device token, because `/ws` is behind the paired-device gate
//! (roadmap-v3 S0c). Before that gate existed these same tests passed *without* a token, which is
//! how the hole survived: the fan-out was proven to work and never proven to be closed. The refusal
//! cases below are the other half.
//!
//! The resume cases at the end drive a device that drops its link and comes back naming the last
//! frame it applied: it is sent what it missed and then the live stream, or — when the edge cannot
//! replay from there — told to `resync`, and a device that names nothing is served exactly as before.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use pos_edge::fanout::REPLAY_CAPACITY;
use pos_edge::pairing::Minter;
use pos_edge::{AppState, EdgeConfig, Fanout, ServerMessage};
use pos_proto::ClockSource;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A connected device, as the test client holds it.
type Device = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The subprotocol name the edge selects; the token rides beside it.
const SUBPROTOCOL: &str = "pos-edge.v1";

/// A state whose pairing table already holds one issued device token — what a device has after it
/// redeemed a code, and what `/ws` now requires.
async fn state_with_paired_device(store: u128) -> (AppState, String) {
    let store = StoreId::new(Ulid::from_u128(store));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("valid addr"), store);
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

/// A `/ws` upgrade request carrying the device token as a subprotocol — the browser's only channel,
/// since the `WebSocket` API cannot set a header.
fn request_with_subprotocol(
    addr: SocketAddr,
    token: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    request_for(addr, "/ws", token)
}

/// [`request_with_subprotocol`] for a path that may carry a query, which is where a device that
/// reconnects names the last frame it applied.
fn request_for(
    addr: SocketAddr,
    path_and_query: &str,
    token: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut request = format!("ws://{addr}{path_and_query}")
        .into_client_request()
        .expect("a valid ws url");
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_str(&format!("{SUBPROTOCOL}, {token}")).expect("a valid header value"),
    );
    request
}

#[tokio::test]
async fn a_published_event_reaches_a_connected_device() {
    let (state, token) = state_with_paired_device(1).await;
    let fanout = state.fanout.clone();
    // Behind the compression layer the served application carries (ADR-0138): an upgrade is a
    // `101` with no body, and a layer that wrapped it anyway would break every kitchen display.
    let app = pos_edge::http::compress(pos_edge::http::router(state));

    // Bind an ephemeral port and serve in the background.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    // Connect a paired client. The handshake completing does not guarantee the server's socket task
    // has subscribed yet, so wait for the subscription before publishing (a broadcast reaches only
    // current subscribers).
    let (mut client, response) =
        tokio_tungstenite::connect_async(request_with_subprotocol(addr, &token))
            .await
            .expect("a paired websocket connects");
    assert_eq!(
        response
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .map(|value| value.to_str().unwrap_or_default()),
        Some(SUBPROTOCOL),
        "the server selects the protocol name and never echoes the token back"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fanout.subscriber_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server never subscribed the socket"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let reached = fanout.publish(&ServerMessage::Event {
        event_type: "table.opened".to_owned(),
        payload: serde_json::json!({ "table_id": "T-7" }),
    });
    assert_eq!(reached, 1, "the one connected device is reached");

    let frame = tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .expect("a frame arrives well within the timeout")
        .expect("the stream is open")
        .expect("a valid frame");

    let text = match frame {
        Message::Text(text) => text.to_string(),
        other => panic!("expected a text frame, got {other:?}"),
    };
    let value: serde_json::Value = serde_json::from_str(&text).expect("json frame");
    assert_eq!(value["type"], "event");
    assert_eq!(value["event_type"], "table.opened");
    assert_eq!(value["payload"]["table_id"], "T-7");

    server.abort();
}

#[tokio::test]
async fn two_devices_both_receive_the_same_change() {
    // The dine-in exit criterion has two devices on one table; both must see a change.
    let (state, token) = state_with_paired_device(2).await;
    let fanout = state.fanout.clone();
    let app = pos_edge::http::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    let (mut first, _) = tokio_tungstenite::connect_async(request_with_subprotocol(addr, &token))
        .await
        .expect("first connects");
    let (mut second, _) = tokio_tungstenite::connect_async(request_with_subprotocol(addr, &token))
        .await
        .expect("second connects");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fanout.subscriber_count() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "both sockets did not subscribe"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert_eq!(fanout.publish(&ServerMessage::Resync), 2);

    for client in [&mut first, &mut second] {
        let frame = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("a frame arrives")
            .expect("stream open")
            .expect("valid frame");
        let text = match frame {
            Message::Text(text) => text.to_string(),
            other => panic!("expected text, got {other:?}"),
        };
        assert!(text.contains("\"type\":\"resync\""));
    }

    server.abort();
}

#[tokio::test]
async fn an_unpaired_host_on_the_lan_is_refused() {
    // The hole S0c closed. `/ws` streams every committed event — orders, bills, settlements — so a
    // laptop plugged into the store switch reading it was a data breach with no command needed.
    let (state, _token) = state_with_paired_device(3).await;
    let fanout = state.fanout.clone();
    let app = pos_edge::http::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    let refused = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
    match refused {
        Err(WsError::Http(response)) => assert_eq!(
            response.status(),
            401,
            "an unpaired connection is refused with the same 401 every other gate gives"
        ),
        Err(other) => panic!("expected an HTTP 401, got {other:?}"),
        Ok(_) => panic!("an unpaired host must not reach the event stream"),
    }
    assert_eq!(
        fanout.subscriber_count(),
        0,
        "a refused connection never subscribes, so it cannot receive even one frame"
    );

    server.abort();
}

#[tokio::test]
async fn a_token_that_was_never_issued_is_refused() {
    // Well-formed but unknown: 32 lowercase hex characters that no pairing minted. It must land on
    // the same 401 as an absent token, so a probe cannot tell a bad token from no token.
    let (state, _token) = state_with_paired_device(4).await;
    let app = pos_edge::http::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    let forged = "f".repeat(32);
    let refused = tokio_tungstenite::connect_async(request_with_subprotocol(addr, &forged)).await;
    match refused {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 401),
        Err(other) => panic!("expected an HTTP 401, got {other:?}"),
        Ok(_) => panic!("a token that was never issued must not reach the event stream"),
    }

    server.abort();
}

#[tokio::test]
async fn a_non_browser_consumer_may_use_the_authorization_header() {
    // A browser cannot set this header, which is why the subprotocol channel exists — but a
    // third-party KDS or a script is not a browser, and should not have to learn the workaround.
    let (state, token) = state_with_paired_device(5).await;
    let fanout = state.fanout.clone();
    let app = pos_edge::http::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    let mut request = format!("ws://{addr}/ws")
        .into_client_request()
        .expect("a valid ws url");
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).expect("a valid header value"),
    );
    let (_client, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("a bearer-authenticated websocket connects");
    assert!(
        response
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .is_none(),
        "a client that offered no subprotocol negotiates none"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fanout.subscriber_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server never subscribed the socket"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    server.abort();
}

// ---- resuming after a drop ------------------------------------------------------------------

/// Serves the edge's router on an ephemeral port.
async fn serve(state: AppState) -> (SocketAddr, JoinHandle<()>) {
    let app = pos_edge::http::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (addr, server)
}

/// Waits until `count` sockets are subscribed, because a broadcast reaches only current subscribers
/// and the handshake completing does not mean the socket task has subscribed yet.
async fn await_subscribers(fanout: &Fanout, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fanout.subscriber_count() < count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server never subscribed the socket"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The next frame the edge sent, as JSON.
async fn next_json(device: &mut Device) -> serde_json::Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), device.next())
        .await
        .expect("a frame arrives well within the timeout")
        .expect("the stream is open")
        .expect("a valid frame");
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    serde_json::from_str(&text).expect("a json frame")
}

/// A committed event of `event_type`, as the application layer publishes one.
fn event(event_type: &str) -> ServerMessage {
    ServerMessage::Event {
        event_type: event_type.to_owned(),
        payload: serde_json::json!({ "order_id": "O-1" }),
    }
}

/// A `/ws` path that names a position to resume after.
fn resume_path(stream_id: &str, after_sequence: u64) -> String {
    format!("/ws?stream_id={stream_id}&after_sequence={after_sequence}")
}

#[tokio::test]
async fn a_device_that_reconnects_is_sent_what_it_missed_and_then_the_live_stream() {
    // The twenty-second Wi-Fi drop. The board saw the table open, lost its link while the order was
    // fired and bumped, and must come back to the fire and the bump rather than to silence.
    let (state, token) = state_with_paired_device(6).await;
    let fanout = state.fanout.clone();
    let (addr, server) = serve(state).await;

    let (mut board, _) = tokio_tungstenite::connect_async(request_with_subprotocol(addr, &token))
        .await
        .expect("a paired board connects");
    await_subscribers(&fanout, 1).await;
    fanout.publish(&event("sales.table.opened"));
    let seen = next_json(&mut board).await;
    assert_eq!(seen["event_type"], "sales.table.opened");
    let stream_id = seen["stream_id"].as_str().expect("a stream id").to_owned();
    let sequence = seen["sequence"].as_u64().expect("a sequence");

    // The link drops, and the store keeps selling.
    drop(board);
    fanout.publish(&event("sales.order_line.fired"));
    fanout.publish(&event("sales.order_line.bumped"));

    let (mut board, _) = tokio_tungstenite::connect_async(request_for(
        addr,
        &resume_path(&stream_id, sequence),
        &token,
    ))
    .await
    .expect("the board reconnects");
    let fired = next_json(&mut board).await;
    let bumped = next_json(&mut board).await;
    assert_eq!(fired["event_type"], "sales.order_line.fired");
    assert_eq!(fired["sequence"], sequence + 1);
    assert_eq!(bumped["event_type"], "sales.order_line.bumped");
    assert_eq!(bumped["sequence"], sequence + 2);

    // Then live, from the frame after the replay — nothing twice and nothing between.
    fanout.publish(&event("billing.bill.settled"));
    let settled = next_json(&mut board).await;
    assert_eq!(settled["type"], "event");
    assert_eq!(settled["event_type"], "billing.bill.settled");
    assert_eq!(settled["sequence"], sequence + 3);

    server.abort();
}

#[tokio::test]
async fn a_device_away_longer_than_the_edge_remembers_is_told_to_reload() {
    let (state, token) = state_with_paired_device(7).await;
    let fanout = state.fanout.clone();
    let stream_id = fanout.stream_id().to_owned();
    let (addr, server) = serve(state).await;

    // More than the buffer holds, so frame 2 — the first this device missed — is gone.
    for _ in 0..(REPLAY_CAPACITY + 5) {
        fanout.publish(&event("sales.order_line.added"));
    }

    let (mut device, _) =
        tokio_tungstenite::connect_async(request_for(addr, &resume_path(&stream_id, 1), &token))
            .await
            .expect("the device reconnects");
    let first = next_json(&mut device).await;
    assert_eq!(
        first["type"], "resync",
        "a gap is said, not replayed around"
    );
    assert!(
        first.get("sequence").is_none(),
        "a resync sent to one device is not part of the stream"
    );

    // The live stream still follows, so what the device reloads is not the last thing it hears.
    fanout.publish(&event("sales.order_line.fired"));
    let live = next_json(&mut device).await;
    assert_eq!(live["event_type"], "sales.order_line.fired");
    assert_eq!(
        live["sequence"],
        u64::try_from(REPLAY_CAPACITY + 6).expect("small")
    );

    server.abort();
}

#[tokio::test]
async fn a_position_from_before_the_edge_restarted_is_told_to_reload() {
    // The stream a device was reading before the edge restarted. Its sequences overlap this one's —
    // both have a frame 1 — so only the stream says the device's position is not on this edge.
    let before_restart = Fanout::new();
    before_restart.publish(&event("sales.table.opened"));

    let (state, token) = state_with_paired_device(8).await;
    let fanout = state.fanout.clone();
    let (addr, server) = serve(state).await;
    fanout.publish(&event("sales.table.opened"));
    fanout.publish(&event("sales.order_line.added"));

    let (mut device, _) = tokio_tungstenite::connect_async(request_for(
        addr,
        &resume_path(before_restart.stream_id(), 1),
        &token,
    ))
    .await
    .expect("the device reconnects");
    assert_eq!(next_json(&mut device).await["type"], "resync");

    server.abort();
}

#[tokio::test]
async fn a_device_that_was_live_but_can_name_no_frame_is_told_to_reload() {
    // A device whose link dropped before it saw a single frame has no stream to name. It asks to
    // resume anyway, from nothing, and the edge answers `resync` once the socket is subscribed — so
    // the reload cannot race the subscription the way one started on `open` could.
    let (state, token) = state_with_paired_device(9).await;
    let fanout = state.fanout.clone();
    let (addr, server) = serve(state).await;
    fanout.publish(&event("sales.order_line.fired"));

    let (mut device, _) =
        tokio_tungstenite::connect_async(request_for(addr, "/ws?after_sequence=0", &token))
            .await
            .expect("the device reconnects");
    assert_eq!(next_json(&mut device).await["type"], "resync");

    server.abort();
}

#[tokio::test]
async fn a_device_that_names_no_position_is_served_live_exactly_as_before() {
    // Every device built before resume existed. Frames it never asked for are not replayed to it,
    // and it is never told to resync for a position it did not name.
    let (state, token) = state_with_paired_device(10).await;
    let fanout = state.fanout.clone();
    let (addr, server) = serve(state).await;
    fanout.publish(&event("sales.table.opened"));
    fanout.publish(&event("sales.order_line.added"));

    let (mut device, _) = tokio_tungstenite::connect_async(request_with_subprotocol(addr, &token))
        .await
        .expect("an old device connects");
    await_subscribers(&fanout, 1).await;
    fanout.publish(&event("sales.order_line.fired"));

    let first = next_json(&mut device).await;
    assert_eq!(
        first["event_type"], "sales.order_line.fired",
        "the first frame is the next one published, as it always was"
    );
    assert_eq!(
        first["sequence"], 3,
        "carrying the position, which it may ignore"
    );

    server.abort();
}
