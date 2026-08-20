// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Dashboards answered from **materialised** rollups (P7 exit,
//! [ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
//!
//! The event log is the source of truth, but scanning it on every dashboard view is O(events) and
//! cannot meet the P7 exit criterion of a sub-10 ms answer. So the rollup is maintained as a
//! materialised read model:
//!
//!  * [`rollup`] — the single fold from events to per-trading-day counts, shared with
//!    [`Cloud::daily_rollups`](crate::cloud::Cloud::daily_rollups) so the materialised and from-log
//!    answers cannot diverge.
//!  * [`projection`] — [`project`] folds each new event once (cursor-advanced, idempotent,
//!    rebuildable by resetting the cursor), and [`dashboard`] answers from the stored rollup with no
//!    log scan.
//!  * [`projector`] — the background loop around [`project`]: on each tick it lists the fleet
//!    ([`StoreCatalog`]) and projects every store, so the materialised rollup stays current. The one
//!    writer of the rollup table.
//!
//! Pure and I/O-free behind the [`RollupStore`] and [`StoreCatalog`] seams; `store-postgres` provides
//! the rollup table and the fleet listing, and [`crate::http`] points `/v1` at [`dashboard`].

pub mod projection;
pub mod projector;
pub mod rollup;

pub use projection::{ProjectReport, RollupError, RollupStore, StoredRollups, dashboard, project};
pub use projector::{FleetReport, StoreCatalog, project_fleet};
