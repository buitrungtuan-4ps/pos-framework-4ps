// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The smallest runnable store.
//!
//! Boots `pos_edge` over the in-memory fakes with a fixed dev store id and no config file — no
//! database, no printer, no network ([`docs/roadmap.md`](../../docs/roadmap.md) P5). It is what
//! `just run-edge` runs, and it exists so a contributor can see the edge come up, seat a table, and
//! watch the change fan out to every device — from the repository alone, with the cable unplugged.

use std::sync::Arc;

use pos_edge::config_client::session_from_config;
use pos_edge::demo;
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

    // The store publishes its own configuration to itself
    // ([ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md) Amendment 1). `bootstrap()` seeds an empty
    // roster and an empty price book — correct for a till that has not synced, and the reason this
    // example could not seat a table: no code signed in, so every domain route answered 403.
    //
    // The document goes through `session_from_config`, the same public seam a real store's synced
    // config goes through, node for node. Nothing here reaches around it.
    let session = match demo::config_document() {
        Some(document) => session_from_config(&EdgeSession::bootstrap(), &document),
        None => {
            // The OS entropy source is the only thing that fails here, and it is the same source the
            // pairing code needs — so this box cannot pair either. Come up anyway and say what is
            // missing: a running edge with a plain "no roster" line beats a silent exit.
            tracing::warn!("could not hash the demo PIN — the till will have no one to sign in");
            EdgeSession::bootstrap()
        }
    };

    // The example numbers receipts in memory; a real store uses its SQLite writer thread (ADR-0025).
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    );

    // Where to listen. `POS_EDGE_BIND` overrides the default so two of these can run at once —
    // a second instance on the same port dies with `AddrInUse`, which is what a contributor hits
    // when the step-budget harness starts one while their own is up
    // ([ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md)).
    //
    // A concrete port, not `:0`: the pairing URL is composed before the listener binds, so port
    // zero would advertise `:0` and the QR would point nowhere. A caller that wants a free port
    // picks one and names it.
    let bind = match std::env::var("POS_EDGE_BIND") {
        Ok(named) => match named.parse() {
            Ok(bind) => bind,
            Err(_) => {
                tracing::error!(%named, "POS_EDGE_BIND is not an address:port — refusing to start");
                return Ok(());
            }
        },
        Err(_) => "127.0.0.1:8787".parse().expect("a valid loopback address"),
    };
    tracing::info!("minimal-edge is coming up — open http://{bind}/ (Ctrl-C to stop)");
    // Printed, not left to be found. Pairing is the first gate and signing in is the second
    // (ADR-0084): the pairing code is minted per boot and logged by the pairing module, and this is
    // the badge the demo roster carries. Both are worthless off this loopback socket, which holds no
    // data and forgets everything on exit.
    tracing::info!(
        "sign in with code {} and PIN {}",
        demo::DEMO_STAFF_CODE,
        demo::DEMO_STAFF_PIN
    );
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
        // The print-agent binding (ADR-0112), in memory too. Nothing claims one here — that takes a
        // manager signed in on a paired device — so the two routes exist and refuse, which is the
        // truth for this example rather than a silent no-op.
        pos_edge::print_agent::InMemoryPrintAgents::new(),
        // …and the queue itself. With no agent bound, nothing is ever enqueued into it: a printer
        // this example published would name no agent, so the dispatch opens the address directly.
        pos_edge::print_queue::InMemoryPrintQueue::new(),
    )
    .await
    .map(|_outcome| ())
}
