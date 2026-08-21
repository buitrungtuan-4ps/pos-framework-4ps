// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The virtual-fleet simulator (`docs/roadmap.md` P12).
//!
//! P12 turns the sizing numbers in
//! [`capacity-and-reliability.md`](../../../docs/capacity-and-reliability.md) from *estimates* into
//! something executable and self-checking, and drives a virtual fleet through the framework's pure
//! decisions so a ring rollout, an offline drain, and a nightly reconciliation are exercised with no
//! hardware and no clock.
//!
//! # What is here, and what is deliberately not
//!
//! - [`capacity`] is the **executable capacity model**: the three published scenarios (A, B, C) as
//!   data, the §2 sizing formulas as pure integer functions, and a [`capacity::reconcile`] that checks
//!   each derived quantity against the published table and *returns* the divergences rather than hiding
//!   them. This is what makes the tables a checked artifact — a formula that stops matching the table
//!   fails a test, and the one place the published estimates do not reconcile is pinned rather than
//!   quietly averaged away.
//! - [`fleet`] drives the **fleet-scale OTA rollout** over the framework's real decision,
//!   [`pos_core::ota::decide_rollout`] — the canary ramp, the kill switch, a revoked key, and a failed
//!   self-test rolling back, each asserted across a whole fleet, not one device.
//! - [`stress`] makes the §4 behavioural stress tests executable: the offline-drain time model, the
//!   webhook-cursor backpressure (a dead endpoint falls behind without growing in memory), and the
//!   nightly reconciliation missing-id diff.
//! - The **real sustained soak on the target hardware** — 222 events/s against a live PostgreSQL with
//!   `NVMe` `fsync` the deciding factor, run for hours without leaking — is *not* here and is not faked:
//!   it needs the VPS and the wall-clock time, so it is an operations/P13 handoff, the same way the
//!   WAL-on-Windows spike (roadmap A4) and the hardware matrix (A5) are. This crate is the harness that
//!   soak plugs into; it does not pretend to be the measurement.
//!
//! Everything here is deterministic: time, rates, and counts are inputs, never read from a clock
//! (`clippy.toml` disallows `SystemTime::now`/`Instant::now`), and every quantity is integer — there is
//! no floating point on a capacity number, exactly as there is none on a money amount.

#![forbid(unsafe_code)]

pub mod capacity;
pub mod fleet;
pub mod stress;
