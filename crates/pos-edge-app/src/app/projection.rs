// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The in-memory projection ([`super::Projection`]): what it records of tables and orders, lines,
//! bills and shifts as events fold into it, rebuilding it from the log at boot, and the snapshots
//! and bill inputs the commands read from it.

mod bills;
mod fold;
mod lines;
mod orders;
mod shifts;
mod snapshots;

// The unit tests name these as `super::`, as they did beside them in `app/mod.rs`.
#[cfg(test)]
use super::{OrderLineId, Projection};
#[cfg(test)]
mod tests;
