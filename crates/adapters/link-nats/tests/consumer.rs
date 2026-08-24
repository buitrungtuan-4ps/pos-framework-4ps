// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `NatsConsumer` against a live NATS server with JetStream — the cloud's ingest cursor.
//!
//! Proves the read side of the link end to end: what the edge (`NatsLink`) publishes is what the
//! cursor reads back, an acknowledged batch advances the durable cursor, a nak redelivers, and an
//! undecodable frame is terminated without wedging the good events behind it. These are the
//! properties `docs/roadmap.md` P7 relies on for "the cursor feed" and "reset-cursor-and-replay".
//!
//! Gated behind the `integration` feature, off by default. Run it with a server reachable:
//!
//! ```text
//! NATS_URL=127.0.0.1:4222 cargo test -p link-nats --features integration --test consumer
//! ```

#![cfg(feature = "integration")]
// Test scaffolding: a broker that cannot be reached, or a fixture that will not build, is an
// unrecoverable test-setup fault, so unwrap/expect here is the right, loud failure.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test scaffolding: an unreachable broker or a bad fixture is an unrecoverable \
              test-setup fault"
)]

use core::time::Duration;
use std::sync::atomic::{AtomicU64, Ordering};

use link_nats::{ConsumerConfig, NatsConfig, NatsConsumer, NatsLink};
use pos_contract_tests::fixtures;
use pos_ports::message_link::MessageLink;
use pos_proto::PROTOCOL_VERSION;
use pos_proto::ids::StoreId;
use pos_proto::protocol::{Hello, MIN_SUPPORTED_PROTOCOL_VERSION};
use pos_proto::ulid::Ulid;

/// Global across cases: each test builds a fresh stream, so a per-test counter would restart at 0
/// and collide stream names on the shared server.
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

/// A fresh stream with its edge link (handshook, so the stream exists) and a cloud cursor bound to
/// it. The subject is returned so a test can publish a raw frame straight to it.
async fn fresh() -> (NatsLink, NatsConsumer, String) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let stream = format!("POS_CONS_{pid}_{n}");
    let subject = format!("pos.cons.{pid}.{n}.events");

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
            filter_subject: subject.clone(),
            batch: 64,
            expires: Duration::from_secs(1),
        },
    )
    .await
    .expect("bind the cloud cursor");

    (link, consumer, subject)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cursor_reads_back_what_the_edge_published() {
    let (link, consumer, _subject) = fresh().await;
    let events = fixtures::activations(store_id(), 1, 5);
    let outcome = link.publish(&events).await.expect("publish the batch");
    assert_eq!(outcome.accepted, 5);

    let batch = consumer.pull().await.expect("pull the batch");
    assert_eq!(batch.len(), 5);
    assert_eq!(batch.poison(), 0);
    assert_eq!(
        batch.events(),
        events.as_slice(),
        "the cursor reads back exactly what was published"
    );
    batch.ack().await.expect("acknowledge the batch");

    // The durable cursor advanced past the acknowledged batch, so a second pull sees nothing.
    let empty = consumer.pull().await.expect("pull again");
    assert!(empty.is_empty(), "an acked batch is not redelivered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nak_returns_the_batch_for_redelivery() {
    let (link, consumer, _subject) = fresh().await;
    let events = fixtures::activations(store_id(), 1, 3);
    link.publish(&events).await.expect("publish the batch");

    let first = consumer.pull().await.expect("pull the batch");
    assert_eq!(first.len(), 3);
    first.nak().await.expect("return the batch");

    // Never acknowledged, so the same batch comes back — the retryable path idempotent ingest needs.
    let again = consumer.pull().await.expect("pull after nak");
    assert_eq!(again.len(), 3);
    assert_eq!(again.events(), events.as_slice());
    again.ack().await.expect("acknowledge the batch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_poison_frame_is_terminated_and_does_not_block_good_events() {
    let (link, consumer, subject) = fresh().await;

    // A raw, non-envelope frame published straight to the stream's subject, ahead of a good event.
    link.client()
        .publish(subject, "not an envelope".into())
        .await
        .expect("publish the corrupt frame");
    link.client()
        .flush()
        .await
        .expect("flush the corrupt frame");
    let events = fixtures::activations(store_id(), 1, 1);
    link.publish(&events).await.expect("publish the good event");

    let batch = consumer.pull().await.expect("pull the batch");
    assert_eq!(batch.poison(), 1, "the corrupt frame is counted");
    assert_eq!(batch.len(), 1, "the good event still comes through");
    assert_eq!(batch.events(), events.as_slice());
    batch.ack().await.expect("acknowledge the good event");

    // The poison was terminated, not merely skipped, so it is never redelivered.
    let empty = consumer.pull().await.expect("pull again");
    assert!(empty.is_empty(), "the terminated frame does not come back");
}
