// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's inventory: the ingredients, recipes and suppliers a tenant authors, and their publish
//! to each store's `inventory` node ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4).
//!
//! - [`inventory`]: the records, the `InventoryStore` seam, and the compile to the `inventory` node.
//! - [`routes`]: `/admin/inventory/*` and `/admin/config/inventory`, which `pos-cloud`'s router
//!   mounts.
//! - [`persistence`]: the seam over PostgreSQL, through `store-postgres`.
//!
//! `pos-cloud` re-exports [`inventory`] under its old path, and its `http` module imports
//! [`routes`] as `inventory`, so `pos_cloud::http::inventory_router` and the store-group batch's
//! `inventory_nodes` keep their paths.

// Moved out of pos-cloud with their `crate::` paths (ADR-0170 decision 4); these imports keep them
// resolving.
use pos_cloud_api::{audit, auth, version};
use pos_cloud_config::config_tree;

pub mod inventory;
mod persistence;
pub mod routes;
