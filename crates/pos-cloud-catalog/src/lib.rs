// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's catalog, with its fees
//! ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4,
//! [ADR-0175](../../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md) decision 2).
//!
//! - [`catalog`] and [`catalog_compiler`]: the item master, menus, taxonomies and layout, and the
//!   compile of a menu to the nodes a store runs.
//! - [`fees`]: fee rules, checked and compiled against the catalog (ADR-0159). The menu publish
//!   republishes fees and a fee rule reads the catalog, so the two are one crate.
//! - [`media`] and [`images`]: item images and their renditions (ADR-0075).
//! - [`tax`]: the tax rates per class and channel (ADR-0074).
//! - [`import`] and [`export`]: the item master as CSV.
//! - [`translations`]: the translation grid (ADR-0043), which the import and export read.
//! - [`routes`]: their routes, which `pos-cloud`'s router mounts.
//! - `persistence`: those seams over PostgreSQL, through `store-postgres`.
//!
//! `pos-cloud` re-exports every module above under its old path (`pos_cloud::catalog`,
//! `pos_cloud::fees`, …), and its `http` module imports each area of [`routes`] under its old name.

// Moved out of pos-cloud with their `crate::` paths (ADR-0170 decision 4); these imports keep them
// resolving.
use pos_cloud_api::{audit, auth, paging, version};
use pos_cloud_config::{config_tree, registry};

pub mod catalog;
pub mod catalog_compiler;
pub mod export;
pub mod fees;
pub mod images;
pub mod import;
pub mod media;
pub mod menu_faults;
mod persistence;
pub mod routes;
pub mod tax;
pub mod translations;
