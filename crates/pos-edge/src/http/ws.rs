// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/ws` endpoint: one socket per device, fed by the fan-out.
//!
//! A device opens one WebSocket and receives a typed event stream; it never polls
//! ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)). Each socket subscribes to the
//! shared [`Fanout`](crate::fanout::Fanout) and forwards every frame. A device that falls behind is
//! told to reload ([`ServerMessage::Resync`]) rather than being sent stale frames or making the
//! server buffer on its behalf.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;

use crate::fanout::ServerMessage;
use crate::state::AppState;

/// Upgrades `GET /ws` to a WebSocket and pumps the fan-out into it.
pub(crate) async fn handler(upgrade: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    upgrade.on_upgrade(move |socket| pump(socket, state))
}

/// Forwards fan-out frames to one device until either side closes.
async fn pump(mut socket: WebSocket, state: AppState) {
    let mut feed = state.fanout.subscribe();
    loop {
        tokio::select! {
            // Server → device: forward each broadcast frame.
            received = feed.recv() => match received {
                Ok(frame) => {
                    if socket.send(Message::Text(frame.as_ref().into())).await.is_err() {
                        break; // the device went away
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // The device fell behind; tell it to reload rather than sending stale frames.
                    if send_resync(&mut socket).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break, // the server is shutting down
            },
            // Device → server: nothing is accepted yet. Drain frames so pings are answered (axum
            // auto-pongs) and a close is honoured.
            incoming = socket.recv() => match incoming {
                // The device closed, the stream ended, or it errored — all mean stop.
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                // Any other frame (text, binary, ping) is ignored; axum answers pings itself.
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Sends a resync instruction to a device that fell behind.
async fn send_resync(socket: &mut WebSocket) -> Result<(), axum::Error> {
    // `Resync` is a fixed, always-serialisable message; the fallback literal is defensive only.
    let json = serde_json::to_string(&ServerMessage::Resync)
        .unwrap_or_else(|_| "{\"type\":\"resync\"}".to_owned());
    socket.send(Message::Text(json.into())).await
}
