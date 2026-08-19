// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The domain: every rule that decides what a sale means.
//!
//! Orders, bills, shifts and tables as explicit state machines; money arithmetic;
//! the permission registry; capability flags; pricing and promotions; inventory
//! and availability. `docs/pos-spec.md` is the prose statement of these rules and
//! this crate is their machine-readable twin — CI checks the two agree.
//!
//! # Sans-I/O
//!
//! This crate performs no I/O and awaits nothing. Its shape is
//! `decide(state, command, ctx) -> Result<Decision, DomainError>`: state arrives
//! already loaded, and a `Decision` carries the events to append, the effects to
//! fire after commit, and the next state. The caller — `pos_edge` or `pos_cloud` —
//! owns the transaction.
//!
//! That is why this crate does not depend on `pos-ports`
//! (`docs/adr/0013-async-strategy.md`), and it is what makes the two hard rules
//! enforceable by shape rather than by review: an event cannot be written outside a
//! transaction, because writing is not something the domain can do; and the clock
//! cannot be read twice within one decision, because `now` arrives as a value.
//!
//! # Tests run in milliseconds
//!
//! No database, no network, no hardware, and no async runtime — the tests are
//! ordinary synchronous functions over in-memory fakes. Property tests cover the
//! four data-correctness laws in `docs/pos-spec.md` §14, and the state-machine
//! tables are checked by exhaustive enumeration rather than sampling.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod billing;
pub mod business_date;
pub mod capability;
pub mod error;
pub mod inventory;
pub mod machines;
pub mod permission;
pub mod state_machine;

pub use business_date::{CutoffHour, StoreTimeZone, derive_business_date};
pub use capability::{Capability, CapabilityContext};
pub use error::DomainError;
pub use inventory::{Availability, Recipe, RecipeBook, StockProjection};
pub use permission::{Grant, Permission, PermissionSet, Role, require};
pub use state_machine::{StateMachine, TransitionError};
