// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The four-level configuration tree the cloud owns and publishes (P7,
//! [ADR-0033](../../../docs/adr/0033-config-tree.md)).
//!
//! Configuration is cloud-owned ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)):
//! the cloud composes a store's effective settings from four levels — Tenant → Brand → Store →
//! Device, most-specific winning — validates the result, versions it, and hands each store either a
//! small delta or, when it has fallen too far behind, a full snapshot. Four decisions live here:
//!
//!  * **Composition** ([`merge`]): a deep merge of the four layers into one effective document.
//!  * **The delta format** ([`merge`]): RFC 7386 JSON Merge Patch, so a delta both patches forward
//!    and can *delete* a key, and `diff` then `apply` round-trips.
//!  * **Validation** ([`validate`]): a version is checked — including `pos-core`'s §10 inter-flag
//!    capability rules — before it is published, so a store never receives an incoherent one, and a
//!    rejected version leaves the last good one current.
//!  * **Snapshot vs delta** ([`tree`]): keyed on *K* — a store within *K* versions of current gets a
//!    delta; one further behind, or holding a version the cloud no longer retains, gets a snapshot.
//!
//! Pure and I/O-free: it produces the [`ConfigUpdate`](pos_ports::config_store::ConfigUpdate) values
//! the `ConfigStore` port carries, but does not persist or publish them — that, and the admin routes,
//! are a later slice.

pub mod merge;
pub mod tree;
pub mod validate;

pub use tree::{ConfigError, ConfigLevel, ConfigTree, DEFAULT_K, SyncOutcome};
pub use validate::{CapabilityValidator, ConfigValidator, StructuralValidator};
