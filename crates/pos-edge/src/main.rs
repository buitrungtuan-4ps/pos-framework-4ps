// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_edge` entry point.
//!
//! Deliberately tiny: install logging, load the bootstrap config, serve. Everything testable lives
//! in the [`pos_edge`] library. The config path comes from `POS_EDGE_CONFIG`, defaulting to
//! `config.toml` in the working directory; on a real store it is written at activation. To run the
//! edge on fakes with no config file, use the `minimal-edge` example (`just run-edge`).

use std::path::PathBuf;

use pos_edge::{EdgeConfig, EdgeError, serve, telemetry};

#[tokio::main]
async fn main() -> Result<(), EdgeError> {
    telemetry::init();

    let path = std::env::var_os("POS_EDGE_CONFIG")
        .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from);
    let config = EdgeConfig::load(&path)?;

    serve(config).await
}
