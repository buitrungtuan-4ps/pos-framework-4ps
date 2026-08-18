// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two ports the domain is allowed to hold.
//!
//! # Why these live here and not in `pos-ports`
//!
//! `pos-core` must not depend on `pos-ports` at all — see
//! [ADR-0013](../../../docs/adr/0013-async-strategy.md). Cargo unifies features across
//! the whole build graph, so a single ports crate with its I/O traits behind a feature
//! would still make those traits nameable from the domain the moment any binary
//! enabled the feature. Keeping the crates siblings turns "the domain performs no
//! I/O" into a property of the dependency graph, checkable with
//! `cargo tree -p pos-core`, rather than a lint somebody has to maintain.
//!
//! These two traits therefore live in `pos-proto`, which both siblings already depend
//! on, and `pos-ports` re-exports them so the documented sixteen-port list keeps
//! exactly one definition of each.
//!
//! # Why they are synchronous when the other fourteen are not
//!
//! Because they are not I/O. Reading a clock and minting an identifier are ambient
//! values, not conversations with another system: neither can block, fail for network
//! reasons, or need cancelling. That makes them ordinary `fn`, trait-object
//! compatible, and trivial to fake.
//!
//! # Why they are ports at all
//!
//! `AGENTS.md` §2 bans calling `SystemTime::now`, `Instant::now` or a random generator
//! directly from the domain, and `clippy.toml` enforces it through `disallowed-methods`.
//! The reason is testability of things that are otherwise almost untestable: a shift
//! crossing midnight, a promotion expiring between adding a line and paying for it, a
//! business date resolved either side of a 04:00 cut-off. With time arriving through a
//! port, those are ordinary unit tests instead of a note asking somebody to try it at
//! four in the morning.

use crate::time::Timestamp;
use crate::ulid::Ulid;

/// Supplies the current instant.
///
/// The only sanctioned source of "now". Implementations wrap the system clock, or an
/// SNTP-corrected clock with drift detection at the edge, or a fixed value in a test.
///
/// Note how the domain consumes this: `pos-core` is handed the **result** of one call
/// for the whole decision, not the source. It therefore cannot read the clock twice
/// within a single decision and get two answers, which removes a class of
/// nondeterminism and makes replaying an event stream exact.
pub trait ClockSource {
    /// The current instant, UTC, to millisecond precision.
    #[must_use]
    fn now(&self) -> Timestamp;
}

/// Mints identifiers.
///
/// `&mut self` is required rather than incidental: ULID monotonicity within a single
/// millisecond is stateful, and an implementation must also **clamp to a
/// non-decreasing timestamp**. Without that clamp an NTP step backwards would emit
/// identifiers that sort before ones already issued, and time-sortability is not a
/// nicety here — the event feed pages by `page_token=<ulid>`, so an out-of-order
/// identifier makes a consumer skip events it has never seen.
pub trait IdGenerator {
    /// The next identifier, never lower than any previously returned.
    #[must_use]
    fn next_id(&mut self) -> Ulid;
}

#[cfg(test)]
mod tests {
    use super::{ClockSource, IdGenerator};
    use crate::time::Timestamp;
    use crate::ulid::Ulid;

    struct FixedClock(Timestamp);

    impl ClockSource for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    /// Counts upward from a seed, which is all a deterministic test needs.
    struct SequentialIds(u128);

    impl IdGenerator for SequentialIds {
        fn next_id(&mut self) -> Ulid {
            self.0 = self.0.saturating_add(1);
            Ulid::from_u128(self.0)
        }
    }

    #[test]
    fn both_ports_are_trivially_fakeable() {
        // The reason the domain suite runs in milliseconds with no runtime: faking
        // time and identity costs four lines each.
        let instant = Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("builds");
        let clock = FixedClock(instant);
        assert_eq!(clock.now(), instant);

        let mut ids = SequentialIds(0);
        assert!(ids.next_id() < ids.next_id());
    }

    #[test]
    fn both_ports_are_trait_object_compatible() {
        // Which the fourteen asynchronous ports are not, and is why these two can be
        // held as `&mut dyn` inside a decision context.
        let instant = Timestamp::from_milliseconds_since_epoch(0).expect("builds");
        let clock: &dyn ClockSource = &FixedClock(instant);
        assert_eq!(clock.now(), instant);

        let mut generator = SequentialIds(41);
        let ids: &mut dyn IdGenerator = &mut generator;
        assert_eq!(ids.next_id(), Ulid::from_u128(42));
    }
}
