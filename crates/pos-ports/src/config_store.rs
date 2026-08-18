// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Configuration snapshots and deltas.
//!
//! Configuration is owned by the cloud ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md))
//! and the store keeps a local copy so it can sell with no internet. This port is that
//! local copy: read the current version, apply what the cloud sent, and — the part that
//! matters when a bad version ships — fall back to the last version that validated.
//!
//! # What this port deliberately does not decide
//!
//! *Whether* the cloud sends a delta or a full snapshot. `docs/roadmap.md` P7 gives the
//! cloud a rule of the form "more than *K* versions behind ⇒ full snapshot", and *K* is
//! still open. The store's side of that is to report the version it holds during the
//! handshake and apply whatever arrives, so nothing here needs to know *K*. Keeping the
//! decision on one side is what stops the two sides disagreeing about it.
//!
//! # Why the document is opaque here
//!
//! The four-level Tenant → Brand → Store → Device tree and its typed keys are P7's
//! design. A port that knew the key set would have to change every time a key was added,
//! which is the opposite of what a boundary is for. So the document crosses as JSON and
//! `pos-core`'s `CapabilityContext` is the one place that interprets it.

use pos_proto::ids::{ConfigVersionId, StoreId};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use core::future::Future;

use crate::error::PortError;
use crate::tx::Transactional;

/// A configuration document, uninterpreted.
///
/// Held as raw JSON so that a store running an older build applies a version containing
/// keys it does not understand rather than refusing it — the same forward-compatibility
/// rule the event envelope follows, and for the same reason: a store that will not accept
/// configuration is a store that stops being manageable.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigDocument(Box<RawValue>);

impl ConfigDocument {
    /// Wraps raw JSON.
    #[must_use]
    pub const fn new(document: Box<RawValue>) -> Self {
        Self(document)
    }

    /// The document's JSON text.
    #[must_use]
    pub fn as_json(&self) -> &str {
        self.0.get()
    }
}

/// Textual comparison, not semantic.
///
/// Two documents differing only in key order or whitespace compare unequal. That is the
/// honest behaviour for a type that stores bytes, and callers that need semantic equality
/// should compare parsed values; the alternative — a `PartialEq` that silently parses —
/// would make an equality check a fallible operation pretending not to be.
impl PartialEq for ConfigDocument {
    fn eq(&self, other: &Self) -> bool {
        self.as_json() == other.as_json()
    }
}

/// A complete configuration document at a version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSnapshot {
    /// Which version this is.
    pub config_version_id: ConfigVersionId,
    /// The store it applies to.
    pub store_id: StoreId,
    /// The document.
    pub document: ConfigDocument,
}

/// A change from one version to the next.
///
/// `from_config_version_id` is what makes a delta safe to apply: a store holding a
/// different version must reject it rather than apply it out of order, because
/// configuration is a tree and an out-of-order patch produces a document nobody
/// authored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigDelta {
    /// The version this delta expects the store to be holding.
    pub from_config_version_id: ConfigVersionId,
    /// The version the store reaches by applying it.
    pub to_config_version_id: ConfigVersionId,
    /// The store it applies to.
    pub store_id: StoreId,
    /// The change, in whatever patch format P7 settles on.
    pub patch: ConfigDocument,
}

/// What the cloud sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "update_kind", rename_all = "snake_case")]
pub enum ConfigUpdate {
    /// Replace everything. Always applicable, whatever version the store holds.
    Snapshot(ConfigSnapshot),
    /// Patch forward by one version. Applicable only from the stated version.
    Delta(ConfigDelta),
}

/// Keeps the store's copy of its configuration.
///
/// # Contract
///
/// 1. **A delta from the wrong version is refused**, with
///    [`PortError::failed_precondition`], and changes nothing.
/// 2. **Applying the same update twice is a no-op.** The cloud publishes at-least-once,
///    so a repeat is expected traffic, not an error.
/// 3. **A snapshot always applies**, whatever the current version — that is what makes it
///    the recovery path for a store too far behind to patch.
/// 4. **Last-known-good survives a bad version.** After a failed apply,
///    [`Self::last_known_good`] still returns the version that was current before it, and
///    that guarantee is why validation failure degrades a store to "stale configuration"
///    rather than to "not selling".
pub trait ConfigStore: Transactional {
    /// The version the store is running, or `None` before first sync.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached.
    fn current(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<ConfigSnapshot>, PortError>> + Send;

    /// The most recent version that applied and validated.
    ///
    /// Equals [`Self::current`] in normal operation and diverges only after a rejected
    /// version, which is the case it exists for.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached.
    fn last_known_good(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<ConfigSnapshot>, PortError>> + Send;

    /// Applies an update in the caller's transaction, returning the version now current.
    ///
    /// Transactional and sharing [`Transactional::Tx`] with [`crate::EventStore`], so a
    /// configuration change and the event recording it commit together or not at all.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] if a delta's `from_config_version_id` is not the
    /// current version, [`PortError::invalid_argument`] if the document is not valid JSON
    /// for the patch format, or [`PortError::unavailable`] if the store cannot be reached.
    fn apply(
        &self,
        tx: &mut <Self as Transactional>::Tx,
        update: &ConfigUpdate,
    ) -> impl Future<Output = Result<ConfigVersionId, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{ConfigDelta, ConfigDocument, ConfigSnapshot, ConfigUpdate};
    use pos_proto::ids::{ConfigVersionId, StoreId};
    use pos_proto::ulid::Ulid;

    fn document(json: &str) -> ConfigDocument {
        ConfigDocument::new(
            serde_json::value::RawValue::from_string(json.to_owned()).expect("json"),
        )
    }

    #[test]
    fn a_document_keeps_the_bytes_it_was_given() {
        // Unknown keys survive, which is the forward-compatibility rule: a store running
        // an older build must still be manageable.
        let raw = r#"{"tables_enabled":true,"a_key_from_the_future":7}"#;
        let config = document(raw);
        assert_eq!(config.as_json(), raw);
    }

    #[test]
    fn equality_is_textual_and_says_so() {
        assert_eq!(document(r#"{"a":1}"#), document(r#"{"a":1}"#));
        assert_ne!(
            document(r#"{"a":1,"b":2}"#),
            document(r#"{"b":2,"a":1}"#),
            "key order changes the bytes, and this type compares bytes"
        );
    }

    #[test]
    fn an_update_says_which_kind_it_is_on_the_wire() {
        // The tag matters: a receiver must not have to guess by looking for a `patch`
        // field, because a future snapshot format might grow one.
        let store_id = StoreId::new(Ulid::from_u128(3));
        let snapshot = ConfigUpdate::Snapshot(ConfigSnapshot {
            config_version_id: ConfigVersionId::new(Ulid::from_u128(9)),
            store_id,
            document: document(r"{}"),
        });
        let json = serde_json::to_string(&snapshot).expect("serialise");
        assert!(
            json.starts_with(r#"{"update_kind":"snapshot""#),
            "got {json}"
        );

        let delta = ConfigUpdate::Delta(ConfigDelta {
            from_config_version_id: ConfigVersionId::new(Ulid::from_u128(9)),
            to_config_version_id: ConfigVersionId::new(Ulid::from_u128(10)),
            store_id,
            patch: document(r"{}"),
        });
        let json = serde_json::to_string(&delta).expect("serialise");
        assert!(json.starts_with(r#"{"update_kind":"delta""#), "got {json}");
    }

    #[test]
    fn a_delta_names_both_ends_so_it_cannot_apply_out_of_order() {
        let delta = ConfigDelta {
            from_config_version_id: ConfigVersionId::new(Ulid::from_u128(1)),
            to_config_version_id: ConfigVersionId::new(Ulid::from_u128(2)),
            store_id: StoreId::new(Ulid::from_u128(3)),
            patch: document(r#"{"service_charge_enabled":false}"#),
        };
        assert_ne!(delta.from_config_version_id, delta.to_config_version_id);
    }
}
