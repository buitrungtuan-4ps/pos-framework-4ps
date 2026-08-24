// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's ULID [`IdGenerator`]: monotonic and time-sortable.
//!
//! `pos-core` mints no identifiers itself — generation is I/O-adjacent and lives behind the
//! [`IdGenerator`] port ([ADR-0013](../../../docs/adr/0013-async-strategy.md)). This is the edge's
//! implementation. Two properties matter, both from
//! [`pos_proto::determinism`](pos_proto::determinism::IdGenerator):
//!
//! - **Non-decreasing timestamp.** If the clock steps backwards (an NTP correction), the generator
//!   clamps to the last millisecond it used, so an id never sorts before one already handed out — the
//!   event feed pages by `page_token=<ulid>`, and an out-of-order id would make a consumer skip
//!   events it never saw.
//! - **Monotonic within a millisecond.** Two ids in the same millisecond differ by incrementing the
//!   random component, so they are strictly increasing rather than merely distinct.
//!
//! The 80 random bits come from a `SplitMix64` stream seeded once from the OS CSPRNG (`getrandom`).
//! A ULID's randomness is not a secret — unlike a pairing code or a device token, which take OS
//! entropy directly ([`crate::pairing`]) — and within-millisecond uniqueness is guaranteed by the
//! increment regardless, so a fast non-cryptographic stream is the right tool here.

use pos_proto::ulid::Ulid;
use pos_proto::{ClockSource, IdGenerator};

/// The low 80 bits of a `u128` — the ULID random component.
const RANDOM_MASK: u128 = (1_u128 << 80) - 1;

/// A monotonic, time-sortable ULID generator over a [`ClockSource`].
#[derive(Debug)]
pub struct EdgeIdGenerator<C> {
    clock: C,
    rng: SplitMix64,
    last_ms: u64,
    last_random: u128,
}

impl<C: ClockSource> EdgeIdGenerator<C> {
    /// Seeds the id stream from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`getrandom::Error`] if the OS entropy source is unavailable — the stream is never seeded with
    /// a fixed value in production.
    pub fn new(clock: C) -> Result<Self, getrandom::Error> {
        let mut seed = [0_u8; 8];
        getrandom::fill(&mut seed)?;
        Ok(Self {
            clock,
            rng: SplitMix64::new(u64::from_le_bytes(seed)),
            last_ms: 0,
            last_random: 0,
        })
    }

    /// Builds a generator with a fixed seed, for deterministic tests.
    #[cfg(test)]
    fn seeded(clock: C, seed: u64) -> Self {
        Self {
            clock,
            rng: SplitMix64::new(seed),
            last_ms: 0,
            last_random: 0,
        }
    }

    /// A fresh 80-bit random component.
    fn fresh_random(&mut self) -> u128 {
        let high = u128::from(self.rng.next_u64());
        let low = u128::from(self.rng.next_u64());
        ((high << 64) | low) & RANDOM_MASK
    }
}

impl<C: ClockSource> IdGenerator for EdgeIdGenerator<C> {
    fn next_id(&mut self) -> Ulid {
        let now = self.clock.now().as_milliseconds_since_epoch();
        let now_ms = u64::try_from(now).unwrap_or(0);
        // Never move the timestamp backwards.
        let mut ms = now_ms.max(self.last_ms);

        let random = if ms == self.last_ms {
            let next = self.last_random.wrapping_add(1) & RANDOM_MASK;
            if next == 0 {
                // The random space wrapped inside one millisecond — astronomically unlikely, but if
                // it ever happens, step to the next millisecond and draw fresh rather than repeat.
                ms = ms.saturating_add(1);
                self.fresh_random()
            } else {
                next
            }
        } else {
            self.fresh_random()
        };

        self.last_ms = ms;
        self.last_random = random;
        Ulid::from_parts(ms, random)
    }
}

/// A tiny, well-known non-cryptographic PRNG. It seeds the ULID random component; it is never used
/// for a secret (those take OS entropy directly).
#[derive(Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::EdgeIdGenerator;
    use pos_proto::time::Timestamp;
    use pos_proto::{ClockSource, IdGenerator};
    use std::cell::Cell;
    use std::rc::Rc;

    /// A clock the test can move after the generator holds it — `Rc` so both keep a handle, `Cell`
    /// so the test can freeze, advance, or step it backwards.
    #[derive(Clone)]
    struct ScriptedClock(Rc<Cell<i64>>);

    impl ScriptedClock {
        fn at(ms: i64) -> Self {
            Self(Rc::new(Cell::new(ms)))
        }
        fn set(&self, ms: i64) {
            self.0.set(ms);
        }
    }

    impl ClockSource for ScriptedClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_milliseconds_since_epoch(self.0.get()).expect("valid instant")
        }
    }

    #[test]
    fn ids_strictly_increase_within_one_millisecond() {
        let mut generator = EdgeIdGenerator::seeded(ScriptedClock::at(1_700_000_000_000), 42);
        let first = generator.next_id();
        let second = generator.next_id();
        let third = generator.next_id();
        assert!(first < second, "same-ms ids increment to stay ordered");
        assert!(second < third);
    }

    #[test]
    fn ids_increase_across_milliseconds() {
        let clock = ScriptedClock::at(1_700_000_000_000);
        let mut generator = EdgeIdGenerator::seeded(clock.clone(), 7);
        let first = generator.next_id();
        clock.set(1_700_000_000_050);
        let second = generator.next_id();
        assert!(
            second > first,
            "a later millisecond sorts after an earlier one"
        );
    }

    #[test]
    fn a_backwards_clock_still_yields_increasing_ids() {
        let clock = ScriptedClock::at(1_700_000_000_005);
        let mut generator = EdgeIdGenerator::seeded(clock.clone(), 99);
        let first = generator.next_id();
        clock.set(1_700_000_000_000); // an NTP correction steps the clock back 5 ms
        let second = generator.next_id();
        assert!(
            second > first,
            "the timestamp clamp keeps an id from sorting before one already issued"
        );
    }

    #[test]
    fn a_fixed_seed_and_clock_reproduce_the_id() {
        let build = || EdgeIdGenerator::seeded(ScriptedClock::at(1_700_000_000_000), 7).next_id();
        assert_eq!(
            build(),
            build(),
            "generation is a pure function of seed and clock"
        );
    }
}
