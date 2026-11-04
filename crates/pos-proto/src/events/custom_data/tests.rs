// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use super::CustomFieldValue;
use crate::envelope::RawPayload;
use crate::events::{DataCustomFieldSet, DataCustomRecordChanged, EventType};
use crate::ids::{
    CustomFieldId, CustomRecordId, ListEntryId, ObjectTypeId, PackageVersionId, SubjectId,
};
use crate::package::DataObject;
use crate::pii::check_field_name;
use crate::quantity::Quantity;
use crate::time::CalendarDate;
use crate::ulid::Ulid;
use crate::wire_enum::Open;

fn value(field: u128) -> CustomFieldValue {
    CustomFieldValue {
        custom_field_id: CustomFieldId::new(Ulid::from_u128(field)),
        integer: None,
        boolean: None,
        list_entry_id: None,
        calendar_date: None,
        quantity: None,
    }
}

/// A choice on an order: its bytes are a contract, and a value with nothing set is a cleared one.
#[test]
fn a_field_set_on_an_order_round_trips_byte_for_byte() {
    let set = DataCustomFieldSet {
        object: Open::from(DataObject::Order),
        object_id: Ulid::from_u128(1),
        values: vec![
            CustomFieldValue {
                list_entry_id: Some(ListEntryId::new(Ulid::from_u128(5))),
                ..value(2)
            },
            value(3),
        ],
        subject_id: None,
        package_version_id: PackageVersionId::new(Ulid::from_u128(9)),
    };
    let written = RawPayload::encode(&set).expect("encodes");
    assert_eq!(
        written.as_json(),
        r#"{"object":"DATA_OBJECT_ORDER","object_id":"00000000000000000000000001","values":[{"custom_field_id":"00000000000000000000000002","list_entry_id":"00000000000000000000000005"},{"custom_field_id":"00000000000000000000000003"}],"package_version_id":"00000000000000000000000009"}"#
    );
    assert_eq!(
        written.decode::<DataCustomFieldSet>().expect("decodes"),
        set
    );
}

/// A reservation with a party size, a date and a quantity; its personal values are reached only
/// through the subject id (ADR-0194 decision 4).
#[test]
fn a_record_carries_typed_values_and_reaches_its_person_by_id_only() {
    let changed = DataCustomRecordChanged {
        custom_record_id: CustomRecordId::new(Ulid::from_u128(1)),
        object_type_id: ObjectTypeId::new(Ulid::from_u128(2)),
        values: vec![
            CustomFieldValue {
                integer: Some(4),
                ..value(3)
            },
            CustomFieldValue {
                boolean: Some(true),
                ..value(4)
            },
            CustomFieldValue {
                calendar_date: Some(CalendarDate::from_ymd(2026, 11, 20).expect("a date")),
                ..value(5)
            },
            CustomFieldValue {
                quantity: Some(Quantity { milli: 1_500 }),
                ..value(6)
            },
        ],
        subject_id: Some(SubjectId::new(Ulid::from_u128(7))),
        package_version_id: PackageVersionId::new(Ulid::from_u128(9)),
    };
    let written = RawPayload::encode(&changed).expect("encodes");
    let json = written.as_json();
    assert!(json.contains(r#""integer":4"#), "{json}");
    assert!(json.contains(r#""calendar_date":"2026-11-20""#), "{json}");
    assert!(
        json.contains(r#""subject_id":"00000000000000000000000007""#),
        "{json}"
    );
    assert_eq!(
        written
            .decode::<DataCustomRecordChanged>()
            .expect("decodes"),
        changed
    );
}

#[test]
fn the_events_are_in_the_catalogue_and_their_names_carry_nothing_personal() {
    assert_eq!(
        EventType::parse("data.custom_field.set"),
        Some(EventType::DataCustomFieldSet)
    );
    assert_eq!(
        EventType::parse("data.custom_record.changed"),
        Some(EventType::DataCustomRecordChanged)
    );
    for name in [
        "integer",
        "boolean",
        "list_entry_id",
        "calendar_date",
        "quantity",
        "custom_field_id",
        "custom_record_id",
        "object_type_id",
        "object_id",
    ] {
        assert_eq!(check_field_name(name), Ok(()), "{name}");
    }
}
