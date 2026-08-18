// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `ClockSource` and `IdGenerator` suites.
//!
//! Synchronous, like the ports themselves, and short — but
//! [`identifiers_never_go_backwards_when_the_clock_does`] earns the whole module. Stores correct
//! drift over SNTP (`docs/architecture.md` §8), so time *does* step backwards. An identifier issued
//! after such a step that sorts below one already issued makes the event feed skip: the feed pages
//! by `page_token=<ulid>`, so a consumer past that point never sees it, and nothing reports an
//! error.
//!
//! Both suites take an explicit clock rather than reading the real one — which is the whole reason
//! these are ports at all ([`pos_proto::determinism`]).

use pos_ports::PortName;
use pos_proto::{ClockSource, IdGenerator};

use crate::harness::{ClockSourceHarness, IdGeneratorHarness};
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `ClockSource` case as a `#[test]`.
///
/// Both suites are synchronous, so `$block_on` is not needed — and asking for one would make a
/// caller invent an executor for a function that cannot await.
#[macro_export]
macro_rules! clock_source_suite {
    ($harness:expr) => {
        $crate::contract_cases_sync! {
            harness = $harness,
            port = $crate::__PORT_CLOCK_SOURCE,
            module = determinism,
            cases = [
                reads_the_instant_it_was_given,
                follows_the_clock_forwards_and_backwards,
            ]
        }
    };
}

/// Emits every `IdGenerator` case as a `#[test]`.
#[macro_export]
macro_rules! id_generator_suite {
    ($harness:expr) => {
        $crate::contract_cases_sync! {
            harness = $harness,
            port = $crate::__PORT_ID_GENERATOR,
            module = determinism,
            cases = [
                mints_distinct_identifiers,
                identifiers_ascend_within_one_millisecond,
                identifiers_never_go_backwards_when_the_clock_does,
                identifiers_carry_the_clocks_millisecond,
            ]
        }
    };
}

fn clock_rule() -> Obligation {
    Obligation::new(PortName::ClockSource, "the only sanctioned source of now")
}

fn monotonicity() -> Obligation {
    Obligation::new(
        PortName::IdGenerator,
        "never lower than any previously returned",
    )
}

/// A clock reports what it was set to.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub fn reads_the_instant_it_was_given<H: ClockSourceHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let clock = harness.fresh(fixtures::instant())?;
    clock_rule().require_eq(
        &clock.now(),
        &fixtures::instant(),
        "a clock set to an instant reads that instant",
    )
}

/// Including backwards, which is what an NTP step does.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub fn follows_the_clock_forwards_and_backwards<H: ClockSourceHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let clock = harness.fresh(fixtures::instant())?;
    let obligation = clock_rule();

    harness.set(&clock, fixtures::at_offset(1_000))?;
    obligation.require_eq(
        &clock.now(),
        &fixtures::at_offset(1_000),
        "the clock moved forward",
    )?;

    harness.set(&clock, fixtures::at_offset(-1_000))?;
    obligation.require_eq(
        &clock.now(),
        &fixtures::at_offset(-1_000),
        "and backwards. A clock that refused to go backwards would hide the drift correction that \
         the identifier monotonicity case exists to survive",
    )
}

/// No repeats.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub fn mints_distinct_identifiers<H: IdGeneratorHarness>(harness: &H) -> Result<(), CaseFailure> {
    let mut generator = harness.fresh(fixtures::instant())?;
    let mut issued = Vec::new();
    for _ in 0..1_000_u32 {
        issued.push(generator.next_id());
    }
    let total = issued.len();
    issued.sort_unstable();
    issued.dedup();
    monotonicity().require_eq(
        &issued.len(),
        &total,
        "a thousand identifiers from a frozen clock are all distinct — the event identifier is \
         also the receiver's idempotency key, so a repeat silently drops an event",
    )
}

/// Within one millisecond, they still ascend.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub fn identifiers_ascend_within_one_millisecond<H: IdGeneratorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let mut generator = harness.fresh(fixtures::instant())?;
    let issued: Vec<_> = (0..100_u32).map(|_| generator.next_id()).collect();
    monotonicity().require_ascending(
        &issued,
        |id| *id,
        "identifiers minted inside one millisecond must still ascend — a busy store mints several \
         per millisecond, and the feed pages by identifier",
    )
}

/// The case this module exists for.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub fn identifiers_never_go_backwards_when_the_clock_does<H: IdGeneratorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let mut generator = harness.fresh(fixtures::at_offset(10_000))?;
    let before = generator.next_id();

    // An SNTP correction of ten seconds backwards. Not hypothetical: it is what a store whose clock
    // has drifted does the moment it reconnects.
    harness.set(&generator, fixtures::instant())?;
    let after = generator.next_id();

    let obligation = monotonicity();
    obligation.require(
        after > before,
        format!(
            "after the clock stepped back ten seconds, the generator returned {after} which does \
             not exceed {before}. The event feed pages by page_token=<ulid>, so a consumer past \
             {before} never sees {after} — and nothing reports an error. The implementation must \
             clamp to a non-decreasing timestamp"
        ),
    )?;

    // And it must keep working, not just clamp once.
    let mut previous = after;
    for _ in 0..10_u32 {
        let next = generator.next_id();
        obligation.require(
            next > previous,
            "and it keeps ascending after the clamp rather than sticking",
        )?;
        previous = next;
    }
    Ok(())
}

/// Under normal conditions the timestamp is real, or the identifier is not time-sortable.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub fn identifiers_carry_the_clocks_millisecond<H: IdGeneratorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let mut generator = harness.fresh(fixtures::instant())?;
    let identifier = generator.next_id();
    let obligation = monotonicity();
    // A ULID's timestamp is unsigned — the format has no room for an instant before 1970 — while a
    // `Timestamp` is signed, so the comparison has to convert rather than assume.
    let expected = u64::try_from(fixtures::instant().as_milliseconds_since_epoch())
        .map_err(|_| CaseFailure::new("the reference instant is before 1970"))?;
    obligation.require_eq(
        &identifier.timestamp_ms(),
        &expected,
        "an identifier's timestamp is the clock's. Without it the clamp above would be the only \
         thing ordering identifiers, and two stores' feeds would interleave arbitrarily",
    )?;

    harness.set(&generator, fixtures::at_offset(5_000))?;
    let later = generator.next_id();
    let expected_later = u64::try_from(fixtures::at_offset(5_000).as_milliseconds_since_epoch())
        .map_err(|_| CaseFailure::new("the reference instant is before 1970"))?;
    obligation.require_eq(
        &later.timestamp_ms(),
        &expected_later,
        "and it tracks the clock forwards",
    )
}
