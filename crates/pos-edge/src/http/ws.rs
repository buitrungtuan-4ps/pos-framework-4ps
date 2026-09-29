// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/ws` endpoint: one socket per device, fed by the fan-out.
//!
//! A device opens one WebSocket and receives a typed event stream; it never polls
//! ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)). Each socket subscribes to the
//! shared [`Fanout`](crate::fanout::Fanout) and forwards every frame. A device that falls behind is
//! told to reload ([`ServerMessage::Resync`]) rather than being sent stale frames or making the
//! server buffer on its behalf.
//!
//! # Resuming after a drop
//!
//! A device that reconnects may say where it left off, with the `stream_id` and `sequence` of the
//! last frame it applied: `GET /ws?stream_id=<stream_id>&after_sequence=<sequence>`. The socket then
//! sends every frame after that one before the live stream, or — when the fan-out cannot replay from
//! there ([`Gap`]) — a `resync` first, so the device reloads instead of trusting a hole. The answer
//! is decided once the socket is already subscribed, so a reload it prompts cannot miss a frame
//! published while it runs.
//!
//! Additive: a device that sends neither parameter is served exactly as before, live from the next
//! frame. Anything else that is not a position the fan-out can honour — one parameter without the
//! other, a sequence that is not a number — is a gap, because a device that asked to resume has
//! said it may have missed something, and the safe reading of a request that cannot be understood is
//! that it did.

use axum::extract::rejection::QueryRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::fanout::{Frame, Gap, ServerMessage};
use crate::state::AppState;

/// The subprotocol name this endpoint speaks.
///
/// A browser client offers `[SUBPROTOCOL, <device token>]`, because the `WebSocket` API cannot set an
/// `Authorization` header and the token has to reach the gate somehow
/// ([`crate::http::auth::require_paired_device_ws`]). The server selects **this name** and never the
/// token, so the credential does not travel back in the handshake response.
pub(crate) const SUBPROTOCOL: &str = "pos-edge.v1";

/// Where a reconnecting device says it left off.
///
/// Both fields are read as text and the sequence is parsed here rather than by the extractor, so a
/// malformed one becomes a [`Gap`] the device can act on instead of a `400` it would reconnect into
/// forever.
#[derive(Debug, Deserialize)]
pub(crate) struct ResumeQuery {
    /// The stream the device's position is on.
    stream_id: Option<String>,
    /// The sequence of the last frame the device applied.
    after_sequence: Option<String>,
}

/// How a socket begins.
#[derive(Debug, PartialEq, Eq)]
enum Start {
    /// From the next frame, as every device did before resume existed.
    Live,
    /// After a position the device named.
    Resume {
        stream_id: String,
        after_sequence: u64,
    },
    /// The device asked to resume and named nothing the fan-out can honour.
    Unresumable,
}

impl Start {
    fn from_query(query: Result<Query<ResumeQuery>, QueryRejection>) -> Self {
        // `Query` ignores keys it does not name, so a query string with neither of these in it is a
        // device that predates resume, and the only way to fail is to repeat one of them.
        let Ok(Query(query)) = query else {
            return Self::Unresumable;
        };
        match (query.stream_id, query.after_sequence) {
            (None, None) => Self::Live,
            (Some(stream_id), Some(after_sequence)) => match after_sequence.parse() {
                Ok(after_sequence) => Self::Resume {
                    stream_id,
                    after_sequence,
                },
                Err(_) => Self::Unresumable,
            },
            (Some(_), None) | (None, Some(_)) => Self::Unresumable,
        }
    }
}

/// Upgrades `GET /ws` to a WebSocket and pumps the fan-out into it.
///
/// Reached only through the paired-device gate: an unpaired host on the store LAN is refused `401`
/// before this runs. Until S0c that gate was absent and this endpoint served the whole
/// committed-event stream — orders, bills, settlements — to anyone who could route to the box.
pub(crate) async fn handler(
    upgrade: WebSocketUpgrade,
    State(state): State<AppState>,
    query: Result<Query<ResumeQuery>, QueryRejection>,
) -> Response {
    let start = Start::from_query(query);
    // Echo the protocol *name* when the client offered it. A client that offered none negotiates
    // none, which RFC 6455 permits and which is what a non-browser consumer authenticating with the
    // `Authorization` header does.
    upgrade
        .protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| pump(socket, state, start))
}

/// Forwards fan-out frames to one device until either side closes, after whatever it missed.
async fn pump(mut socket: WebSocket, state: AppState, start: Start) {
    // Subscribed before anything is sent, on every path, so a frame published while the replay or
    // the resync is on its way is already queued for this socket.
    let (missed, mut feed) = match start {
        Start::Live => (Ok(Vec::new()), state.fanout.subscribe()),
        Start::Resume {
            stream_id,
            after_sequence,
        } => {
            let resumed = state.fanout.resume(&stream_id, after_sequence);
            (resumed.missed, resumed.receiver)
        }
        Start::Unresumable => (Err(Gap), state.fanout.subscribe()),
    };
    let caught_up = match missed {
        Ok(frames) => send_all(&mut socket, &frames).await,
        // The device cannot be told what it missed; tell it to reload instead.
        Err(Gap) => send_resync(&mut socket).await,
    };
    if caught_up.is_err() {
        return; // the device went away mid-replay
    }

    loop {
        tokio::select! {
            // Server → device: forward each broadcast frame.
            received = feed.recv() => match received {
                Ok(frame) => {
                    if send_frame(&mut socket, &frame).await.is_err() {
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

/// Sends one fan-out frame.
async fn send_frame(socket: &mut WebSocket, frame: &Frame) -> Result<(), axum::Error> {
    socket.send(Message::Text(frame.as_ref().into())).await
}

/// Sends the frames a reconnecting device missed, oldest first — at most
/// [`REPLAY_CAPACITY`](crate::fanout::REPLAY_CAPACITY) of them.
async fn send_all(socket: &mut WebSocket, frames: &[Frame]) -> Result<(), axum::Error> {
    for frame in frames {
        send_frame(socket, frame).await?;
    }
    Ok(())
}

/// Sends a resync instruction to a device that fell behind, or that cannot be resumed.
async fn send_resync(socket: &mut WebSocket) -> Result<(), axum::Error> {
    // `Resync` is a fixed, always-serialisable message; the fallback literal is defensive only.
    let json = serde_json::to_string(&ServerMessage::Resync)
        .unwrap_or_else(|_| "{\"type\":\"resync\"}".to_owned());
    socket.send(Message::Text(json.into())).await
}

#[cfg(test)]
mod tests {
    use axum::extract::Query;

    use super::{ResumeQuery, Start};

    fn query(stream_id: Option<&str>, after_sequence: Option<&str>) -> Start {
        Start::from_query(Ok(Query(ResumeQuery {
            stream_id: stream_id.map(str::to_owned),
            after_sequence: after_sequence.map(str::to_owned),
        })))
    }

    #[test]
    fn a_device_that_names_no_position_is_served_live_as_before() {
        assert_eq!(query(None, None), Start::Live);
    }

    #[test]
    fn a_device_that_names_a_position_resumes_from_it() {
        assert_eq!(
            query(Some("S"), Some("41")),
            Start::Resume {
                stream_id: "S".to_owned(),
                after_sequence: 41
            }
        );
    }

    #[test]
    fn half_a_position_or_an_unreadable_one_is_unresumable() {
        assert_eq!(query(Some("S"), None), Start::Unresumable);
        assert_eq!(query(None, Some("41")), Start::Unresumable);
        assert_eq!(query(Some("S"), Some("forty")), Start::Unresumable);
        assert_eq!(query(Some("S"), Some("-1")), Start::Unresumable);
    }
}
