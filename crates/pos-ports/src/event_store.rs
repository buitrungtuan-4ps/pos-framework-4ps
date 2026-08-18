// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The event log and the outbox.
//!
//! Two responsibilities in one port because they share a transaction and nothing else
//! can: `docs/architecture.md` §2 puts the outbox row and the state change in the same
//! commit, and splitting them across two ports would make that arrangement optional.
//!
//! # The three contract obligations
//!
//! `docs/architecture.md` §5 states them: ordered read-back, idempotency by ULID, and
//! survival of a crash mid-transaction. All three are checked by
//! `pos_contract_tests::event_store`, against every implementation including the fakes.
//!
//! # Why the cursor is not a ULID
//!
//! Events sort by `event_id`, so paging the outbox by ULID looks free. It loses writes.
//! If transaction 1 commits *A* and *C* while transaction 2 still holds *B*, with
//! `A < B < C`, then a reader acknowledging "through *C*" has skipped *B* — and because
//! acknowledgement is a high-water mark, *B* is not late, it is gone. The store server
//! is a single writer today, which hides it; the first adapter with concurrent writers
//! finds it in production as missing revenue. So the outbox is ordered by
//! [`OutboxPosition`], assigned at commit, and opaque. See
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §3.

use core::num::NonZeroU32;

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{EventId, StoreId};
use serde::{Deserialize, Serialize};

use core::future::Future;

use crate::error::PortError;
use crate::tx::Transactional;

/// A position in the outbox.
///
/// Assigned by the adapter **at commit**, monotone within a store, and deliberately
/// opaque: it is not an event identifier, not a timestamp, and not a row count. The only
/// operations a caller needs are comparison and round-tripping through storage, so those
/// are the only ones offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutboxPosition(u64);

impl OutboxPosition {
    /// The position before any event, so a fresh reader starts here.
    pub const START: Self = Self(0);

    /// Wraps an adapter-assigned position.
    #[must_use]
    pub const fn new(position: u64) -> Self {
        Self(position)
    }

    /// The underlying value, for an adapter persisting it.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One undelivered event, with the position that acknowledges it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRecord {
    /// Where this event sits in commit order.
    pub position: OutboxPosition,
    /// The event, still in its wire form — the outbox never needs to interpret it.
    pub envelope: EventEnvelope<RawPayload>,
}

/// What an append did.
///
/// Appending an event that is already stored is a **success**, not a conflict: the
/// retry that produced it is the same retry the outbox protocol requires, and failing it
/// would turn at-least-once delivery into a stuck queue. The counts are reported so a
/// caller can tell a genuine write from a replay, which is what the reconciliation job in
/// `docs/architecture.md` §8 needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppendOutcome {
    /// Events stored by this call.
    pub appended: u32,
    /// Events already present, matched by `event_id`, and therefore ignored.
    pub duplicates: u32,
}

/// Which events to read back.
///
/// A struct rather than four arguments, because three of the four are optional and a
/// positional call would be unreadable at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    /// The store whose log to read. Never optional: cross-store reads are not a
    /// capability this port offers, and in the cloud row-level security would refuse
    /// them anyway.
    pub store_id: StoreId,
    /// Exclusive lower bound. `None` starts at the beginning of the log.
    pub after: Option<EventId>,
    /// Most events to return.
    pub limit: NonZeroU32,
}

impl EventQuery {
    /// A query for the first `limit` events of a store's log.
    #[must_use]
    pub const fn first(store_id: StoreId, limit: NonZeroU32) -> Self {
        Self {
            store_id,
            after: None,
            limit,
        }
    }

    /// The same query continued after `event_id`.
    #[must_use]
    pub const fn after(mut self, event_id: EventId) -> Self {
        self.after = Some(event_id);
        self
    }
}

/// Appends and reads events, and drains the outbox.
///
/// # Contract
///
/// An implementation must satisfy all of the following. Every one is checked by the
/// shared suite; an adapter that passes is swappable, and one that does not is not.
///
/// 1. **Ordered read-back.** [`Self::read`] returns events in ascending `event_id`
///    order, and a query continued with [`EventQuery::after`] returns each event exactly
///    once with no gap and no repeat.
/// 2. **Idempotency by ULID.** Appending an `event_id` already stored stores nothing and
///    reports it as a duplicate. The stored copy wins; the incoming one is discarded
///    without comparison, because a byte-level difference at the same identifier is a
///    sender bug that a store cannot fix.
/// 3. **Survival of a crash mid-transaction.** After abrupt process or power loss, the
///    log contains every event from every committed transaction and no event from any
///    uncommitted one. The suite drives this through the harness, not through this trait
///    — see ADR-0026 §6.
/// 4. **Outbox monotonicity.** Positions increase in commit order, and
///    [`Self::acknowledge_outbox`] is a high-water mark: acknowledging the same position
///    twice is a no-op rather than an error or a double-removal.
pub trait EventStore: Transactional {
    /// Appends events and their outbox rows in the caller's transaction.
    ///
    /// The `tx` parameter is what makes an event written outside a transaction
    /// inexpressible. It is `&mut` so that two concurrent appends on one transaction do
    /// not compile.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if any envelope belongs to a different store than
    /// the rest, [`PortError::resource_exhausted`] if the outbox is at its configured
    /// depth, or [`PortError::unavailable`] if the store cannot be reached.
    fn append(
        &self,
        tx: &mut <Self as Transactional>::Tx,
        events: &[EventEnvelope<RawPayload>],
    ) -> impl Future<Output = Result<AppendOutcome, PortError>> + Send;

    /// Reads a page of the log.
    ///
    /// Outside a transaction on purpose: reads are the hot path for the cursor feed in
    /// `docs/architecture.md` §6.2 and must not queue behind a writer.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached. An unknown `store_id`
    /// yields an empty page rather than [`PortError::not_found`] — a store with no events
    /// and a store that does not exist are indistinguishable to a log, and pretending
    /// otherwise would make the reader's first poll a special case.
    fn read(
        &self,
        query: &EventQuery,
    ) -> impl Future<Output = Result<Vec<EventEnvelope<RawPayload>>, PortError>> + Send;

    /// Whether an event is already stored.
    ///
    /// Exists so an ingest path can answer "have I seen this?" without reading the
    /// payload back. `docs/capacity-and-reliability.md` puts ingest at 222 events per
    /// second sustained, so this is a hot query and deserves its own method rather than a
    /// one-element [`Self::read`].
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached.
    fn contains(
        &self,
        store_id: StoreId,
        event_id: EventId,
    ) -> impl Future<Output = Result<bool, PortError>> + Send;

    /// The next batch of events awaiting upload, in commit order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached.
    fn outbox_batch(
        &self,
        store_id: StoreId,
        after: OutboxPosition,
        limit: NonZeroU32,
    ) -> impl Future<Output = Result<Vec<OutboxRecord>, PortError>> + Send;

    /// Marks everything up to and including `through` as delivered.
    ///
    /// A high-water mark, so this is idempotent and out-of-order acknowledgements cannot
    /// resurrect an already-delivered event. Returns how many rows this call removed,
    /// which is zero for a repeat.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached.
    fn acknowledge_outbox(
        &self,
        store_id: StoreId,
        through: OutboxPosition,
    ) -> impl Future<Output = Result<u64, PortError>> + Send;

    /// How many events are waiting.
    ///
    /// The number behind the status bar's "Offline — selling normally" count
    /// (`docs/ui-ux.md` §4) and behind the queue-depth alert in
    /// `docs/capacity-and-reliability.md`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached.
    fn outbox_depth(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<u64, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{AppendOutcome, EventQuery, OutboxPosition};
    use core::num::NonZeroU32;
    use pos_proto::ids::{EventId, StoreId};
    use pos_proto::ulid::Ulid;

    #[test]
    fn a_fresh_reader_starts_before_every_event() {
        // START must sort below any position an adapter can assign, or the first batch is
        // silently skipped.
        assert!(OutboxPosition::START < OutboxPosition::new(1));
        assert_eq!(OutboxPosition::START.get(), 0);
    }

    #[test]
    fn positions_round_trip_through_storage() {
        let position = OutboxPosition::new(u64::MAX);
        let json = serde_json::to_string(&position).expect("serialise");
        assert_eq!(
            json,
            u64::MAX.to_string(),
            "transparent, so it stores as a number"
        );
        let back: OutboxPosition = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, position);
    }

    #[test]
    fn a_query_continues_from_where_the_last_page_ended() {
        let store = StoreId::new(Ulid::from_u128(7));
        let limit = NonZeroU32::new(100).expect("positive");
        let first = EventQuery::first(store, limit);
        assert!(first.after.is_none(), "the first page has no lower bound");

        let last_seen = EventId::new(Ulid::from_u128(42));
        let next = first.clone().after(last_seen);
        assert_eq!(next.after, Some(last_seen));
        assert_eq!(next.store_id, store, "continuing does not change the store");
    }

    #[test]
    fn an_append_that_stored_nothing_is_still_a_success() {
        // The shape of the type is the argument: a replayed batch reports duplicates and
        // returns Ok, because the outbox protocol depends on replay being harmless.
        let replayed = AppendOutcome {
            appended: 0,
            duplicates: 3,
        };
        assert_eq!(replayed.appended, 0);
        assert_eq!(
            AppendOutcome::default(),
            AppendOutcome {
                appended: 0,
                duplicates: 0
            }
        );
    }
}
