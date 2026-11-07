// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::sync::Arc;

use pos_core::package::{EnteredValue, Entry, PersonalText};
use pos_core::permission::{Permission, PermissionSet};
use pos_fakes::FakeStore;
use pos_proto::SalesChannel;
use pos_proto::ids::{CustomFieldId, CustomRecordId, ObjectTypeId};
use pos_proto::package::{DataObject, PackageContent};
use pos_proto::text::TranslationKey;

use super::{PersonalOnPaper, PrintedField, PrintedObjects, PrintedValue};
use crate::app::tests::{a_counter_order, actor, identity};
use crate::app::{Edge, EdgeSession};
use crate::receipt::InMemoryReceipts;

const COVERS: &str = "01J0000000000000000000CF01";
const PARTY: &str = "01J0000000000000000000CF02";
const CONTACT: &str = "01J0000000000000000000CF03";
const SEATING: &str = "01J0000000000000000000CF04";
const UNDECLARED: &str = "01J0000000000000000000CF09";

/// A whole-number field on an order, and a reservation type with a party size, a choice of seating
/// and a personal note. Every value below is a demo value.
fn package() -> PackageContent {
    serde_json::from_value(serde_json::json!({
        "package_id": "01J00000000000000000000P01",
        "package_version_id": "01J00000000000000000000V01",
        "version_label": "Reservations",
        "min_edge_version": "0.0.0",
        "data": {
            "custom_fields": [{"custom_field_id": COVERS, "object": "DATA_OBJECT_ORDER",
                "label_key": "data.order.covers", "field_type": "CUSTOM_FIELD_TYPE_INT", "personal": false}],
            "object_types": [{"object_type_id": "01J00000000000000000000B01", "label_key": "data.reservation",
                "fields": [
                    {"custom_field_id": PARTY, "label_key": "data.reservation.party",
                     "field_type": "CUSTOM_FIELD_TYPE_INT", "required": true, "personal": false},
                    {"custom_field_id": SEATING, "label_key": "data.reservation.seating",
                     "field_type": "CUSTOM_FIELD_TYPE_CHOICE", "personal": false,
                     "custom_list_id": "01J0000000000000000000CA01"},
                    {"custom_field_id": CONTACT, "label_key": "data.reservation.contact",
                     "field_type": "CUSTOM_FIELD_TYPE_TEXT", "personal": true, "retention_days": 30,
                     "consent_purpose_key": "data.consent.reservation"}
                ]}],
            "lists": [{"custom_list_id": "01J0000000000000000000CA01", "label_key": "data.list.seating",
                "entries": [{"list_entry_id": "01J0000000000000000000EE01", "label_key": "data.seating.window"}]}]
        }
    }))
    .expect("the package parses")
}

fn edge() -> Edge<FakeStore> {
    let mut session = EdgeSession::bootstrap();
    session.package = Some(Arc::new(package()));
    Edge::new(
        FakeStore::default(),
        identity(),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
}

fn field(id: &str) -> CustomFieldId {
    id.parse().expect("a field id")
}

/// A reservation of four by the window, with a demo note.
async fn a_reservation(edge: &Edge<FakeStore>) -> CustomRecordId {
    let reservation: ObjectTypeId = "01J00000000000000000000B01".parse().expect("a type id");
    let entries = [
        Entry {
            custom_field_id: field(PARTY),
            value: EnteredValue::Integer(4),
        },
        Entry {
            custom_field_id: field(SEATING),
            value: EnteredValue::ListEntry("01J0000000000000000000EE01".parse().expect("an id")),
        },
        Entry {
            custom_field_id: field(CONTACT),
            value: EnteredValue::Text(PersonalText::new("demo note")),
        },
    ];
    edge.save_custom_record(
        actor(),
        reservation,
        None,
        &entries,
        &[TranslationKey::new("data.consent.reservation")],
    )
    .await
    .expect("saves")
    .custom_record_id
    .expect("a record")
}

fn ids(fields: &[&str]) -> Vec<CustomFieldId> {
    fields.iter().map(|id| field(id)).collect()
}

fn printed(label: &str, value: PrintedValue) -> PrintedField {
    PrintedField {
        label_key: TranslationKey::new(label),
        value,
    }
}

#[tokio::test]
async fn a_slip_prints_its_fields_in_the_templates_order_and_skips_what_it_cannot() {
    let edge = edge();
    let record = a_reservation(&edge).await;
    let fields = edge
        .printed_fields(
            &ids(&[SEATING, UNDECLARED, COVERS, PARTY]),
            PrintedObjects::Record(record),
            PersonalOnPaper::Withheld,
        )
        .await;
    assert_eq!(
        fields,
        vec![
            printed(
                "data.reservation.seating",
                PrintedValue::Entry(TranslationKey::new("data.seating.window"))
            ),
            printed("data.reservation.party", PrintedValue::Integer(4)),
        ],
        "an undeclared field and one on another object are skipped, never an error"
    );
    assert_eq!(
        edge.printed_channel(PrintedObjects::Record(record)),
        SalesChannel::Unspecified,
        "a record has no channel and takes the base template"
    );
}

#[tokio::test]
async fn a_personal_value_prints_only_on_the_persons_copy() {
    let edge = edge();
    let record = a_reservation(&edge).await;
    let only_contact = ids(&[CONTACT]);
    let contact =
        |personal| edge.printed_fields(&only_contact, PrintedObjects::Record(record), personal);

    assert_eq!(
        contact(PersonalOnPaper::Withheld).await,
        vec![printed("data.reservation.contact", PrintedValue::Withheld)],
        "another document prints the label and no value"
    );
    assert_eq!(
        contact(PersonalOnPaper::Shown).await,
        vec![printed(
            "data.reservation.contact",
            PrintedValue::Personal("demo note".to_owned())
        )]
    );
    assert_eq!(
        contact(PersonalOnPaper::Reprinted(actor())).await,
        vec![printed(
            "data.reservation.contact",
            PrintedValue::Personal("demo note".to_owned())
        )],
        "a reprint by someone holding data.personal.read"
    );

    let mut session = (*edge.session()).clone();
    session.granted = PermissionSet::EMPTY.with(Permission::WriteDataRecord);
    edge.apply_session(session);
    assert_eq!(
        contact(PersonalOnPaper::Reprinted(actor())).await,
        vec![printed("data.reservation.contact", PrintedValue::Withheld)],
        "a reprint by someone without data.personal.read withholds it"
    );
}

#[tokio::test]
async fn a_receipt_prints_its_orders_fields() {
    let edge = edge();
    let order_id = a_counter_order(&edge).await;
    edge.set_custom_fields(
        actor(),
        DataObject::Order,
        order_id.as_ulid(),
        &[Entry {
            custom_field_id: field(COVERS),
            value: EnteredValue::Integer(3),
        }],
        &[],
    )
    .await
    .expect("sets");
    assert_eq!(
        edge.printed_fields(
            &ids(&[COVERS]),
            PrintedObjects::Order(order_id),
            PersonalOnPaper::Withheld
        )
        .await,
        vec![printed("data.order.covers", PrintedValue::Integer(3))]
    );
    assert_ne!(
        edge.printed_channel(PrintedObjects::Order(order_id)),
        SalesChannel::Unspecified,
        "the order's own channel picks its template"
    );
}
