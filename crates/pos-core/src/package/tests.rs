// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Runs the vectors the till runs too, so the two implementations of the rule stay equal.

use serde::Deserialize;

use pos_proto::package::{TimeSlot, Variant};

use super::{VariantContext, VariantRefusal, check_variants, current_time_slot, resolve};
use crate::campaign::{LocalTime, Weekday};

const VECTORS: &str = include_str!("../../../pos-proto/testdata/package_variants.json");

#[derive(Deserialize)]
struct Vectors {
    resolve: Vec<ResolveCase>,
    current_time_slot: Vec<SlotCase>,
    check: Vec<CheckCase>,
}

#[derive(Deserialize)]
struct ResolveCase {
    name: String,
    variants: Vec<Variant<String>>,
    context: VariantContext,
    expect: String,
}

#[derive(Deserialize)]
struct SlotCase {
    name: String,
    time_slots: Vec<TimeSlot>,
    at: At,
    expect: Option<String>,
}

#[derive(Deserialize)]
struct At {
    weekday: usize,
    minute_of_day: u16,
}

#[derive(Deserialize)]
struct CheckCase {
    name: String,
    variants: Vec<Variant<String>>,
    time_slots: Vec<TimeSlot>,
    expect: String,
}

fn vectors() -> Vectors {
    serde_json::from_str(VECTORS).expect("the vectors parse")
}

const WEEK: [Weekday; 7] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
    Weekday::Sunday,
];

#[test]
fn every_resolve_vector_answers_as_written() {
    let cases = vectors().resolve;
    assert!(!cases.is_empty());
    for case in cases {
        let answer = resolve(&case.variants, &case.context);
        assert_eq!(answer, Some(&case.expect), "{}", case.name);
    }
}

#[test]
fn every_time_slot_vector_answers_as_written() {
    for case in vectors().current_time_slot {
        let at = LocalTime {
            weekday: *WEEK.get(case.at.weekday).expect("a weekday from 0 to 6"),
            minute_of_day: case.at.minute_of_day,
        };
        let answer = current_time_slot(&case.time_slots, at);
        assert_eq!(answer, case.expect.as_deref(), "{}", case.name);
    }
}

#[test]
fn every_check_vector_answers_as_written() {
    for case in vectors().check {
        let answer = match check_variants(&case.variants, &case.time_slots) {
            Ok(()) => "OK",
            Err(VariantRefusal::NoBaseVariant) => "NO_BASE_VARIANT",
            Err(VariantRefusal::SameSelector { .. }) => "SAME_SELECTOR",
            Err(VariantRefusal::DeviceStatedTwice(_)) => "DEVICE_STATED_TWICE",
            Err(VariantRefusal::UnknownTimeSlot(_)) => "UNKNOWN_TIME_SLOT",
        };
        assert_eq!(answer, case.expect, "{}", case.name);
    }
}

/// Every resolve vector's variants are ones the publish check passes, so no vector pins an answer
/// for variants a store could never receive.
#[test]
fn every_resolve_vector_is_publishable() {
    let slots: Vec<TimeSlot> = serde_json::from_str(
        r#"[{"time_slot_id":"lunch","label_key":"package.slot.lunch","schedules":[]}]"#,
    )
    .expect("slots parse");
    for case in vectors().resolve {
        assert_eq!(
            check_variants(&case.variants, &slots),
            Ok(()),
            "{}",
            case.name
        );
    }
}

/// Variants with no base answer nothing rather than something arbitrary.
#[test]
fn variants_without_a_base_answer_nothing_outside_their_selectors() {
    let variants: Vec<Variant<String>> = serde_json::from_str(
        r#"[{"selector":{"device_class":"DEVICE_CLASS_TABLET"},"body":"tablet"}]"#,
    )
    .expect("variants parse");
    assert_eq!(resolve(&variants, &VariantContext::default()), None);
}
