// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `tracing` subscriber.
//!
//! # The one rule
//!
//! Store logs travel to the cloud for the remote tail
//! ([`docs/architecture.md`](../../../docs/architecture.md)), so a log line is data that leaves the
//! store. It therefore records **identifiers and counts, never PII** — no guest name, phone, address
//! or card detail ever enters a span or an event. [`pos_proto::pii`] marks the values this rule is
//! about; the discipline here is to log the id, not the person. The workspace already forbids
//! `println!`/`eprintln!` in this crate, so `tracing` is the only way out and this is the only place
//! it is configured.

use tracing_subscriber::EnvFilter;

/// Installs the process-wide log subscriber, once.
///
/// Reads the filter from `RUST_LOG`, defaulting to `info`. Calling it more than once (as a test that
/// spins up several servers might) is harmless: the second install fails quietly and the first
/// subscriber stands.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // `try_init` returns Err if a global subscriber is already set. That is the idempotent case, not
    // a failure worth surfacing, so it is discarded deliberately.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
