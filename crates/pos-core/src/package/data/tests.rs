// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::package::{CustomField, PackageData};

use super::{DataRefusal, FieldAt, FieldFault, check_data};
use crate::package::{ContentRefusal, check_content};

const LIST: &str = r#"{"custom_list_id":"01J0000000000000000000CA01","label_key":"data.list.occasion","entries":[{"list_entry_id":"01J0000000000000000000EE01","label_key":"data.occasion.birthday"}]}"#;

/// A field on an order: a choice from the list.
const ON_ORDER: &str = r#"{"custom_field_id":"01J0000000000000000000CF01","object":"DATA_OBJECT_ORDER","label_key":"data.order.occasion","field_type":"CUSTOM_FIELD_TYPE_CHOICE","personal":false,"custom_list_id":"01J0000000000000000000CA01"}"#;

/// A field of a reservation: a party size.
const PARTY: &str = r#"{"custom_field_id":"01J0000000000000000000CF02","label_key":"data.reservation.party","field_type":"CUSTOM_FIELD_TYPE_INT","required":true,"personal":false}"#;

/// A personal field, declared in full.
const CONTACT: &str = r#"{"custom_field_id":"01J0000000000000000000CF03","label_key":"data.reservation.contact","field_type":"CUSTOM_FIELD_TYPE_TEXT","personal":true,"retention_days":30,"consent_purpose_key":"data.consent.reservation"}"#;

fn field(text: &str) -> CustomField {
    serde_json::from_str(text).expect("field parses")
}

/// A data part with one list, one field on an order and a reservation with `fields`.
fn data(fields: &[&str]) -> PackageData {
    serde_json::from_str(&format!(
        r#"{{"custom_fields":[{ON_ORDER}],"object_types":[{{"object_type_id":"01J00000000000000000000B01","label_key":"data.reservation","fields":[{}]}}],"lists":[{LIST}]}}"#,
        fields.join(",")
    ))
    .expect("data parses")
}

/// One change made to a field.
type Change = fn(&mut CustomField);

fn type_field(field: usize, fault: FieldFault) -> Result<(), DataRefusal> {
    Err(DataRefusal::Field(
        FieldAt::Type {
            object_type: 0,
            field,
        },
        fault,
    ))
}

#[test]
fn a_well_formed_part_without_personal_fields_passes() {
    assert_eq!(check_data(&data(&[PARTY])), Ok(()));
    assert_eq!(check_data(&PackageData::default()), Ok(()));
}

/// Every personal field is refused until personal fields are available (ADR-0206 decision 11), with
/// the code the console shows.
#[test]
fn a_personal_field_is_unavailable_and_says_so() {
    let refused = check_data(&data(&[PARTY, CONTACT]));
    assert_eq!(refused, type_field(1, FieldFault::PersonalUnavailable));
    let refusal = refused.expect_err("refused");
    assert_eq!(refusal.code(), "PERSONAL_FIELD_UNAVAILABLE");
    assert_eq!(refusal.field(), "data.object_types.fields.personal");
    let content = ContentRefusal::Data(refusal);
    assert_eq!(content.code(), "PERSONAL_FIELD_UNAVAILABLE");
    assert_eq!(ContentRefusal::VersionLabel.code(), "INVALID");
}

/// One change to the party-size field, and the fault it earns.
#[test]
fn each_fault_in_a_field_is_named() {
    let party = field(PARTY);
    let faults: [(Change, FieldFault); 11] = [
        (|f| f.personal = None, FieldFault::PersonalUndeclared),
        (
            |f| f.field_type = field(CONTACT).field_type,
            FieldFault::TextNotPersonal,
        ),
        (|f| f.retention_days = Some(30), FieldFault::Retention),
        (
            |f| f.consent_purpose_key = field(CONTACT).consent_purpose_key,
            FieldFault::Consent,
        ),
        (|f| f.object = field(ON_ORDER).object, FieldFault::Object),
        (
            |f| f.label_key = pos_proto::text::TranslationKey::new("Party Size"),
            FieldFault::LabelKey,
        ),
        (
            |f| f.field_type = pos_proto::wire_enum::Open::default(),
            FieldFault::FieldType,
        ),
        (
            |f| f.custom_list_id = field(ON_ORDER).custom_list_id,
            FieldFault::List,
        ),
        (
            |f| f.unit_key = Some(pos_proto::text::TranslationKey::new("data.unit")),
            FieldFault::Unit,
        ),
        (
            |f| f.custom_field_id = field(ON_ORDER).custom_field_id,
            FieldFault::DuplicateId,
        ),
        (
            |f| {
                f.personal = Some(true);
                f.retention_days = Some(0);
                f.consent_purpose_key = field(CONTACT).consent_purpose_key;
            },
            FieldFault::Retention,
        ),
    ];
    for (change, fault) in faults {
        let mut changed = party.clone();
        change(&mut changed);
        let text = serde_json::to_string(&changed).expect("serialise");
        assert_eq!(
            check_data(&data(&[&text])),
            type_field(0, fault),
            "{fault:?}"
        );
    }
}

/// A choice naming a list nobody declared, and a quantity with no unit.
#[test]
fn a_choice_needs_a_declared_list_and_a_quantity_a_unit() {
    let mut choice = field(ON_ORDER);
    choice.object = pos_proto::wire_enum::Open::default();
    choice.custom_field_id = field(PARTY).custom_field_id;
    choice.custom_list_id = Some(pos_proto::ids::CustomListId::new(
        pos_proto::ulid::Ulid::from_parts(1, 9),
    ));
    let text = serde_json::to_string(&choice).expect("serialise");
    assert_eq!(check_data(&data(&[&text])), type_field(0, FieldFault::List));
    let quantity = PARTY.replace("CUSTOM_FIELD_TYPE_INT", "CUSTOM_FIELD_TYPE_QUANTITY");
    assert_eq!(
        check_data(&data(&[&quantity])),
        type_field(0, FieldFault::Unit)
    );
}

/// No personal field on a shift (ADR-0206 decision 8), and one object's personal fields share one
/// retention period (decision 2).
#[test]
fn personal_fields_stay_off_staff_and_share_a_period() {
    let on_shift = CONTACT.replace(
        r#""label_key""#,
        r#""object":"DATA_OBJECT_SHIFT","label_key""#,
    );
    let mut part = data(&[]);
    part.custom_fields = vec![field(&on_shift)];
    assert_eq!(
        check_data(&part),
        Err(DataRefusal::Field(
            FieldAt::Object(0),
            FieldFault::PersonalOnStaff
        ))
    );
    let longer = CONTACT
        .replace("CF03", "CF04")
        .replace(r#""retention_days":30"#, r#""retention_days":60"#);
    assert_eq!(
        check_data(&data(&[CONTACT, &longer])),
        type_field(1, FieldFault::RetentionDisagrees)
    );
}

#[test]
fn lists_and_types_are_checked() {
    let mut part = data(&[PARTY]);
    let mut empty: pos_proto::package::CustomList = serde_json::from_str(LIST).expect("list");
    empty.entries.clear();
    part.lists.push(empty);
    assert_eq!(check_data(&part), Err(DataRefusal::List(1)));

    let mut part = data(&[PARTY]);
    let again = part.object_types.first().cloned().expect("a type");
    part.object_types.push(again);
    assert_eq!(check_data(&part), Err(DataRefusal::ObjectType(1)));
}

/// The content check runs the data part's, so the cloud and the edge both refuse it.
#[test]
fn the_content_check_runs_the_data_check() {
    let mut content: pos_proto::package::PackageContent = serde_json::from_str(
        r#"{"package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01","version_label":"March","min_edge_version":"0.14.1"}"#,
    )
    .expect("content parses");
    content.data = data(&[PARTY, CONTACT]);
    assert_eq!(
        check_content(&content),
        Err(ContentRefusal::Data(DataRefusal::Field(
            FieldAt::Type {
                object_type: 0,
                field: 1
            },
            FieldFault::PersonalUnavailable
        )))
    );
}
