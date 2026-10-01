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
//! # A device that reconnects resumes, and one that cannot is told so
//!
//! A kitchen display that drops Wi-Fi for twenty seconds used to come back to the live stream and
//! nothing else: the fires, bumps and settles of those twenty seconds were simply never sent, and
//! nothing told it so. So every frame is numbered. [`Fanout::publish`] stamps it with this fan-out's
//! `stream_id` and the next `sequence`, and keeps the most recent [`REPLAY_CAPACITY`] frames in a
//! [`ReplayBuffer`]. A device that reconnects names the last frame it applied, and
//! [`Fanout::resume`] hands back every frame after it before the live stream carries on.
//!
//! The buffer is **one**, shared by every device, and bounded by count — not a backlog kept per
//! device, which is what ADR-0018 rules out. It lives in memory and nowhere else, so a restarted edge
//! holds nothing to replay; the `stream_id` is what lets it say so, since it is minted afresh with
//! each fan-out and a sequence means nothing on a stream it was not issued on. Whenever a resume
//! cannot be honoured — the frames after it have been evicted, the stream is another one, or the
//! position is from the future — the answer is [`Gap`], which the socket turns into the same resync a
//! lagging device gets. A device reloads rather than trusting a stream with a hole in it.
//!
//! Resuming from the durable event log instead, so that a restart could be ridden out as well, is
//! roadmap-v3 **B6.5**, and it is not a cheap read: the log pages by `event_id`, which is not commit
//! order (see `pos_ports::event_store`), and `/ws` is served over [`AppState`](crate::state::AppState),
//! which holds no store.
//!
//! # One serialisation, many sends
//!
//! [`Fanout::publish`] serialises a [`ServerMessage`] once and broadcasts the bytes as a shared
//! [`Arc<str>`], so N connected devices cost one JSON encode, not N. The replay buffer holds the same
//! `Arc`s, so keeping a frame to replay costs a pointer, not a copy.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pos_proto::ClockSource;
use pos_proto::ulid::Ulid;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::clock::SystemClock;

/// How many undelivered frames the fan-out holds per subscriber before it declares that subscriber
/// behind. Large enough to ride out a brief stall, small enough that a wedged device cannot pin
/// meaningful memory.
pub const FANOUT_CAPACITY: usize = 256;

/// How many of the most recent frames the fan-out keeps to replay to a device that reconnects.
///
/// Sized from the store's own numbers ([`docs/capacity-and-reliability.md`](../../../docs/capacity-and-reliability.md)
/// §1): one to three writes a second at peak, a settle being the largest at a handful of events. A
/// thousand frames is over a minute and a half of the busiest service at ten a second, and far longer
/// on an ordinary one, which covers a Wi-Fi blip or a tablet that slept for a moment. An event is
/// about a kilobyte, so the whole buffer is about a megabyte, against the 200–400 MB the edge runs
/// in. A device away for longer than this covers is past what a replay should carry anyway, and
/// reloads.
pub const REPLAY_CAPACITY: usize = 1024;

/// A pre-serialised server→client frame, shared cheaply across every subscriber.
pub type Frame = Arc<str>;

/// The fan-out channel a request handler publishes to and every WebSocket subscribes to.
///
/// Cloneable and cheap to clone: a clone is another handle to the one underlying channel and the one
/// replay buffer, which is why it can live in [`AppState`](crate::state::AppState) and be handed to
/// every handler.
#[derive(Debug, Clone)]
pub struct Fanout {
    sender: broadcast::Sender<Frame>,
    /// The frames a reconnecting device can be replayed. Behind a plain [`Mutex`] and never held
    /// across an `.await`: [`Self::publish`] takes it for one encode and one send, and
    /// [`Self::resume`] for one subscribe and a copy of at most [`REPLAY_CAPACITY`] pointers.
    replay: Arc<Mutex<ReplayBuffer>>,
    /// Which stream the sequences are issued on — a fresh one per fan-out, so per edge process.
    stream_id: Arc<str>,
}

impl Fanout {
    /// A fresh fan-out with [`FANOUT_CAPACITY`], a [`REPLAY_CAPACITY`] buffer, and a new stream.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _receiver) = broadcast::channel(FANOUT_CAPACITY);
        Self {
            sender,
            replay: Arc::new(Mutex::new(ReplayBuffer::new(REPLAY_CAPACITY))),
            stream_id: fresh_stream_id(),
        }
    }

    /// A receiver for one connected device, from the next frame on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.sender.subscribe()
    }

    /// A receiver for a device that reconnects, with every frame it missed since `after_sequence` on
    /// `stream_id` — or [`Gap`] when that cannot be told completely.
    ///
    /// Subscribes under the same lock [`Self::publish`] numbers and sends under, so every frame is
    /// either in the replay or on the receiver and never both: nothing is sent twice, and nothing
    /// falls between the two.
    #[must_use]
    pub fn resume(&self, stream_id: &str, after_sequence: u64) -> Resumed {
        let replay = self.lock_replay();
        let receiver = self.sender.subscribe();
        let missed = if stream_id == &*self.stream_id {
            replay.after(after_sequence)
        } else {
            // Another stream: a sequence this fan-out never issued, most often from before the edge
            // restarted. Replaying "after" it would pair it with frames that are not the ones missed.
            Err(Gap)
        };
        drop(replay);
        Resumed { missed, receiver }
    }

    /// Serialises `message` once, numbers it, keeps it to replay, and broadcasts it to every
    /// subscriber.
    ///
    /// Returns how many subscribers it reached, which is zero when no device is connected — an
    /// ordinary state, not an error, so it is reported rather than surfaced as a failure.
    #[expect(
        clippy::must_use_candidate,
        reason = "the reach count is advisory; publishing for its side effect and ignoring it is valid"
    )]
    pub fn publish(&self, message: &ServerMessage) -> usize {
        // Numbered, kept and sent under one lock, so the buffer and the channel hold frames in one
        // order and a sequence is never sent before the one below it. Two request handlers publish
        // concurrently whenever two tills commit at once.
        let mut replay = self.lock_replay();
        let numbered = Numbered {
            message,
            stream_id: &self.stream_id,
            sequence: replay.next_sequence(),
        };
        let Ok(json) = serde_json::to_string(&numbered) else {
            // A `ServerMessage` is built from owned, always-serialisable data; there is nothing a
            // caller could do about an encode failure, and dropping the frame is safer than panicking
            // in a request handler. The sequence is not consumed, so no device sees a hole for it.
            return 0;
        };
        let frame: Frame = Arc::from(json.as_str());
        replay.push(Arc::clone(&frame));
        self.sender.send(frame).unwrap_or(0)
    }

    /// Which stream this fan-out numbers its frames on.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// How many devices are currently subscribed.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Locks the replay buffer, recovering from a poisoned lock rather than propagating the panic.
    /// Every critical section leaves the buffer whole, so the value behind a poisoned lock is sound.
    fn lock_replay(&self) -> MutexGuard<'_, ReplayBuffer> {
        self.replay.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Fanout {
    fn default() -> Self {
        Self::new()
    }
}

/// A fresh stream identifier: the time the fan-out was made, and randomness.
///
/// Not a secret: it only has to differ from the stream before it. The process id stands in for the
/// randomness when the OS entropy source cannot be read, because two edges alive at once never share
/// one and a restart already moves the millisecond.
fn fresh_stream_id() -> Arc<str> {
    let millis = u64::try_from(SystemClock.now().as_milliseconds_since_epoch()).unwrap_or(0);
    let mut entropy = [0_u8; 16];
    let randomness = if getrandom::fill(&mut entropy).is_ok() {
        u128::from_le_bytes(entropy)
    } else {
        u128::from(std::process::id())
    };
    Arc::from(Ulid::from_parts(millis, randomness).to_string())
}

/// What [`Fanout::resume`] hands a reconnecting device.
#[derive(Debug)]
pub struct Resumed {
    /// Every frame after the device's position, oldest first — none when it is already current — or
    /// [`Gap`] when the fan-out cannot say.
    pub missed: Result<Vec<Frame>, Gap>,
    /// The live stream, from the first frame after the last one in `missed`.
    pub receiver: broadcast::Receiver<Frame>,
}

/// A resume that cannot be honoured: at least one frame after the position is no longer held, or was
/// never issued on this stream. The device must reload rather than trust what it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap;

/// The most recent frames, each at its sequence, for replaying to a device that reconnects.
///
/// Dense and in order: the frames held are exactly the sequences from [`Self::oldest_sequence`] to
/// [`Self::last_sequence`], which is what lets [`Self::after`] answer by position rather than search,
/// and tell a complete replay from one with a hole in it.
#[derive(Debug)]
pub struct ReplayBuffer {
    frames: VecDeque<Frame>,
    capacity: usize,
    /// The sequence of the newest frame, and `0` before the first — so the first frame is `1`, and a
    /// device that has applied nothing names `0`.
    last_sequence: u64,
}

impl ReplayBuffer {
    /// An empty buffer that keeps at most `capacity` frames. At `0` it keeps none, so any resume
    /// that missed a frame is a [`Gap`].
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
            last_sequence: 0,
        }
    }

    /// The sequence the next frame will be kept at.
    #[must_use]
    pub const fn next_sequence(&self) -> u64 {
        self.last_sequence.saturating_add(1)
    }

    /// The sequence of the newest frame kept, or `0` before the first.
    #[must_use]
    pub const fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// The sequence of the oldest frame still held — one past [`Self::last_sequence`] when none is.
    #[must_use]
    pub fn oldest_sequence(&self) -> u64 {
        let held = u64::try_from(self.frames.len()).unwrap_or(u64::MAX);
        self.last_sequence.saturating_sub(held).saturating_add(1)
    }

    /// How many frames are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether no frame is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Keeps `frame` at [`Self::next_sequence`], evicting the oldest once the buffer is full, and
    /// returns the sequence it was kept at.
    ///
    /// The frame's own bytes must already carry that sequence, which is why [`Fanout::publish`]
    /// reads [`Self::next_sequence`] before it serialises and calls this under the same lock.
    pub fn push(&mut self, frame: Frame) -> u64 {
        self.last_sequence = self.next_sequence();
        self.frames.push_back(frame);
        while self.frames.len() > self.capacity {
            self.frames.pop_front();
        }
        self.last_sequence
    }

    /// Every frame after `sequence`, oldest first.
    ///
    /// # Errors
    ///
    /// [`Gap`] when the answer would be incomplete: a frame after `sequence` has already been
    /// evicted, or `sequence` is newer than any frame issued, which no honest device can hold.
    pub fn after(&self, sequence: u64) -> Result<Vec<Frame>, Gap> {
        if sequence > self.last_sequence {
            return Err(Gap);
        }
        let first_missed = sequence.saturating_add(1);
        let oldest = self.oldest_sequence();
        if first_missed < oldest {
            return Err(Gap);
        }
        let skip = usize::try_from(first_missed.saturating_sub(oldest)).map_err(|_| Gap)?;
        Ok(self.frames.iter().skip(skip).cloned().collect())
    }
}

/// A message as it goes on the wire: its own fields, beside where it sits in the stream.
///
/// A wrapper rather than two more fields on every [`ServerMessage`] variant, because the position is
/// the fan-out's to assign — a caller building a message cannot know it — and flattening keeps the
/// frame one flat object, so a client dispatching on `type` reads it exactly as before and one that
/// predates the position ignores two fields it does not know.
#[derive(Serialize)]
struct Numbered<'a> {
    #[serde(flatten)]
    message: &'a ServerMessage,
    stream_id: &'a str,
    sequence: u64,
}

/// A message the edge pushes to a connected device.
///
/// Serialised with an internal `type` tag, so a client dispatches on one field. `#[non_exhaustive]`
/// because the domain routes (a later P5 slice) add message kinds, and a client built against an
/// older shape must ignore an unknown `type` rather than fail.
///
/// Every frame [`Fanout::publish`] sends also carries `stream_id` and `sequence`, the position a
/// reconnecting device names to be replayed what it missed. A `resync` the socket sends one device
/// on its own carries neither, because it is not part of the stream.
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
    /// The device fell behind the fan-out, or asked to resume from a position the fan-out cannot
    /// replay from, and must reload a fresh snapshot rather than trust an incomplete stream.
    Resync,
    /// The edge applied a new store configuration
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// item 7). An open till reloads what it draws from configuration — the floor, the menu, the
    /// layout, the locale and the reason codes — without a new sign-in. The reload reads whatever the
    /// edge holds by then, so the version only names what was applied.
    ConfigApplied {
        /// The cloud's id for the configuration version the edge now runs.
        config_version_id: String,
    },
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        FANOUT_CAPACITY, Fanout, Frame, Gap, REPLAY_CAPACITY, ReplayBuffer, ServerMessage,
    };
    use tokio::sync::broadcast::error::RecvError;

    fn event(index: usize) -> ServerMessage {
        ServerMessage::Event {
            event_type: format!("e{index}"),
            payload: serde_json::Value::Null,
        }
    }

    fn frame(text: &str) -> Frame {
        Arc::from(text)
    }

    fn texts(frames: &[Frame]) -> Vec<&str> {
        frames.iter().map(AsRef::as_ref).collect()
    }

    fn parse(frame: &Frame) -> serde_json::Value {
        serde_json::from_str(frame).expect("a json frame")
    }

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
            fanout.publish(&event(index));
        }

        match receiver.recv().await {
            Err(RecvError::Lagged(missed)) => {
                assert!(missed >= 1, "the subscriber was told it fell behind");
            }
            other => panic!("expected a Lagged signal, got {other:?}"),
        }
    }

    #[test]
    fn a_published_frame_carries_its_stream_and_the_next_sequence_beside_its_own_fields() {
        let fanout = Fanout::new();
        let mut receiver = fanout.subscribe();

        fanout.publish(&ServerMessage::Event {
            event_type: "sales.table.opened".to_owned(),
            payload: serde_json::json!({ "table_id": "T-7" }),
        });
        fanout.publish(&ServerMessage::Resync);

        let first = parse(&receiver.try_recv().expect("the first frame"));
        assert_eq!(
            first["type"], "event",
            "still one flat object tagged by type"
        );
        assert_eq!(first["event_type"], "sales.table.opened");
        assert_eq!(first["payload"]["table_id"], "T-7");
        assert_eq!(first["stream_id"], fanout.stream_id());
        assert_eq!(first["sequence"], 1, "the first frame on a stream is one");

        let second = parse(&receiver.try_recv().expect("the second frame"));
        assert_eq!(second["type"], "resync");
        assert_eq!(
            second["sequence"], 2,
            "every published frame is numbered, a resync too"
        );
    }

    #[test]
    fn every_fan_out_is_a_stream_of_its_own() {
        // What makes a sequence from before a restart unmistakable for one issued since.
        let first = Fanout::new();
        let second = Fanout::new();
        assert_ne!(first.stream_id(), second.stream_id());
        assert_eq!(first.stream_id().len(), 26, "a ULID, like every identifier");
        assert_eq!(
            first.clone().stream_id(),
            first.stream_id(),
            "a clone is a handle to the same stream"
        );
    }

    #[test]
    fn the_buffer_numbers_from_one_and_holds_no_more_than_its_capacity() {
        let mut buffer = ReplayBuffer::new(3);
        assert!(buffer.is_empty());
        assert_eq!(buffer.last_sequence(), 0);
        assert_eq!(buffer.next_sequence(), 1);

        for (index, text) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
            let kept_at = buffer.push(frame(text));
            assert_eq!(kept_at, u64::try_from(index + 1).expect("small"));
        }

        assert_eq!(buffer.len(), 3, "bounded: the two oldest were evicted");
        assert_eq!(buffer.oldest_sequence(), 3);
        assert_eq!(buffer.last_sequence(), 5);
    }

    #[test]
    fn the_shipped_buffer_is_bounded_too() {
        let fanout = Fanout::new();
        for index in 0..(REPLAY_CAPACITY + 10) {
            fanout.publish(&event(index));
        }
        let replay = fanout.lock_replay();
        assert_eq!(replay.len(), REPLAY_CAPACITY);
        assert_eq!(
            replay.last_sequence(),
            u64::try_from(REPLAY_CAPACITY + 10).expect("small")
        );
    }

    #[test]
    fn a_resume_is_replayed_every_frame_after_its_position_in_order() {
        let mut buffer = ReplayBuffer::new(4);
        for text in ["a", "b", "c", "d", "e", "f"] {
            buffer.push(frame(text));
        }
        // Held: 3 (c) … 6 (f).
        assert_eq!(texts(&buffer.after(4).expect("held")), ["e", "f"]);
        assert_eq!(
            texts(
                &buffer
                    .after(2)
                    .expect("the oldest held is the first missed")
            ),
            ["c", "d", "e", "f"]
        );
        assert!(
            buffer.after(6).expect("current").is_empty(),
            "a device that is current is replayed nothing"
        );
    }

    #[test]
    fn a_position_older_than_the_buffer_holds_is_a_gap() {
        let mut buffer = ReplayBuffer::new(4);
        for text in ["a", "b", "c", "d", "e", "f"] {
            buffer.push(frame(text));
        }
        // Frame 2 (b) is gone, so a device that applied only 1 cannot be told what it missed.
        assert_eq!(buffer.after(1), Err(Gap));
        assert_eq!(buffer.after(0), Err(Gap));
    }

    #[test]
    fn a_position_from_the_future_is_a_gap() {
        // A sequence no frame was issued at: from another process, or made up. Replaying nothing
        // would tell the device it is current when it is not.
        let mut buffer = ReplayBuffer::new(4);
        buffer.push(frame("a"));
        assert_eq!(buffer.after(2), Err(Gap));
        assert_eq!(ReplayBuffer::new(4).after(1), Err(Gap));
    }

    #[test]
    fn an_empty_stream_is_current_for_a_device_that_has_applied_nothing() {
        assert_eq!(ReplayBuffer::new(4).after(0), Ok(Vec::new()));
    }

    #[test]
    fn a_buffer_that_keeps_nothing_turns_every_miss_into_a_gap() {
        let mut buffer = ReplayBuffer::new(0);
        buffer.push(frame("a"));
        buffer.push(frame("b"));
        assert!(buffer.is_empty());
        assert_eq!(buffer.after(2), Ok(Vec::new()), "current is still current");
        assert_eq!(buffer.after(1), Err(Gap));
    }

    #[test]
    fn a_resume_replays_what_was_missed_and_then_continues_live_with_nothing_twice() {
        let fanout = Fanout::new();
        for index in 0..5 {
            fanout.publish(&event(index));
        }

        let mut resumed = fanout.resume(fanout.stream_id(), 2);
        let missed = resumed.missed.expect("frames 3 to 5 are held");
        let sequences: Vec<_> = missed
            .iter()
            .map(|frame| parse(frame)["sequence"].clone())
            .collect();
        assert_eq!(sequences, [3, 4, 5]);

        fanout.publish(&event(5));
        let live = parse(&resumed.receiver.try_recv().expect("the next live frame"));
        assert_eq!(
            live["sequence"], 6,
            "the live stream starts where the replay ended"
        );
        assert!(
            resumed.receiver.try_recv().is_err(),
            "and nothing from the replay arrives again"
        );
    }

    #[test]
    fn a_position_on_another_stream_is_a_gap_and_the_device_still_gets_the_live_stream() {
        let before_restart = Fanout::new();
        before_restart.publish(&event(0));
        before_restart.publish(&event(1));

        let fanout = Fanout::new();
        fanout.publish(&event(0));
        fanout.publish(&event(1));
        fanout.publish(&event(2));

        // Sequence 1 exists on both streams; only the stream says it is not this one's.
        let mut resumed = fanout.resume(before_restart.stream_id(), 1);
        assert_eq!(resumed.missed.map(|frames| frames.len()), Err(Gap));

        fanout.publish(&event(3));
        let live = parse(&resumed.receiver.try_recv().expect("live after the gap"));
        assert_eq!(live["sequence"], 4);
    }
}
