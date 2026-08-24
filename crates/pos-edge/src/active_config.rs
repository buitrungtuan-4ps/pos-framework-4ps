// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The running configuration, hot-swappable with a last-known-good (P5).
//!
//! Configuration is owned by the cloud ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md))
//! and synced to the edge as a [`ConfigSnapshot`]. When a new version arrives, the edge must adopt it
//! **without a restart and in well under a second** — a store cannot down tools to pick up a price
//! change — and it must **never be bricked by a bad one**. This holder is that mechanism: an atomic
//! swap of the active snapshot, with the previous good version retained so a change that turns out
//! wrong can be rolled back one step.
//!
//! Reads are the hot path (a handler reads config to answer a screen), so [`ActiveConfig::current`]
//! takes only a short read lock and clones an [`Arc`] — cloning the pointer, not the document.
//!
//! Validation of a snapshot's *content* against the config schema is P7's (the schema is defined
//! with the cloud config tree); this holder is generic over a validator so the mechanism stands now
//! and the schema slots in later without reshaping callers.

use std::sync::{Arc, PoisonError, RwLock};

use pos_ports::config_store::ConfigSnapshot;
use pos_proto::ids::ConfigVersionId;

/// A configuration change refused because it did not validate. The active config is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRejected {
    /// Why the candidate was refused — for the log and the operator, never the raw document.
    pub reason: String,
}

/// The edge's running configuration and the last version that applied cleanly.
#[derive(Debug)]
pub struct ActiveConfig {
    current: RwLock<Arc<ConfigSnapshot>>,
    last_known_good: RwLock<Arc<ConfigSnapshot>>,
}

impl ActiveConfig {
    /// Starts with `initial` as both the current and the last-known-good configuration.
    #[must_use]
    pub fn new(initial: ConfigSnapshot) -> Self {
        let initial = Arc::new(initial);
        Self {
            current: RwLock::new(Arc::clone(&initial)),
            last_known_good: RwLock::new(initial),
        }
    }

    /// The active configuration. Cheap: clones an `Arc`, holding the read lock only long enough to.
    #[must_use]
    pub fn current(&self) -> Arc<ConfigSnapshot> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// The last configuration that applied cleanly — the rollback target.
    #[must_use]
    pub fn last_known_good(&self) -> Arc<ConfigSnapshot> {
        Arc::clone(
            &self
                .last_known_good
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Applies `candidate` if `validate` accepts it, promoting the outgoing config to last-known-good.
    ///
    /// On success the active config becomes `candidate` and the version it replaced becomes the
    /// rollback target, so [`Self::rollback`] steps back exactly one good version. On rejection the
    /// active config and the last-known-good are both untouched — a bad config cannot brick the store.
    ///
    /// # Errors
    ///
    /// [`ConfigRejected`] carrying the validator's reason, if the candidate does not validate.
    pub fn try_apply<F>(
        &self,
        candidate: ConfigSnapshot,
        validate: F,
    ) -> Result<ConfigVersionId, ConfigRejected>
    where
        F: FnOnce(&ConfigSnapshot) -> Result<(), String>,
    {
        validate(&candidate).map_err(|reason| ConfigRejected { reason })?;
        let version = candidate.config_version_id;
        let outgoing = self.current();
        {
            let mut current = self.current.write().unwrap_or_else(PoisonError::into_inner);
            *current = Arc::new(candidate);
        }
        {
            let mut lkg = self
                .last_known_good
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            *lkg = outgoing;
        }
        Ok(version)
    }

    /// Reverts the active configuration to the last-known-good.
    ///
    /// Returns the version now active. Idempotent: rolling back when the active config already equals
    /// the last-known-good simply keeps it.
    pub fn rollback(&self) -> ConfigVersionId {
        let good = self.last_known_good();
        let version = good.config_version_id;
        let mut current = self.current.write().unwrap_or_else(PoisonError::into_inner);
        *current = good;
        version
    }
}

#[cfg(test)]
mod tests {
    use super::ActiveConfig;
    use pos_ports::config_store::{ConfigDocument, ConfigSnapshot};
    use pos_proto::ids::{ConfigVersionId, StoreId};
    use pos_proto::ulid::Ulid;

    fn snapshot(version: u128) -> ConfigSnapshot {
        let document = serde_json::value::RawValue::from_string(format!("{{\"v\":{version}}}"))
            .expect("valid json");
        ConfigSnapshot {
            config_version_id: ConfigVersionId::new(Ulid::from_u128(version)),
            store_id: StoreId::new(Ulid::from_u128(1)),
            document: ConfigDocument::new(document),
        }
    }

    fn version(n: u128) -> ConfigVersionId {
        ConfigVersionId::new(Ulid::from_u128(n))
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "must match the validator signature try_apply requires"
    )]
    fn accept(_snapshot: &ConfigSnapshot) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn a_valid_config_becomes_active_and_the_old_one_the_rollback_target() {
        let active = ActiveConfig::new(snapshot(1));
        assert_eq!(active.current().config_version_id, version(1));

        let applied = active.try_apply(snapshot(2), accept).expect("valid");
        assert_eq!(applied, version(2));
        assert_eq!(active.current().config_version_id, version(2));
        assert_eq!(active.last_known_good().config_version_id, version(1));
    }

    #[test]
    fn a_rejected_config_changes_nothing() {
        let active = ActiveConfig::new(snapshot(1));
        active.try_apply(snapshot(2), accept).expect("valid");

        let rejected = active.try_apply(snapshot(3), |_| Err("bad shape".to_owned()));
        assert!(rejected.is_err(), "a bad config is refused");
        assert_eq!(
            active.current().config_version_id,
            version(2),
            "the active config is untouched"
        );
        assert_eq!(
            active.last_known_good().config_version_id,
            version(1),
            "the rollback target is untouched"
        );
    }

    #[test]
    fn rollback_steps_back_one_good_version() {
        let active = ActiveConfig::new(snapshot(1));
        active.try_apply(snapshot(2), accept).expect("valid");
        assert_eq!(active.current().config_version_id, version(2));

        let now = active.rollback();
        assert_eq!(now, version(1));
        assert_eq!(active.current().config_version_id, version(1));
    }
}
