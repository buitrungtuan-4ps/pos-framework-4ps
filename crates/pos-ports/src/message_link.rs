// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The durable store → cloud channel.
//!
//! # This port has no transaction, and cannot have one
//!
//! NATS is a separate system and no two-phase commit exists between it and SQLite. Any
//! design that appears to publish transactionally is either losing events on a crash
//! between commit and publish, or publishing them twice. That is not a limitation to work
//! around — it is the reason the outbox exists, and it fixes the delivery guarantee at
//! **at-least-once**: commit, publish, acknowledge, and a crash anywhere in that sequence
//! replays. Consumers are idempotent by ULID, which
//! [`crate::EventStore`]'s contract already requires. See
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §4.
//!
//! # Outbound only
//!
//! `docs/architecture.md` §3 makes the link one-directional by design: the store never
//! waits on the cloud, so the cloud needs no automatic failover. Configuration arrives by
//! the store *pulling*, not by the cloud pushing down this channel.

use core::num::NonZeroU32;

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::protocol::{Hello, HelloOutcome};

use core::future::Future;

use crate::error::PortError;

/// What a publish achieved.
///
/// # Why acceptance is a prefix
///
/// A batch of fifty events may be accepted as the first thirty. Reporting a count rather
/// than a boolean lets the caller acknowledge exactly those thirty and retry the rest,
/// which keeps the outbox draining instead of restarting the whole batch on every partial
/// failure. Reporting only success or failure would make a link that is 60% healthy behave
/// like one that is 0% healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PublishOutcome {
    /// How many events, counted from the start of the batch, the cloud durably accepted.
    pub accepted: u32,
}

impl PublishOutcome {
    /// Every event in a batch of `count` was accepted.
    #[must_use]
    pub const fn all(count: u32) -> Self {
        Self { accepted: count }
    }

    /// Whether the whole batch landed.
    #[must_use]
    pub const fn is_complete(self, batch_size: u32) -> bool {
        self.accepted >= batch_size
    }
}

/// How much room is left on the far side.
///
/// `docs/capacity-and-reliability.md` puts an alert at 80% of a JetStream stream's
/// `max_bytes` or `max_age`, and the failure it guards against is subtle: a full stream
/// halts synchronisation silently while stores keep selling and their outboxes keep
/// growing. So the number has to be observable from the store side, not only from the
/// broker's own metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkCapacity {
    /// Messages the far side currently holds.
    pub messages: u64,
    /// The most it will hold before refusing, or `None` if the adapter cannot say.
    pub message_limit: Option<u64>,
    /// Bytes the far side currently holds.
    pub bytes: u64,
    /// The most it will hold before refusing, or `None` if the adapter cannot say.
    pub byte_limit: Option<u64>,
}

impl LinkCapacity {
    /// Whether either limit is at or past `percent` of capacity.
    ///
    /// Integer arithmetic on purpose — `clippy.toml` bans floating point across the whole
    /// workspace, and a percentage of a byte count has no business being approximate.
    /// Widened to `u128` rather than saturated: `u64::MAX / 100 * 100` saturates against a
    /// limit of `u64::MAX`, which would report a stream at one percent as *full* and make
    /// the alert cry wolf until nobody reads it. A limit of zero is reported as full rather
    /// than divided by.
    #[must_use]
    pub fn is_at_least(self, percent: u32) -> bool {
        fn crosses(used: u64, limit: Option<u64>, percent: u32) -> bool {
            match limit {
                None => false,
                Some(0) => true,
                Some(limit) => u128::from(used) * 100 >= u128::from(limit) * u128::from(percent),
            }
        }
        crosses(self.messages, self.message_limit, percent)
            || crosses(self.bytes, self.byte_limit, percent)
    }
}

/// Carries events from a store to its cloud.
///
/// # Contract
///
/// 1. **At-least-once, never at-most-once.** A publish that returns an error may still
///    have delivered. An adapter must never respond to an ambiguous result by discarding
///    events — the outbox is what makes the retry safe.
/// 2. **Acceptance is a prefix.** If [`PublishOutcome::accepted`] is *n*, then exactly the
///    first *n* events of the batch are durable on the far side. Returning *n* while
///    having accepted a different subset would corrupt the caller's cursor.
/// 3. **The handshake happens once per connection, not once per publish.** ADR-0024 fixes
///    that, and it is why [`Self::handshake`] is separate: a version check on every
///    message would be both wasteful and a different protocol.
/// 4. **Refusal degrades to "not syncing", never to "not selling".** An adapter that
///    receives [`HelloOutcome::Refused`] reports it and stops publishing; it must not
///    signal anything that could reach a sales path.
pub trait MessageLink: Send + Sync {
    /// Negotiates the protocol version and the lease, once per connection.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the cloud cannot be reached. A *refusal* is not an
    /// error — it is [`HelloOutcome::Refused`] in the success path, because the store
    /// carries on selling either way and only the caller can decide what to log.
    fn handshake(
        &self,
        hello: &Hello,
    ) -> impl Future<Output = Result<HelloOutcome, PortError>> + Send;

    /// Publishes a batch, in order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the cloud cannot be reached,
    /// [`PortError::resource_exhausted`] if the far side is full, or
    /// [`PortError::failed_precondition`] if no handshake has succeeded on this
    /// connection.
    fn publish(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> impl Future<Output = Result<PublishOutcome, PortError>> + Send;

    /// How full the far side is.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the cloud cannot be reached.
    fn capacity(&self) -> impl Future<Output = Result<LinkCapacity, PortError>> + Send;

    /// The largest batch this link accepts in one call.
    ///
    /// Synchronous and non-failing because it is a property of the adapter's
    /// configuration, not a question for the far side. The caller needs it to size an
    /// outbox read, and a fallible async call there would mean two round trips to drain
    /// one batch.
    #[must_use]
    fn max_batch_size(&self) -> NonZeroU32;
}

#[cfg(test)]
mod tests {
    use super::{LinkCapacity, PublishOutcome};

    #[test]
    fn a_partial_accept_is_a_prefix_the_caller_can_act_on() {
        let outcome = PublishOutcome { accepted: 30 };
        assert!(
            !outcome.is_complete(50),
            "twenty events still need retrying"
        );
        assert!(outcome.is_complete(30));
        assert!(PublishOutcome::all(50).is_complete(50));
    }

    #[test]
    fn the_eighty_percent_alert_fires_on_either_limit() {
        let by_messages = LinkCapacity {
            messages: 800,
            message_limit: Some(1_000),
            bytes: 0,
            byte_limit: Some(1_000_000),
        };
        assert!(by_messages.is_at_least(80));
        assert!(!by_messages.is_at_least(81));

        let by_bytes = LinkCapacity {
            messages: 0,
            message_limit: Some(1_000),
            bytes: 900_000,
            byte_limit: Some(1_000_000),
        };
        assert!(by_bytes.is_at_least(80), "one limit crossing is enough");
    }

    #[test]
    fn an_unbounded_stream_never_alerts_and_a_zero_limit_always_does() {
        let unbounded = LinkCapacity {
            messages: u64::MAX,
            message_limit: None,
            bytes: u64::MAX,
            byte_limit: None,
        };
        assert!(
            !unbounded.is_at_least(1),
            "no limit means no percentage of one"
        );

        let refuses_everything = LinkCapacity {
            messages: 0,
            message_limit: Some(0),
            bytes: 0,
            byte_limit: None,
        };
        assert!(
            refuses_everything.is_at_least(80),
            "a limit of zero is full, not a division by zero"
        );
    }

    #[test]
    fn a_huge_limit_does_not_make_a_near_empty_stream_look_full() {
        // The regression this guards: `used * 100` and `limit * percent` both saturate in
        // u64 long before u64::MAX, and saturating both sides makes `1% >= 80%` come out
        // true. An alert that fires at one percent is an alert nobody reads.
        let barely_used = LinkCapacity {
            messages: u64::MAX / 100,
            message_limit: Some(u64::MAX),
            bytes: 0,
            byte_limit: None,
        };
        assert!(!barely_used.is_at_least(80));

        let genuinely_full = LinkCapacity {
            messages: u64::MAX,
            message_limit: Some(u64::MAX),
            bytes: 0,
            byte_limit: None,
        };
        assert!(genuinely_full.is_at_least(80));
    }
}
