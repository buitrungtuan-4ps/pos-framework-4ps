// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's rules module: `pos-rules` and `pos-core`'s flow engine, built for
//! `wasm32-unknown-unknown` and loaded once by the till at boot, never on a tap
//! ([ADR-0193](../../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md) decision 2,
//! [ADR-0202](../../../docs/adr/0202-cel-as-the-crate-and-the-spike-found-it.md) decision 1).
//!
//! One engine for the edge and the till: the till asks the same `available_steps` and
//! `missing_prerequisite` the edge's re-check is built on
//! ([ADR-0192](../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)), over
//! conditions compiled by the same `pos-rules`.
//!
//! # The boundary
//!
//! A request is JSON, and so is the answer ([`engine`]). The module exports three functions and
//! no more ([`abi`]): one that reserves the input buffer and says where it is, one that answers the
//! request written there and returns the answer's length, and one that says where the answer is.
//! The till copies bytes in and out through the module's memory. No pointer crosses the boundary
//! into Rust, so the only `unsafe` is the attribute that exports a name, fenced in [`abi`].
//!
//! Every error is an answer, `{"ok":false,"error":…}`: a malformed or oversized request, an
//! unknown flow, a fact with a fraction. The release build aborts on a panic, so no path here may
//! panic on any input.

pub mod abi;
pub mod engine;
#[cfg(test)]
mod tests;

pub use engine::{Engine, REQUEST_BYTES_MAX};
