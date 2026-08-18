// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! In-memory implementations of all sixteen ports.
//!
//! # What this crate is for
//!
//! Two things, and the second is the important one.
//!
//! It lets `pos-core`'s suite run with no database, no network and no hardware, which is what makes
//! it finish in milliseconds.
//!
//! And it is the first implementation to pass every contract suite. `docs/architecture.md` §5 says
//! the suites are what make *swappable* a verified fact — but there is a circularity in that claim
//! if the fakes are exempt: the domain suite runs against the fakes, so a fake that disagrees with
//! the real store makes every domain test a test of the wrong thing. So these are held to exactly
//! the same suites as `store-sqlite` will be. `tests/contract.rs` runs all sixteen.
//!
//! # Fixed capacities, on purpose
//!
//! `docs/capacity-and-reliability.md` describes back-pressure as the mechanism that keeps the
//! system bounded, and back-pressure is not testable against an unbounded fake. So the queues here
//! have limits and return
//! [`RESOURCE_EXHAUSTED`](pos_ports::PortError::resource_exhausted) at them — which also means the
//! contract cases that check back-pressure are checking something real.
//!
//! # Not a mock
//!
//! Nothing here records calls for a test to assert on, and there is no `expect_called_once`. A fake
//! is a working implementation with a simpler substrate; a mock is a recording of an interaction.
//! The distinction matters because the suites assert on *behaviour*, and behaviour is what a mock
//! cannot provide.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod determinism;
pub mod devices;
pub mod executor;
pub mod harness;
pub mod infra;
pub mod store;
pub mod vendors;

use std::sync::{Mutex, MutexGuard, PoisonError};

pub use determinism::{FakeClock, FakeIdGenerator};
pub use devices::{FakePaymentTerminal, FakePrinter};
pub use executor::run_ready;
pub use infra::{FakeBlobStore, FakeKeyVault, FakeLink, FakeMetricsSink, FakeSigner};
pub use store::{FakeStore, FakeTx};
pub use vendors::{FakeDeliveryVendor, FakeErp, FakeFiscal, FakeIntake, FakeShipping};

/// Locks a mutex, recovering from poisoning instead of panicking.
///
/// A mutex is poisoned when a thread panicked while holding it. Every caller here would then
/// `unwrap` and panic in turn — but `unwrap_used` and `panic` are denied across this workspace, and
/// more to the point a panicking cascade turns one failed test into an unreadable run. Recovering
/// the guard keeps the data (which is still structurally sound; only the invariant the panicking
/// thread was mid-way through establishing is suspect) and lets the original failure be the one
/// reported.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
