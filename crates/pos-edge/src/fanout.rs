// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store-LAN fan-out: one committed change, pushed to every device.
//!
//! When the edge applies a decision, every device showing that table, order or bill must see it
//! fast enough that two people ringing up one table never fight over stale state. The budget is
//! **under 50 ms** ([`docs/capacity-and-reliability.md`](../../../docs/capacity-and-reliability.md)),
//! and this meets it by construction: the fan-out is a single in-process
//! [`tokio::sync::broadcast`] channel ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)), so
//! delivery is a clone and a send, not a round trip.
//!
//! # Bounded, so a slow device degrades itself
//!
//! The channel has a fixed capacity ([`FANOUT_CAPACITY`]). A device that falls behind — a tablet
//! asleep in a drawer — does not make the server buffer an unbounded backlog on its behalf: it
//! receives a lag signal and is told to reload a fresh snapshot ([`ServerMessage::Resync`]). Memory
//! is bounded by design, the same discipline the SQLite writer uses
//! ([ADR-0015](../../../docs/adr/0015-sqlite-access.md)).
//!
//! # One serialisation, many sends
//!
//! [`Fanout::publish`] serialises a [`ServerMessage`] once and broadcasts the bytes as a shared
//! [`Arc<str>`], so N connected devices cost one JSON encode, not N.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

/// How many undelivered frames the fan-out holds per subscriber before it declares that subscriber
/// behind. Large enough to ride out a brief stall, small enough that a wedged device cannot pin
/// meaningful memory.
pub const FANOUT_CAPACITY: usize = 256;

/// A pre-serialised server→client frame, shared cheaply across every subscriber.
pub type Frame = Arc<str>;

/// The fan-out channel a request handler publishes to and every WebSocket subscribes to.
///
/// Cloneable and cheap to clone: a clone is another handle to the one underlying channel, which is
/// why it can live in [`AppState`](crate::state::AppState) and be handed to every handler.
#[derive(Debug, Clone)]
pub struct Fanout {
    sender: broadcast::Sender<Frame>,
}

impl Fanout {
    /// A fresh fan-out with [`FANOUT_CAPACITY`].
    #[must_use]
    pub fn new() -> Self {
        let (sender, _receiver) = broadcast::channel(FANOUT_CAPACITY);
        Self { sender }
    }

    /// A receiver for one connected device.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.sender.subscribe()
    }

    /// Serialises `message` once and broadcasts it to every subscriber.
    ///
    /// Returns how many subscribers it reached, which is zero when no device is connected — an
    /// ordinary state, not an error, so it is reported rather than surfaced as a failure.
    #[expect(
        clippy::must_use_candidate,
        reason = "the reach count is advisory; publishing for its side effect and ignoring it is valid"
    )]
    pub fn publish(&self, message: &ServerMessage) -> usize {
        let Ok(json) = serde_json::to_string(message) else {
            // A `ServerMessage` is built from owned, always-serialisable data; there is nothing a
            // caller could do about an encode failure, and dropping the frame is safer than panicking
            // in a request handler.
            return 0;
        };
        self.sender.send(Arc::from(json.as_str())).unwrap_or(0)
    }

    /// How many devices are currently subscribed.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for Fanout {
    fn default() -> Self {
        Self::new()
    }
}

/// A message the edge pushes to a connected device.
///
/// Serialised with an internal `type` tag, so a client dispatches on one field. `#[non_exhaustive]`
/// because the domain routes (a later P5 slice) add message kinds, and a client built against an
/// older shape must ignore an unknown `type` rather than fail.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    /// A committed domain event. `event_type` is the dotted wire name; `payload` is its body.
    Event {
        /// The dotted event type, e.g. `table.opened`.
        event_type: String,
        /// The event body.
        payload: serde_json::Value,
    },
    /// The device fell behind the fan-out and must reload a fresh snapshot rather than trust an
    /// incomplete stream.
    Resync,
}

#[cfg(test)]
mod tests {
    use super::{FANOUT_CAPACITY, Fanout, ServerMessage};
    use tokio::sync::broadcast::error::RecvError;

    #[test]
    fn messages_carry_a_type_tag() {
        let resync = serde_json::to_value(ServerMessage::Resync).expect("serialises");
        assert_eq!(resync["type"], "resync");

        let event = serde_json::to_value(ServerMessage::Event {
            event_type: "table.opened".to_owned(),
            payload: serde_json::json!({ "table_id": "T-7" }),
        })
        .expect("serialises");
        assert_eq!(event["type"], "event");
        assert_eq!(event["event_type"], "table.opened");
    }

    #[tokio::test]
    async fn a_subscriber_receives_a_published_frame() {
        let fanout = Fanout::new();
        let mut receiver = fanout.subscribe();
        assert_eq!(fanout.subscriber_count(), 1);

        let reached = fanout.publish(&ServerMessage::Resync);
        assert_eq!(reached, 1);

        let frame = receiver.recv().await.expect("a frame");
        assert!(frame.contains("\"type\":\"resync\""));
    }

    #[test]
    fn publishing_with_no_subscribers_reaches_nobody_and_is_not_an_error() {
        let fanout = Fanout::new();
        assert_eq!(fanout.publish(&ServerMessage::Resync), 0);
    }

    #[tokio::test]
    async fn a_slow_subscriber_lags_rather_than_growing_unboundedly() {
        let fanout = Fanout::new();
        let mut receiver = fanout.subscribe();

        // Overfill the channel without reading it: the oldest frames are dropped, which is the
        // bounded-memory guarantee the WebSocket layer turns into a resync.
        for index in 0..(FANOUT_CAPACITY + 2) {
            fanout.publish(&ServerMessage::Event {
                event_type: format!("e{index}"),
                payload: serde_json::Value::Null,
            });
        }

        match receiver.recv().await {
            Err(RecvError::Lagged(missed)) => {
                assert!(missed >= 1, "the subscriber was told it fell behind")
            }
            other => panic!("expected a Lagged signal, got {other:?}"),
        }
    }
}
