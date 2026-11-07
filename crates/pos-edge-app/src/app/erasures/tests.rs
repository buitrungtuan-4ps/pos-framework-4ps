// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_fakes::FakeStore;
use pos_ports::subject_store::{REDACTION, SubjectStore};
use pos_proto::events::EventType;
use pos_proto::ids::{DeviceId, SubjectId};
use pos_proto::ulid::Ulid;

use crate::app::custom_data::tests::{consent, edge_over, entries, logged, reservation};
use crate::app::tests::{actor, identity};

fn box_device() -> DeviceId {
    DeviceId::new(identity().store_id.as_ulid())
}

/// A record with a demo personal note, and its subject.
async fn a_record_with_a_note(edge: &crate::app::Edge<FakeStore>) -> SubjectId {
    edge.save_custom_record(
        actor(),
        reservation(),
        None,
        &entries(4, Some("demo note")),
        &consent(),
    )
    .await
    .expect("saves")
    .subject_id
    .expect("a subject holds the note")
}

fn masked_events(events: &[(EventType, String)]) -> Vec<&String> {
    events
        .iter()
        .filter(|(kind, _)| *kind == EventType::DataSubjectMasked)
        .map(|(_, json)| json)
        .collect()
}

#[tokio::test]
async fn an_erasure_masks_the_record_and_confirms_it_once() {
    let edge = edge_over(FakeStore::default());
    let subject_id = a_record_with_a_note(&edge).await;

    let confirmed = edge
        .erase_subjects(box_device(), &[subject_id])
        .await
        .expect("erases");

    assert_eq!(confirmed, 1);
    let record = edge
        .store
        .fetch(identity().store_id, subject_id)
        .await
        .expect("reads")
        .expect("the record is kept, masked");
    assert!(record.is_masked());
    assert!(record.fields.values().all(|value| value == REDACTION));
    let events = logged(&edge.store).await;
    let masked = masked_events(&events);
    assert_eq!(masked.len(), 1);
    let json = masked.first().expect("one confirmation");
    assert!(json.contains(&subject_id.to_string()));
    assert!(!json.contains("demo note"), "no value reaches the event");

    let again = edge
        .erase_subjects(box_device(), &[subject_id])
        .await
        .expect("erases");
    assert_eq!(again, 0, "a node applied twice confirms nothing twice");
    assert_eq!(masked_events(&logged(&edge.store).await).len(), 1);
}

#[tokio::test]
async fn a_restarted_edge_remembers_what_it_confirmed_from_its_log() {
    let store = FakeStore::default();
    let edge = edge_over(store.clone());
    let subject_id = a_record_with_a_note(&edge).await;
    edge.erase_subjects(box_device(), &[subject_id])
        .await
        .expect("erases");

    let restarted = edge_over(store.clone());
    restarted.rebuild().await.expect("rebuilds from the log");
    let again = restarted
        .erase_subjects(box_device(), &[subject_id])
        .await
        .expect("erases");

    assert_eq!(again, 0);
    assert_eq!(masked_events(&logged(&store).await).len(), 1);
}

#[tokio::test]
async fn a_subject_the_store_holds_nothing_for_is_confirmed_with_nothing_written() {
    let edge = edge_over(FakeStore::default());
    let unknown = SubjectId::new(Ulid::from_u128(0x5AB));

    let confirmed = edge
        .erase_subjects(box_device(), &[unknown])
        .await
        .expect("erases");

    assert_eq!(confirmed, 1);
    assert!(
        edge.store
            .fetch(identity().store_id, unknown)
            .await
            .expect("reads")
            .is_none(),
        "no record is made for a subject the store never held"
    );
    assert_eq!(masked_events(&logged(&edge.store).await).len(), 1);
}
