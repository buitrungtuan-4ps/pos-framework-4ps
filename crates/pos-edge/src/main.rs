// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_edge` entry point.
//!
//! Deliberately tiny: install logging, load the bootstrap config, open the SQLite store, compose the
//! application [`Edge`], serve. Everything testable lives in the [`pos_edge`] library. The config
//! path comes from `POS_EDGE_CONFIG`, defaulting to `config.toml`; on a real store it is written at
//! activation. To run the edge on fakes with no config file, use the `minimal-edge` example
//! (`just run-edge`).

use std::path::PathBuf;
use std::sync::Arc;

use pos_edge::{Edge, EdgeConfig, EdgeError, EdgeSession, StoreIdentity, serve, telemetry};
use store_sqlite::SqliteStore;

#[tokio::main]
async fn main() -> Result<(), EdgeError> {
    telemetry::init();

    let path = std::env::var_os("POS_EDGE_CONFIG")
        .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from);
    let config = EdgeConfig::load(&path)?;

    // The real edge stores events in SQLite (ADR-0015); the example uses the in-memory fakes.
    let store = SqliteStore::open(&config.store_path).map_err(EdgeError::Store)?;
    let identity = StoreIdentity::for_store(config.store_id);
    // The store's own writer thread is the gapless receipt authority (ADR-0025); a clone shares it,
    // so the loop that appends the settled event and the authority that numbers it are one store.
    let receipts = Arc::new(store.clone());
    let edge = Arc::new(
        Edge::new(store, identity, EdgeSession::bootstrap(), receipts)
            .map_err(EdgeError::Entropy)?,
    );

    serve(config, edge).await
}
