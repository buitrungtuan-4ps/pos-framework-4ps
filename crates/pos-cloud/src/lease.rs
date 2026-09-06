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
    ) -> impl Future<Output = Result<LeaseBump, LeaseStoreError>> + Send;

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
