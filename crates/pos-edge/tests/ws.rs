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

use std::time::Duration;

use futures_util::StreamExt;
use pos_edge::{AppState, EdgeConfig, ServerMessage};
use pos_proto::ClockSource;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// The subprotocol name the edge selects; the token rides beside it.
const SUBPROTOCOL: &str = "pos-edge.v1";

/// A state whose pairing table already holds one issued device token — what a device has after it
/// redeemed a code, and what `/ws` now requires.
fn state_with_paired_device(store: u128) -> (AppState, String) {
    let store = StoreId::new(Ulid::from_u128(store));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("valid addr"), store);
    let state = AppState::new(config);
    let now = state.clock.now();
    let code = state.pairing.mint(now).expect("mint a pairing code");
    let token = state
        .pairing
        .redeem(&code, now)
        .expect("redeem does not fail")
        .expect("a live code yields a token");
    (state, token.as_str().to_owned())
}

/// A `/ws` upgrade request carrying the device token as a subprotocol — the browser's only channel,
/// since the `WebSocket` API cannot set a header.
fn request_with_subprotocol(
    addr: std::net::SocketAddr,
    token: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut request = format!("ws://{addr}/ws")
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
    let (state, token) = state_with_paired_device(1);
    let fanout = state.fanout.clone();
    let app = pos_edge::http::router(state);

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
    let (state, token) = state_with_paired_device(2);
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
    let (state, _token) = state_with_paired_device(3);
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
    let (state, _token) = state_with_paired_device(4);
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
    let (state, token) = state_with_paired_device(5);
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
