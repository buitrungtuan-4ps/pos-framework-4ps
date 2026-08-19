// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's start-up and serving errors.
//!
//! These are the failures that stop the process from running at all — a missing config, an address
//! already in use. Failures *inside* a request are the port `PortError` mapped to an AIP-193 body by
//! the HTTP layer, not this type.

use std::net::SocketAddr;

/// A failure that prevents `pos_edge` from starting or continuing to serve.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EdgeError {
    /// The configuration file could not be read.
    #[error("could not read config at {path}: {source}")]
    ConfigRead {
        /// The path that could not be read.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The configuration text was not valid.
    #[error("could not parse config: {0}")]
    ConfigParse(toml::de::Error),

    /// The listen address could not be bound — most often already in use.
    #[error("could not bind {addr}: {source}")]
    Bind {
        /// The address the edge tried to bind.
        addr: SocketAddr,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The server stopped with an error after starting.
    #[error("server error: {0}")]
    Serve(std::io::Error),
}
