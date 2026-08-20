// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The production ingest path end to end: a live NATS cursor driving `Cloud::ingest`.
//!
//! `link-nats`'s own suite proves the cursor mechanics (read-back, ack, nak, poison) and
//! `tests/cloud.rs` proves ingest idempotency on the fake. This joins them: events published to real
//! JetStream are pulled by [`pos_cloud::cursor::pump`] and land in the store, and a redelivery is
//! folded to duplicates rather than stored twice — the "cursor drives ingest, idempotently"
//! guarantee `docs/roadmap.md` P7 rests on. A [`FakeStore`] stands in for PostgreSQL, so this needs
//! only NATS.
//!
//! Gated behind the `integration` feature, off by default:
//!
//! ```text
//! NATS_URL=127.0.0.1:4222 cargo test -p pos-cloud --features integration --test cursor
//! ```

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test scaffolding: an unreachable broker or a bad fixture is an unrecoverable \
              test-setup fault"
)]

use core::time::Duration;
use std::sync::atomic::{AtomicU64, Ordering};

use link_nats::{ConsumerConfig, NatsConfig, NatsConsumer, NatsLink};
use pos_cloud::Cloud;
use pos_cloud::cursor::pump;
use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_ports::message_link::MessageLink;
use pos_proto::PROTOCOL_VERSION;
use pos_proto::ids::StoreId;
use pos_proto::protocol::{Hello, MIN_SUPPORTED_PROTOCOL_VERSION};
use pos_proto::ulid::Ulid;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn nats_url() -> String {
    std::env::var("NATS_URL").unwrap_or_else(|_| "127.0.0.1:4222".to_owned())
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0x0ADA))
}

fn hello() -> Hello {
    Hello {
        protocol_version_min: MIN_SUPPORTED_PROTOCOL_VERSION,
        protocol_version_max: PROTOCOL_VERSION,
        product_version: pos_proto::ReleaseTag::new("v0.1.0"),
        store_id: store_id(),
        lease_token: None,
    }
}

/// A fresh stream, its edge link (handshook so the stream exists), and a cloud cursor over it.
async fn fresh() -> (NatsLink, NatsConsumer) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let stream = format!("POS_PUMP_{pid}_{n}");
    let subject = format!("pos.pump.{pid}.{n}.events");

    let link = NatsLink::connect(
        &nats_url(),
        NatsConfig {
            stream: stream.clone(),
            subject: subject.clone(),
            max_messages: -1,
            max_bytes: -1,
        },
    )
    .await
    .expect("connect the edge link");
    link.handshake(&hello())
        .await
        .expect("the handshake creates the stream");

    let consumer = NatsConsumer::connect(
        &nats_url(),
        ConsumerConfig {
            stream,
            durable: format!("cloud_{pid}_{n}"),
            filter_subject: subject,
            batch: 64,
            expires: Duration::from_secs(1),
        },
    )
    .await
    .expect("bind the cloud cursor");

    (link, consumer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cursor_ingests_a_published_batch_and_folds_a_replay_to_duplicates() {
    let (link, consumer) = fresh().await;
    let cloud = Cloud::new(FakeStore::new());
    let events = fixtures::activations(store_id(), 1, 4);

    // The edge publishes; the cloud cursor pulls and ingests.
    link.publish(&events).await.expect("publish the batch");
    let first = pump(&consumer, &cloud).await.expect("pump");
    assert_eq!(first.ingested, 4);
    assert_eq!(first.duplicates, 0);
    assert_eq!(first.poison, 0);
    assert!(!first.redelivered);

    // The events are durable in the store, folded into one trading day.
    let total: u64 = cloud
        .daily_rollups(store_id())
        .await
        .expect("rollups")
        .iter()
        .map(|day| day.total_events)
        .sum();
    assert_eq!(total, 4);

    // At-least-once delivery: the same events published again carry the same ids, so ingest folds
    // them to duplicates and the log does not grow.
    link.publish(&events).await.expect("republish the batch");
    let second = pump(&consumer, &cloud).await.expect("pump the replay");
    assert_eq!(second.ingested, 0);
    assert_eq!(second.duplicates, 4);

    let total_after: u64 = cloud
        .daily_rollups(store_id())
        .await
        .expect("rollups")
        .iter()
        .map(|day| day.total_events)
        .sum();
    assert_eq!(total_after, 4, "a replay must not grow the log");
}
