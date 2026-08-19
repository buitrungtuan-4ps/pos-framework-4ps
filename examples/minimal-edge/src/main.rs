// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The smallest runnable store.
//!
//! Boots `pos_edge` with a fixed dev store id and no config file — no database, no printer, no
//! network. It is what `just run-edge` runs, and it exists so a contributor can see the edge come up
//! from the repository alone ([`docs/roadmap.md`](../../docs/roadmap.md) P5).
//!
//! As the P5 slices land, this example grows to compose the edge over `pos-fakes` — an in-memory
//! `EventStore`, a `FakeClock`, a `FakeIdGenerator` — so the whole dine-in flow runs here with the
//! cable unplugged, exactly as it will against the real adapters.

use pos_edge::{EdgeConfig, EdgeError, serve, telemetry};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

#[tokio::main]
async fn main() -> Result<(), EdgeError> {
    telemetry::init();

    // A fixed identifier so the example is reproducible; a real store is activated with its own.
    let store_id = StoreId::new(Ulid::from_u128(1));
    let bind = "127.0.0.1:8787".parse().expect("a valid loopback address");

    tracing::info!("minimal-edge is coming up — open http://{bind}/ (Ctrl-C to stop)");
    serve(EdgeConfig::new(bind, store_id)).await
}
