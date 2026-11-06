// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::ids::{CustomFieldId, ListEntryId, ObjectTypeId};
use pos_proto::package::{DataObject, PackageData};
use pos_proto::text::TranslationKey;

use super::{
    EnteredValue, Entry, EntryFault, EntryRefusal, EntryTarget, PersonalText, check_entry,
};

/// A reservation type with a required party size, a personal contact note and a deprecated flag,
/// and an order field choosing an occasion from a list with one current and one retired entry.
const DATA: &str = r#"{
  "custom_fields":[{"custom_field_id":"01J0000000000000000000CF01","object":"DATA_OBJECT_ORDER","label_key":"data.order.occasion","field_type":"CUSTOM_FIELD_TYPE_CHOICE","personal":false,"custom_list_id":"01J0000000000000000000CA01"}],
  "object_types":[{"object_type_id":"01J00000000000000000000B01","label_key":"data.reservation","fields":[
    {"custom_field_id":"01J0000000000000000000CF02","label_key":"data.reservation.party","field_type":"CUSTOM_FIELD_TYPE_INT","required":true,"personal":false},
    {"custom_field_id":"01J0000000000000000000CF03","label_key":"data.reservation.contact","field_type":"CUSTOM_FIELD_TYPE_TEXT","personal":true,"retention_days":30,"consent_purpose_key":"data.consent.reservation"},
    {"custom_field_id":"01J0000000000000000000CF04","label_key":"data.reservation.window","field_type":"CUSTOM_FIELD_TYPE_BOOL","personal":false,"deprecated":true}
  ]}],
  "lists":[{"custom_list_id":"01J0000000000000000000CA01","label_key":"data.list.occasion","entries":[
    {"list_entry_id":"01J0000000000000000000EE01","label_key":"data.occasion.birthday"},
    {"list_entry_id":"01J0000000000000000000EE02","label_key":"data.occasion.old","deprecated":true}
  ]}]
}"#;

fn data() -> PackageData {
    serde_json::from_str(DATA).expect("data parses")
}

fn id(text: &str) -> CustomFieldId {
    text.parse().expect("a field id")
}

fn reservation() -> ObjectTypeId {
    "01J00000000000000000000B01".parse().expect("a type id")
}

const OCCASION: &str = "01J0000000000000000000CF01";
const PARTY: &str = "01J0000000000000000000CF02";
const CONTACT: &str = "01J0000000000000000000CF03";
const WINDOW: &str = "01J0000000000000000000CF04";

fn entry(field: &str, value: EnteredValue) -> Entry {
    Entry {
        custom_field_id: id(field),
        value,
    }
}

fn consent() -> Vec<TranslationKey> {
    vec![TranslationKey::new("data.consent.reservation")]
}

fn creating(kind: &ObjectTypeId) -> EntryTarget<'_> {
    EntryTarget::Record {
        object_type_id: kind,
        creating: true,
    }
}

fn refused(field: &str, fault: EntryFault) -> EntryRefusal {
    EntryRefusal {
        custom_field_id: Some(id(field)),
        fault,
    }
}

#[test]
fn a_record_splits_into_its_event_values_and_its_personal_ones() {
    let kind = reservation();
    let checked = check_entry(
        &data(),
        creating(&kind),
        &[
            entry(PARTY, EnteredValue::Integer(4)),
            entry(CONTACT, EnteredValue::Text(PersonalText::new("demo note"))),
        ],
        &consent(),
    )
    .expect("fits");

    assert_eq!(checked.values.len(), 1, "only the party size travels");
    assert_eq!(checked.values.first().and_then(|v| v.integer), Some(4));
    let note = checked.personal.get(&id(CONTACT)).cloned().flatten();
    assert_eq!(note.as_ref().map(PersonalText::as_str), Some("demo note"));
    assert_eq!(checked.retention_days, Some(30));
    assert_eq!(checked.consent_purpose_keys, consent());
}

#[test]
fn a_personal_value_needs_its_consent_and_clearing_one_does_not() {
    let kind = reservation();
    let target = EntryTarget::Record {
        object_type_id: &kind,
        creating: false,
    };
    let note = entry(CONTACT, EnteredValue::Text(PersonalText::new("demo note")));
    assert_eq!(
        check_entry(&data(), target, &[note], &[]),
        Err(refused(CONTACT, EntryFault::ConsentMissing))
    );
    let cleared = check_entry(
        &data(),
        target,
        &[entry(CONTACT, EnteredValue::Cleared)],
        &[],
    )
    .expect("clearing needs no consent");
    assert_eq!(cleared.personal.get(&id(CONTACT)), Some(&None));
    assert!(cleared.consent_purpose_keys.is_empty());
}

#[test]
fn a_new_record_needs_its_required_fields() {
    let kind = reservation();
    assert_eq!(
        check_entry(&data(), creating(&kind), &[], &[]),
        Err(refused(PARTY, EntryFault::Required))
    );
    let target = EntryTarget::Record {
        object_type_id: &kind,
        creating: false,
    };
    assert_eq!(
        check_entry(&data(), target, &[], &[]).map(|c| c.values.len()),
        Ok(0),
        "a change lists only what changed"
    );
    assert_eq!(
        check_entry(&data(), target, &[entry(PARTY, EnteredValue::Cleared)], &[]),
        Err(refused(PARTY, EntryFault::Required))
    );
}

#[test]
fn each_fault_in_a_value_is_named() {
    let kind = reservation();
    let target = EntryTarget::Record {
        object_type_id: &kind,
        creating: false,
    };
    let long = PersonalText::new("x".repeat(1_025));
    let cases = [
        (
            entry(PARTY, EnteredValue::Boolean(true)),
            refused(PARTY, EntryFault::WrongType),
        ),
        (
            entry(WINDOW, EnteredValue::Boolean(true)),
            refused(WINDOW, EntryFault::Deprecated),
        ),
        (
            entry(OCCASION, EnteredValue::Integer(1)),
            refused(OCCASION, EntryFault::UnknownField),
        ),
        (
            entry(CONTACT, EnteredValue::Text(long)),
            refused(CONTACT, EntryFault::TextTooLong),
        ),
    ];
    for (value, refusal) in cases {
        assert_eq!(
            check_entry(&data(), target, &[value], &consent()),
            Err(refusal)
        );
    }
    let twice = [
        entry(PARTY, EnteredValue::Integer(2)),
        entry(PARTY, EnteredValue::Integer(3)),
    ];
    assert_eq!(
        check_entry(&data(), target, &twice, &[]),
        Err(refused(PARTY, EntryFault::Repeated))
    );
}

#[test]
fn a_choice_takes_only_a_current_entry_of_its_list() {
    let target = EntryTarget::Object(DataObject::Order);
    let pick = |entry_id: &str| {
        let list_entry_id: ListEntryId = entry_id.parse().expect("an entry id");
        check_entry(
            &data(),
            target,
            &[entry(OCCASION, EnteredValue::ListEntry(list_entry_id))],
            &[],
        )
    };
    let checked = pick("01J0000000000000000000EE01").expect("a current entry");
    assert_eq!(
        checked.retention_days, None,
        "an order here declares nothing personal"
    );
    assert!(
        checked
            .values
            .first()
            .is_some_and(|v| v.list_entry_id.is_some())
    );
    assert_eq!(
        pick("01J0000000000000000000EE02"),
        Err(refused(OCCASION, EntryFault::UnknownListEntry))
    );
}

#[test]
fn an_unknown_or_retired_type_takes_no_new_record() {
    let stranger: ObjectTypeId = "01J00000000000000000000B09".parse().expect("a type id");
    let unknown = EntryRefusal {
        custom_field_id: None,
        fault: EntryFault::UnknownObjectType,
    };
    assert_eq!(
        check_entry(&data(), creating(&stranger), &[], &[]),
        Err(unknown)
    );
    let mut retired = data();
    for kind in &mut retired.object_types {
        kind.deprecated = true;
    }
    let kind = reservation();
    assert_eq!(
        check_entry(&retired, creating(&kind), &[], &[]),
        Err(unknown)
    );
}

#[test]
fn personal_text_never_shows_in_debug() {
    let text = PersonalText::new("demo note");
    assert_eq!(format!("{text:?}"), "PersonalText(..)");
    let value = EnteredValue::Text(text);
    assert!(!format!("{value:?}").contains("demo"));
}
