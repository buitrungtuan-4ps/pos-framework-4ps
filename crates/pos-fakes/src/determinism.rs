// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A settable clock and a monotonic identifier generator.
//!
//! [`FakeIdGenerator`] is the one fake here with real logic in it. Its clamp is the same clamp a
//! production generator needs, and the contract case that drives the clock ten seconds backwards
//! checks it — so this is not a stub, it is the reference implementation of a rule that is easy to
//! get wrong and silent when wrong.

use std::sync::{Arc, Mutex};

use pos_proto::{ClockSource, IdGenerator, Timestamp, Ulid};

use crate::lock;

/// A clock that reads whatever it was last set to.
///
/// Shared by handle, so a harness can move it while an implementation holds it.
#[derive(Debug, Clone)]
pub struct FakeClock {
    at: Arc<Mutex<Timestamp>>,
}

impl FakeClock {
    /// A clock reading `at`.
    #[must_use]
    pub fn new(at: Timestamp) -> Self {
        Self {
            at: Arc::new(Mutex::new(at)),
        }
    }

    /// Moves the clock, forwards or backwards.
    ///
    /// Backwards matters: `docs/architecture.md` §8 has stores correcting drift over SNTP, and that
    /// correction is what the identifier clamp exists to survive.
    pub fn set(&self, at: Timestamp) {
        *lock(&self.at) = at;
    }
}

impl ClockSource for FakeClock {
    fn now(&self) -> Timestamp {
        *lock(&self.at)
    }
}

/// Mints ULIDs from a [`FakeClock`], clamped to be non-decreasing.
///
/// # The clamp
///
/// A ULID is a 48-bit millisecond timestamp followed by 80 bits of randomness, and it is sortable
/// only because the timestamp leads. When the clock steps backwards, a generator that used it
/// directly would emit an identifier sorting below ones already issued — and because the event feed
/// pages by `page_token=<ulid>`, a consumer past that point would never see it. No error, no gap in
/// any count, just missing sales.
///
/// So: track the highest millisecond issued, never emit below it, and increment the counter within a
/// millisecond. The counter stands in for randomness, which is what makes a fake deterministic; a
/// production generator draws from a vetted CSPRNG and keeps this same clamp.
#[derive(Debug, Clone)]
pub struct FakeIdGenerator {
    clock: FakeClock,
    state: Arc<Mutex<GeneratorState>>,
}

#[derive(Debug)]
struct GeneratorState {
    highest_millisecond: u64,
    counter: u64,
}

impl FakeIdGenerator {
    /// A generator reading `clock`.
    #[must_use]
    pub fn new(clock: FakeClock) -> Self {
        Self {
            clock,
            state: Arc::new(Mutex::new(GeneratorState {
                highest_millisecond: 0,
                counter: 0,
            })),
        }
    }

    /// The clock this generator reads, so a harness can move it.
    #[must_use]
    pub fn clock(&self) -> &FakeClock {
        &self.clock
    }
}

impl IdGenerator for FakeIdGenerator {
    fn next_id(&mut self) -> Ulid {
        let observed = u64::try_from(self.clock.now().as_milliseconds_since_epoch()).unwrap_or(0);
        let mut state = lock(&self.state);

        if observed > state.highest_millisecond {
            state.highest_millisecond = observed;
            state.counter = 0;
        } else {
            // Either the clock stood still or it went backwards. Both are handled the same way, and
            // that is the point: the generator does not need to know which, only that it must not
            // go down.
            state.counter = state.counter.saturating_add(1);
        }

        Ulid::from_parts(state.highest_millisecond, u128::from(state.counter))
    }
}

#[cfg(test)]
mod tests {
    use super::{FakeClock, FakeIdGenerator};
    use pos_proto::{ClockSource, IdGenerator, Timestamp};

    fn at(milliseconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(milliseconds).unwrap_or(Timestamp::EPOCH)
    }

    #[test]
    fn the_clock_moves_both_ways() {
        let clock = FakeClock::new(at(1_000));
        assert_eq!(clock.now(), at(1_000));
        clock.set(at(500));
        assert_eq!(clock.now(), at(500));
    }

    #[test]
    fn a_backwards_step_does_not_lower_an_identifier() {
        // The unit-level twin of the contract case. Kept here as well because this is where the
        // clamp lives, and a failure here should point at this file.
        let clock = FakeClock::new(at(10_000));
        let mut generator = FakeIdGenerator::new(clock.clone());
        let before = generator.next_id();

        clock.set(at(1_000));
        let after = generator.next_id();
        assert!(
            after > before,
            "after a backwards clock step, {after} must still exceed {before}"
        );
        assert_eq!(
            after.timestamp_ms(),
            before.timestamp_ms(),
            "and it holds the high-water millisecond rather than the clock's"
        );
    }

    #[test]
    fn identifiers_ascend_within_a_millisecond() {
        let clock = FakeClock::new(at(1_000));
        let mut generator = FakeIdGenerator::new(clock);
        let issued: Vec<_> = (0..1_000_u32).map(|_| generator.next_id()).collect();
        for (left, right) in issued.iter().zip(issued.iter().skip(1)) {
            assert!(left < right);
        }
    }
}
