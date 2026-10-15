// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's configuration base: what every business area's routes publish through
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4).
//!
//! - [`config_tree`]: the four-level Tenant→Brand→Store→Device tree, its validation, its versions,
//!   and the store it persists through.
//! - [`publish`]: a node written onto a store's layer under a conditional write, with the
//!   prerequisite and settings checks every publish runs; [`setting_refusal`]: the refusals a
//!   settings value earns, and the recorded-confirmation check ([`confirmed_values`]).
//! - [`route`]: what the configuration routes share — the tenant query, the published version, the
//!   unpublished-store and level refusals, a node read as the store runs it, and where a tenant's
//!   stores sit (`TenantLayout`).
//! - [`registry`]: tenants, brands, stores and devices (ADR-0065).
//! - [`settings`]: the values a tenant writes once for many stores (ADR-0160).
//! - [`store_groups`]: store groups, the delivery cohorts a publish and a settings value reach
//!   (ADR-0122), and the batch history.
//! - [`qr_ordering`]: QR ordering's one switch, which every Store-layer publish aligns, and the
//!   one-time migration that settled it ([`data_migrations`]).
//! - [`persistence`]: those stores over PostgreSQL, through `store-postgres`.
//!
//! `pos-cloud` re-exports the modules under their old paths, so `pos_cloud::config_tree::…` and the
//! rest still name them. The routes that compose every area's nodes — store-group batches,
//! releases, the config history — stay in `pos-cloud`, above the areas.

// The record version moved to pos-cloud-api (ADR-0170 decision 4); imported so the moved modules'
// `crate::version::…` paths still resolve.
use pos_cloud_api::version;

pub mod config_tree;
pub mod confirmed_values;
pub mod data_migrations;
pub mod persistence;
pub mod publish;
pub mod qr_ordering;
pub mod registry;
pub mod route;
pub mod setting_refusal;
pub mod settings;
pub mod store_groups;
