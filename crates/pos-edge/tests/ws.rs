// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The WebSocket fan-out: a committed change reaches every connected device.
//!
//! Unlike `tests/http.rs`, this binds a real ephemeral port and connects a real WebSocket client,
//! because a WebSocket upgrade is exactly what `oneshot` cannot exercise. It proves the fan-out path
//! end to end: a device connects, the edge publishes, and the device receives the frame. The 50 ms
//! budget is a property of the in-process broadcast (ADR-0018), so it is not asserted as a
//! wall-clock — a timing assertion on a shared CI runner would flake; delivery is what is proven.

use std::time::Duration;

use futures_util::StreamExt;
use pos_edge::{AppState, EdgeConfig, ServerMessage};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn a_published_event_reaches_a_connected_device() {
    let store = StoreId::new(Ulid::from_u128(1));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("valid addr"), store);
    let state = AppState::new(config);
    let fanout = state.fanout.clone();
    let app = pos_edge::http::router(state);

    // Bind an ephemeral port and serve in the background.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    // Connect a client. The handshake completing does not guarantee the server's socket task has
    // subscribed yet, so wait for the subscription before publishing (a broadcast reaches only
    // current subscribers).
    let (mut client, _response) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("websocket connects");

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
    let store = StoreId::new(Ulid::from_u128(2));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("valid addr"), store);
    let state = AppState::new(config);
    let fanout = state.fanout.clone();
    let app = pos_edge::http::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    let url = format!("ws://{addr}/ws");
    let (mut first, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("first connects");
    let (mut second, _) = tokio_tungstenite::connect_async(&url)
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
