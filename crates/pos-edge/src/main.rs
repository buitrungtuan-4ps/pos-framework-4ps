// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_edge` entry point.
//!
//! Deliberately tiny: install logging, load the bootstrap config, open the SQLite store, compose the
//! application [`Edge`], serve. Everything testable lives in the [`pos_edge`] library. The config
//! path comes from `POS_EDGE_CONFIG`, defaulting to `config.toml`; on a real store it is written at
//! activation. To run the edge on fakes with no config file, use the `minimal-edge` example
//! (`just run-edge`).
//!
//! One flag: `--self-test`, which the over-the-air installer runs against a *staged* binary before
//! swapping it in ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1).

use std::path::PathBuf;
use std::sync::Arc;

use pos_edge::{Edge, EdgeConfig, EdgeError, EdgeSession, StoreIdentity, serve, telemetry};
use store_sqlite::SqliteStore;

/// The flag [`UpdateInstaller::self_test`](pos_edge::UpdateInstaller::self_test) runs the staged
/// binary with.
const SELF_TEST_FLAG: &str = "--self-test";

#[tokio::main]
async fn main() -> Result<(), EdgeError> {
    telemetry::init();

    let path = std::env::var_os("POS_EDGE_CONFIG")
        .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from);

    if std::env::args().any(|argument| argument == SELF_TEST_FLAG) {
        return self_test(&path);
    }

    let config = EdgeConfig::load(&path)?;

    // The real edge stores events in SQLite (ADR-0015); the example uses the in-memory fakes.
    let store = SqliteStore::open(&config.store_path).map_err(EdgeError::Store)?;
    let identity = StoreIdentity::for_store(config.store_id);
    // The store's own writer thread is the gapless receipt authority (ADR-0025); a clone shares it,
    // so the loop that appends the settled event and the authority that numbers it are one store.
    let receipts = Arc::new(store.clone());
    // The same single writer thread is the durable daily queue-number authority (ADR-0064), so a
    // relayed takeaway order gets a number that survives a restart; another clone carries it into
    // `serve`, which builds the relay's intake from it (ADR-0087).
    let queue = store.clone();
    // And the same writer thread is the durable OTA self-test authority (ADR-0048's
    // highest-precedence rule reads it, and an install deliberately restarts the edge, so the
    // verdict has to be on disk rather than in process memory — ADR-0055 Amendment 1).
    let ota_state = store.clone();
    let edge = Arc::new(
        Edge::new(store, identity, EdgeSession::bootstrap(), receipts)
            .map_err(EdgeError::Entropy)?,
    );

    // Replay the durable log into the projection before serving, so a restart resumes exactly where
    // the last committed transaction left off (ADR-0015, the crash-recovery half of P5).
    edge.rebuild().await.map_err(EdgeError::Rebuild)?;

    serve(config, edge, queue, ota_state).await
}

/// The pre-commit smoke test the OTA installer runs against a *staged* binary: can these bytes run
/// on this box, and can they read this store's configuration?
///
/// It answers the questions that are worth answering before a swap — the wrong architecture, a
/// truncated download, a missing shared library, a config the new version's parser rejects — and it
/// does so by the act of getting this far: reaching `main` means the loader accepted the executable.
///
/// **It deliberately does not open the database.** The binary being tested is a *second* process
/// while the running edge still owns that file, and `SqliteStore::open` migrates; two writers and a
/// schema change against a live store is a worse risk than the coverage it would buy. Whether this
/// version can come up for real is the question the boot confirmation answers, after the swap, in
/// the only process that owns the store (ADR-0055 Amendment 1).
fn self_test(config_path: &std::path::Path) -> Result<(), EdgeError> {
    let config = EdgeConfig::load(config_path)?;
    tracing::info!(
        version = pos_edge::VERSION,
        store_id = %config.store_id,
        "self-test passed: this binary runs and reads this store's configuration"
    );
    Ok(())
}
