// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The single-active lease and the invoice-range handoff across a machine swap
//! ([ADR-0049](../../../docs/adr/0049-single-active-lease.md)).
//!
//! A store runs on one active machine, but machines get swapped. This module decides *who is active*
//! and hands a replacement a *fresh invoice-number range*, as pure functions the simulator can exhaust
//! (`docs/roadmap.md` P12). Two properties are the whole point:
//!
//!  * **The lease never expires while offline.** [`lease_standing`] is a function of two
//!    [`LeaseGeneration`]s and nothing else — no clock — so a store cut off from the cloud for days
//!    keeps selling. A machine stops being active only when a *newer* generation is deliberately issued.
//!  * **The replacement's invoice range is disjoint from the old one's.** [`issue_replacement`] starts
//!    the new range where the previous ended, so even a window where the old machine is still (wrongly)
//!    live cannot mint a legal invoice number the new machine also mints.
//!
//! The wire's opaque `LeaseToken` (`pos_proto::protocol`) stays the credential the edge presents; the
//! generation here is the *order* that decides supersession. Legal invoice numbering is distinct from
//! the per-store gapless receipt counter ([ADR-0025](../../../docs/adr/0025-receipt-number-authority.md)).

use core::cmp::Ordering;

/// A lease generation — the store's monotonic counter of how many leases it has issued. The authority
/// increments it for each replacement; a device compares the generation it holds against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeaseGeneration(u64);

impl LeaseGeneration {
    /// A generation from its raw value. The first lease a store ever issues is generation `0`.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// The next generation. Saturates at [`u64::MAX`] — unreachable in practice, since a store issues a
    /// handful of leases in its life, and a saturating bump fails safe rather than wrapping to `0`.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A half-open range of legal invoice numbers, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvoiceRange {
    /// The first number in the range (inclusive).
    pub start: u64,
    /// One past the last number in the range (exclusive).
    pub end: u64,
}

impl InvoiceRange {
    /// A range from its half-open bounds.
    #[must_use]
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// Whether the range holds no numbers (`start >= end`).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start >= self.end
    }

    /// Whether `number` falls inside the range.
    #[must_use]
    pub const fn contains(self, number: u64) -> bool {
        number >= self.start && number < self.end
    }

    /// Whether this range and `other` share no number — the invariant across a lease handoff.
    #[must_use]
    pub const fn is_disjoint_from(self, other: Self) -> bool {
        self.end <= other.start || other.end <= self.start
    }
}

/// A lease grant the cloud issues to the one active device for a store: its generation and the invoice
/// range it may allocate from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseGrant {
    /// The generation this grant carries.
    pub generation: LeaseGeneration,
    /// The disjoint invoice-number range this holder allocates from.
    pub invoice_range: InvoiceRange,
}

/// A device's standing relative to its store's authoritative lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseStanding {
    /// The device holds the current generation — it is the active machine and may sell.
    Active,
    /// A newer generation exists; a replacement has taken the lease and this device must go read-only.
    Superseded,
    /// The device claims a generation ahead of the authority — impossible without corruption or a
    /// forged token, so it is refused rather than trusted.
    Invalid,
}

/// Decides a device's standing from the generation it holds and the store's authoritative generation.
///
/// Purely a function of the two generations — there is no clock, so a lease never expires while the
/// device is offline ([ADR-0049](../../../docs/adr/0049-single-active-lease.md)).
#[must_use]
pub fn lease_standing(held: LeaseGeneration, authoritative: LeaseGeneration) -> LeaseStanding {
    match held.value().cmp(&authoritative.value()) {
        Ordering::Equal => LeaseStanding::Active,
        Ordering::Less => LeaseStanding::Superseded,
        Ordering::Greater => LeaseStanding::Invalid,
    }
}

/// Issues the next lease grant for a replacement machine, given the outgoing grant and the size of the
/// fresh invoice range to allocate.
///
/// The new range begins exactly where the previous one ended, so the two are **disjoint** — an old
/// machine still offline and still issuing invoices from its own range cannot mint a number the new
/// machine will also mint ([ADR-0049](../../../docs/adr/0049-single-active-lease.md)). The generation
/// strictly increases.
#[must_use]
pub fn issue_replacement(previous: &LeaseGrant, range_size: u64) -> LeaseGrant {
    let start = previous.invoice_range.end;
    LeaseGrant {
        generation: previous.generation.next(),
        invoice_range: InvoiceRange::new(start, start.saturating_add(range_size)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InvoiceRange, LeaseGeneration, LeaseGrant, LeaseStanding, issue_replacement, lease_standing,
    };

    fn generation(value: u64) -> LeaseGeneration {
        LeaseGeneration::new(value)
    }

    #[test]
    fn a_device_holding_the_current_generation_is_active() {
        assert_eq!(
            lease_standing(generation(3), generation(3)),
            LeaseStanding::Active
        );
    }

    #[test]
    fn a_device_behind_the_authority_is_superseded() {
        assert_eq!(
            lease_standing(generation(2), generation(3)),
            LeaseStanding::Superseded,
            "a replacement took the lease; the old machine goes read-only"
        );
    }

    #[test]
    fn a_device_ahead_of_the_authority_is_invalid() {
        assert_eq!(
            lease_standing(generation(4), generation(3)),
            LeaseStanding::Invalid,
            "claiming a generation the store never issued is refused, not trusted"
        );
    }

    #[test]
    fn the_standing_ignores_time_entirely() {
        // There is no clock parameter to pass, which is the point: the same two generations always give
        // the same verdict, so no amount of offline time can expire an active lease. This test exists to
        // pin that property — if a clock is ever added here, it stops compiling.
        let held = generation(7);
        let authority = generation(7);
        assert_eq!(lease_standing(held, authority), LeaseStanding::Active);
        assert_eq!(lease_standing(held, authority), LeaseStanding::Active);
    }

    #[test]
    fn a_replacement_gets_a_disjoint_forward_range_and_the_next_generation() {
        let previous = LeaseGrant {
            generation: generation(0),
            invoice_range: InvoiceRange::new(1000, 2000),
        };
        let next = issue_replacement(&previous, 1000);
        assert_eq!(next.generation, generation(1), "the generation increments");
        assert_eq!(next.invoice_range, InvoiceRange::new(2000, 3000));
        assert!(
            next.invoice_range.is_disjoint_from(previous.invoice_range),
            "the new range shares no number with the old one"
        );
    }

    #[test]
    fn consecutive_replacements_never_overlap() {
        let mut grant = LeaseGrant {
            generation: generation(0),
            invoice_range: InvoiceRange::new(0, 500),
        };
        let mut history = vec![grant];
        for _ in 0..5_u8 {
            let next = issue_replacement(&grant, 500);
            assert_eq!(
                next.generation.value(),
                grant.generation.value() + 1,
                "each swap strictly increases the generation"
            );
            for earlier in &history {
                assert!(
                    next.invoice_range.is_disjoint_from(earlier.invoice_range),
                    "a fresh range is disjoint from every range ever issued before it"
                );
            }
            history.push(next);
            grant = next;
        }
    }

    #[test]
    fn invoice_range_contains_and_disjointness() {
        let range = InvoiceRange::new(10, 20);
        assert!(range.contains(10) && range.contains(19));
        assert!(!range.contains(20), "the end is exclusive");
        assert!(!range.contains(9));
        assert!(range.is_disjoint_from(InvoiceRange::new(20, 30)));
        assert!(!range.is_disjoint_from(InvoiceRange::new(19, 30)));
        assert!(InvoiceRange::new(5, 5).is_empty());
    }
}
