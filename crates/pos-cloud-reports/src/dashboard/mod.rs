// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Dashboards answered from **materialised** rollups
//! ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)): [`rollup`], the single fold
//! from events to per-trading-day figures, and [`projection`], which folds each new event once and
//! answers from the stored rollup with no log scan. `pos-cloud`'s projector runs [`project`] for
//! every store.

pub mod from_log;
pub mod projection;
pub mod rollup;

pub use projection::{
    ProjectReport, RollupError, RollupStore, StoredRollups, admitted_devices, dashboard, project,
    revenue, xz_report,
};
pub use rollup::RollupWindow;
