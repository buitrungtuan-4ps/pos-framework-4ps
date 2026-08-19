// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The shared state every request handler is given.
//!
//! axum hands each handler a clone of this, so it is deliberately cheap to clone: the configuration
//! sits behind an [`Arc`], and [`BuildInfo`] is a handful of `&'static str`. As the domain routes and
//! the WebSocket fan-out land ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)), the
//! composed ports and the broadcast sender join this struct — each behind an `Arc` or an owned handle
//! that is cheap to clone, so the shape stays the same.

use std::sync::Arc;

use crate::clock::SystemClock;
use crate::config::EdgeConfig;
use crate::fanout::Fanout;
use crate::pairing::Pairing;

/// Identity the health probe reports: what this binary is and which protocol it speaks. All of it is
/// compile-time constant, none of it is PII.
#[derive(Debug, Clone, Copy)]
pub struct BuildInfo {
    /// The package name, from `CARGO_PKG_NAME`.
    pub service: &'static str,
    /// The package version, from `CARGO_PKG_VERSION`.
    pub version: &'static str,
    /// The cloud–edge wire protocol version this binary speaks
    /// ([`pos_proto::PROTOCOL_VERSION`]).
    pub protocol_version: u32,
}

impl BuildInfo {
    /// The build information for this compilation.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            service: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            protocol_version: pos_proto::PROTOCOL_VERSION,
        }
    }
}

impl Default for BuildInfo {
    fn default() -> Self {
        Self::current()
    }
}

/// The state shared across all request handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The bootstrap configuration, shared read-only.
    pub config: Arc<EdgeConfig>,
    /// What this binary is, for the health probe.
    pub build: BuildInfo,
    /// The store-LAN fan-out every WebSocket subscribes to and every applied decision publishes to.
    pub fanout: Fanout,
    /// The one sanctioned clock (ADR-0030 needs it for pairing-code expiry; everything time-related
    /// reads it).
    pub clock: SystemClock,
    /// Device pairing state — the live codes and issued device tokens (ADR-0030).
    pub pairing: Arc<Pairing>,
}

impl AppState {
    /// Builds the shared state from a loaded configuration, with a fresh fan-out and pairing state.
    #[must_use]
    pub fn new(config: EdgeConfig) -> Self {
        Self::with_fanout(config, Fanout::new())
    }

    /// Builds the shared state over an existing fan-out.
    ///
    /// The composed edge shares one fan-out between the application loop (which publishes) and the
    /// `/ws` route (which subscribes), so a device sees a committed change; this constructor is how
    /// that one channel reaches both.
    #[must_use]
    pub fn with_fanout(config: EdgeConfig, fanout: Fanout) -> Self {
        Self {
            config: Arc::new(config),
            build: BuildInfo::current(),
            fanout,
            clock: SystemClock,
            pairing: Arc::new(Pairing::new()),
        }
    }
}
