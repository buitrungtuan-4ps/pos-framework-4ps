// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use super::{PackageMode, PublishedPackage, Variant, VariantSelector};
use crate::devices::DeviceClass;
use crate::enums::SalesChannel;
use crate::wire_enum::Open;

const LOCKED: &str = r#"{"mode":"PACKAGE_MODE_LOCKED","locked_version_id":"01J00000000000000000000V01","content":{"package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01","version_label":"March screens","min_edge_version":"0.15.0","time_slots":[{"time_slot_id":"lunch","label_key":"package.slot.lunch","schedules":[{"days":31,"start_minute":660,"end_minute":840}]}]}}"#;

/// A store with no pin is written as nothing, so a document without the node and one with an empty
/// node read the same: the default package (ADR-0189 decision 7).
#[test]
fn a_node_that_pins_nothing_is_written_as_nothing() {
    let node: PublishedPackage = serde_json::from_str("{}").expect("parses");
    assert_eq!(node, PublishedPackage::default());
    assert_eq!(node.mode.known(), PackageMode::Unspecified);
    assert_eq!(serde_json::to_string(&node).expect("serialise"), "{}");
}

/// The node's bytes are a contract with every edge that holds one: a lock and its content round-trip
/// byte for byte.
#[test]
fn a_locked_node_round_trips_byte_for_byte() {
    let node: PublishedPackage = serde_json::from_str(LOCKED).expect("parses");
    assert_eq!(node.mode.known(), PackageMode::Locked);
    let content = node.content.as_ref().expect("content");
    assert_eq!(Some(content.package_version_id), node.locked_version_id);
    assert_eq!(content.time_slots.len(), 1);
    assert_eq!(serde_json::to_string(&node).expect("serialise"), LOCKED);
}

/// A mode from a newer release is kept as it came, so a round trip does not lose it, and reads as no
/// pin here.
#[test]
fn a_mode_from_a_newer_release_is_kept_and_reads_as_no_pin() {
    let text = r#"{"mode":"PACKAGE_MODE_SOMETHING_NEWER"}"#;
    let node: PublishedPackage = serde_json::from_str(text).expect("parses");
    assert_eq!(node.mode.known(), PackageMode::Unspecified);
    assert_eq!(serde_json::to_string(&node).expect("serialise"), text);
}

fn selector(text: &str) -> VariantSelector {
    serde_json::from_str(text).expect("selector parses")
}

/// The base variant states nothing and is written without a selector; each dimension counts once,
/// the device once whether by id or by class.
#[test]
fn a_selector_counts_the_dimensions_it_states() {
    let base: Variant<u8> = serde_json::from_str(r#"{"body":1}"#).expect("parses");
    assert!(base.selector.is_base());
    assert_eq!(
        serde_json::to_string(&base).expect("serialise"),
        r#"{"body":1}"#
    );

    assert_eq!(
        selector(r#"{"device_class":"DEVICE_CLASS_UNSPECIFIED"}"#).dimensions_stated(),
        0
    );
    assert_eq!(
        selector(r#"{"device_class":"DEVICE_CLASS_TABLET"}"#).dimensions_stated(),
        1
    );
    assert_eq!(
        selector(
            r#"{"device_id":"01J00000000000000000000D01","device_class":"DEVICE_CLASS_TABLET"}"#
        )
        .dimensions_stated(),
        1
    );
    let all = selector(
        r#"{"device_class":"DEVICE_CLASS_TERMINAL","role_id":"01J00000000000000000000R01","sales_channel":"SALES_CHANNEL_DINE_IN","time_slot_id":"lunch"}"#,
    );
    assert_eq!(all.dimensions_stated(), 4);
    assert_eq!(all.device_class, Open::from_known(DeviceClass::Terminal));
    assert_eq!(all.sales_channel, Open::from_known(SalesChannel::DineIn));
}

/// A device class from a newer release states the device dimension, so a variant for it is never
/// mistaken for a base variant.
#[test]
fn a_class_from_a_newer_release_still_states_the_device() {
    let newer = selector(r#"{"device_class":"DEVICE_CLASS_WRISTWATCH"}"#);
    assert!(newer.states_device());
    assert!(!newer.is_base());
}
