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
//! - [`fanout`] — the store-LAN fan-out: one committed change broadcast to every device under 50 ms
//!   ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)).
//! - [`http`] — the axum router: the health probe, the embedded UI, and the `/ws` WebSocket today;
//!   the domain routes as they land.
//! - [`activation`] — the first-boot device activation flow and the boot gate: a generic sub-router
//!   composing [`CloudSync`](pos_ports::CloudSync) and [`KeyVault`](pos_ports::KeyVault), merged into
//!   the app once its concrete adapters are selected
//!   ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
//! - [`server`] — binds the listener and serves with graceful shutdown.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod activation;
pub mod active_config;
pub mod app;
pub mod auth;
pub mod clock;
pub mod config;
pub mod countries;
pub mod discovery;
pub mod error;
pub mod fanout;
pub mod http;
pub mod idgen;
pub mod order_in;
pub mod ota;
pub mod pairing;
pub mod queue;
pub mod receipt;
pub mod server;
pub mod sntp;
pub mod state;
pub mod telemetry;

pub use activation::{activation_router, boot_standing};
pub use active_config::{ActiveConfig, ConfigRejected};
pub use app::{
    AppError, BillView, Edge, EdgeSession, LineDraft, LineView, ShiftView, StoreIdentity, TableView,
};
pub use auth::{Lockout, SignIn};
pub use clock::SystemClock;
pub use config::EdgeConfig;
pub use discovery::{Advertiser, NoopAdvertiser};
pub use error::EdgeError;
pub use fanout::{Fanout, ServerMessage};
pub use idgen::EdgeIdGenerator;
pub use order_in::{EdgeOrderIn, InMemoryIntakeLedger, IntakeLedger};
pub use ota::{InstallError, OtaUpdater, UpdateError, UpdateInstaller, UpdateOutcome, UpdatePlan};
pub use pairing::{Code, DeviceToken, Pairing};
pub use queue::{InMemoryQueueNumbers, QueueNumberAuthority};
pub use receipt::{InMemoryReceipts, ReceiptAuthority};
pub use server::serve;
pub use sntp::{Drift, assess as assess_drift};
pub use state::{AppState, BuildInfo};
