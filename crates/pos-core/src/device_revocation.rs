// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `revoked_devices` configuration key: the tills a store must stop trusting
//! ([ADR-0118 §6](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md)).
//!
//! A manager standing at the counter retires a lost tablet through `POST /api/pair/revoke`. This is
//! the other half: the console does it for a store nobody can reach — a box in a shop that is
//! closed, or one whose only admitted till is the tablet that walked out of the door.
//!
//! # Why a deny-list and not a status
//!
//! The obvious shape is a mutable per-device status the store mirrors. It is the wrong one, because
//! configuration is *restorable*: `POST /admin/config/versions/{id}/restore` reverts the whole
//! effective document, and a status that reverted would hand a stolen tablet its access back. A
//! deny-list that only ever grows, applied once per id and never revisited, cannot be un-said by a
//! rollback.
//!
//! **Monotone is built here, not inherited.** `fleet_update.revoked_key_ids` is the deny-list this
//! record's §6 cites as precedent, and it is not monotone on either side: the publish route
//! overwrites the whole node from a request body, and the edge replaces `session.fleet_update`
//! wholesale on every poll, so a shorter *valid* list un-revokes a signing key immediately. The
//! shape is worth copying; the authoring semantics are not. Three separate things make this list
//! monotone, and all three are in this change:
//!
//! 1. The publish route appends to the list it read **inside** the conditional-write retry, so two
//!    admins revoking two tills cannot lose each other's entry.
//! 2. The store records each id as applied in its own durable table, and never applies one twice.
//! 3. Revoking is a *durable deletion* — the `paired_devices` row goes — so the effect survives the
//!    boot whether or not the document that caused it does.
//!
//! The cap on the list is not a policy about how many tablets a store may lose; it is a bound on a
//! node that by construction never shrinks and travels in every config pull.

use serde::Deserialize;

use pos_proto::ids::DeviceId;

/// The most ids one store's deny-list may carry.
///
/// The list only ever grows and rides every config pull, so it needs a ceiling somewhere. A store
/// that has retired five hundred tills has a problem this node cannot fix, and the refusal names the
/// cap so an operator reading it knows the list itself is the thing to prune (by publishing a
/// document without the entries whose devices are long gone — the applied record on each box, not
/// the node, is what keeps those revocations true).
pub const MAX_REVOKED_DEVICES: usize = 512;

/// The tills this store must stop trusting, as the config document carries them.
///
/// A typed view of the node, so the cloud validates it before publishing and the edge parses it
/// before acting, both through [`DeviceRevocationConfig::validate`] — the same shape the OTA nodes
/// beside it in the tree use.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeviceRevocationConfig {
    /// The local device ids to retire, each a ULID the *store* minted at pairing.
    ///
    /// **Required, deliberately, where every other node in the tree defaults its fields.** A
    /// deny-list is not like the rest of the configuration: if `device_ids` defaulted, a node whose
    /// key was misspelled or renamed — `{"device_id": [...]}` — would deserialize to an *empty*
    /// list, and the store would treat a document naming a stolen tablet as a document naming
    /// nothing. Silently. The whole posture of this node is that a shape the store cannot read is
    /// refused whole and logged, so the one field it turns on must be the field whose absence makes
    /// it fail. An empty `device_ids` is still legal — that is a store that has had nothing retired.
    ///
    /// Unknown *sibling* keys are still accepted (no `deny_unknown_fields`), so a later version can
    /// add one without breaking a store running an older build — the tree's forward-compatibility
    /// rule, which this keeps.
    pub device_ids: Vec<String>,
}

/// A validated deny-list: every entry is a parseable device id, and no entry repeats.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceRevocations {
    device_ids: Vec<DeviceId>,
}

impl DeviceRevocations {
    /// The ids to retire, in the order the document listed them.
    ///
    /// Order is preserved rather than sorted, because it is the order an operator published them in
    /// and the store's log reads better for it. Nothing downstream depends on the order: the apply
    /// is a set operation.
    #[must_use]
    pub fn ids(&self) -> &[DeviceId] {
        &self.device_ids
    }
}

impl DeviceRevocationConfig {
    /// Validates and parses the deny-list.
    ///
    /// # Errors
    ///
    /// Every human-readable violation (not just the first), so an operator fixing a rejected
    /// document sees the whole list: an id that is not a ULID, a repeated id, or a list past
    /// [`MAX_REVOKED_DEVICES`].
    pub fn validate(&self) -> Result<DeviceRevocations, Vec<String>> {
        let mut violations = Vec::new();
        if self.device_ids.len() > MAX_REVOKED_DEVICES {
            violations.push(format!(
                "revoked_devices.device_ids holds {} ids, past the cap of {MAX_REVOKED_DEVICES}",
                self.device_ids.len()
            ));
        }
        let mut device_ids: Vec<DeviceId> = Vec::with_capacity(self.device_ids.len());
        for text in &self.device_ids {
            let Ok(device_id) = text.parse::<DeviceId>() else {
                // The offender is named: these are published configuration, never secrets, and an
                // operator who cannot see which entry was refused cannot fix it.
                violations.push(format!(
                    "revoked_devices.device_ids has an id that is not a ULID: {text}"
                ));
                continue;
            };
            if device_ids.contains(&device_id) {
                violations.push(format!(
                    "revoked_devices.device_ids lists the same id twice: {text}"
                ));
                continue;
            }
            device_ids.push(device_id);
        }
        if violations.is_empty() {
            Ok(DeviceRevocations { device_ids })
        } else {
            Err(violations)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceRevocationConfig, MAX_REVOKED_DEVICES};

    use pos_proto::ids::DeviceId;
    use pos_proto::ulid::Ulid;

    fn text(value: u128) -> String {
        DeviceId::new(Ulid::from_u128(value)).to_string()
    }

    #[test]
    fn an_explicitly_empty_list_validates_as_empty() {
        let parsed = DeviceRevocationConfig::default()
            .validate()
            .expect("a node that lists no ids is a node that revokes nothing");
        assert!(parsed.ids().is_empty());
    }

    // The deserialize behaviour `device_ids` being required buys — a misspelled key refused rather
    // than read as an empty list — is asserted at the two call sites that must refuse it, in
    // `pos_edge::device_revocations` and the cloud's `CapabilityValidator`. It cannot be asserted
    // here: `pos-core` has no `serde_json` dependency, deliberately (ADR-0013).

    #[test]
    fn every_id_parses_and_the_order_is_kept() {
        let config = DeviceRevocationConfig {
            device_ids: vec![text(7), text(3)],
        };
        let parsed = config.validate().expect("two ULIDs validate");
        assert_eq!(
            parsed.ids(),
            [
                DeviceId::new(Ulid::from_u128(7)),
                DeviceId::new(Ulid::from_u128(3)),
            ],
            "the published order survives, so the store's log reads as the operator published it"
        );
    }

    #[test]
    fn an_id_that_is_not_a_ulid_is_refused_by_name() {
        let violations = DeviceRevocationConfig {
            device_ids: vec![text(1), "till-by-the-window".to_owned()],
        }
        .validate()
        .expect_err("a non-ULID entry is a violation");
        assert_eq!(violations.len(), 1);
        assert!(
            violations.iter().any(|v| v.contains("till-by-the-window")),
            "the reason names the offending entry: {violations:?}"
        );
    }

    #[test]
    fn a_repeated_id_is_refused_rather_than_silently_deduplicated() {
        // Not a harmless duplicate: the list is authored by appending, so the same id twice means
        // two publishes disagreed about what the list already held, and that is worth seeing.
        let violations = DeviceRevocationConfig {
            device_ids: vec![text(4), text(4)],
        }
        .validate()
        .expect_err("a repeat is a violation");
        assert!(
            violations.iter().any(|v| v.contains("twice")),
            "{violations:?}"
        );
    }

    #[test]
    fn every_violation_is_reported_not_just_the_first() {
        let violations = DeviceRevocationConfig {
            device_ids: vec!["nope".to_owned(), text(2), text(2)],
        }
        .validate()
        .expect_err("both a bad id and a repeat are violations");
        assert_eq!(violations.len(), 2, "{violations:?}");
    }

    #[test]
    fn a_list_past_the_cap_is_refused_and_the_reason_names_the_cap() {
        let device_ids = (0..=MAX_REVOKED_DEVICES as u128)
            .map(text)
            .collect::<Vec<_>>();
        let violations = DeviceRevocationConfig { device_ids }
            .validate()
            .expect_err("one past the cap is a violation");
        assert!(
            violations
                .iter()
                .any(|v| v.contains(&MAX_REVOKED_DEVICES.to_string())),
            "{violations:?}"
        );
    }
}
