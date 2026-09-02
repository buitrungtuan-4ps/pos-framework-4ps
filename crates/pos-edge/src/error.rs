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

    /// The configuration parsed but a value would misbehave, so the edge refuses to start with it.
    #[error("invalid config: {0}")]
    Config(String),

    /// The durable device registry could not be read at boot (ADR-0091).
    ///
    /// Fatal on purpose: starting with an empty pairing table would silently unpair a store that is
    /// in fact paired, and an operator would then re-pair every till to fix a problem that was
    /// never theirs.
    #[error("could not load the device registry: {0}")]
    DeviceRegistry(pos_ports::error::PortError),

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

    /// The compiled-in country modules are inconsistent — two claim the same country — so the build
    /// must not start ([ADR-0027](../../../docs/adr/0027-country-modules.md)).
    #[error("country modules are inconsistent: {0}")]
    Country(pos_country::RegistryError),

    /// The event store could not be opened at start-up.
    #[error("could not open the store: {0}")]
    Store(pos_ports::PortError),

    /// The OS entropy source needed to seed the id generator was unavailable.
    #[error("entropy source unavailable: {0}")]
    Entropy(getrandom::Error),

    /// The projection could not be rebuilt from the event log at start-up — the log was unreadable
    /// or an event would not decode (a corrupt log).
    #[error("could not rebuild the projection from the log: {0}")]
    Rebuild(crate::app::AppError),
}
