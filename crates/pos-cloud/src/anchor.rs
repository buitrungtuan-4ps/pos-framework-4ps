// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's half of the chained event log
//! ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
//!
//! # Why a chain needs this to mean anything
//!
//! [`pos_proto::chain`] says it plainly: a hash chain on its own catches careless editing and
//! storage corruption, not deliberate fraud. The hash function is in the source, so anyone holding
//! the database file can edit a record, re-derive every later link, and produce a chain that
//! verifies. `crates/adapters/store-sqlite/tests/chain.rs` has two tests that assert exactly this —
//! they exist to stop anyone believing otherwise.
//!
//! What a store **cannot** do is rewrite what the cloud has already received. Each shift close
//! publishes a `store.chain.anchored` event carrying the chain's length and its head. This module is
//! what the cloud does with them: it keeps them, and it refuses one that contradicts a record it
//! already holds.
//!
//! # The one contradiction this closes, and the one it does not
//!
//! **Closed here: recomputation at a length the cloud has already seen.** A store that rewrites its
//! history and re-derives the chain arrives at a *different* head for the *same* length. The cloud
//! holds the original. Two different heads at one length is not an ambiguity to resolve — it is two
//! irreconcilable claims about one log, and [`AnchorVerdict::Forked`] refuses the newcomer rather
//! than overwriting the record that makes the contradiction visible.
//!
//! **Not closed here: truncation followed by continued trading.** A store cut back to length 20 and
//! then traded on publishes its next anchor at a length *above* anything the cloud holds, and from
//! the anchors alone that is indistinguishable from honest growth. What gives it away is not an
//! anchor but the events: the cloud ends up holding two different events claiming the same
//! `chain.seq` for one store. That check is a property of the event log, not of this ledger, and it
//! is deliberately a separate piece of work rather than something half-done here.
//!
//! Stating the limit is the point. A mechanism believed to catch more than it does is worse than one
//! that catches nothing, because nobody looks any further.
//!
//! # An anchor is never rejected at ingest
//!
//! A refused anchor is still an event a store emitted, and the event log is the source of truth: it
//! is stored like any other. What is refused is its promotion to the store's head. The evidence of
//! the contradiction is the pair — what the cloud held and what it was offered — so throwing either
//! away would destroy the finding.

use pos_ports::dynamic::BoxFuture;
use pos_proto::chain::ChainHash;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{EventId, StoreId, TenantId};
use pos_proto::{BusinessDate, Timestamp};

/// A failure of the anchor ledger itself — the database is unreachable.
#[derive(Debug, thiserror::Error)]
#[error("the anchor ledger failed: {0}")]
pub struct AnchorError(String);

impl AnchorError {
    /// A ledger failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// One anchor: a store's chain as it stood at a shift close, durable outside that store's reach.
///
/// `observed_at` is the **store's** clock — the `event_time` of the anchor event — and is recorded
/// as the store's claim about when the shift closed, not as the cloud's finding. The cloud's own
/// clock appears on [`AnchorConflict::noticed_at`], where it is load-bearing: when a contradiction
/// was seen is a fact about the cloud, and a tamperer controls the other one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreAnchor {
    /// The store whose chain this is.
    pub store: StoreId,
    /// How many records the chain held when the anchor was taken.
    pub chain_seq: u64,
    /// The hash of the last of them.
    pub head: ChainHash,
    /// How many records carry no chain because they predate it. Not a fault — it tells a store that
    /// upgraded mid-life from one that lost its history.
    pub unchained: u64,
    /// The store's own timestamp on the anchor event.
    pub observed_at: Timestamp,
}

/// What the cloud already holds that bears on an offered anchor.
///
/// Both answers come from one call so the verdict is decided against a single consistent read. Two
/// separate lookups could straddle a concurrent write and produce a "head" and a "same length" that
/// never coexisted, which is how a false fork gets reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldAnchors {
    /// The highest-`chain_seq` anchor held for the store, if any.
    pub head: Option<StoreAnchor>,
    /// The anchor held at exactly the offered length, if any.
    pub at_offered_length: Option<StoreAnchor>,
}

/// What the cloud decided about an offered anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorVerdict {
    /// Longer than anything held, contradicting nothing: accepted, and this is the store's new head.
    Extends,
    /// Identical to one already held: a re-delivery. Accepted as a no-op.
    ///
    /// This is the common case, not an edge case. Ingest is at-least-once and idempotent by event id
    /// ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §4), and reconciliation re-pushes whole
    /// windows ([ADR-0040](../../../docs/adr/0040-reconciliation.md)), so an anchor the cloud
    /// already has will arrive again as a matter of routine. Treating that as a fork would fill the
    /// console with alarms raised by the delivery guarantee working correctly.
    Repeat,
    /// Shorter than the head, at a length the cloud had not recorded, contradicting nothing held:
    /// accepted into the history, and the head does not move.
    ///
    /// Innocent and real: a broker gap can lose one anchor and deliver the next, and reconciliation
    /// then re-pushes the missing one late. Calling that tampering would be a false accusation
    /// produced by the recovery mechanism doing its job. It is recorded rather than refused, and the
    /// head is left where it is, because a late arrival is not news about the present.
    Backfills,
    /// Contradicts an anchor already held: the same length, a different head. **Refused.**
    Forked {
        /// The head the cloud holds at that length — the record the offered one contradicts.
        held: ChainHash,
    },
}

impl AnchorVerdict {
    /// Whether this verdict means the anchor becomes the store's head.
    #[must_use]
    pub fn advances_the_head(&self) -> bool {
        matches!(self, Self::Extends)
    }

    /// Whether this verdict is a refusal a human has to look at.
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Forked { .. })
    }
}

/// A contradiction, as it is handed to the ledger to record.
///
/// Counts, hashes and ids: this is store telemetry, and there is no field here a customer or a
/// member of staff appears in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictReport {
    /// The store that offered the contradicting anchor.
    pub store: StoreId,
    /// The chain length both heads claim to describe.
    pub chain_seq: u64,
    /// The head the cloud already held at that length.
    pub held_head: ChainHash,
    /// The head it was offered instead.
    pub offered_head: ChainHash,
    /// The anchor event that carried the offered head, so the finding can be traced to a record.
    pub offered_event_id: EventId,
}

/// A recorded contradiction, as a reader gets it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorConflict {
    /// The conflict's id, derived by the ledger from the finding itself rather than minted fresh.
    ///
    /// Deliberately **not** chronological, and that is the trade: ordering comes from
    /// [`noticed_at`](Self::noticed_at), and what an id derived from the contradiction buys instead
    /// is that re-delivering the same bad anchor lands on the row that is already there. Ingest is
    /// at-least-once and reconciliation re-pushes whole windows, so a freshly-minted id would turn
    /// one finding into a row per delivery — a flood, in exactly the case somebody needs to read
    /// the list.
    pub conflict_id: String,
    /// When the **cloud** saw the contradiction, from the server's clock.
    pub noticed_at: Timestamp,
    /// What was contradicted, and by what.
    pub report: ConflictReport,
}

/// Keeps a store's anchors and the contradictions among them
/// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
///
/// Filled by `store-postgres` in the shipped cloud and by a fake in a test, the same way
/// [`crate::reconcile::ReconcileStore`] is.
///
/// `dyn`-compatible on purpose — boxed futures rather than `impl Future` — for the same reason
/// [`crate::cloud::StoreOwners`] is: [`crate::cloud::Cloud`] holds one, and making it a type
/// parameter instead would put a name on every construction site of a struct that has many.
pub trait AnchorLedger: Send + Sync + core::fmt::Debug {
    /// What is already held for `store` that bears on an anchor of length `chain_seq`.
    ///
    /// # Errors
    ///
    /// [`AnchorError`] if the ledger could not be read.
    fn held_for(
        &self,
        tenant: TenantId,
        store: StoreId,
        chain_seq: u64,
    ) -> BoxFuture<'_, Result<HeldAnchors, AnchorError>>;

    /// Records `anchor` into `tenant`'s history.
    ///
    /// Idempotent by `(tenant, store, chain_seq)`: a re-delivered anchor must store nothing and
    /// succeed, because at-least-once ingest guarantees the re-delivery happens.
    ///
    /// # Errors
    ///
    /// [`AnchorError`] if the ledger could not be written.
    fn accept<'a>(
        &'a self,
        tenant: TenantId,
        anchor: &'a StoreAnchor,
    ) -> BoxFuture<'a, Result<(), AnchorError>>;

    /// Records a refused anchor, deriving the conflict's id from the finding and stamping the
    /// cloud's own clock.
    ///
    /// Idempotent: the same contradiction recorded twice is one row.
    ///
    /// # Errors
    ///
    /// [`AnchorError`] if the ledger could not be written.
    fn record_conflict<'a>(
        &'a self,
        tenant: TenantId,
        report: &'a ConflictReport,
    ) -> BoxFuture<'a, Result<(), AnchorError>>;

    /// Lists `tenant`'s most recent contradictions, newest first, capped at `limit`. An optional
    /// `store` narrows to one store.
    ///
    /// # Errors
    ///
    /// [`AnchorError`] if the ledger could not be read.
    fn list_conflicts(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        limit: u32,
    ) -> BoxFuture<'_, Result<Vec<AnchorConflict>, AnchorError>>;
}

/// Reads the events the cloud holds for one store's trading day, so the chain they make can be
/// recomputed ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
///
/// # Why this is not `EventStore::read`
///
/// [`pos_ports::event_store::EventQuery`] pages a store's log by `event_id` and has no notion of a
/// trading day. Adding one would change a port every adapter implements to serve a question only
/// the cloud asks. [ADR-0040](../../../docs/adr/0040-reconciliation.md) already made this choice
/// for the reconciliation diff — a seam here, filled by `store-postgres` — and this follows it.
///
/// The day is the filter because it is the index the event log already carries
/// ([ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)); the chain positions are read
/// out of the envelopes afterwards, so nothing is filtered on a JSON operator and no column is
/// promoted onto the most-written table in the system.
pub trait ChainWindow: Send + Sync + core::fmt::Debug {
    /// Every event the cloud holds for `store` over `days_back + 1` trading days ending at `day`,
    /// in `event_id` order, capped at `limit`.
    ///
    /// `days_back` is there because a shift that crosses the store's own day cut-off puts part of
    /// its window on the previous trading date. A window that holds more than `limit` events returns
    /// the first `limit` of them; the caller then sees the short read as an *incomplete* window
    /// rather than as a clean one, which is the honest reading and the one
    /// [ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md) decision 4
    /// asks for.
    ///
    /// # Errors
    ///
    /// [`AnchorError`] if the log could not be read.
    fn events_in_window<'a>(
        &'a self,
        tenant: TenantId,
        store: StoreId,
        day: &'a BusinessDate,
        days_back: u8,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<EventEnvelope<RawPayload>>, AnchorError>>;
}

/// Decides what to do with an offered anchor, given what the cloud holds.
///
/// Pure, and separated from the ledger on purpose: this is the rule the whole mechanism rests on, so
/// it is stated once, in one place, with no database in front of it. Every case below is a test.
#[must_use]
pub fn judge(held: &HeldAnchors, offered: &StoreAnchor) -> AnchorVerdict {
    // A record at the same length settles it either way, and settles it first: an anchor that
    // repeats one already held is a re-delivery no matter where the head sits, and one that
    // disagrees with it is a fork no matter where the head sits.
    if let Some(same_length) = held.at_offered_length.as_ref() {
        return if same_length.head == offered.head {
            AnchorVerdict::Repeat
        } else {
            AnchorVerdict::Forked {
                held: same_length.head.clone(),
            }
        };
    }
    match held.head.as_ref() {
        // Not longer than the head, and unrecorded at its own length: a late arrival.
        //
        // `>=` rather than `>`, although an equal length would have been caught above: this reads
        // the two fields of one struct, and a ledger that answered inconsistently would otherwise
        // have its bug promoted into a new head.
        Some(longest) if longest.chain_seq >= offered.chain_seq => AnchorVerdict::Backfills,
        _ => AnchorVerdict::Extends,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_proto::Ulid;

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(7))
    }

    fn anchor(chain_seq: u64, head: &str) -> StoreAnchor {
        StoreAnchor {
            store: store(),
            chain_seq,
            head: ChainHash::from_hex(head),
            unchained: 0,
            observed_at: Timestamp::from_milliseconds_since_epoch(1_700_000_000_000)
                .expect("a fixed instant inside the representable range"),
        }
    }

    #[test]
    fn the_first_anchor_a_store_ever_sends_extends_nothing() {
        let verdict = judge(&HeldAnchors::default(), &anchor(12, "aa"));
        assert_eq!(verdict, AnchorVerdict::Extends);
        assert!(verdict.advances_the_head());
    }

    #[test]
    fn a_longer_chain_extends_the_head() {
        let held = HeldAnchors {
            head: Some(anchor(12, "aa")),
            at_offered_length: None,
        };
        assert_eq!(judge(&held, &anchor(30, "bb")), AnchorVerdict::Extends);
    }

    #[test]
    fn the_same_anchor_arriving_twice_is_a_no_op() {
        // At-least-once ingest and reconciliation's re-push both guarantee this happens; it must not
        // read as a contradiction.
        let held = HeldAnchors {
            head: Some(anchor(30, "bb")),
            at_offered_length: Some(anchor(30, "bb")),
        };
        let verdict = judge(&held, &anchor(30, "bb"));
        assert_eq!(verdict, AnchorVerdict::Repeat);
        assert!(!verdict.advances_the_head());
        assert!(!verdict.is_refusal());
    }

    #[test]
    fn an_anchor_re_delivered_below_the_head_is_still_a_no_op() {
        // The same anchor, arriving late after a longer one was already accepted. The head must not
        // move backwards to it.
        let held = HeldAnchors {
            head: Some(anchor(30, "bb")),
            at_offered_length: Some(anchor(12, "aa")),
        };
        let verdict = judge(&held, &anchor(12, "aa"));
        assert_eq!(verdict, AnchorVerdict::Repeat);
        assert!(!verdict.advances_the_head());
    }

    #[test]
    fn an_anchor_the_cloud_missed_backfills_without_moving_the_head() {
        // A broker gap lost the anchor at 20 and delivered the one at 30; reconciliation re-pushes
        // 20 afterwards. Nothing held contradicts it, so it is filed, not alarmed on.
        let held = HeldAnchors {
            head: Some(anchor(30, "bb")),
            at_offered_length: None,
        };
        let verdict = judge(&held, &anchor(20, "cc"));
        assert_eq!(verdict, AnchorVerdict::Backfills);
        assert!(!verdict.advances_the_head());
        assert!(!verdict.is_refusal());
    }

    #[test]
    fn two_different_heads_at_one_length_is_a_fork() {
        // The recomputation case: the store rewrote its history and re-derived the chain, so its
        // log of length 30 now hashes to something else. The cloud holds the original.
        let held = HeldAnchors {
            head: Some(anchor(30, "bb")),
            at_offered_length: Some(anchor(30, "bb")),
        };
        let verdict = judge(&held, &anchor(30, "ff"));
        assert_eq!(
            verdict,
            AnchorVerdict::Forked {
                held: ChainHash::from_hex("bb"),
            }
        );
        assert!(verdict.is_refusal());
        assert!(!verdict.advances_the_head());
    }

    #[test]
    fn a_fork_below_the_head_is_still_a_fork() {
        let held = HeldAnchors {
            head: Some(anchor(30, "bb")),
            at_offered_length: Some(anchor(12, "aa")),
        };
        assert!(judge(&held, &anchor(12, "ff")).is_refusal());
    }

    #[test]
    fn an_inconsistent_ledger_never_produces_a_new_head() {
        // A ledger that reported a head of 30 and nothing at 30 would, under a strict `>`, have its
        // own bug turned into a head moving backwards. It backfills instead.
        let held = HeldAnchors {
            head: Some(anchor(30, "bb")),
            at_offered_length: None,
        };
        assert_eq!(judge(&held, &anchor(30, "ff")), AnchorVerdict::Backfills);
    }
}
