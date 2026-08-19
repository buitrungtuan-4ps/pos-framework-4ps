// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos_edge` — the store binary's library half.
//!
//! This crate is the thin application layer [ADR-0013](../../../docs/adr/0013-async-strategy.md)
//! calls for: it owns the async runtime, the HTTP surface and the transaction, and it composes the
//! synchronous [`pos_core`] domain with whatever adapters the fork selected. It serves the operator
//! UI to every device on the store LAN and, in later slices, applies each decision inside one
//! transaction and fans the result out over a WebSocket.
//!
//! The [`main`](../pos_edge/index.html) entry point is deliberately tiny; everything testable lives
//! here so the HTTP surface can be exercised without binding a socket (see `tests/http.rs`).
//!
//! # What lives where
//!
//! - [`config`] — the edge's configuration, loaded from disk (with last-known-good retention in a
//!   later slice).
//! - [`telemetry`] — the `tracing` subscriber, which records identifiers and counts but **never PII**
//!   ([`pos_proto::pii`]).
//! - [`http`] — the axum router: the health probe and the embedded UI today, the domain routes and
//!   the WebSocket fan-out as they land ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)).
//! - [`server`] — binds the listener and serves with graceful shutdown.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod config;
pub mod error;
pub mod http;
pub mod server;
pub mod state;
pub mod telemetry;

pub use config::EdgeConfig;
pub use error::EdgeError;
pub use server::serve;
pub use state::{AppState, BuildInfo};
