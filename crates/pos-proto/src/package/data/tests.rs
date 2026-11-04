// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use super::{CustomFieldType, DataObject, PackageData};
use crate::package::PackageContent;

/// A reservation with a non-personal party size and a personal contact, a party-size field on an
/// order, and the list the order's field draws from. Fake labels only.
const DATA: &str = r#"{"custom_fields":[{"custom_field_id":"01J0000000000000000000CF01","object":"DATA_OBJECT_ORDER","label_key":"data.order.occasion","field_type":"CUSTOM_FIELD_TYPE_CHOICE","personal":false,"custom_list_id":"01J0000000000000000000CA01"}],"object_types":[{"object_type_id":"01J00000000000000000000B01","label_key":"data.reservation","fields":[{"custom_field_id":"01J0000000000000000000CF02","label_key":"data.reservation.party","field_type":"CUSTOM_FIELD_TYPE_INT","required":true,"personal":false},{"custom_field_id":"01J0000000000000000000CF03","label_key":"data.reservation.contact","field_type":"CUSTOM_FIELD_TYPE_TEXT","personal":true,"retention_days":30,"consent_purpose_key":"data.consent.reservation"}]}],"lists":[{"custom_list_id":"01J0000000000000000000CA01","label_key":"data.list.occasion","entries":[{"list_entry_id":"01J0000000000000000000EE01","label_key":"data.occasion.birthday"},{"list_entry_id":"01J0000000000000000000EE02","label_key":"data.occasion.old","deprecated":true}]}]}"#;

const CONTENT: &str = r#"{"package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01","version_label":"March","min_edge_version":"0.15.0"}"#;

/// A data part's bytes are a contract with every edge that holds one: it round-trips byte for byte.
#[test]
fn a_data_part_round_trips_byte_for_byte() {
    let data: PackageData = serde_json::from_str(DATA).expect("parses");
    let occasion = data.custom_fields.first().expect("a field on an order");
    assert_eq!(occasion.object.known(), DataObject::Order);
    let reservation = data.object_types.first().expect("a reservation");
    let contact = reservation.fields.get(1).expect("its contact");
    assert_eq!(contact.field_type.known(), CustomFieldType::Text);
    assert_eq!(contact.personal, Some(true));
    assert_eq!(contact.retention_days, Some(30));
    let list = data.lists.first().expect("a list");
    assert!(list.entries.get(1).expect("a second entry").deprecated);
    assert_eq!(serde_json::to_string(&data).expect("serialise"), DATA);
}

/// A version that declares no data is written as it was before the part existed, so its bytes and
/// every edge that holds it stay the same.
#[test]
fn a_version_without_data_keeps_its_bytes() {
    let content: PackageContent = serde_json::from_str(CONTENT).expect("parses");
    assert!(content.data.is_empty());
    assert_eq!(serde_json::to_string(&content).expect("serialise"), CONTENT);
}

/// Whether a field is personal is left absent when the author did not say: nothing defaults it.
#[test]
fn personal_is_absent_until_the_author_says() {
    let field = r#"{"custom_field_id":"01J0000000000000000000CF02","label_key":"data.party","field_type":"CUSTOM_FIELD_TYPE_INT"}"#;
    let parsed: super::CustomField = serde_json::from_str(field).expect("parses");
    assert_eq!(parsed.personal, None);
    assert_eq!(serde_json::to_string(&parsed).expect("serialise"), field);
}

/// A field type or object from a newer release is kept as it came, and reads as unspecified here.
#[test]
fn a_type_from_a_newer_release_is_kept() {
    let field = r#"{"custom_field_id":"01J0000000000000000000CF02","object":"DATA_OBJECT_SOMETHING_NEWER","label_key":"data.party","field_type":"CUSTOM_FIELD_TYPE_SOMETHING_NEWER","personal":false}"#;
    let parsed: super::CustomField = serde_json::from_str(field).expect("parses");
    assert_eq!(parsed.field_type.known(), CustomFieldType::Unspecified);
    assert_eq!(parsed.object.known(), DataObject::Unspecified);
    assert_eq!(serde_json::to_string(&parsed).expect("serialise"), field);
}
