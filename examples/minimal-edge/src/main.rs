// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The smallest runnable store.
//!
//! Boots `pos_edge` over the in-memory fakes with a fixed dev store id and no config file — no
//! database, no printer, no network ([`docs/roadmap.md`](../../docs/roadmap.md) P5). It is what
//! `just run-edge` runs, and it exists so a contributor can see the edge come up, seat a table, and
//! watch the change fan out to every device — from the repository alone, with the cable unplugged.

use std::sync::Arc;

use pos_edge::{
    Edge, EdgeConfig, EdgeError, EdgeSession, InMemoryLease, InMemoryOtaState,
    InMemoryQueueNumbers, InMemoryReceipts, StoreIdentity, serve, telemetry,
};
use pos_fakes::FakeStore;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

#[tokio::main]
async fn main() -> Result<(), EdgeError> {
    telemetry::init();

    // A fixed identifier so the example is reproducible; a real store is activated with its own.
    let store_id = StoreId::new(Ulid::from_u128(1));
    let identity = StoreIdentity::for_store(store_id);
    // The example numbers receipts in memory; a real store uses its SQLite writer thread (ADR-0025).
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    );

    let bind = "127.0.0.1:8787".parse().expect("a valid loopback address");
    tracing::info!("minimal-edge is coming up — open http://{bind}/ (Ctrl-C to stop)");
    // The queue-number authority the relay's intake would use; the example has no `cloud_url`, so no
    // relay runs and it is never allocated from — a real store passes its SQLite writer (ADR-0064).
    // The OTA self-test authority. In memory, like everything else here — and it is never read:
    // the example has no `bin/current` layout, so no over-the-air updater is composed at all
    // (ADR-0055 Amendment 1). A real store passes its SQLite writer, which survives the restart an
    // install performs.
    // The outcome says whether the stop was an update's restart; here it never is — no updater is
    // composed — so Ctrl-C is the only way this ends and there is nothing to act on.
    serve(
        EdgeConfig::new(bind, store_id),
        edge,
        InMemoryQueueNumbers::default(),
        InMemoryOtaState::new(),
        // The lease authority (ADR-0108), in memory like the rest — behind an `Arc` because two
        // loops share one authority (the OTA tick weighs the standing, the heartbeat reports the
        // generation held), and `SqliteStore` is `Clone` where this is not. Never consulted here:
        // with no updater composed there is no tick, and no cloud to publish a `lease` node.
        std::sync::Arc::new(InMemoryLease::new()),
    )
    .await
    .map(|_outcome| ())
}
