// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::collections::BTreeMap;
use std::sync::Mutex;

use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_ports::tx::{Transactional, TxContext};
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::{
    CustomFieldValue, DataCustomFieldSet, DataCustomRecordChanged, DataSubjectMasked, EventType,
};
use pos_proto::ids::{
    CustomFieldId, CustomRecordId, EventId, ListEntryId, ObjectTypeId, PackageVersionId, StoreId,
    SubjectId, TenantId,
};
use pos_proto::package::DataObject;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use super::{
    CustomObject, CustomPass, CustomValueChange, CustomValueRow, CustomValueStore,
    fold_custom_data, project_custom_values,
};

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn tenant() -> TenantId {
    TenantId::new(Ulid::from_u128(2))
}

fn field(id: u128) -> CustomFieldValue {
    CustomFieldValue {
        custom_field_id: CustomFieldId::new(Ulid::from_u128(id)),
        integer: None,
        boolean: None,
        list_entry_id: None,
        calendar_date: None,
        quantity: None,
    }
}

/// The `seed`th event of the store's log, carrying `payload` as `event_type`.
fn event<P: serde::Serialize>(
    seed: u32,
    event_type: EventType,
    payload: &P,
) -> EventEnvelope<RawPayload> {
    let mut event = fixtures::activations(store(), seed, 1).remove(0);
    event.event_type = EventTypeRef::from_known(event_type);
    event.data = RawPayload::encode(payload).expect("encode");
    event
}

/// An order's occasion chosen, and a second field cleared.
fn occasion_set(seed: u32) -> EventEnvelope<RawPayload> {
    event(
        seed,
        EventType::DataCustomFieldSet,
        &DataCustomFieldSet {
            object: Open::from(DataObject::Order),
            object_id: Ulid::from_u128(10),
            values: vec![
                CustomFieldValue {
                    list_entry_id: Some(ListEntryId::new(Ulid::from_u128(5))),
                    ..field(3)
                },
                field(4),
            ],
            subject_id: None,
            package_version_id: PackageVersionId::new(Ulid::from_u128(9)),
        },
    )
}

/// A reservation for `party` guests, with a personal record.
fn reservation(seed: u32, party: i64) -> EventEnvelope<RawPayload> {
    event(
        seed,
        EventType::DataCustomRecordChanged,
        &DataCustomRecordChanged {
            custom_record_id: CustomRecordId::new(Ulid::from_u128(20)),
            object_type_id: ObjectTypeId::new(Ulid::from_u128(21)),
            values: vec![CustomFieldValue {
                integer: Some(party),
                ..field(6)
            }],
            subject_id: Some(SubjectId::new(Ulid::from_u128(30))),
            package_version_id: PackageVersionId::new(Ulid::from_u128(9)),
        },
    )
}

#[test]
fn a_field_set_sets_its_values_and_clears_an_empty_one() {
    let changes = fold_custom_data(&occasion_set(1));
    assert_eq!(changes.len(), 2);
    let Some(CustomValueChange::Set(row)) = changes.first() else {
        panic!("the occasion is set: {changes:?}");
    };
    assert_eq!(
        row.object,
        CustomObject::Existing(Open::from(DataObject::Order))
    );
    assert_eq!(row.object_id, Ulid::from_u128(10));
    assert_eq!(
        row.value.list_entry_id,
        Some(ListEntryId::new(Ulid::from_u128(5)))
    );
    assert_eq!(
        changes.get(1),
        Some(&CustomValueChange::Cleared {
            object_id: Ulid::from_u128(10),
            custom_field_id: CustomFieldId::new(Ulid::from_u128(4)),
        })
    );
}

#[test]
fn a_record_keeps_its_type_and_only_the_id_of_its_person() {
    let changes = fold_custom_data(&reservation(1, 4));
    let Some(CustomValueChange::Set(row)) = changes.first() else {
        panic!("the party size is set: {changes:?}");
    };
    assert_eq!(
        row.object,
        CustomObject::Record(ObjectTypeId::new(Ulid::from_u128(21)))
    );
    assert_eq!(row.object_id, Ulid::from_u128(20));
    assert_eq!(row.value.integer, Some(4));
    assert_eq!(row.subject_id, Some(SubjectId::new(Ulid::from_u128(30))));
    assert!(
        matches!(changes.last(), Some(CustomValueChange::Subject { subject_id, .. })
            if *subject_id == SubjectId::new(Ulid::from_u128(30))),
        "the store is recorded as holding the subject: {changes:?}"
    );
}

/// The store confirmed masking the reservation's subject.
fn masked(seed: u32) -> EventEnvelope<RawPayload> {
    event(
        seed,
        EventType::DataSubjectMasked,
        &DataSubjectMasked {
            subject_id: SubjectId::new(Ulid::from_u128(30)),
            masked_time: Timestamp::from_milliseconds_since_epoch(1_000).expect("a time"),
        },
    )
}

#[test]
fn a_confirmed_erasure_marks_its_subject_masked() {
    assert_eq!(
        fold_custom_data(&masked(1)),
        vec![CustomValueChange::SubjectMasked {
            subject_id: SubjectId::new(Ulid::from_u128(30)),
            masked_time: Timestamp::from_milliseconds_since_epoch(1_000).expect("a time"),
        }]
    );
}

#[test]
fn a_value_with_no_person_names_no_subject() {
    assert!(
        fold_custom_data(&occasion_set(1))
            .iter()
            .all(|change| !matches!(change, CustomValueChange::Subject { .. }))
    );
}

#[test]
fn any_other_event_or_an_undecodable_one_changes_nothing() {
    let other = fixtures::activations(store(), 1, 1).remove(0);
    assert!(fold_custom_data(&other).is_empty());
    let mut broken = occasion_set(1);
    broken.data = RawPayload::encode(&serde_json::json!({ "object_id": 7 })).expect("encode");
    assert!(fold_custom_data(&broken).is_empty());
}

/// The rows of one store and its cursor, keyed by object and field as the real table is.
#[derive(Default)]
struct FakeValues {
    rows: Mutex<BTreeMap<(Ulid, CustomFieldId), CustomValueRow>>,
    cursor: Mutex<Option<EventId>>,
}

impl CustomValueStore for FakeValues {
    async fn cursor(&self, _: TenantId, _: StoreId) -> Result<Option<EventId>, PortError> {
        Ok(*self.cursor.lock().expect("lock"))
    }

    async fn apply(
        &self,
        _: TenantId,
        _: StoreId,
        changes: &[CustomValueChange],
        cursor: EventId,
    ) -> Result<(), PortError> {
        let mut rows = self.rows.lock().expect("lock");
        for change in changes {
            match change {
                CustomValueChange::Set(row) => {
                    rows.insert((row.object_id, row.value.custom_field_id), row.clone());
                }
                CustomValueChange::Cleared {
                    object_id,
                    custom_field_id,
                } => {
                    rows.remove(&(*object_id, *custom_field_id));
                }
                CustomValueChange::Subject { .. } | CustomValueChange::SubjectMasked { .. } => {}
            }
        }
        *self.cursor.lock().expect("lock") = Some(cursor);
        Ok(())
    }
}

async fn append(log: &FakeStore, events: &[EventEnvelope<RawPayload>]) {
    let mut tx = log.begin().await.expect("begin");
    log.append(&mut tx, events).await.expect("append");
    tx.commit().await.expect("commit");
}

/// The latest value wins, each event is folded once, and a second pass with nothing new reads
/// nothing.
#[tokio::test]
async fn a_store_is_projected_once_per_event_and_the_latest_value_wins() {
    let log = FakeStore::new();
    let values = FakeValues::default();
    append(
        &log,
        &[
            occasion_set(1),
            fixtures::activations(store(), 2, 1).remove(0),
            reservation(3, 4),
        ],
    )
    .await;
    assert_eq!(
        project_custom_values(&log, &values, tenant(), store())
            .await
            .expect("project"),
        CustomPass {
            read: 3,
            masked: false
        }
    );
    append(&log, &[reservation(4, 6), masked(5)]).await;
    assert_eq!(
        project_custom_values(&log, &values, tenant(), store())
            .await
            .expect("project"),
        CustomPass {
            read: 2,
            masked: true
        },
        "a confirmed erasure is reported, so its id can come off the node"
    );
    assert_eq!(
        project_custom_values(&log, &values, tenant(), store())
            .await
            .expect("project"),
        CustomPass::default()
    );

    let rows = values.rows.lock().expect("lock");
    assert_eq!(
        rows.len(),
        2,
        "the occasion and the party size; the cleared field has no row"
    );
    let party = rows
        .get(&(Ulid::from_u128(20), CustomFieldId::new(Ulid::from_u128(6))))
        .expect("the party size");
    assert_eq!(party.value.integer, Some(6));
}
