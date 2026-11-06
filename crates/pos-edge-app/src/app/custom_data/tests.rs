// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::package::{EnteredValue, Entry, EntryFault, PersonalText};
use pos_core::permission::PermissionSet;
use pos_fakes::FakeStore;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::subject_store::SubjectStore;
use pos_proto::events::EventType;
use pos_proto::ids::{CustomFieldId, ObjectTypeId};
use pos_proto::package::{DataObject, PackageContent};
use pos_proto::text::TranslationKey;

use super::CONSENT_FIELD_PREFIX;
use crate::app::tests::{a_counter_order, actor, identity};
use crate::app::{AppError, Edge, EdgeSession, MILLISECONDS_PER_DAY};
use crate::receipt::InMemoryReceipts;

const PARTY: &str = "01J0000000000000000000CF02";
const CONTACT: &str = "01J0000000000000000000CF03";
const OCCASION: &str = "01J0000000000000000000CF01";

/// A package with a reservation type (a required party size and a personal note kept 30 days),
/// and a whole-number field on an order.
fn package() -> PackageContent {
    serde_json::from_value(serde_json::json!({
        "package_id": "01J00000000000000000000P01",
        "package_version_id": "01J00000000000000000000V01",
        "version_label": "Reservations",
        "min_edge_version": "0.0.0",
        "data": {
            "custom_fields": [{"custom_field_id": OCCASION, "object": "DATA_OBJECT_ORDER",
                "label_key": "data.order.covers", "field_type": "CUSTOM_FIELD_TYPE_INT", "personal": false}],
            "object_types": [{"object_type_id": "01J00000000000000000000B01", "label_key": "data.reservation",
                "fields": [
                    {"custom_field_id": PARTY, "label_key": "data.reservation.party",
                     "field_type": "CUSTOM_FIELD_TYPE_INT", "required": true, "personal": false},
                    {"custom_field_id": CONTACT, "label_key": "data.reservation.contact",
                     "field_type": "CUSTOM_FIELD_TYPE_TEXT", "personal": true, "retention_days": 30,
                     "consent_purpose_key": "data.consent.reservation"}
                ]}]
        }
    }))
    .expect("the package parses")
}

fn edge_over(store: FakeStore) -> Edge<FakeStore> {
    let mut session = EdgeSession::bootstrap();
    session.package = Some(Arc::new(package()));
    Edge::new(
        store,
        identity(),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds")
}

fn field(id: &str) -> CustomFieldId {
    id.parse().expect("a field id")
}

fn reservation() -> ObjectTypeId {
    "01J00000000000000000000B01".parse().expect("a type id")
}

fn consent() -> Vec<TranslationKey> {
    vec![TranslationKey::new("data.consent.reservation")]
}

fn entries(party: i64, note: Option<&str>) -> Vec<Entry> {
    let mut entries = vec![Entry {
        custom_field_id: field(PARTY),
        value: EnteredValue::Integer(party),
    }];
    if let Some(note) = note {
        entries.push(Entry {
            custom_field_id: field(CONTACT),
            value: EnteredValue::Text(PersonalText::new(note)),
        });
    }
    entries
}

/// Every event the store holds, as stored JSON.
async fn logged(store: &FakeStore) -> Vec<(EventType, String)> {
    let limit = NonZeroU32::new(100).expect("non-zero");
    store
        .read(&EventQuery::first(identity().store_id, limit))
        .await
        .expect("reads")
        .iter()
        .filter_map(|envelope| {
            let known = envelope.event_type.known()?;
            Some((
                known,
                serde_json::to_string(&envelope.data).expect("encodes"),
            ))
        })
        .collect()
}

#[tokio::test]
async fn a_record_puts_its_personal_value_in_the_subject_store_and_never_in_the_event() {
    let edge = edge_over(FakeStore::default());
    let saved = edge
        .save_custom_record(
            actor(),
            reservation(),
            None,
            &entries(4, Some("demo note")),
            &consent(),
        )
        .await
        .expect("saves");

    assert_eq!(saved.values.len(), 1, "only the party size is held");
    let subject_id = saved.subject_id.expect("a subject holds the note");
    let events = logged(&edge.store).await;
    assert_eq!(events.len(), 1);
    let (kind, json) = events.first().expect("one event");
    assert_eq!(*kind, EventType::DataCustomRecordChanged);
    assert!(
        !json.contains("demo note"),
        "the note never reaches an event"
    );
    assert!(json.contains(&subject_id.to_string()));

    let record = edge
        .store
        .fetch(identity().store_id, subject_id)
        .await
        .expect("reads")
        .expect("the subject is recorded");
    assert_eq!(
        record.fields.get(CONTACT).map(String::as_str),
        Some("demo note")
    );
    let consent_field = format!("{CONSENT_FIELD_PREFIX}data.consent.reservation");
    assert!(
        record.fields.contains_key(&consent_field),
        "the consent is recorded with it"
    );
    let kept = record.retain_until.map(|until| {
        until.as_milliseconds_since_epoch() - record.collected_at.as_milliseconds_since_epoch()
    });
    assert_eq!(
        kept,
        Some(30 * MILLISECONDS_PER_DAY),
        "kept for the field's own 30 days"
    );
}

#[tokio::test]
async fn a_personal_value_without_its_consent_is_refused_and_nothing_is_written() {
    let edge = edge_over(FakeStore::default());
    let refused = edge
        .save_custom_record(
            actor(),
            reservation(),
            None,
            &entries(2, Some("demo note")),
            &[],
        )
        .await;

    assert!(matches!(
        refused,
        Err(AppError::CustomEntryRefused(refusal)) if refusal.fault == EntryFault::ConsentMissing
    ));
    assert!(logged(&edge.store).await.is_empty());
}

#[tokio::test]
async fn an_edit_keeps_the_subject_and_its_clock_and_a_clear_removes_the_value() {
    let edge = edge_over(FakeStore::default());
    let first = edge
        .save_custom_record(
            actor(),
            reservation(),
            None,
            &entries(4, Some("demo note")),
            &consent(),
        )
        .await
        .expect("saves");
    let subject_id = first.subject_id.expect("a subject");
    let before = edge
        .store
        .fetch(identity().store_id, subject_id)
        .await
        .expect("reads");

    let cleared = [Entry {
        custom_field_id: field(CONTACT),
        value: EnteredValue::Cleared,
    }];
    let changed = edge
        .save_custom_record(
            actor(),
            reservation(),
            first.custom_record_id,
            &cleared,
            &[],
        )
        .await
        .expect("changes");

    assert_eq!(changed.subject_id, Some(subject_id), "the same subject");
    assert_eq!(changed.values, first.values, "the party size is unchanged");
    let after = edge
        .store
        .fetch(identity().store_id, subject_id)
        .await
        .expect("reads")
        .expect("still recorded");
    assert!(
        !after.fields.contains_key(CONTACT),
        "the cleared value is gone"
    );
    let before = before.expect("recorded");
    assert_eq!(after.collected_at, before.collected_at);
    assert_eq!(
        after.retain_until, before.retain_until,
        "an edit keeps the clock"
    );
}

#[tokio::test]
async fn a_new_record_needs_its_required_field_and_an_unknown_record_is_refused() {
    let edge = edge_over(FakeStore::default());
    let missing = edge
        .save_custom_record(actor(), reservation(), None, &[], &[])
        .await;
    assert!(matches!(
        missing,
        Err(AppError::CustomEntryRefused(refusal)) if refusal.fault == EntryFault::Required
    ));
    let stranger = Some(pos_proto::ids::CustomRecordId::new(
        pos_proto::ulid::Ulid::from_u128(77),
    ));
    let unknown = edge
        .save_custom_record(actor(), reservation(), stranger, &entries(2, None), &[])
        .await;
    assert!(matches!(unknown, Err(AppError::UnknownCustomRecord)));
}

#[tokio::test]
async fn a_field_on_an_order_needs_the_order_and_the_permissions() {
    let edge = edge_over(FakeStore::default());
    let covers = [Entry {
        custom_field_id: field(OCCASION),
        value: EnteredValue::Integer(3),
    }];
    let stranger = pos_proto::ulid::Ulid::from_u128(4242);
    let unknown = edge
        .set_custom_fields(actor(), DataObject::Order, stranger, &covers, &[])
        .await;
    assert!(matches!(unknown, Err(AppError::UnknownObject)));

    let order_id = a_counter_order(&edge).await;
    let set = edge
        .set_custom_fields(actor(), DataObject::Order, order_id.as_ulid(), &covers, &[])
        .await
        .expect("sets");
    assert_eq!(set.values.first().and_then(|v| v.integer), Some(3));
    assert_eq!(set.subject_id, None, "nothing personal, no subject");

    let mut session = (*edge.session()).clone();
    session.granted = PermissionSet::EMPTY;
    edge.apply_session(session);
    let denied = edge
        .set_custom_fields(actor(), DataObject::Order, order_id.as_ulid(), &covers, &[])
        .await;
    assert!(
        matches!(denied, Err(AppError::Domain(_))),
        "data.record.write is needed"
    );
}

#[tokio::test]
async fn a_restarted_edge_holds_the_same_records() {
    let store = FakeStore::default();
    let edge = edge_over(store.clone());
    let saved = edge
        .save_custom_record(
            actor(),
            reservation(),
            None,
            &entries(6, Some("demo note")),
            &consent(),
        )
        .await
        .expect("saves");

    let restarted = edge_over(store);
    restarted.rebuild().await.expect("rebuilds");
    let id = saved.custom_record_id.expect("a record id");
    assert_eq!(restarted.custom_record(id), Some(saved.clone()));
    assert_eq!(restarted.custom_records(reservation(), 10), vec![saved]);
}
