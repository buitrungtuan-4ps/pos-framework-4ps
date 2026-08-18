// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Delivery marketplaces.
//!
//! Grab Food and ShopeeFood today; the shape is the same for any marketplace that pushes
//! orders at a restaurant and expects an answer within a service-level window.
//!
//! # Two directions, two ports
//!
//! An order *arriving* from a marketplace comes in through [`crate::OrderIn`], which every
//! external channel shares. This port is the outbound half: accepting, rejecting, reporting
//! ready, and telling the vendor how busy the kitchen is. Splitting them that way is what
//! makes QR ordering nearly free
//! ([ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)) — a new channel needs a
//! caller, not a new pipeline.
//!
//! # Busy mode is the offline story
//!
//! `docs/architecture.md` §6.1: when the store goes offline, the vendor sees *busy*. That is
//! the honest behaviour — a marketplace that keeps taking orders nobody can see produces
//! angry customers and a cancellation rate that costs the store its ranking. So
//! [`DeliveryVendor::set_busy`] is not a nicety; it is what stops an outage becoming a
//! commercial problem.
//!
//! # The vendor's clock, not ours
//!
//! Accept and reject are bounded by the vendor's own window, and missing it is an automatic
//! rejection on their side with a penalty. That deadline is data the adapter reports rather
//! than a constant the framework assumes, because it differs per vendor and changes without
//! notice.

use core::fmt;
use core::future::Future;

use pos_proto::{OrderId, ReasonCodeId, StoreId, Timestamp};

use crate::error::PortError;

/// The vendor's identifier for an order.
///
/// Distinct from [`OrderId`]: the framework's identifier is a ULID it minted, and the vendor's
/// is whatever the vendor uses. Both are kept, because a support conversation quotes the
/// vendor's and every internal query uses ours.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VendorOrderRef(Box<str>);

impl VendorOrderRef {
    /// Wraps a vendor reference.
    #[must_use]
    pub fn new(reference: impl Into<Box<str>>) -> Self {
        Self(reference.into())
    }

    /// The reference as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VendorOrderRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for VendorOrderRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VendorOrderRef({})", self.0)
    }
}

/// How long the kitchen needs.
///
/// Minutes, because that is the granularity every marketplace uses and a promise finer than
/// that is a promise nobody can keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrepTime {
    /// Minutes from acceptance to ready-for-pickup.
    pub minutes: u16,
}

impl PrepTime {
    /// A preparation time in minutes.
    #[must_use]
    pub const fn minutes(minutes: u16) -> Self {
        Self { minutes }
    }
}

/// Whether the store is taking orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BusyMode {
    /// Taking orders normally.
    Open,
    /// Taking orders but slower than usual, with the revised time.
    Busy {
        /// What to tell customers.
        prep_time: PrepTime,
    },
    /// Not taking orders. What the store reports when it goes offline.
    Closed,
}

impl BusyMode {
    /// Whether the vendor should keep sending orders.
    #[must_use]
    pub const fn accepts_orders(self) -> bool {
        !matches!(self, Self::Closed)
    }
}

/// A marketplace order awaiting a decision, with the vendor's deadline attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDecision {
    /// The vendor's reference.
    pub vendor_order_ref: VendorOrderRef,
    /// Our identifier for the same order.
    pub order_id: OrderId,
    /// When the vendor stops waiting. Reported rather than assumed, because it differs per
    /// vendor and changes without notice.
    pub decide_by: Timestamp,
}

impl PendingDecision {
    /// Whether the window has closed as of `now`.
    ///
    /// Takes `now` as a value rather than reading a clock, for the same reason the domain does
    /// ([ADR-0013](../../../docs/adr/0013-async-strategy.md)): a deadline check that reads its
    /// own clock cannot be tested at the boundary where it matters.
    #[must_use]
    pub fn has_expired(&self, now: Timestamp) -> bool {
        now > self.decide_by
    }
}

/// Talks to one delivery marketplace.
///
/// # Contract
///
/// 1. **Every call is idempotent by [`VendorOrderRef`].** Accepting an order twice is one
///    acceptance. Marketplace APIs are retried constantly and none of them promise
///    exactly-once.
/// 2. **Accepting an already-rejected order fails** with
///    [`PortError::failed_precondition`], and vice versa. A vendor that silently accepts a
///    contradictory transition leaves the store and the marketplace disagreeing about whether
///    food is being cooked.
/// 3. **`set_busy` is the offline signal and must be safe to send repeatedly.** The store
///    re-asserts it on reconnect rather than tracking whether it already did.
/// 4. **A missed deadline is the vendor's decision, not an error here.** An adapter reports
///    [`PortError::failed_precondition`] when the window has closed; it does not retry into it.
pub trait DeliveryVendor: Send + Sync {
    /// Which marketplace this is, for logs and per-adapter metrics.
    #[must_use]
    fn vendor_name(&self) -> &'static str;

    /// Accepts an order and promises a preparation time.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] if the decision window has closed or the order was
    /// already rejected, [`PortError::not_found`] if the vendor does not recognise the
    /// reference, or [`PortError::unavailable`] if the vendor cannot be reached.
    fn accept(
        &self,
        vendor_order_ref: &VendorOrderRef,
        prep_time: PrepTime,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Rejects an order with a reason from the cloud-managed list.
    ///
    /// The reason is a [`ReasonCodeId`] rather than free text because `docs/pos-spec.md` §12
    /// requires reasons to come from a managed list — that is what makes rejection rates
    /// comparable between stores instead of a collection of one-off sentences.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] if the window has closed or the order was already
    /// accepted, [`PortError::not_found`] if the reference is unknown, or
    /// [`PortError::unavailable`] if the vendor cannot be reached.
    fn reject(
        &self,
        vendor_order_ref: &VendorOrderRef,
        reason_code_id: ReasonCodeId,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Tells the vendor the food is ready for collection.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] if the order was never accepted,
    /// [`PortError::not_found`] if the reference is unknown, or
    /// [`PortError::unavailable`] if the vendor cannot be reached.
    fn mark_ready(
        &self,
        vendor_order_ref: &VendorOrderRef,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Sets whether and how fast the store is taking orders.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the vendor cannot be reached. A caller that fails to set
    /// [`BusyMode::Closed`] has a store the marketplace still believes is open, which is worth
    /// retrying hard.
    fn set_busy(
        &self,
        store_id: StoreId,
        mode: BusyMode,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Orders the vendor is still waiting on a decision for.
    ///
    /// Polled on reconnect, because a marketplace push that arrived while the store was
    /// offline is a decision window running down with nobody watching it.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the vendor cannot be reached.
    fn pending_decisions(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<PendingDecision>, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{BusyMode, PendingDecision, PrepTime, VendorOrderRef};
    use pos_proto::{OrderId, Timestamp, Ulid};

    fn instant(milliseconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(milliseconds).expect("builds")
    }

    #[test]
    fn only_closed_stops_the_vendor_sending_orders() {
        assert!(BusyMode::Open.accepts_orders());
        assert!(
            BusyMode::Busy {
                prep_time: PrepTime::minutes(45)
            }
            .accepts_orders()
        );
        assert!(
            !BusyMode::Closed.accepts_orders(),
            "this is what the store reports when it goes offline"
        );
    }

    #[test]
    fn a_deadline_is_checked_against_a_passed_in_clock() {
        // Reading a clock inside this check would make the boundary case — a decision made in
        // the same millisecond the window closes — untestable.
        let decision = PendingDecision {
            vendor_order_ref: VendorOrderRef::new("GF-12345"),
            order_id: OrderId::new(Ulid::from_u128(1)),
            decide_by: instant(1_000),
        };
        assert!(!decision.has_expired(instant(999)));
        assert!(
            !decision.has_expired(instant(1_000)),
            "the deadline itself is still inside the window"
        );
        assert!(decision.has_expired(instant(1_001)));
    }

    #[test]
    fn a_vendor_reference_prints_for_a_support_conversation() {
        let reference = VendorOrderRef::new("GF-12345");
        assert_eq!(reference.to_string(), "GF-12345");
        assert_eq!(format!("{reference:?}"), "VendorOrderRef(GF-12345)");
    }
}
