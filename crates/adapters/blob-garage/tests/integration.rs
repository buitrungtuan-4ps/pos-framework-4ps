// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `blob-garage` against a live S3-compatible object store (MinIO or Garage).
//!
//! Runs the shared `BlobStore` contract suite — the same cases as the in-memory fake — so the
//! hand-rolled `SigV4` and HTTP are proven against a real server rather than reasoned about. The
//! signer's arithmetic is checked separately and server-free in `src/sign.rs`.
//!
//! Gated behind the `integration` feature, off by default, so the pull-request build stays
//! infrastructure-free. Run it with a server reachable:
//!
//! ```text
//! S3_ENDPOINT=http://localhost:9000 S3_ACCESS_KEY=minioadmin S3_SECRET_KEY=minioadmin \
//!   cargo test -p blob-garage --features integration
//! ```

#![cfg(feature = "integration")]
// Test scaffolding: the harness setup is outside the `#[test]` scope `allow-expect-in-tests` covers.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::needless_pass_by_value,
    reason = "test scaffolding: an unreachable object store or a bad fixture is an unrecoverable \
              test-setup fault, and the error-to-HarnessError converter is used point-free with \
              `map_err`"
)]

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use blob_garage::S3Blobs;
use pos_contract_tests::harness::{BlobStoreHarness, HarnessError, Setup};

/// Drives a future on a fresh multi-thread runtime with IO enabled — the adapter's sockets need it.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a multi-thread tokio runtime")
        .block_on(future)
}

fn endpoint() -> String {
    std::env::var("S3_ENDPOINT").unwrap_or_else(|_| "http://localhost:9000".to_owned())
}
fn region() -> String {
    std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned())
}
fn access_key() -> String {
    std::env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_owned())
}
fn secret_key() -> String {
    std::env::var("S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".to_owned())
}

fn port_err(error: pos_ports::PortError) -> HarnessError {
    HarnessError::new(error.to_string())
}

/// Global across every case: the suite builds a fresh harness per test, so a per-harness counter
/// would restart at 0 and collide buckets between cases on the shared server.
static NEXT_BUCKET: AtomicU64 = AtomicU64::new(0);

/// A harness that hands every case its own freshly-created bucket, which is the clean slate the
/// contract requires without having to empty anything. The bucket name carries the process id so a
/// re-run never collides with a previous run's leftovers.
struct StoreHarness;

impl StoreHarness {
    fn new() -> Self {
        Self
    }
}

impl BlobStoreHarness for StoreHarness {
    type Store = S3Blobs;

    async fn fresh(&self) -> Setup<S3Blobs> {
        let n = NEXT_BUCKET.fetch_add(1, Ordering::Relaxed);
        let bucket = format!("pos-blobtest-{}-{n}", std::process::id());
        let store = S3Blobs::new(
            &endpoint(),
            &bucket,
            &region(),
            &access_key(),
            &secret_key(),
        )
        .map_err(port_err)?;
        store.ensure_bucket().await.map_err(port_err)?;
        Ok(store)
    }
}

mod blob_store {
    use super::{StoreHarness, block_on};
    pos_contract_tests::blob_store_suite!(StoreHarness::new(), block_on);
}
