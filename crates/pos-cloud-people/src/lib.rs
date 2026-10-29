// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's people and access
//! ([ADR-0070](../../../docs/adr/0070-people-and-access.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4,
//! [ADR-0175](../../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md)).
//!
//! - [`people`]: employees, role templates and assignments, and their store seams.
//! - [`people_compiler`]: the compile of a store's people to its `permissions` node.
//! - [`people_readiness`]: whether each role a store runs has someone to fill it.
//! - [`people_republish`]: the queue that republishes a store's people when what it compiles from
//!   changes, and the loop that drains it.
//! - [`routes`]: the people, capability, publish and readiness routes, which `pos-cloud`'s router
//!   mounts.
//! - `persistence`: those seams over PostgreSQL, through `store-postgres`.
//!
//! `pos-cloud` re-exports [`people`], [`people_compiler`], [`people_readiness`] and
//! [`people_republish`] under their old paths, and its `http` module imports [`routes`] as `people`,
//! so `pos_cloud::http::people_router` and the rest keep their paths.

// Moved out of pos-cloud with their `crate::` paths (ADR-0170 decision 4); these imports keep them
// resolving.
use pos_cloud_api::{audit, auth, health, paging, version};
use pos_cloud_config::{config_tree, registry, settings, store_groups};

mod assignment_schema;
pub mod people;
pub mod people_compiler;
pub mod people_readiness;
pub mod people_republish;
mod persistence;
pub mod routes;
