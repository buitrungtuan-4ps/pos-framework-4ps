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
//! The engine ([`merge`], [`tree`], [`validate`]) is pure and I/O-free: it produces the
//! [`ConfigUpdate`](pos_ports::config_store::ConfigUpdate) values the `ConfigStore` port carries but
//! does not itself perform I/O. [`store`] is the persistence seam over it — a store's whole tree
//! ([`ConfigTreeState`]) round-trips through one JSON document per `(tenant, store)`, so the tree
//! survives a restart with its last good version still current. The admin routes that author layers
//! are a later slice.

pub mod merge;
pub mod store;
pub mod tree;
pub mod validate;

pub use store::{ConfigStoreError, ConfigTreeStore};
pub use tree::{
    ConfigError, ConfigLevel, ConfigTree, ConfigTreeState, DEFAULT_K, PublishedVersion, SyncOutcome,
};
pub use validate::{CapabilityValidator, ConfigValidator, StructuralValidator};

/// What a node needs the store to already hold before it can be published — [ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §7.
///
/// One table, read by every publish path. The batch path had these rules from the day store groups
/// landed; the single-store path did not, so the same menu that a batch would *skip* for want of a
/// tax table published happily from the Menus screen and produced a shop that boots, syncs, shows
/// the menu, takes the order, and raises `TaxRateNotConfigured` at the payment screen (finding
/// **F7**). Two paths with two answers is the bug; one table is the fix.
///
/// A release is the third reader, and the one that made the table's *order* matter as well as its
/// contents ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §6): a
/// release carrying both `tax` and `menu` satisfies its own prerequisite only if the activator
/// applies them in dependency order. It lives here rather than beside any one of its readers so
/// that adding a rule cannot reach two of the three.
///
/// §7 seeds it with exactly the rules below and says the rest are added as nodes acquire
/// dependencies.
pub const NODE_PREREQUISITES: &[(&str, &[&str])] = &[("menu", &["tax", "locale"])];

/// What `node` must already be published before it can be, or an empty slice.
#[must_use]
pub fn prerequisites_for(node: &str) -> &'static [&'static str] {
    NODE_PREREQUISITES
        .iter()
        .find(|(key, _)| *key == node)
        .map_or(&[], |(_, needs)| *needs)
}
