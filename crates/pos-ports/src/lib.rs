// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The sixteen boundaries between the framework and the outside world.
//!
//! Every external system — database, broker, printer, terminal, marketplace, tax
//! authority — is one implementation of one trait defined here. The list is fixed
//! by `docs/adr/0021-corrected-port-list.md`; adding a seventeenth needs an ADR
//! merged first.
//!
//! # Shape
//!
//! Ports are small and role-shaped. An adapter that has to write
//! `unimplemented!()` means the port is wrong, not the adapter
//! (`docs/design-principles.md`, interface segregation).
//!
//! Two of the sixteen are synchronous and are re-exported from `pos-proto`
//! rather than defined here, so that there is exactly one definition of each:
//! `ClockSource` and `IdGenerator`. The other fourteen are asynchronous,
//! declared with native `async fn` in trait — no procedural macro, no boxing on
//! the happy path. Where a family needs runtime selection between several
//! compiled-in adapters, this crate also carries a hand-written object-safe
//! mirror of that trait. See `docs/adr/0013-async-strategy.md`.
//!
//! # `pos-core` does not depend on this crate
//!
//! That is deliberate and it is what makes "the domain performs no I/O" a
//! property of the dependency graph rather than a lint. Do not add the edge.
//!
//! # Contract tests
//!
//! Each port ships a shared test suite that every implementation must pass; that
//! is what makes "swappable" a verified fact rather than a claim. The suites live
//! in `pos-contract-tests`, which is not subject to this crate's dependency
//! allow-list because it needs an executor.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]
