// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The four-level configuration tree: compose, version, validate, and decide what to send a store.
//!
//! [`ConfigTree`] holds one store's four layers (Tenant → Brand → Store → Device) and the history of
//! effective documents it has published. Publishing a change composes the layers, validates the
//! result, and — only if it validates — appends a new version; a rejected version changes nothing,
//! so the last good version stays current ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md),
//! [ADR-0033](../../../docs/adr/0033-config-tree.md)). [`ConfigTree::update_for`] then answers a
//! store's sync: a delta when the store is close behind, a full snapshot when it is more than *K*
//! versions behind or holding a version the cloud no longer has.

use serde_json::Value;

use pos_ports::config_store::{ConfigDelta, ConfigDocument, ConfigSnapshot, ConfigUpdate};
use pos_proto::ids::{ConfigVersionId, StoreId};

use super::merge::{diff, merge_layers};
use super::validate::ConfigValidator;

/// How many versions a store may be behind before it gets a full snapshot rather than a delta
/// ([ADR-0033](../../../docs/adr/0033-config-tree.md)). The cloud keeps at least this much history to
/// diff against; a store older than it cannot be patched and is resynced whole.
pub const DEFAULT_K: usize = 20;

/// The four configuration levels, least specific first. A more-specific level overrides a
/// less-specific one ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLevel {
    /// The whole tenant.
    Tenant,
    /// A brand within the tenant.
    Brand,
    /// A store within the brand.
    Store,
    /// A single device within the store.
    Device,
}

impl ConfigLevel {
    /// The levels in override order, least specific first.
    pub const ORDER: [Self; 4] = [Self::Tenant, Self::Brand, Self::Store, Self::Device];

    /// This level's index into the layer array.
    const fn index(self) -> usize {
        match self {
            Self::Tenant => 0,
            Self::Brand => 1,
            Self::Store => 2,
            Self::Device => 3,
        }
    }
}

/// Why a publish was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The composed document failed validation; the listed violations must be fixed. Nothing was
    /// published, so the last good version is still current.
    #[error("the configuration is invalid: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

/// What a store should be sent to reach the current version.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncOutcome {
    /// The store already holds the current version; send nothing.
    UpToDate,
    /// The store should apply this update.
    Deliver(ConfigUpdate),
}

/// One published version: its id and the effective document it resolves to.
#[derive(Debug, Clone)]
struct Version {
    id: ConfigVersionId,
    effective: Value,
}

/// A store's configuration authority: its four layers, its published history, and its validator.
#[derive(Debug)]
pub struct ConfigTree<V> {
    store_id: StoreId,
    layers: [Value; 4],
    history: Vec<Version>,
    validator: V,
    k: usize,
}

impl<V: ConfigValidator> ConfigTree<V> {
    /// A fresh tree for `store_id` with empty layers and no published version.
    #[must_use]
    pub fn new(store_id: StoreId, validator: V) -> Self {
        Self {
            store_id,
            layers: [
                Value::Object(serde_json::Map::new()),
                Value::Object(serde_json::Map::new()),
                Value::Object(serde_json::Map::new()),
                Value::Object(serde_json::Map::new()),
            ],
            history: Vec::new(),
            validator,
            k: DEFAULT_K,
        }
    }

    /// Overrides the snapshot-fallback threshold *K*.
    #[must_use]
    pub fn with_k(mut self, k: usize) -> Self {
        self.k = k;
        self
    }

    /// Replaces one level's document and publishes the resulting version under `version_id`.
    ///
    /// Composes the four layers with `level` replaced, validates the effective document, and only if
    /// it validates commits the layer and appends the version. A rejected version leaves the tree
    /// exactly as it was — the last good version stays current.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Invalid`] with every violation if the composed document does not validate.
    pub fn publish(
        &mut self,
        level: ConfigLevel,
        document: Value,
        version_id: ConfigVersionId,
    ) -> Result<ConfigVersionId, ConfigError> {
        let mut refs: [&Value; 4] = [
            &self.layers[0],
            &self.layers[1],
            &self.layers[2],
            &self.layers[3],
        ];
        refs[level.index()] = &document;
        let effective = merge_layers(&refs);

        if let Err(violations) = self.validator.validate(&effective) {
            return Err(ConfigError::Invalid(violations));
        }

        self.layers[level.index()] = document;
        self.history.push(Version {
            id: version_id,
            effective,
        });
        Ok(version_id)
    }

    /// The current (latest published) version id, or `None` before the first publish.
    #[must_use]
    pub fn current_version(&self) -> Option<ConfigVersionId> {
        self.history.last().map(|version| version.id)
    }

    /// The effective document of a published version, for the admin view and for tests.
    #[must_use]
    pub fn effective_at(&self, version_id: ConfigVersionId) -> Option<&Value> {
        self.history
            .iter()
            .find(|version| version.id == version_id)
            .map(|version| &version.effective)
    }

    /// The current effective document — always a validated one, so it is also the last-known-good.
    #[must_use]
    pub fn current_effective(&self) -> Option<&Value> {
        self.history.last().map(|version| &version.effective)
    }

    /// Decides what to send a store that reports holding `held` (or `None` if it has never synced).
    ///
    /// A snapshot when the store has nothing, holds an unknown version, or is more than *K* versions
    /// behind; a delta otherwise; nothing when it is already current.
    #[must_use]
    pub fn update_for(&self, held: Option<ConfigVersionId>) -> SyncOutcome {
        let Some(current) = self.history.last() else {
            return SyncOutcome::UpToDate; // Nothing published yet.
        };

        let held = match held {
            None => return SyncOutcome::Deliver(self.snapshot(current)),
            Some(held) if held == current.id => return SyncOutcome::UpToDate,
            Some(held) => held,
        };

        // The store is behind. A delta is possible only if we still hold the version it has, and it
        // is within K of current; otherwise resync it whole.
        match self.position(held) {
            Some(index) if self.behind(index) <= self.k => {
                let from_effective = self
                    .history
                    .get(index)
                    .map_or(&Value::Null, |version| &version.effective);
                let patch = diff(from_effective, &current.effective);
                SyncOutcome::Deliver(ConfigUpdate::Delta(ConfigDelta {
                    from_config_version_id: held,
                    to_config_version_id: current.id,
                    store_id: self.store_id,
                    patch: to_document(&patch),
                }))
            }
            _ => SyncOutcome::Deliver(self.snapshot(current)),
        }
    }

    /// A full snapshot of `current`.
    fn snapshot(&self, current: &Version) -> ConfigUpdate {
        ConfigUpdate::Snapshot(ConfigSnapshot {
            config_version_id: current.id,
            store_id: self.store_id,
            document: to_document(&current.effective),
        })
    }

    /// The index of a held version in the history, if still retained.
    fn position(&self, held: ConfigVersionId) -> Option<usize> {
        self.history.iter().position(|version| version.id == held)
    }

    /// How many versions behind current an index sits.
    fn behind(&self, index: usize) -> usize {
        self.history.len().saturating_sub(1).saturating_sub(index)
    }
}

/// Serializes a [`Value`] into a [`ConfigDocument`]. A `Value` always serializes, so the error path
/// is unreachable; it degrades to an empty object rather than panicking.
fn to_document(value: &Value) -> ConfigDocument {
    match serde_json::value::to_raw_value(value) {
        Ok(raw) => ConfigDocument::new(raw),
        Err(_) => ConfigDocument::new(
            serde_json::value::to_raw_value(&Value::Object(serde_json::Map::new()))
                .unwrap_or_else(|_| unreachable!("an empty object always serializes")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigError, ConfigLevel, ConfigTree, SyncOutcome};

    use serde_json::{Value, json};

    use pos_ports::config_store::ConfigUpdate;
    use pos_proto::ids::{ConfigVersionId, StoreId};
    use pos_proto::ulid::Ulid;

    use crate::config_tree::merge::apply_merge_patch;
    use crate::config_tree::validate::{CapabilityValidator, StructuralValidator};

    fn store_id() -> StoreId {
        StoreId::new(Ulid::from_u128(0x5709))
    }

    fn version(n: u128) -> ConfigVersionId {
        ConfigVersionId::new(Ulid::from_u128(n))
    }

    fn document(update: &ConfigUpdate) -> Value {
        let raw = match update {
            ConfigUpdate::Snapshot(snapshot) => snapshot.document.as_json(),
            ConfigUpdate::Delta(delta) => delta.patch.as_json(),
        };
        serde_json::from_str(raw).expect("valid json document")
    }

    #[test]
    fn publishing_composes_the_layers_into_the_effective_document() {
        let mut tree = ConfigTree::new(store_id(), StructuralValidator);
        tree.publish(
            ConfigLevel::Tenant,
            json!({"currency": "VND", "tips_enabled": false}),
            version(1),
        )
        .expect("tenant publishes");
        tree.publish(
            ConfigLevel::Store,
            json!({"tips_enabled": true}),
            version(2),
        )
        .expect("store publishes");

        assert_eq!(
            tree.current_effective(),
            Some(&json!({"currency": "VND", "tips_enabled": true})),
            "the store level overrode the tenant level"
        );
        assert_eq!(tree.current_version(), Some(version(2)));
    }

    #[test]
    fn an_invalid_version_is_rejected_and_the_last_good_stays_current() {
        let mut tree = ConfigTree::new(store_id(), CapabilityValidator);
        tree.publish(
            ConfigLevel::Store,
            json!({"pay_first_enabled": true, "tables_enabled": false}),
            version(1),
        )
        .expect("a coherent version publishes");

        // Now try to turn on an incompatible pair.
        let rejected = tree.publish(
            ConfigLevel::Store,
            json!({"pay_first_enabled": true, "tables_enabled": true}),
            version(2),
        );
        assert!(matches!(rejected, Err(ConfigError::Invalid(_))));
        assert_eq!(
            tree.current_version(),
            Some(version(1)),
            "the rejected version did not become current; the last good one stayed"
        );
    }

    #[test]
    fn a_store_with_nothing_gets_a_snapshot() {
        let mut tree = ConfigTree::new(store_id(), StructuralValidator);
        tree.publish(ConfigLevel::Tenant, json!({"a": 1}), version(1))
            .expect("publish");
        match tree.update_for(None) {
            SyncOutcome::Deliver(ConfigUpdate::Snapshot(snapshot)) => {
                assert_eq!(snapshot.config_version_id, version(1));
            }
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    #[test]
    fn a_current_store_is_told_it_is_up_to_date() {
        let mut tree = ConfigTree::new(store_id(), StructuralValidator);
        tree.publish(ConfigLevel::Tenant, json!({"a": 1}), version(1))
            .expect("publish");
        assert_eq!(tree.update_for(Some(version(1))), SyncOutcome::UpToDate);
    }

    #[test]
    fn a_store_a_few_versions_behind_gets_a_delta_that_reproduces_current() {
        let mut tree = ConfigTree::new(store_id(), StructuralValidator);
        tree.publish(ConfigLevel::Tenant, json!({"a": 1, "b": 2}), version(1))
            .expect("v1");
        tree.publish(ConfigLevel::Store, json!({"b": 20}), version(2))
            .expect("v2");
        tree.publish(ConfigLevel::Store, json!({"b": 20, "c": 3}), version(3))
            .expect("v3");

        match tree.update_for(Some(version(1))) {
            SyncOutcome::Deliver(update @ ConfigUpdate::Delta(_)) => {
                let ConfigUpdate::Delta(ref delta) = update else {
                    unreachable!()
                };
                assert_eq!(delta.from_config_version_id, version(1));
                assert_eq!(delta.to_config_version_id, version(3));
                // Applying the delta to v1's effective document must reproduce v3's exactly.
                let mut held = tree.effective_at(version(1)).expect("v1 retained").clone();
                apply_merge_patch(&mut held, &document(&update));
                assert_eq!(held, *tree.effective_at(version(3)).expect("v3"));
            }
            other => panic!("expected a delta, got {other:?}"),
        }
    }

    #[test]
    fn a_store_more_than_k_versions_behind_gets_a_snapshot() {
        let mut tree = ConfigTree::new(store_id(), StructuralValidator).with_k(2);
        for n in 1..=5 {
            tree.publish(ConfigLevel::Store, json!({"n": n}), version(n))
                .expect("publish");
        }
        // Holding v1 while current is v5 is 4 behind, past K=2.
        match tree.update_for(Some(version(1))) {
            SyncOutcome::Deliver(ConfigUpdate::Snapshot(snapshot)) => {
                assert_eq!(snapshot.config_version_id, version(5));
            }
            other => panic!("expected a snapshot past K, got {other:?}"),
        }
        // But holding v4 is only 1 behind, within K=2, so a delta.
        assert!(matches!(
            tree.update_for(Some(version(4))),
            SyncOutcome::Deliver(ConfigUpdate::Delta(_))
        ));
    }

    #[test]
    fn a_store_holding_an_unknown_version_gets_a_snapshot() {
        let mut tree = ConfigTree::new(store_id(), StructuralValidator);
        tree.publish(ConfigLevel::Tenant, json!({"a": 1}), version(1))
            .expect("publish");
        // Version 999 was never published (or has been pruned): resync whole.
        assert!(matches!(
            tree.update_for(Some(version(999))),
            SyncOutcome::Deliver(ConfigUpdate::Snapshot(_))
        ));
    }
}
