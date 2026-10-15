// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The kitchen: firing a line, an order or a course to the stations that make it, bumping a ticket
//! when it is ready, and the live orders the floor and the kitchen display read.

mod board;
mod fire;

// The unit tests name these as `super::`, as they did beside them in `app/mod.rs`.
#[cfg(test)]
use super::{AppError, MAX_COOKING, Projection};
#[cfg(test)]
mod tests;
