// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's reporting routes: the daily rollups, revenue with its fee and tax exports, and the
//! X/Z report ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md),
//! [ADR-0081](../../../../docs/adr/0081-reports-and-analytics.md)).
//!
//! Handlers only. Each reads the materialised rollup through [`crate::dashboard`], which folds and
//! answers; the CSV files are written by [`crate::export`], and the report types live in
//! [`crate::cloud`]. [`crate::http::router`] mounts the routes.
//!
//! - [`rollups`]: the `/v1` and `/admin` daily rollup reads, their CSV export, and the reset.
//! - [`revenue`]: the revenue read and its three CSV exports (totals, fees by code, tax by
//!   component).
//! - [`xz`]: the X or Z report for one trading day.

pub(crate) mod revenue;
pub(crate) mod rollups;
pub(crate) mod xz;
