// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `package` config node: the package version a store runs, and how HQ pinned it
//! ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md)).
//!
//! The node has two halves, written by two steps of the cloud's compile:
//!
//! - **The pin** ([`PublishedPackage::mode`] and the version ids beside it) is a setting
//!   (ADR-0189 decision 2), authored at tenant, brand, group or store scope and resolved in the
//!   cloud, most specific scope first.
//! - **The content** ([`PackageContent`]) is the pinned version, compiled into the store's document
//!   at publish (decision 3). The edge applies it, retains it and rolls it back as any node.
//!
//! A package's parts (screens, flows, rules and data definitions, ADR-0190 to ADR-0194) join
//! [`PackageContent`] as additive fields with the phases that type them. Every part may carry
//! variants ([`Variant`]), each chosen on the store by its [`VariantSelector`] through one rule that
//! `pos-core` and the till each implement once, held equal by the shared vectors in
//! `crates/pos-proto/testdata/package_variants.json`.
//!
//! No `deny_unknown_fields`, as for every published node: an edge on an older release applies a
//! node that carries a field it does not know.

use serde::{Deserialize, Serialize};

use crate::campaign::PublishedSchedule;
use crate::devices::DeviceClass;
use crate::enums::SalesChannel;
use crate::flow::Flow;
use crate::ids::{DeviceId, PackageId, PackageVersionId};
use crate::text::{ReleaseTag, TranslationKey};
use crate::ulid::Ulid;
use crate::wire_enum;
use crate::wire_enum::{Open, WireEnum, is_absent};

mod data;

pub use data::{
    CustomField, CustomFieldType, CustomList, DataObject, FIELDS_PER_OBJECT_MAX, LIST_ENTRIES_MAX,
    ListEntry, OBJECT_TYPES_MAX, ObjectType, PackageData, TEXT_BYTES_MAX,
};

/// The most versions a choice may allow.
pub const ALLOWED_VERSIONS_MAX: usize = 32;

/// The largest compiled version, in bytes of its JSON; a larger one is refused at publish
/// (ADR-0189 decision 3).
pub const CONTENT_BYTES_MAX: usize = 1024 * 1024;

/// The most time slots a package declares.
pub const TIME_SLOTS_MAX: usize = 16;

/// The most weekly windows one time slot is made of.
pub const SCHEDULES_PER_TIME_SLOT_MAX: usize = 14;

/// The longest `version_label` and `time_slot_id`, in characters.
pub const LABEL_CHARS_MAX: usize = 64;

wire_enum! {
    /// How a store is pinned to a package version (ADR-0189 decision 2).
    ///
    /// `PACKAGE_MODE_UNSPECIFIED` is no pin: the store runs the default package its release carries
    /// (decision 7).
    PackageMode, prefix = "PACKAGE_MODE";
    /// The store runs `locked_version_id`.
    Locked = "LOCKED",
    /// The store runs the version its people chose from `allowed_version_ids`, or
    /// `default_version_id` until they choose.
    Choose = "CHOOSE",
}

/// The `package` node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPackage {
    /// How the store is pinned. Left off the wire while absent.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub mode: Open<PackageMode>,
    /// The version a [`PackageMode::Locked`] store runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_version_id: Option<PackageVersionId>,
    /// The versions a [`PackageMode::Choose`] store's people may choose from, at most
    /// [`ALLOWED_VERSIONS_MAX`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_version_ids: Vec<PackageVersionId>,
    /// The version a [`PackageMode::Choose`] store runs until its people choose; one of
    /// [`Self::allowed_version_ids`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_version_id: Option<PackageVersionId>,
    /// The version the store's people chose: written only at store scope, only from the allowed
    /// list, and only by a console user holding `admin.package.choose` for the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_version_id: Option<PackageVersionId>,
    /// The pinned version, as the cloud compiled it into this store's document. Absent while the
    /// store runs the default package, and while it waits for an edge release the version needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<PackageContent>,
}

impl PublishedPackage {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "package";
}

/// One published package version, as it rides the store's document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackageContent {
    /// The package.
    pub package_id: PackageId,
    /// This version. A published version never changes.
    pub package_version_id: PackageVersionId,
    /// The version's name as HQ reads it in the console, at most [`LABEL_CHARS_MAX`] characters.
    /// Never shown on a till.
    pub version_label: String,
    /// The oldest edge release that runs this version, which the cloud computes at publish
    /// (ADR-0189 decision 5). An edge on an older release refuses the version as a whole.
    pub min_edge_version: ReleaseTag,
    /// The package's named time slots, in the order that decides which one is current.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub time_slots: Vec<TimeSlot>,
    /// The package's flows ([ADR-0192](../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)),
    /// each with its variants; the variants of one flow share its `flow_id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<Variant<Flow>>,
    /// The package's data part: custom fields, object types and lists
    /// ([ADR-0194](../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)).
    /// Empty, and left off the wire, in a version that declares none.
    #[serde(default, skip_serializing_if = "PackageData::is_empty")]
    pub data: PackageData,
}

/// A named part of the week a variant may be chosen for, such as lunch.
///
/// A store is in at most one slot at a time: the first slot of [`PackageContent::time_slots`] one of
/// whose windows holds the store's local time, read in its time zone through `ClockSource`. A slot
/// listed later that also holds that moment is not current until the earlier one ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TimeSlot {
    /// The slot's id within its package, which a [`VariantSelector`] names: at most
    /// [`LABEL_CHARS_MAX`] characters of `a`–`z`, `0`–`9` and `_`.
    pub time_slot_id: String,
    /// The translation key of the slot's name in the console.
    pub label_key: TranslationKey,
    /// The weekly windows the slot is made of, at most [`SCHEDULES_PER_TIME_SLOT_MAX`]. Each is
    /// half-open and may wrap past midnight.
    pub schedules: Vec<PublishedSchedule>,
}

/// One variant of a package part: what a till shows or runs when its selector is the one chosen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Variant<T> {
    /// When the variant applies. Empty for the part's base variant.
    #[serde(default, skip_serializing_if = "VariantSelector::is_base")]
    pub selector: VariantSelector,
    /// The part itself, in the shape the part's own type gives it.
    pub body: T,
}

/// The dimensions a variant states: zero to four of device, role, sales channel and time slot
/// (ADR-0189 decision 4). A dimension left absent matches every store, device and moment.
///
/// The device dimension is stated by `device_id` or by `device_class`, never both, which the cloud
/// refuses at publish. A token from a newer release states its dimension and matches nothing here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VariantSelector {
    /// One device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub device_id: Option<DeviceId>,
    /// Every device of a class.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub device_class: Open<DeviceClass>,
    /// The role a signed-in person holds, by its role id in the tenant's people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub role_id: Option<Ulid>,
    /// The sales channel of the order on screen.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub sales_channel: Open<SalesChannel>,
    /// A time slot of the package, by its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub time_slot_id: Option<String>,
}

impl VariantSelector {
    /// Whether the selector states the device dimension, by id or by class.
    #[must_use]
    pub fn states_device(&self) -> bool {
        self.device_id.is_some() || self.states_device_class()
    }

    /// Whether the selector names a device class, a token from a newer release included.
    #[must_use]
    pub fn states_device_class(&self) -> bool {
        stated(&self.device_class)
    }

    /// Whether the selector states the role dimension.
    #[must_use]
    pub const fn states_role(&self) -> bool {
        self.role_id.is_some()
    }

    /// Whether the selector states the sales-channel dimension.
    #[must_use]
    pub fn states_sales_channel(&self) -> bool {
        stated(&self.sales_channel)
    }

    /// Whether the selector states the time-slot dimension.
    #[must_use]
    pub const fn states_time_slot(&self) -> bool {
        self.time_slot_id.is_some()
    }

    /// How many of the four dimensions the selector states.
    #[must_use]
    pub fn dimensions_stated(&self) -> u8 {
        [
            self.states_device(),
            self.states_role(),
            self.states_sales_channel(),
            self.states_time_slot(),
        ]
        .into_iter()
        .map(u8::from)
        .sum()
    }

    /// Whether the selector states nothing: the part's base variant.
    #[must_use]
    pub fn is_base(&self) -> bool {
        self.dimensions_stated() == 0
    }
}

/// Whether a dimension's token states it: any value but an absent one, a token from a newer release
/// included.
fn stated<E: WireEnum>(value: &Open<E>) -> bool {
    !is_absent(value)
}

#[cfg(test)]
mod tests;
