// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's floor and kitchen
//! ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4,
//! [ADR-0175](../../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md)).
//!
//! - [`floorplan`]: a store's areas, tables, stations and routing rules, and their store seams.
//! - [`floor_compiler`]: the compile of a store's floor to the `floor` and `kitchen` nodes.
//! - [`qr`]: the signed table token and the guardrail decision QR ordering takes (ADR-0057).
//! - [`routes`]: the floor authoring, publish and table-QR routes, which `pos-cloud`'s router
//!   mounts.
//! - `persistence`: those seams over PostgreSQL, through `store-postgres`.
//!
//! `pos-cloud` re-exports [`floorplan`], [`floor_compiler`] and [`qr`] under their old paths, and
//! its `http` module imports [`routes`] as `floor`, so `pos_cloud::http::floor_router` and the rest
//! keep their paths. The guest-facing `POST /v1/qr/orders` endpoint stays in `pos-cloud`.

// Moved out of pos-cloud with their `crate::` paths (ADR-0170 decision 4); these imports keep them
// resolving.
use pos_cloud_api::{audit, auth, version};
use pos_cloud_catalog::catalog;
use pos_cloud_config::{config_tree, registry};

pub mod floor_compiler;
pub mod floorplan;
mod persistence;
pub mod qr;
pub mod routes;
