// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which variant of a package part a device runs, and the checks that make that answer certain
//! ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 4).
//!
//! Brand and store are resolved in the cloud, by the pin. Device, role, sales channel and time slot
//! change within one store's day, so they are resolved here, on the store, by [`resolve`]. The till
//! implements the same rule in TypeScript, and both run the vectors in
//! `crates/pos-proto/testdata/package_variants.json`.
//!
//! # The rule
//!
//! 1. A variant is a candidate when every dimension its selector states matches the context: the
//!    device by `device_id`, or by class; the role by one of the signed-in person's roles; the sales
//!    channel; the store's current time slot ([`current_time_slot`]).
//! 2. The candidate stating the most dimensions wins.
//! 3. A tie goes by the fixed order device, role, sales channel, time slot: a candidate stating an
//!    earlier dimension wins.
//! 4. Then a device named by id wins over a device named by class.
//! 5. Then the variant listed first wins. Only roles reach this step with two candidates, because a
//!    person may hold two roles that each have a variant.
//!
//! Every part has a base variant that states nothing, and no two variants share a selector
//! ([`check_variants`], at publish and on apply), so the rule always answers, and answers the same.

use pos_proto::devices::DeviceClass;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::DeviceId;
use pos_proto::package::{TimeSlot, Variant, VariantSelector};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::{Open, WireEnum};

use crate::campaign::{LocalTime, Schedule, WeekdaySet};

mod content;
mod data;
mod evolution;

pub use content::{ContentRefusal, LABEL_KEY_CHARS_MAX, check_content};
pub use data::{DataRefusal, FieldAt, FieldFault, PERSONAL_FIELDS_AVAILABLE, check_data};
pub use evolution::{EvolutionRefusal, check_evolution};

/// What a variant is chosen against: the device, the person and the moment on screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct VariantContext {
    /// The device the till runs on, when it is paired.
    #[serde(default)]
    pub device_id: Option<DeviceId>,
    /// The device's class. `DEVICE_CLASS_UNSPECIFIED` matches no variant for a class.
    #[serde(default)]
    pub device_class: Open<DeviceClass>,
    /// The roles the signed-in person holds, none while nobody is signed in.
    #[serde(default)]
    pub role_ids: Vec<Ulid>,
    /// The sales channel of the order on screen, if any.
    #[serde(default)]
    pub sales_channel: Open<SalesChannel>,
    /// The store's current time slot ([`current_time_slot`]), if any.
    #[serde(default)]
    pub time_slot_id: Option<String>,
}

/// Why a part's variants cannot be published or applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VariantRefusal {
    /// No variant states nothing, so some context would have nothing to run.
    NoBaseVariant,
    /// Two variants have the same selector, so neither could be told from the other.
    SameSelector {
        /// The position of the first.
        first: usize,
        /// The position of the second.
        second: usize,
    },
    /// A variant names its device both by id and by class.
    DeviceStatedTwice(usize),
    /// A variant names a time slot the package does not declare.
    UnknownTimeSlot(usize),
}

impl core::fmt::Display for VariantRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoBaseVariant => f.write_str("the part has no base variant"),
            Self::SameSelector { first, second } => {
                write!(f, "variants {first} and {second} have the same selector")
            }
            Self::DeviceStatedTwice(index) => {
                write!(
                    f,
                    "variant {index} names its device both by id and by class"
                )
            }
            Self::UnknownTimeSlot(index) => write!(
                f,
                "variant {index} names a time slot the package does not declare"
            ),
        }
    }
}

impl core::error::Error for VariantRefusal {}

/// Checks that [`resolve`] answers for every context, and answers the same: one base variant, no
/// two equal selectors, a device named once, and only the package's own time slots.
///
/// # Errors
///
/// The first [`VariantRefusal`] found, in the variants' order.
pub fn check_variants<T>(
    variants: &[Variant<T>],
    time_slots: &[TimeSlot],
) -> Result<(), VariantRefusal> {
    for (index, variant) in variants.iter().enumerate() {
        let selector = &variant.selector;
        if selector.device_id.is_some() && selector.states_device_class() {
            return Err(VariantRefusal::DeviceStatedTwice(index));
        }
        if let Some(slot) = &selector.time_slot_id
            && !time_slots.iter().any(|known| &known.time_slot_id == slot)
        {
            return Err(VariantRefusal::UnknownTimeSlot(index));
        }
        if let Some(first) = variants
            .iter()
            .take(index)
            .position(|earlier| earlier.selector == *selector)
        {
            return Err(VariantRefusal::SameSelector {
                first,
                second: index,
            });
        }
    }
    if variants.iter().any(|variant| variant.selector.is_base()) {
        Ok(())
    } else {
        Err(VariantRefusal::NoBaseVariant)
    }
}

/// The variant a device runs in `context`, by the rule in the module documentation. `None` only
/// for variants [`check_variants`] refuses, which have no base variant.
#[must_use]
pub fn resolve<'a, T>(variants: &'a [Variant<T>], context: &VariantContext) -> Option<&'a T> {
    let mut best: Option<(Rank, &'a T)> = None;
    for variant in variants {
        if !matches(&variant.selector, context) {
            continue;
        }
        let rank = Rank::of(&variant.selector);
        // Strictly greater, so of two equal ranks the variant listed first stays.
        if best.as_ref().is_none_or(|(held, _)| rank > *held) {
            best = Some((rank, &variant.body));
        }
    }
    best.map(|(_, body)| body)
}

/// The store's current time slot at `at`: the first of `time_slots` one of whose windows holds it.
#[must_use]
pub fn current_time_slot(time_slots: &[TimeSlot], at: LocalTime) -> Option<&str> {
    time_slots
        .iter()
        .find(|slot| {
            slot.schedules.iter().any(|window| {
                Schedule {
                    days: WeekdaySet::from_bits(window.days),
                    start_minute: window.start_minute,
                    end_minute: window.end_minute,
                }
                .is_active_at(at)
            })
        })
        .map(|slot| slot.time_slot_id.as_str())
}

/// How a candidate ranks; a greater rank wins. The number of dimensions stated first, then one bit
/// per step of the tie-break, the earliest step the highest: device, role, sales channel, time
/// slot, then a device named by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank(u8, u8);

impl Rank {
    fn of(selector: &VariantSelector) -> Self {
        let order = [
            selector.states_device(),
            selector.states_role(),
            selector.states_sales_channel(),
            selector.states_time_slot(),
            selector.device_id.is_some(),
        ]
        .into_iter()
        .fold(0_u8, |bits, stated| (bits << 1) | u8::from(stated));
        Self(selector.dimensions_stated(), order)
    }
}

/// Whether every dimension `selector` states matches `context`. A token this release does not know
/// matches nothing, and neither does a stated dimension the context lacks.
fn matches(selector: &VariantSelector, context: &VariantContext) -> bool {
    let device_id = selector
        .device_id
        .is_none_or(|wanted| context.device_id == Some(wanted));
    let device_class = !selector.states_device_class()
        || known_and_equal(&selector.device_class, &context.device_class);
    let role = selector
        .role_id
        .is_none_or(|wanted| context.role_ids.contains(&wanted));
    let sales_channel = !selector.states_sales_channel()
        || known_and_equal(&selector.sales_channel, &context.sales_channel);
    let time_slot = selector
        .time_slot_id
        .as_ref()
        .is_none_or(|wanted| context.time_slot_id.as_ref() == Some(wanted));
    device_id && device_class && role && sales_channel && time_slot
}

/// Whether a stated token is one this release knows and equals the context's.
fn known_and_equal<E: WireEnum>(stated: &Open<E>, held: &Open<E>) -> bool {
    !stated.is_unspecified() && stated.known() == held.known()
}

#[cfg(test)]
mod tests;
