// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A stopping store empties its outbox before it exits
//! ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md), production-readiness
//! **D8**).
//!
//! The loop already drained whatever was waiting when it woke. What it did not do was drain on the
//! way *out*: a stop returned from [`EventPublisher::run`] immediately, so every event committed
//! since the last pass — up to a whole idle interval of selling — sat in the outbox. On a box in a
//! shop that costs nothing, because the SQLite file survives the restart. On a placement torn down
//! rather than restarted, its volume goes with it and so do those events
//! ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
//!
//! Each case drives the real [`EventPublisher`] over the in-memory fakes, and uses the shutdown
//! future itself as the clock: it lets the loop settle into its idle sleep, commits events *into
//! that window*, and only then resolves. That is exactly the sequence the defect lost.

use std::sync::Arc;
use std::time::Duration;

use pos_contract_tests::fixtures;
use pos_edge::event_publish::EventPublisher;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::{FakeLink, FakeStore};
use pos_ports::{EventStore, Transactional, TxContext};
use pos_proto::ReleaseTag;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

/// The store every case is.
fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

/// An edge over `store`, freshly booted — the publisher only ever reads its log.
fn edge_over(store: FakeStore) -> Arc<Edge<FakeStore>> {
    Arc::new(
        Edge::new(
            store,
            StoreIdentity::for_store(store_id()),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    )
}

/// Commits `count` events to the outbox in one transaction, the way the application layer does.
async fn commit_events(store: &FakeStore, count: u32) {
    let mut tx = store.begin().await.expect("open a transaction");
    store
        .append(&mut tx, &fixtures::activations(store_id(), 1, count))
        .await
        .expect("append to the log");
    tx.commit().await.expect("commit the transaction");
}

/// Long enough for the loop to make its first pass and settle into its idle sleep, short enough that
/// a case costs no real time. The loop's own idle interval is five seconds, so this cannot race it.
const SETTLE: Duration = Duration::from_millis(50);

/// How many events a case commits — under the link's 256 batch size, so a *single* drain suffices
/// and the case is about draining at all rather than about batching.
const COMMITTED: u32 = 5;

#[tokio::test]
async fn a_stop_drains_what_was_committed_since_the_last_pass() {
    let store = FakeStore::default();
    let edge = edge_over(store.clone());
    let link = FakeLink::new();
    let publisher = EventPublisher::new(Arc::clone(&edge), link.clone(), ReleaseTag::new("v1.0.0"));

    // The shutdown future *is* the scenario: settle, sell, stop. Committing before the stop
    // resolves puts the events squarely in the window the loop is asleep through, which is the
    // window D8 lost.
    let sell_then_stop = async {
        tokio::time::sleep(SETTLE).await;
        commit_events(&store, COMMITTED).await;
    };

    publisher.run(sell_then_stop).await;

    assert_eq!(
        link.published().len(),
        COMMITTED as usize,
        "a stop must publish what was committed since the last pass, not abandon it"
    );
    assert_eq!(
        store
            .outbox_depth(store_id())
            .await
            .expect("read the outbox depth"),
        0,
        "everything published must also be acknowledged, or the next boot re-sends it"
    );
}

#[tokio::test]
async fn a_stop_over_a_link_that_cannot_take_them_leaves_the_events_durable() {
    let store = FakeStore::default();
    let edge = edge_over(store.clone());
    let link = FakeLink::new();
    let publisher = EventPublisher::new(Arc::clone(&edge), link.clone(), ReleaseTag::new("v1.0.0"));

    // The handshake succeeds, then the far side fills up. The drain has nothing to gain by
    // retrying, so it must give up rather than spin out its whole budget — and it must not
    // acknowledge a single record it did not place.
    let fill_then_stop = async {
        tokio::time::sleep(SETTLE).await;
        commit_events(&store, COMMITTED).await;
        link.fill();
    };

    let started = tokio::time::Instant::now();
    publisher.run(fill_then_stop).await;
    let elapsed = started.elapsed();

    assert!(
        link.published().is_empty(),
        "a full stream accepts nothing, so nothing may be reported as published"
    );
    assert_eq!(
        store
            .outbox_depth(store_id())
            .await
            .expect("read the outbox depth"),
        u64::from(COMMITTED),
        "an unpublished event stays in the outbox: the next boot is what publishes it"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a drain the link cannot serve must stop asking, not spend its whole budget: took {elapsed:?}"
    );
}

#[tokio::test]
async fn a_stop_before_the_link_ever_came_up_keeps_every_event() {
    let store = FakeStore::default();
    let edge = edge_over(store.clone());
    let link = FakeLink::new();
    // Severed from the start: the handshake never succeeds, so the loop never reaches a state where
    // draining is even possible. It must still return on a stop rather than back off for the
    // refusal interval, and it must leave the log untouched.
    link.sever();
    let publisher = EventPublisher::new(Arc::clone(&edge), link.clone(), ReleaseTag::new("v1.0.0"));

    commit_events(&store, COMMITTED).await;

    let stop = async {
        tokio::time::sleep(SETTLE).await;
    };

    let started = tokio::time::Instant::now();
    publisher.run(stop).await;
    let elapsed = started.elapsed();

    assert!(
        link.published().is_empty(),
        "a severed link publishes nothing"
    );
    assert_eq!(
        store
            .outbox_depth(store_id())
            .await
            .expect("read the outbox depth"),
        u64::from(COMMITTED),
        "a store that never reached the cloud keeps every event it committed"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a stop must not wait out the refused-handshake backoff: took {elapsed:?}"
    );
}
