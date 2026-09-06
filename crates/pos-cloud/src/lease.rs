// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's authoritative lease generation
//! ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md), closing
//! [ADR-0049](../../../docs/adr/0049-single-active-lease.md)'s cloud half).
//!
//! ADR-0049 made "one store, one active machine" a comparison of two `LeaseGeneration`s and left
//! *persisting the authoritative one* to `Fiscalization` in P10 — bundled with allocating a legal
//! invoice range. The bundle is why neither was built: the range needs a tax authority, which is a
//! per-country registration question this repository cannot answer; the generation needs a row and
//! an increment. ADR-0108 splits them, and this seam is the generation.
//!
//! # Why a bump and nothing else
//!
//! The only write is [`LeaseStore::bump`]. There is no set-to-a-value and no decrement, because an
//! authority that takes a number from its caller is not an authority, and a generation that can move
//! backwards is not monotonic — which is the whole mechanism. The `lease` config node the edge reads
//! is **derived** from this row by the bump that wrote it; no admin route accepts one in a body.
//!
//! # Why the edge placement rides along
//!
//! [ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md) gives a store one new
//! attribute — *where* its edge runs — and makes the bump the only thing that writes it. That is not
//! a convenience: order the two writes and there is an interval in which the store record and the
//! lease disagree, and every reader inside it is reading a lie. Write the column first and the
//! console says "Offline-capable: no" about a box that is still selling; write it when the move
//! settles, hours later, and the console promises offline trading about a store that has been hosted
//! since Tuesday. So [`LeaseStore::bump`] takes the placement and returns it, one act.
//!
//! A trait, so it runs against an in-memory fake in tests and a `store-postgres` upsert in the
//! cloud (the impl lives in [`crate::persistence`], the SQL in `store-postgres`).

use core::future::Future;

use pos_core::lease::LeaseGeneration;
use pos_proto::enums::EdgePlacement;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::WireEnum as _;

/// What one bump wrote: the generation it issued, and the edge placement the store now has
/// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// The pair is the point. `edge_placement` is written by the bump and by nothing else, so returning
/// it here is the only way a caller learns the store's placement without a second read — and a
/// second read is precisely the window ADR-0110 closed by making the two one write. A bump that
/// named no placement still gets one back: whatever the store already had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseBump {
    /// The generation just issued. `0` for a store's first-ever lease.
    pub generation: LeaseGeneration,
    /// Where the machine holding that generation runs.
    pub edge_placement: EdgePlacement,
    /// The generation this bump displaced and nothing has yet proved drained, or `None` for a
    /// store's first-ever lease — which supersedes nobody.
    ///
    /// It is recorded by the same statement that issues the new generation, because the cloud's
    /// only other memory of the old machine is `store_liveness`, and that row is overwritten the
    /// instant the incoming machine says hello. Until something proves the old box drained — a
    /// heartbeat reporting *that* generation with an empty outbox, or an admin who read a
    /// powered-off machine directly — a handover is in flight.
    pub superseded_generation: Option<LeaseGeneration>,
}

/// What a *reader* can say about where a store's edge runs.
///
/// Three states, not two, and the third is the reason this is an enum rather than an
/// `Option<EdgePlacement>`. A reader that collapses them makes a store sound safer than it is:
///
/// - [`Self::NeverIssued`] — the cloud has never bumped this store, so `store_lease` has no row for
///   it and nothing has ever asserted a placement. Per
///   [ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md) every store in the
///   fleet today *is* in-store, so a reader may treat this as in-store; it is an absence of a
///   record, not an absence of knowledge.
/// - [`Self::Known`] — the row exists and its token decoded.
/// - [`Self::Unreadable`] — the row exists and its token did **not** decode. This binary is older
///   than the database, a fork added a fourth mode, or the row is corrupt. It is an absence of
///   *knowledge*, and the opposite of the first case: the store may well be hosted.
///
/// Folding `Unreadable` into `NeverIssued` is the specific bug this type exists to prevent. Both
/// would become "in-store", and in-store is the mode where a quiet store is *probably still
/// trading* — so a hosted store that has gone dark would page one severity too low, silently, with
/// nothing in any log to say a token failed to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePlacement {
    /// No lease row: the cloud has never issued this store a generation.
    NeverIssued,
    /// The stored token decoded to a mode this binary knows.
    Known(EdgePlacement),
    /// A lease row exists but its token is not one this binary recognises.
    Unreadable,
}

impl StorePlacement {
    /// Whether a quiet store in this state can still be trading.
    ///
    /// This is the whole reason the fleet read carries a placement, and it is deliberately
    /// pessimistic in exactly one direction: [`Self::Unreadable`] answers `false`, the same as a
    /// hosted store, because "we could not read where this store runs" must never be reported as
    /// "and it is probably fine". [`Self::NeverIssued`] answers `true` — ADR-0110 records that
    /// every store in the fleet today is in-store, so no-record means in-store, which is a fact
    /// about the fleet rather than a guess.
    #[must_use]
    pub const fn may_trade_offline(self) -> bool {
        match self {
            Self::NeverIssued => true,
            Self::Known(placement) => matches!(placement, EdgePlacement::InStore),
            Self::Unreadable => false,
        }
    }

    /// The wire token for this placement, or `None` when there is nothing to report.
    ///
    /// Both non-`Known` states answer `None` rather than inventing `EDGE_PLACEMENT_UNSPECIFIED`:
    /// on the wire that token means *this message did not say*, and a reader that emitted it would
    /// be saying something. A field that is simply absent is the honest shape for "no row" and for
    /// "a row this binary cannot read" alike — the two are distinguished for the alert engine by
    /// [`Self::may_trade_offline`], not by a token a console would have to render.
    #[must_use]
    pub fn as_wire(self) -> Option<&'static str> {
        match self {
            Self::Known(placement) => Some(placement.as_wire()),
            Self::NeverIssued | Self::Unreadable => None,
        }
    }
}

/// What a conditional bump did ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// Two of the three arms are refusals a caller can fix and retry, which is why they are outcomes and
/// not [`LeaseStoreError`]s: that type funnels to `503 the service is unavailable`, and a caller told
/// `503` retries the same losing request. The same reasoning
/// [`UpdateOutcome`](crate::version::UpdateOutcome) applies on the config tree.
///
/// **The two refusals are genuinely different questions, and a retry separates them.** A caller that
/// re-sends a bump which actually landed is *stale*, not *blocked*: the row moved on, so it gets
/// `VersionMismatch` — and telling it `Undrained` instead would send it to acknowledge a handover it
/// has already caused. Order matters for the same reason, and the write's own `WHERE` evaluates the
/// generation first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseBumpOutcome {
    /// The generation was issued.
    Issued(LeaseBump),
    /// The row is not at the generation the request's `If-Match` named. Answers `412`.
    ///
    /// `current` is what the row holds now, or `None` for a store with no lease row at all. Read
    /// after the refusal, so it can be stale by the time it is rendered — it is for the message, not
    /// for a decision. The decision was the write's `WHERE`, which cannot race.
    VersionMismatch {
        /// What the row holds now, or `None` for a store with no lease row.
        current: Option<LeaseGeneration>,
    },
    /// A handover is still in flight and the request did not acknowledge it: `superseded` names a
    /// machine holding events this cloud has never seen. Answers `422`
    /// ([ADR-0096](../../../docs/adr/0096-unprocessable-status.md)).
    ///
    /// You do not move a store off a machine whose events are still on it — not without a person
    /// saying, by name and in the trail, that those events are being abandoned.
    Undrained {
        /// The generation whose machine still owes this cloud events.
        superseded: LeaseGeneration,
    },
}

/// What settling a handover by hand did ([`LeaseStore::settle_handover`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettleOutcome {
    /// The named generation was the one in flight, and the handover is now settled.
    Settled,
    /// The row's `superseded_generation` is not the one the request named. Answers `422`
    /// ([ADR-0096](../../../docs/adr/0096-unprocessable-status.md)).
    ///
    /// `current` is what the row holds instead — `None` when the handover is already settled,
    /// usually because the outgoing machine's own last heartbeat got there first.
    ///
    /// **Not treated as an idempotent success**, and that is the decision worth stating. This write
    /// records a person attesting to a specific fact: *the machine holding generation N holds no
    /// events*. If the row is not on N, the attestation is about a different machine, and answering
    /// `204` would put it in the trail as though it were about this one.
    NotSuperseded {
        /// What the row's `superseded_generation` holds now.
        current: Option<LeaseGeneration>,
    },
    /// The cloud has never issued this store a lease, so there is no handover to settle.
    NoLease,
}

/// What retiring a handover did ([`LeaseStore::retire`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetireOutcome {
    /// The handover is recorded as retired.
    Retired,
    /// A handover is still in flight: the outgoing machine may still hold events. Answers `422`.
    ///
    /// You do not decide a machine is no longer needed while it may be the only place a night's
    /// trading exists. Settle it first — by draining it, or by attesting that it is empty.
    Undrained {
        /// The generation whose machine has not been proved drained.
        superseded: LeaseGeneration,
    },
    /// This store's current handover is already retired. Answers `422`.
    ///
    /// Refused rather than overwritten: a second write would replace the first decision's who and
    /// when with a later person's, and the row is the durable record of exactly that.
    AlreadyRetired {
        /// When the decision was made.
        at: Timestamp,
        /// The deciding admin's id. An id and not an email — the trail carries the email, and an
        /// operational row has no business holding one.
        by: String,
    },
    /// The write refused, and by the time the row was read it satisfied both of its conditions:
    /// another admin resolved the race in between. The caller re-reads and retries. Answers `409`.
    ///
    /// Reported rather than retried, because a retry would be a second attempt at a decision the
    /// caller made once, against a row that has changed underneath them.
    Raced,
    /// The store is on generation `0` — its first-ever lease — so no machine has ever been replaced
    /// and there is nothing to retire. Answers `422`.
    ///
    /// The same fact [`handover_state`] reports as `None`, on the write side: generation `0`
    /// supersedes nobody, so a store that holds it has never handed anything over. Distinct from
    /// [`Self::NoLease`], where the cloud has issued no lease at all.
    NeverSuperseded,
    /// The cloud has never issued this store a lease, so there is no handover to retire.
    NoLease,
}

/// Issues and reads a store's authoritative lease generation.
pub trait LeaseStore {
    /// Issues this store's **next** generation and returns it: the act of saying "a different
    /// machine is the store now".
    ///
    /// A store that has never held a lease starts at generation `0` — ADR-0049's "the first lease a
    /// store ever issues is generation `0`" — so the first bump does not supersede anybody; it
    /// establishes the counter that later bumps move. Every bump after that supersedes whatever box
    /// holds the previous number.
    ///
    /// `edge_placement` is `Some` when the bump is **moving** the store to a machine in a different
    /// place, and `None` when it is replacing the machine where it already is — ADR-0003's swap. A
    /// `None` keeps whatever the store had; there is no route that writes the placement on its own,
    /// which is what makes the column and the generation impossible to disagree.
    ///
    /// # Errors
    ///
    /// [`LeaseStoreError`] if the row could not be written.
    fn bump(
        &self,
        tenant: TenantId,
        store: StoreId,
        issued_at: Timestamp,
        edge_placement: Option<EdgePlacement>,
        acknowledge_undrained: Option<LeaseGeneration>,
        expected_generation: Option<LeaseGeneration>,
    ) -> impl Future<Output = Result<LeaseBumpOutcome, LeaseStoreError>> + Send;

    /// The store's authoritative generation, or `None` if it has never been issued a lease.
    ///
    /// `None` is deliberately not `LeaseGeneration::new(0)`: a store nobody has ever issued a lease
    /// to has no machine that can be superseded, and a store on generation `0` has exactly one that
    /// can. Collapsing them would start refusing updates on a fleet that never opted in.
    ///
    /// # Errors
    ///
    /// [`LeaseStoreError`] if the row could not be read.
    fn current(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> impl Future<Output = Result<Option<LeaseGeneration>, LeaseStoreError>> + Send;

    /// Settles a handover by hand: clears `superseded_generation` when it holds `superseded`.
    ///
    /// The second of the two ways a handover closes
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)). The first is
    /// automatic — the outgoing machine's last heartbeat reports that generation with an empty
    /// outbox. This is the one for a machine that will never send another heartbeat: a box already
    /// powered off, or one whose disk an operator has read directly. It is an attestation, so it is
    /// audited, and the person's name is against it.
    ///
    /// # Why the caller names the generation rather than sending `If-Match`
    ///
    /// The named value *is* the precondition, and here it is strictly stronger than the row's
    /// version. Any bump necessarily changes `superseded_generation` — it sets it to the generation
    /// being displaced, which is by construction one the column did not already hold — so a
    /// concurrent bump makes this write's `WHERE` fail on its own. An `If-Match` on top would refuse
    /// exactly the same requests, while inviting a caller to think the two could disagree.
    ///
    /// # Errors
    ///
    /// [`LeaseStoreError`] if the row could not be written.
    fn settle_handover(
        &self,
        tenant: TenantId,
        store: StoreId,
        superseded: LeaseGeneration,
    ) -> impl Future<Output = Result<SettleOutcome, LeaseStoreError>> + Send;

    /// Records that a settled handover's outgoing machine — its box, its database and its hosting —
    /// is no longer needed.
    ///
    /// Nothing transitions into this on its own, and nothing ever will: it is a judgement about
    /// money and risk that no fact in this system implies. It refuses while a handover is still in
    /// flight, because a machine that may hold the only copy of a night's sales is not one anybody
    /// can decide is unnecessary.
    ///
    /// `retired_by` is the acting admin's id. The audit trail carries the email; this row does not,
    /// because an operational table that accumulates staff addresses is one that has quietly changed
    /// what it holds.
    ///
    /// # Errors
    ///
    /// [`LeaseStoreError`] if the row could not be written.
    fn retire(
        &self,
        tenant: TenantId,
        store: StoreId,
        retired_at: Timestamp,
        retired_by: &str,
    ) -> impl Future<Output = Result<RetireOutcome, LeaseStoreError>> + Send;
}

/// A failure of the lease store itself — the row could not be read or written.
#[derive(Debug, thiserror::Error)]
#[error("the lease store failed: {0}")]
pub struct LeaseStoreError(String);

impl LeaseStoreError {
    /// Wraps a message (for the server's log — a lease generation is a counter, not a person).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The `lease` config node the cloud publishes from an issued generation.
///
/// One place builds it, so the node's shape cannot drift between the bump route and whatever reads
/// it back. The edge parses this into `pos_core::lease::LeaseConfig`.
#[must_use]
pub fn lease_node(generation: LeaseGeneration) -> serde_json::Value {
    serde_json::json!({ "generation": generation.value() })
}

#[cfg(test)]
mod tests {
    use super::{LeaseGeneration, lease_node};

    #[test]
    fn the_published_node_is_what_the_edge_parses() {
        let node = lease_node(LeaseGeneration::new(4));
        assert_eq!(node, serde_json::json!({ "generation": 4 }));
        let parsed: pos_core::lease::LeaseConfig =
            serde_json::from_value(node).expect("the edge's own type reads it back");
        assert_eq!(parsed.generation(), LeaseGeneration::new(4));
    }
}

/// The three states of a store's machine handover, and the fourth answer: *there isn't one*
/// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// Derived at read time from facts the fleet row already carries, exactly as `online` and
/// `config_current` are. Deriving it in one place is what keeps the console from re-implementing the
/// rule in TypeScript and getting the same-beat guard below wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoverState {
    /// A bump has landed and `settled` is not yet true. The outgoing machine may still hold events
    /// this cloud has never seen.
    TakingOver,
    /// The store's latest heartbeat reports the authoritative generation with an empty outbox, and
    /// nothing is recorded as superseded. The move is done; the old machine is still *there*.
    Settled,
    /// A person has looked at a settled handover and decided the old machine, its database and its
    /// hosting are no longer needed. Nothing transitions into this on its own.
    Retired,
}

impl HandoverState {
    /// The wire token, for a console that renders a badge.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::TakingOver => "taking-over",
            Self::Settled => "settled",
            Self::Retired => "retired",
        }
    }
}

/// Everything [`handover_state`] needs, named rather than passed as seven positional arguments.
///
/// A struct because the last three fields are only meaningful *together*, and a positional call
/// would let two of them be swapped silently — which is exactly the bug the same-beat rule exists to
/// prevent.
#[derive(Debug, Clone, Copy, Default)]
pub struct HandoverFacts {
    /// The store's authoritative generation, or `None` if it has never been issued a lease.
    pub authoritative: Option<LeaseGeneration>,
    /// The generation the last bump displaced and nothing has proved drained, or `None`.
    pub superseded: Option<LeaseGeneration>,
    /// When a person retired this handover, or `None`.
    pub retired_at: Option<Timestamp>,
    /// The generation the box last reported holding, or `None` if it has never said.
    pub held: Option<LeaseGeneration>,
    /// The outbox depth that box reported, or `None`.
    pub outbox_depth: Option<u64>,
    /// When it reported that depth, or `None`.
    pub outbox_reported_at: Option<Timestamp>,
    /// When it reported that generation, or `None`.
    pub lease_reported_at: Option<Timestamp>,
}

/// Which state a store's handover is in, or `None` when the store has never had one.
///
/// # Why `None` is a real answer and not a fourth state
///
/// A store on generation `0` has never been replaced: `0` is its first-ever lease, which supersedes
/// nobody. ADR-0110 defines `taking-over` as "a bump has landed and `settled` is not yet true",
/// which read literally would catch every such store and put a mid-handover badge on a fleet that
/// has never handed anything over. A store with no lease row at all is the same answer for a simpler
/// reason. Neither is a handover in any state; the honest report is that there is nothing to report,
/// and a console renders no badge rather than a reassuring one.
///
/// # The same-beat guard
///
/// `settled` requires the store's latest heartbeat to report the authoritative generation **with**
/// an empty outbox. Those two facts have to come from *one message*. `store_liveness` COALESCEs
/// `outbox_depth` and `lease_generation` independently, each with its own instant, so the stored row
/// can hold generation `N + 1` recorded today beside a zero depth recorded last week under
/// generation `N`. Reading that pair as settled would declare a handover finished while the old
/// machine still held a night's trading — the failure the column was created to prevent. Equal
/// instants mean one beat carried both, because [`record_heartbeat`] stamps each only when its value
/// is present and stamps both from the same clock reading.
///
/// [`record_heartbeat`]: https://docs.rs/store-postgres
#[must_use]
pub fn handover_state(facts: HandoverFacts) -> Option<HandoverState> {
    // Retirement first, and it is checked before everything else deliberately. It is the terminal
    // state and the only one a person authored, so a reader must not have it hidden by a derivation
    // about machines. The two cannot both be set on a live row — retiring refuses while a handover
    // is in flight, and a bump clears the retirement — but the order is fixed here so a row that
    // somehow held both still reads as the human decision rather than silently as the machine one.
    if facts.retired_at.is_some() {
        return Some(HandoverState::Retired);
    }
    if facts.superseded.is_some() {
        return Some(HandoverState::TakingOver);
    }
    // No lease, or a first-ever lease: nothing has been handed over. See the note above.
    let authoritative = facts.authoritative?;
    if authoritative.value() == 0 {
        return None;
    }
    let same_beat =
        facts.outbox_reported_at.is_some() && facts.outbox_reported_at == facts.lease_reported_at;
    if same_beat && facts.held == Some(authoritative) && facts.outbox_depth == Some(0) {
        return Some(HandoverState::Settled);
    }
    // A handover has happened and nothing proves it finished. That covers a box that has not
    // reported since the bump, one still reporting the old generation, and one reporting a backlog
    // — all of them mid-handover, and none of them a state a clock should be allowed to age out of.
    Some(HandoverState::TakingOver)
}

#[cfg(test)]
mod handover_tests {
    use super::{HandoverFacts, HandoverState, handover_state};
    use pos_core::lease::LeaseGeneration;
    use pos_proto::time::Timestamp;

    /// A store whose box is reporting the authoritative generation, empty, from one beat.
    fn settled_facts(generation: u64) -> HandoverFacts {
        let beat = Timestamp::from_milliseconds_since_epoch(1_777_000_000_000).expect("an instant");
        HandoverFacts {
            authoritative: Some(LeaseGeneration::new(generation)),
            superseded: None,
            retired_at: None,
            held: Some(LeaseGeneration::new(generation)),
            outbox_depth: Some(0),
            outbox_reported_at: Some(beat),
            lease_reported_at: Some(beat),
        }
    }

    #[test]
    fn a_store_that_has_never_been_handed_over_reports_nothing() {
        // No lease row at all.
        assert_eq!(handover_state(HandoverFacts::default()), None);

        // Generation 0 is a store's *first* lease and supersedes nobody. ADR-0110's wording, read
        // literally, would call this `taking-over` and badge a fleet that has handed over nothing.
        assert_eq!(handover_state(settled_facts(0)), None);
        assert_eq!(
            handover_state(HandoverFacts {
                authoritative: Some(LeaseGeneration::new(0)),
                ..HandoverFacts::default()
            }),
            None,
            "and a generation-0 store that has never reported is still not mid-handover"
        );
    }

    #[test]
    fn an_in_flight_handover_outranks_every_other_reading() {
        // Everything about the *new* machine looks perfect, and a generation still owes events.
        let facts = HandoverFacts {
            superseded: Some(LeaseGeneration::new(0)),
            ..settled_facts(1)
        };
        assert_eq!(handover_state(facts), Some(HandoverState::TakingOver));
    }

    #[test]
    fn a_retirement_is_reported_ahead_of_any_machine_derivation() {
        let retired = Timestamp::from_milliseconds_since_epoch(1_777_000_009_000).expect("instant");
        let facts = HandoverFacts {
            retired_at: Some(retired),
            ..settled_facts(1)
        };
        assert_eq!(handover_state(facts), Some(HandoverState::Retired));
    }

    #[test]
    fn settled_needs_both_facts_from_the_same_beat() {
        assert_eq!(
            handover_state(settled_facts(1)),
            Some(HandoverState::Settled)
        );

        // The stored row's two instants disagree, so the pair came from two different messages:
        // generation 1 reported today beside a zero depth reported under generation 0 last week.
        // Reading that as settled is exactly the failure `superseded_generation` exists to prevent.
        let stale_depth = Timestamp::from_milliseconds_since_epoch(1_776_000_000_000).expect("ts");
        assert_eq!(
            handover_state(HandoverFacts {
                outbox_reported_at: Some(stale_depth),
                ..settled_facts(1)
            }),
            Some(HandoverState::TakingOver),
            "two facts from two beats are not evidence about one machine"
        );
    }

    #[test]
    fn a_box_that_is_behind_or_backlogged_or_silent_is_still_taking_over() {
        // Still holding the generation it was replaced from.
        assert_eq!(
            handover_state(HandoverFacts {
                held: Some(LeaseGeneration::new(0)),
                ..settled_facts(1)
            }),
            Some(HandoverState::TakingOver)
        );
        // Reporting the right generation with forty unsent sales.
        assert_eq!(
            handover_state(HandoverFacts {
                outbox_depth: Some(40),
                ..settled_facts(1)
            }),
            Some(HandoverState::TakingOver)
        );
        // Has not reported at all since the bump. `None` is not zero, and must not read as drained.
        assert_eq!(
            handover_state(HandoverFacts {
                held: None,
                outbox_depth: None,
                outbox_reported_at: None,
                lease_reported_at: None,
                ..settled_facts(1)
            }),
            Some(HandoverState::TakingOver),
            "a store that has said nothing has not proved anything"
        );
    }
}
