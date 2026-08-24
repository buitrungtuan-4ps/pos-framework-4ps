// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Deterministic test data.
//!
//! Every value here is derived from a seed, and nothing reads a clock or a random source. That
//! is not tidiness — a contract suite that produced different data on each run would report
//! failures nobody could reproduce, and the first response to an unreproducible failure is to
//! disable the test.
//!
//! # Why these functions panic rather than returning `Result`
//!
//! The first draft of this module returned a fallback value when a fixture could not be built —
//! the epoch instead of the reference instant, `{}` instead of the payload. That is worse than a
//! panic, and quietly so: every ordering assertion in every suite would still have passed while
//! comparing the wrong things. A fixture is not the thing under test, so the only useful
//! behaviour when one cannot be constructed is to stop the run and say which one.

#![expect(
    clippy::expect_used,
    reason = "a fixture that cannot be built must stop the test run and name itself. The \
              alternative — substituting a fallback value — makes every assertion downstream \
              pass while comparing something other than what it claims to compare."
)]

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{BrandId, DeviceId, EventId, StoreId, TenantId};
use pos_proto::{BusinessDate, Timestamp, Ulid};

/// A fixed instant, so a failure at a boundary is a failure at the same boundary tomorrow.
///
/// 2026-01-01T00:00:00Z. Chosen for legibility rather than significance.
pub const EPOCH_MILLISECONDS: i64 = 1_767_225_600_000;

/// The reference instant.
///
/// # Panics
///
/// If [`EPOCH_MILLISECONDS`] is not a representable instant, which would mean this file had been
/// edited wrongly rather than that anything under test had failed.
#[must_use]
pub fn instant() -> Timestamp {
    at_offset(0)
}

/// The reference instant plus `milliseconds`, which may be negative.
///
/// # Panics
///
/// If the result is not a representable instant.
#[must_use]
pub fn at_offset(milliseconds: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(EPOCH_MILLISECONDS.saturating_add(milliseconds))
        .expect("the reference instant plus a small offset is representable")
}

/// The reference business date.
///
/// # Panics
///
/// If 2026-01-01 is not a valid date.
#[must_use]
pub fn business_date() -> BusinessDate {
    BusinessDate::from_ymd(2026, 1, 1).expect("2026-01-01 is a valid date")
}

/// An event identifier derived from `seed`, so `event_id(1) < event_id(2)`.
///
/// Ordering by seed is what lets a case assert read-back order without knowing anything about
/// ULID internals.
#[must_use]
pub fn event_id(seed: u32) -> EventId {
    EventId::new(Ulid::from_u128(u128::from(seed)))
}

/// An envelope for a `device.activation.completed` event, with `event_id(seed)`.
///
/// That event type because it is the least meaningful one in the catalogue: a single identifier
/// field, no money, no order, no state machine. A suite that used `sales.order_line.added`
/// would be quietly testing the domain instead of the port.
///
/// # Panics
///
/// If the fixture payload cannot be serialised.
#[must_use]
pub fn activation(store_id: StoreId, seed: u32) -> EventEnvelope<RawPayload> {
    envelope(
        store_id,
        seed,
        &pos_proto::events::DeviceActivationCompleted {
            activated_device_id: DeviceId::new(Ulid::from_u128(u128::from(seed))),
        },
    )
}

/// An envelope carrying `payload`, with `event_id(seed)`.
///
/// Taking the payload as a value rather than as JSON text keeps the fixture honest: an event
/// whose body does not match its declared type is not something a store should have to store.
///
/// # Panics
///
/// If `payload` cannot be serialised.
#[must_use]
pub fn envelope<P>(store_id: StoreId, seed: u32, payload: &P) -> EventEnvelope<RawPayload>
where
    P: pos_proto::envelope::EventPayload + serde::Serialize,
{
    EventEnvelope {
        event_id: event_id(seed),
        event_type: P::EVENT_TYPE.into(),
        event_time: at_offset(i64::from(seed)),
        business_date: business_date(),
        schema_version: P::SCHEMA_VERSION,
        tenant_id: TenantId::new(Ulid::from_u128(1)),
        brand_id: BrandId::new(Ulid::from_u128(1)),
        store_id,
        device_id: DeviceId::new(Ulid::from_u128(1)),
        employee_id: None,
        shift_id: None,
        data: RawPayload::encode(payload).expect("a fixture payload serialises"),
    }
}

/// The same `event_id` as `activation(store_id, seed)`, with a different body.
///
/// Exists for one obligation: appending an identifier already stored must keep the stored copy
/// and report a duplicate, *without comparing bodies*. Proving that needs two events that
/// collide on identity and differ in content.
///
/// # Panics
///
/// If the fixture payload cannot be serialised.
#[must_use]
pub fn activation_with_other_body(store_id: StoreId, seed: u32) -> EventEnvelope<RawPayload> {
    envelope(
        store_id,
        seed,
        &pos_proto::events::DeviceActivationCompleted {
            // A different device, so the bodies differ while the identity does not.
            activated_device_id: DeviceId::new(Ulid::from_u128(u128::MAX)),
        },
    )
}

/// The reference calendar date.
///
/// Distinct from [`business_date`] and deliberately so: a tax authority recognises calendar days
/// and no cut-off hour, so `Fiscalization` takes this type and `ErpSink` takes the other.
/// `pos-proto` makes them mutually unconvertible, which is what stops the mix-up happening by
/// assignment.
///
/// # Panics
///
/// If 2026-01-01 is not a valid date.
#[must_use]
pub fn calendar_date() -> pos_proto::CalendarDate {
    pos_proto::CalendarDate::from_ymd(2026, 1, 1).expect("2026-01-01 is a valid date")
}

/// A configuration document from JSON text.
///
/// Here rather than in the config suite so the `expect` exemption argued for at the top of this
/// module covers it: a malformed literal is a typo in a fixture, and substituting an empty
/// document would make the assertion downstream compare `{}` against `{}` and pass.
///
/// # Panics
///
/// If `json` is not valid JSON.
#[must_use]
pub fn config_document(json: &str) -> pos_ports::ConfigDocument {
    pos_ports::ConfigDocument::new(
        serde_json::value::RawValue::from_string(json.to_owned())
            .expect("a fixture document is valid JSON"),
    )
}

/// A run of `count` events starting at `first_seed`, ordered.
#[must_use]
pub fn activations(
    store_id: StoreId,
    first_seed: u32,
    count: u32,
) -> Vec<EventEnvelope<RawPayload>> {
    (first_seed..first_seed.saturating_add(count))
        .map(|seed| activation(store_id, seed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        EPOCH_MILLISECONDS, activation, activation_with_other_body, activations, at_offset,
        business_date, event_id, instant,
    };
    use pos_proto::Ulid;
    use pos_proto::ids::StoreId;

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(9))
    }

    #[test]
    fn the_reference_instant_and_date_are_what_they_claim() {
        assert_eq!(instant().as_milliseconds_since_epoch(), EPOCH_MILLISECONDS);
        assert_eq!(business_date().year(), 2026);
        assert_eq!(business_date().month(), 1);
        assert_eq!(business_date().day(), 1);
    }

    #[test]
    fn identifiers_sort_by_seed() {
        // Which is what lets a case assert read-back order without knowing anything about how a
        // ULID is laid out.
        assert!(event_id(1).as_ulid() < event_id(2).as_ulid());
        assert!(event_id(2).as_ulid() < event_id(1_000).as_ulid());
    }

    #[test]
    fn offsets_move_the_clock_in_both_directions() {
        assert!(at_offset(-1) < instant());
        assert!(at_offset(1) > instant());
    }

    #[test]
    fn a_run_of_events_is_contiguous_and_ordered() {
        let run = activations(store(), 10, 5);
        assert_eq!(run.len(), 5);
        assert_eq!(run.first().map(|event| event.event_id), Some(event_id(10)));
        assert_eq!(run.last().map(|event| event.event_id), Some(event_id(14)));
        for (left, right) in run.iter().zip(run.iter().skip(1)) {
            assert!(
                left.event_id.as_ulid() < right.event_id.as_ulid(),
                "a run must be ordered, or every ordering case is vacuous"
            );
        }
    }

    #[test]
    fn the_colliding_fixture_collides_on_identity_and_differs_in_body() {
        // Both halves matter. Same identifier, or the idempotency case tests nothing; different
        // body, or it cannot tell "kept the stored copy" from "overwrote with an identical one".
        let original = activation(store(), 7);
        let collision = activation_with_other_body(store(), 7);
        assert_eq!(original.event_id, collision.event_id);
        assert_ne!(original.data.as_json(), collision.data.as_json());
    }
}
