// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's reports ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md),
//! [ADR-0081](../../../docs/adr/0081-reports-and-analytics.md)), moved out of `pos-cloud` for the
//! reporting pilot ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md)
//! decision 4).
//!
//! - [`dashboard`]: the fold from events to per-day rollups, revenue, cash and admitted devices,
//!   its materialised projection, and the reads that answer from it.
//! - [`types`]: what those reads return.
//! - [`export`]: the reports' CSV files.
//! - [`rollups`], [`revenue`], [`xz`]: the routes, which `pos-cloud`'s router mounts with a
//!   [`state::ReportsState`] it builds from its own state.
//! - [`till_names`]: the one thing the reports ask of the cloud's configuration.
//!
//! `pos-cloud` re-exports the dashboard modules, the types and the CSV writers under their old
//! paths. The projector loop that keeps the rollups current stays there, with the task health it
//! reports to.

pub mod dashboard;
pub mod export;
mod persistence;
pub mod refusal;
pub mod revenue;
pub mod rollups;
pub mod state;
pub mod till_names;
pub mod types;
pub mod xz;
