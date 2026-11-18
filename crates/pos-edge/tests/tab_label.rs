// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A guest's label on a bar tab
//! ([ADR-0221](../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)
//! decision 2): written only where the store has turned `tabs.guest_label` on, kept in the subject
//! store and in no event, listed beside the tab's number, cleared by an empty label, hidden once the
//! setting is off, and masked by the store's retention sweep.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_core::capability::{Capability, CapabilityContext};
use pos_core::decision::Actor;
use pos_edge::app::{AppError, TabLabel};
use pos_edge::queue::InMemoryQueueNumbers;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::subject_store::SubjectStore;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, EmployeeId, OrderId, StoreId};
use pos_proto::tabs::{GuestLabel, PublishedTabs};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

/// A fake guest's label; no real person.
const FAKE_LABEL: &str = "Fake Guest 7";

fn bartender() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn bar(store: FakeStore, label: GuestLabel) -> Edge<FakeStore> {
    let mut session = EdgeSession::bootstrap();
    session.capabilities = CapabilityContext::NONE.with(Capability::Tabs);
    session.tabs = PublishedTabs {
        guest_label: Open::from_known(label),
    };
    Edge::new(
        store,
        StoreIdentity::for_store(store_id()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator")
}

async fn open_tab(edge: &Edge<FakeStore>) -> OrderId {
    edge.open_tab(
        bartender(),
        SalesChannel::Takeaway,
        &InMemoryQueueNumbers::new(),
    )
    .await
    .expect("opens a tab")
    .order_id
}

fn label(text: &str) -> Option<TabLabel> {
    TabLabel::parse(text).expect("a label within its bounds")
}

#[test]
fn a_label_is_refused_where_the_store_keeps_numbers_only() {
    run_ready(async {
        let edge = bar(FakeStore::default(), GuestLabel::Off);
        let tab = open_tab(&edge).await;
        let refused = edge.label_tab(bartender(), tab, label(FAKE_LABEL)).await;
        assert!(
            matches!(refused, Err(AppError::GuestLabelOff)),
            "{refused:?}"
        );
    });
}

#[test]
fn a_label_is_bounded_and_never_shown_by_debug() {
    assert!(TabLabel::parse(&"x".repeat(33)).is_err());
    assert!(TabLabel::parse("a\u{7}b").is_err());
    assert_eq!(TabLabel::parse("   ").expect("blank clears"), None);
    let kept = label(&"x".repeat(32)).expect("32 characters");
    assert_eq!(format!("{kept:?}"), "TabLabel(..)");
}

#[test]
fn a_label_stays_in_the_subject_store_and_is_listed_beside_its_tab() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = bar(store.clone(), GuestLabel::On);
        let tab = open_tab(&edge).await;
        let other = OrderId::new(Ulid::from_u128(999));
        let unknown = edge.label_tab(bartender(), other, label(FAKE_LABEL)).await;
        assert!(
            matches!(unknown, Err(AppError::UnknownOrder)),
            "{unknown:?}"
        );

        edge.label_tab(bartender(), tab, label(FAKE_LABEL))
            .await
            .expect("labels the tab");
        let labels = edge.tab_labels(&[tab]).await.expect("reads");
        assert_eq!(labels.get(&tab).map(String::as_str), Some(FAKE_LABEL));

        let query = EventQuery::first(store_id(), NonZeroU32::new(200).expect("a positive limit"));
        let events = store.read(&query).await.expect("reads the log");
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| !event.data.as_json().contains(FAKE_LABEL)),
            "no event carries the label"
        );

        let hidden = bar(store.clone(), GuestLabel::Off);
        assert!(
            hidden.tab_labels(&[tab]).await.expect("reads").is_empty(),
            "off hides it"
        );

        edge.label_tab(bartender(), tab, None)
            .await
            .expect("clears");
        assert!(
            edge.tab_labels(&[tab]).await.expect("reads").is_empty(),
            "cleared"
        );

        edge.label_tab(bartender(), tab, label(FAKE_LABEL))
            .await
            .expect("labels again");
        let far = Timestamp::from_milliseconds_since_epoch(4_102_444_800_000).expect("an instant");
        store
            .mask_before(store_id(), far, far)
            .await
            .expect("sweeps");
        assert!(
            edge.tab_labels(&[tab]).await.expect("reads").is_empty(),
            "masked"
        );
    });
}
