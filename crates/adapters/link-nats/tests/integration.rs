// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `link-nats` against a live NATS server with JetStream.
//!
//! Runs the shared `MessageLink` contract suite — the same cases as the in-memory fake — so the
//! at-least-once, back-pressure, and handshake obligations are proven against real JetStream. The
//! unhealthy cases are the point: `sever` drains the connection so a publish must fail retryably
//! rather than silently discard, and `fill` publishes the stream to its (small) cap so the next
//! publish reports `resource_exhausted`.
//!
//! Gated behind the `integration` feature, off by default. Run it with a server reachable:
//!
//! ```text
//! NATS_URL=127.0.0.1:4222 cargo test -p link-nats --features integration
//! ```

#![cfg(feature = "integration")]
// Test scaffolding: the harness lives outside the `#[test]` scope `allow-expect-in-tests` covers.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::needless_pass_by_value,
    reason = "test scaffolding: an unreachable broker or a bad fixture is an unrecoverable \
              test-setup fault, and the error converter is used point-free with map_err"
)]

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use link_nats::{NatsConfig, NatsLink};
use pos_contract_tests::fixtures;
use pos_contract_tests::harness::{HarnessError, MessageLinkHarness, Setup};
use pos_ports::message_link::MessageLink;
use pos_proto::Ulid;
use pos_proto::ids::StoreId;

/// A small stream cap, so `fill` reaches it in a handful of publishes.
const MAX_MESSAGES: i64 = 8;

/// Global across cases: the suite builds a fresh harness per test, so a per-harness counter would
/// restart at 0 and collide stream names between cases on the shared server.
static NEXT_STREAM: AtomicU64 = AtomicU64::new(0);

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a multi-thread tokio runtime")
        .block_on(future)
}

fn nats_url() -> String {
    std::env::var("NATS_URL").unwrap_or_else(|_| "127.0.0.1:4222".to_owned())
}

fn port_err(error: pos_ports::PortError) -> HarnessError {
    HarnessError::new(error.to_string())
}

struct LinkHarness;

impl LinkHarness {
    fn new() -> Self {
        Self
    }
}

impl MessageLinkHarness for LinkHarness {
    type Link = NatsLink;

    async fn fresh(&self) -> Setup<NatsLink> {
        // A fresh stream name per case, carrying the process id so a re-run never sees a previous
        // run's messages — the empty-stream start every case assumes.
        let n = NEXT_STREAM.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let config = NatsConfig {
            stream: format!("POS_TEST_{pid}_{n}"),
            subject: format!("pos.test.{pid}.{n}.events"),
            max_messages: MAX_MESSAGES,
            max_bytes: -1,
        };
        NatsLink::connect(&nats_url(), config)
            .await
            .map_err(port_err)
    }

    async fn sever(&self, link: &NatsLink) -> Setup<()> {
        // Drain permanently closes the shared connection, so subsequent JetStream calls fail — the
        // ambiguous result the "never at-most-once" obligation checks.
        link.client()
            .drain()
            .await
            .map_err(|error| HarnessError::new(error.to_string()))
    }

    async fn fill(&self, link: &NatsLink) -> Setup<()> {
        // Publish the stream to its cap; `discard: new` then refuses further messages.
        let count = u32::try_from(MAX_MESSAGES).unwrap_or(0);
        let events = fixtures::activations(self.store_id(), 1, count);
        link.publish(&events).await.map_err(port_err)?;
        Ok(())
    }

    fn store_id(&self) -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }
}

mod message_link {
    use super::{LinkHarness, block_on};
    pos_contract_tests::message_link_suite!(LinkHarness::new(), block_on);
}
