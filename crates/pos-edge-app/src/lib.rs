// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's application layer: load, decide, apply, publish
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4).
//!
//! - [`app`]: `Edge` and every command, with the projection it folds events into. The billing,
//!   shift and floor commands are `impl Edge` blocks, so they live here with `Edge`.
//! - [`auth`] and [`durable_auth`]: sign-in sessions, the lockout and the idle window, and the
//!   device registry they write through to (ADR-0030, ADR-0091).
//! - [`fanout`]: the store-LAN fan-out (ADR-0018).
//! - [`clock`], [`idgen`] and [`sntp`]: the clock, the id generator and the clock's drift check.
//! - [`lease_state`], [`queue`] and [`receipt`]: the store lease, queue numbers and receipt
//!   numbers, with their SQLite impls.
//! - [`line_notes`] and [`sync_status`]: line notes and what the till shows of the cloud link.
//!
//! `pos-edge` re-exports every module under its old path (`pos_edge::app`, `pos_edge::auth`, …),
//! so its routes, tests and `examples/minimal-edge` name them as before. Each module that logs keeps
//! its old `pos_edge::…` `tracing` target through a `LOG_TARGET` constant, so log filters and the
//! durable log's exclusions (ADR-0117 decision 6) see the same targets.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod app;
pub mod auth;
pub mod clock;
pub mod durable_auth;
pub mod fanout;
pub mod idgen;
pub mod lease_state;
pub mod line_notes;
pub mod queue;
pub mod receipt;
pub mod sntp;
pub mod sync_status;
