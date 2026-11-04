// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The wake the cloud sends a store when a publish changed its configuration
//! ([ADR-0195](../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)).
//!
//! A wake only triggers the pull the store already runs; it carries no configuration. It names the
//! version the publish produced so a store that already holds it does nothing, and nothing else: no
//! tenant, no content, no personal data, because every store on the bus can read every subject
//! until per-store permissions land (ADR-0195 decision 5).
//!
//! No `deny_unknown_fields`: an older edge reads a wake that carries a field it does not know.

use serde::{Deserialize, Serialize};

use crate::ids::{ConfigVersionId, StoreId};

/// The first part of every wake subject, shared with nothing else on the bus.
pub const CONFIG_WAKE_SUBJECT_PREFIX: &str = "pos.store.";

/// The last part of a wake subject.
pub const CONFIG_WAKE_SUBJECT_SUFFIX: &str = ".config";

/// The largest wake payload a receiver reads, in bytes. A wake is one id; anything larger is not
/// one.
pub const CONFIG_WAKE_BYTES_MAX: usize = 1024;

/// A publish changed this store's configuration to `config_version_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigWake {
    /// The version the publish produced. A store holding it already does nothing.
    pub config_version_id: ConfigVersionId,
}

impl ConfigWake {
    /// The core NATS subject wakes for `store_id` travel on: `pos.store.<store_id>.config`. Not a
    /// JetStream subject; no stream captures it (ADR-0195 decision 1).
    #[must_use]
    pub fn subject(store_id: StoreId) -> String {
        format!("{CONFIG_WAKE_SUBJECT_PREFIX}{store_id}{CONFIG_WAKE_SUBJECT_SUFFIX}")
    }

    /// Reads a wake off the wire, or `None` for anything that is not one: too large, not JSON, or
    /// missing its version. A receiver treats `None` as a wake that names no version, which still
    /// pulls, because a pull is cheap and the poll would deliver the same thing.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() > CONFIG_WAKE_BYTES_MAX {
            return None;
        }
        serde_json::from_slice(payload).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{CONFIG_WAKE_BYTES_MAX, ConfigWake};
    use crate::ids::{ConfigVersionId, StoreId};
    use crate::ulid::Ulid;

    #[test]
    fn a_wake_names_only_the_version_and_travels_on_its_stores_subject() {
        let wake = ConfigWake {
            config_version_id: ConfigVersionId::new(Ulid::from_u128(9)),
        };
        let wire = serde_json::to_value(&wake).expect("serialise");
        assert_eq!(
            wire,
            serde_json::json!({ "config_version_id": Ulid::from_u128(9).to_string() }),
            "one field: no tenant, no content"
        );
        let store = StoreId::new(Ulid::from_u128(7));
        assert_eq!(
            ConfigWake::subject(store),
            format!("pos.store.{store}.config")
        );
    }

    #[test]
    fn decode_reads_a_wake_and_refuses_anything_else() {
        let id = Ulid::from_u128(9).to_string();
        let wake = format!(r#"{{"config_version_id":"{id}","later_field":1}}"#);
        assert_eq!(
            ConfigWake::decode(wake.as_bytes()).map(|w| w.config_version_id.to_string()),
            Some(id),
            "a field from a later release is ignored"
        );
        assert_eq!(ConfigWake::decode(b"{}"), None);
        assert_eq!(ConfigWake::decode(b"not json"), None);
        assert_eq!(
            ConfigWake::decode(&vec![b' '; CONFIG_WAKE_BYTES_MAX + 1]),
            None
        );
    }
}
