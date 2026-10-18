// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cash shift and its drawer: opening, counting and closing a shift, the cash paid in and out
//! and the drawer opened without a sale, the shift a device reads, and the drawer an act is on.

mod cash;
mod close;
mod count;
mod drawer_access;
mod drawers;
mod open;
mod reads;

// The unit tests name these as `super::`, as they did beside them in `app/mod.rs`.
#[cfg(test)]
use super::{AppError, Projection, ShiftRecord};
#[cfg(test)]
mod tests;
