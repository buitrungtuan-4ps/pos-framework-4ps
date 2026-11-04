// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `StoreWake` contract suite against a live NATS server, with the cloud's side,
//! `publish_config_wake`, sending every wake (ADR-0195).
//!
//! Gated behind the `integration` feature, off by default:
//!
//! ```text
//! NATS_URL=127.0.0.1:4222 cargo test -p link-nats --features integration --test wake
//! ```

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: an unbuildable runtime is an unrecoverable test-setup fault"
)]

use core::time::Duration;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use link_nats::{NatsStoreWake, publish_config_wake};
use pos_contract_tests::harness::{HarnessError, Setup, StoreWakeHarness};
use pos_proto::Ulid;
use pos_proto::ids::StoreId;
use pos_proto::wake::ConfigWake;

/// Global across cases, which share the server: each case gets stores no other case hears.
static NEXT_STORE: AtomicU64 = AtomicU64::new(0);

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

fn setup<E: core::fmt::Display>(error: E) -> HarnessError {
    HarnessError::new(error.to_string())
}

async fn client() -> Setup<async_nats::Client> {
    async_nats::connect(nats_url()).await.map_err(setup)
}

/// A store id unique to this process and case, so a re-run never hears an earlier one.
fn fresh_store() -> StoreId {
    let n = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
    let pid = u128::from(std::process::id());
    StoreId::new(Ulid::from_u128((pid << 64) | u128::from(n)))
}

struct WakeHarness {
    stores: (StoreId, StoreId),
}

impl WakeHarness {
    fn new() -> Self {
        Self {
            stores: (fresh_store(), fresh_store()),
        }
    }
}

impl StoreWakeHarness for WakeHarness {
    type Wake = NatsStoreWake;

    fn store_ids(&self) -> (StoreId, StoreId) {
        self.stores
    }

    async fn subscribe(&self, store_id: StoreId) -> Setup<NatsStoreWake> {
        let wake = NatsStoreWake::subscribe(client().await?, store_id)
            .await
            .map_err(setup)?;
        // The subscription reaches the server before the case sends to it from another connection.
        wake.client().flush().await.map_err(setup)?;
        Ok(wake)
    }

    async fn send(&self, store_id: StoreId, wake: &ConfigWake) -> Setup<()> {
        let sender = client().await?;
        publish_config_wake(&sender, store_id, wake)
            .await
            .map_err(setup)?;
        sender.flush().await.map_err(setup)
    }

    async fn reconnect(&self, wake: &NatsStoreWake) -> Setup<()> {
        wake.client().force_reconnect().await.map_err(setup)
    }

    fn patience(&self) -> Duration {
        Duration::from_secs(2)
    }
}

mod store_wake {
    use super::{WakeHarness, block_on};
    pos_contract_tests::store_wake_suite!(WakeHarness::new(), block_on);
}
