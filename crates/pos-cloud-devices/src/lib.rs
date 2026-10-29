// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's devices: how a box first earns a credential, and the devices a store runs
//! ([ADR-0041](../../../docs/adr/0041-device-onboarding.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4).
//!
//! - [`activation`]: the once-only activation-code exchange (ADR-0050).
//! - [`claim`]: claiming a box that was installed with no store (ADR-0148).
//! - [`devices`]: the device proposals a store makes and the console approves, and the
//!   `DeviceProposalStore` seam.
//! - [`routes`]: the activation, claim, onboarding, printer, terminal, publish and revoke routes,
//!   which `pos-cloud`'s router mounts.
//! - `persistence`: those seams over PostgreSQL, through `store-postgres`.
//!
//! `pos-cloud` re-exports [`activation`], [`claim`] and [`devices`] under their old paths, and its
//! `http` module imports [`routes`] as `devices`, so `pos_cloud::http::device_router` and the rest
//! keep their paths. The admitted-device roster, which reads the reports' rollups through the
//! application state, stays in `pos-cloud`.

// Moved out of pos-cloud with their `crate::` paths (ADR-0170 decision 4); these imports keep them
// resolving.
use pos_cloud_api::{audit, auth};
use pos_cloud_config::config_tree;

pub mod activation;
pub mod claim;
pub mod devices;
mod persistence;
pub mod routes;
