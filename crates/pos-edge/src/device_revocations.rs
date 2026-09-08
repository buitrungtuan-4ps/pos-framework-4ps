// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Carrying out the cloud's `revoked_devices` deny-list
//! ([ADR-0118 §6](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md)).
//!
//! A manager at the counter retires a lost tablet through `POST /api/pair/revoke`. This is the path
//! for the store nobody can reach: the console publishes the till's local device id onto a
//! store-level `revoked_devices` node, and the box carries it out on its next config pull.
//!
//! # Beside the session, not inside it
//!
//! [`session_from_config`](crate::config_client::session_from_config) returns an
//! [`EdgeSession`](pos_proto::session::EdgeSession) — it is pure, synchronous, and has no error
//! channel, so it can neither `await` a registry write nor refuse anything. This apply is both. So
//! it lives on its own carrier and is called beside the session rebuild, which is exactly what
//! `apply_origins` does for the `origins` node and for the same two reasons: a different reader, and
//! a node that is refused whole rather than partially absorbed.
//!
//! # Refusing whole, and why that is not the usual posture
//!
//! [`ConfigDocument`](pos_ports::config_store::ConfigDocument) states the tree's forward-compatible
//! rule outright: a store applies a version containing keys it does not understand rather than
//! refusing it, because "a store that will not accept configuration is a store that stops being
//! manageable". A new edge-side refusal has to be argued for, and this one is:
//!
//! - **A malformed node is refused whole.** Half of a deny-list is not a smaller deny-list, it is a
//!   security control with a hole in it, and the store cannot tell which half is missing.
//! - **A document that would retire more than one bound till at once is refused whole**, and so is
//!   one that would leave the store with no admitted device at all. Both are the blast radius §6
//!   caps: this rail exists to retire *a* lost tablet, and an operator (or a mistake, or a
//!   compromised console session) using it to close a shop's whole floor is not the same act.
//!
//! Refusing is not a dead end. The deny-list is monotone in *authoring* — the publish route only
//! appends — but the escape hatch from a refused document is a config rollback, and it works here
//! precisely because monotonicity does not depend on the node: a revocation already carried out
//! deleted the `paired_devices` row, and no document can put it back.
//!
//! # What the refusal costs
//!
//! The store's only channel for a refusal is its log. `HeartbeatReport` carries no config field, and
//! `pump_once` records the version as held either way — so the console will show this store as fully
//! current after it refused the node. That is a real gap and it is stated here rather than hidden:
//! an operator whose revoke does not take effect has to read the box's log (ADR-0117 gives a
//! headless box one) to find out why. Reporting a refusal upward needs a channel that does not
//! exist yet, and building one is not in this slice.

use std::sync::Arc;

use pos_core::device_revocation::DeviceRevocationConfig;
use pos_ports::error::PortError;
use pos_proto::ids::DeviceId;

use crate::durable_auth::DurableAuth;
use crate::pairing::Pairing;

/// The node key this module owns.
const NODE: &str = "revoked_devices";

/// What the store should do with a published deny-list.
///
/// A pure value, decided from three inputs and nothing else — the document, what the store has
/// already carried out, and what it currently has paired — so the blast-radius rules can be tested
/// without a registry, a clock, or a config pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The document says nothing about revocations. The previous state stands; this is the case
    /// every store that has never had a device revoked from the console is in.
    Silent,
    /// Every id on the list has already been carried out. The normal steady state once a revocation
    /// has landed: the node stays in the document forever and this is what stops it re-firing.
    AlreadyDone,
    /// Refused whole, with every reason. Nothing is revoked and nothing is recorded — a refusal that
    /// recorded half the list would not be a refusal.
    Refused(Vec<String>),
    /// Carry these out.
    Apply {
        /// Ids the store currently has paired: these are revoked, and then recorded.
        bound: Vec<DeviceId>,
        /// Ids the store does not have paired — already retired at the till, or never here. These
        /// are recorded and *not* revoked: calling revoke on a device that is not there would emit
        /// a second `device.admission.revoked` for an admission the cloud has already been told
        /// about, which is noise the fold does not need.
        unbound: Vec<DeviceId>,
    },
}

/// Decides what to do with the `revoked_devices` node of `document`.
///
/// `applied` is every id this store has already carried out, and `paired` every id it currently has
/// admitted. Both are sets as far as this function is concerned; order is irrelevant.
#[must_use]
pub fn decide(document: &serde_json::Value, applied: &[DeviceId], paired: &[DeviceId]) -> Decision {
    let Some(node) = document.get(NODE) else {
        return Decision::Silent;
    };
    // `to_string` → `from_str`, never `from_value`: some wire types deserialize from a *borrowed*
    // `&str` that `from_value` cannot supply, and this must accept exactly what the cloud's
    // validator accepted. The node is small, so the round trip costs nothing worth measuring.
    let parsed = serde_json::to_string(node)
        .ok()
        .and_then(|text| serde_json::from_str::<DeviceRevocationConfig>(&text).ok());
    let Some(config) = parsed else {
        return Decision::Refused(vec![format!(
            "the {NODE} node is not a list of device ids: it is refused whole rather than applied \
             in part"
        )]);
    };
    let revocations = match config.validate() {
        Ok(revocations) => revocations,
        Err(violations) => return Decision::Refused(violations),
    };
    let pending: Vec<DeviceId> = revocations
        .ids()
        .iter()
        .filter(|id| !applied.contains(id))
        .copied()
        .collect();
    if pending.is_empty() {
        return Decision::AlreadyDone;
    }
    let (bound, unbound): (Vec<DeviceId>, Vec<DeviceId>) =
        pending.iter().partition(|id| paired.contains(id));
    // The cap is measured against *bound* devices only. An entry naming a till the store no longer
    // has costs the shop nothing, and counting those would mean a document could become
    // permanently unappliable simply by naming devices that are long gone.
    let mut violations = Vec::new();
    if bound.len() > 1 {
        violations.push(format!(
            "the {NODE} node would retire {} admitted devices at once, and this rail retires one: \
             it is refused whole. Retire them one publish at a time, or bump the store's lease to \
             supersede the whole box",
            bound.len()
        ));
    }
    // Set containment, not `bound.len() == paired.len()`. Comparing cardinalities fails *open* on
    // an input this function cannot exclude: an adapter whose `paired_devices()` returns two rows
    // for one device — a fork that keeps the pre-re-pair row, say — gives `paired = [X, X]` and
    // `bound = [X]`, the lengths disagree, and the store retires its last till after all. Asking
    // "is every paired device on the way out" answers the question the rule is actually about.
    if !paired.is_empty() && paired.iter().all(|id| bound.contains(id)) {
        violations.push(format!(
            "the {NODE} node would leave this store with no admitted device: nobody could sell, and \
             nobody could pair a replacement without physical access to the box. It is refused \
             whole. To take a whole box out of service, bump the store's lease; to retire its last \
             till, somebody has to be at the box"
        ));
    }
    // Both, not the first. The operator's only channel for a refusal is this store's log, so a
    // refusal that named one of two problems would cost them a whole publish cycle to find the
    // other — and on a store nobody can reach, a cycle is not cheap.
    if !violations.is_empty() {
        return Decision::Refused(violations);
    }
    Decision::Apply { bound, unbound }
}

/// The store's half of the deny-list: the pairing state that actually gates a request, and the
/// durable record of what has already been carried out.
///
/// Holds `Arc<Pairing>` rather than the registry alone, and that is not a convenience. Writing the
/// registry directly would delete the row and leave `Pairing`'s in-memory digest map untouched —
/// and that map is what [`Pairing::device_for`] answers every request from. The stolen tablet would
/// keep working until the next restart, which `pairing.rs` calls the worst of the three possible
/// outcomes. Only [`Pairing::revoke`] does both, and it is also what emits the
/// `device.admission.revoked` the fleet console reads.
pub struct DeviceRevocations {
    pairing: Arc<Pairing>,
    registry: Arc<dyn DurableAuth>,
}

/// A name and nothing else. Neither field is `Debug` — one is a trait object — and neither would be
/// worth printing if it were: the pairing state's contents are token digests. `ConfigClient` derives
/// `Debug`, so this has to exist; making it say nothing is the honest version.
impl core::fmt::Debug for DeviceRevocations {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("DeviceRevocations")
    }
}

impl DeviceRevocations {
    /// The carrier `compose` hands to the config loop and to the boot restore.
    #[must_use]
    pub fn new(pairing: Arc<Pairing>, registry: Arc<dyn DurableAuth>) -> Self {
        Self { pairing, registry }
    }

    /// Carries out the deny-list in `document`, and reports how many devices it retired.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the applied-revocation record could not be read, or if any device on the
    /// list could not be retired. Both are the caller's signal to **hold the config version back**
    /// so the same document is fetched and attempted again — see
    /// [`ConfigClient::pump_once`](crate::config_client::ConfigClient::pump_once). Without that,
    /// the version would be recorded as held, the cloud would answer "already current" to every
    /// later pull, and the store would never see the document again: a device left trading that the
    /// console was told had been retired.
    ///
    /// Every id is still attempted before the error is returned. One tablet whose row will not
    /// delete must not stop the next one being retired.
    pub async fn apply(&self, document: &serde_json::Value) -> Result<usize, PortError> {
        let applied = self.registry.revocations_applied().await?;
        let paired: Vec<DeviceId> = self
            .pairing
            .paired_devices()
            .into_iter()
            .map(|(device_id, _)| device_id)
            .collect();
        match decide(document, &applied, &paired) {
            Decision::Silent | Decision::AlreadyDone => Ok(0),
            Decision::Refused(violations) => {
                for violation in &violations {
                    // Published configuration, never a secret, so the reason is logged in full: an
                    // operator who cannot see why the node was refused cannot fix it, and this log
                    // is the only place they will see it.
                    tracing::warn!(%violation, "a published device revocation was refused");
                }
                Ok(0)
            }
            Decision::Apply { bound, unbound } => {
                let mut retired = 0;
                // Kept, not returned early: every id is attempted, and the *caller* is told at the
                // end so it holds the config version back and the whole document is retried.
                let mut failure = None;
                for device_id in bound {
                    // Revoke first, record second. A crash between them re-applies on the next
                    // pull, which is idempotent; recording first and crashing would leave a device
                    // marked done that was never retired, and nothing would ever retire it.
                    if let Err(error) = self.pairing.revoke(device_id).await {
                        tracing::error!(
                            %error,
                            %device_id,
                            "a device the console retired could not be retired here, and it still \
                             has access; this config version will be held back and the whole \
                             document attempted again on the next pull"
                        );
                        failure = Some(error);
                        continue;
                    }
                    self.record(device_id).await;
                    retired += 1;
                    tracing::warn!(
                        %device_id,
                        "retired a device on the console's instruction; its token no longer resolves"
                    );
                }
                for device_id in unbound {
                    self.record(device_id).await;
                    tracing::info!(
                        %device_id,
                        "the console retired a device this store does not have paired, so there was \
                         nothing to do; recorded as carried out"
                    );
                }
                match failure {
                    Some(error) => Err(error),
                    None => Ok(retired),
                }
            }
        }
    }

    /// Records one id as carried out, logging rather than failing if it cannot be written.
    ///
    /// The only failure here that is *not* worth holding the config version back for. The device is
    /// already retired — durably, its pairing row deleted — so the security outcome has already
    /// happened; what is missing is only the bookkeeping that stops the id being re-decided. And
    /// the cost of that is small and self-healing: the id is no longer paired, so the next time the
    /// store does see the document (a newer version, or this box's next boot, which re-reads the
    /// stored one unconditionally) it lands in `unbound` and is recorded then, with no second event.
    async fn record(&self, device_id: DeviceId) {
        if let Err(error) = self.registry.record_revocation_applied(device_id).await {
            tracing::warn!(
                %error,
                %device_id,
                "could not record that this device's revocation was carried out; the device IS \
                 retired, and the record will be made the next time this store reads the document"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Decision, decide};

    use pos_proto::ids::DeviceId;
    use pos_proto::ulid::Ulid;

    use serde_json::json;

    fn device(n: u128) -> DeviceId {
        DeviceId::new(Ulid::from_u128(n))
    }

    fn node(ids: &[u128]) -> serde_json::Value {
        let listed = ids
            .iter()
            .map(|n| device(*n).to_string())
            .collect::<Vec<_>>();
        json!({ "revoked_devices": { "device_ids": listed } })
    }

    #[test]
    fn a_document_with_no_node_is_silent() {
        assert_eq!(
            decide(&json!({ "menu_version": 3 }), &[], &[device(1)]),
            Decision::Silent
        );
    }

    #[test]
    fn a_bound_device_on_the_list_is_applied() {
        assert_eq!(
            decide(&node(&[1]), &[], &[device(1), device(2)]),
            Decision::Apply {
                bound: vec![device(1)],
                unbound: vec![],
            }
        );
    }

    #[test]
    fn an_id_already_carried_out_is_not_carried_out_again() {
        // The whole reason the applied record exists: the node stays in the document forever, and
        // without this the store would re-revoke and re-report on every thirty-second poll.
        assert_eq!(
            decide(&node(&[1]), &[device(1)], &[device(2)]),
            Decision::AlreadyDone
        );
    }

    #[test]
    fn an_id_the_store_does_not_have_is_recorded_but_not_revoked() {
        // A manager retired this till at the counter before the console's publish landed. There is
        // nothing to revoke, and revoking anyway would emit a second event for one admission.
        assert_eq!(
            decide(&node(&[9]), &[], &[device(1), device(2)]),
            Decision::Apply {
                bound: vec![],
                unbound: vec![device(9)],
            }
        );
    }

    #[test]
    fn a_growing_list_applies_only_its_new_entry() {
        let decision = decide(&node(&[1, 2]), &[device(1)], &[device(2), device(3)]);
        assert_eq!(
            decision,
            Decision::Apply {
                bound: vec![device(2)],
                unbound: vec![],
            },
            "the monotone list grows, and only the part that is new is acted on"
        );
    }

    #[test]
    fn a_shorter_list_cannot_un_revoke_because_there_is_no_such_decision() {
        // The rollback hazard §6 exists for. A document that drops an entry produces `AlreadyDone`
        // for what is left, and there is no variant of `Decision` that restores anything — the
        // deleted `paired_devices` row is what makes that true, and this asserts the *shape* that
        // makes it impossible to express.
        assert_eq!(
            decide(&node(&[]), &[device(1)], &[device(2)]),
            Decision::AlreadyDone,
            "an empty list is not an instruction to un-revoke"
        );
    }

    #[test]
    fn two_bound_devices_at_once_is_refused_whole() {
        let Decision::Refused(violations) =
            decide(&node(&[1, 2]), &[], &[device(1), device(2), device(3)])
        else {
            panic!("retiring two admitted tills at once must be refused");
        };
        assert!(
            violations.iter().any(|v| v.contains("one")),
            "{violations:?}"
        );
    }

    #[test]
    fn retiring_the_last_admitted_device_is_refused_whole() {
        let Decision::Refused(violations) = decide(&node(&[1]), &[], &[device(1)]) else {
            panic!("a store must not be left with nothing admitted");
        };
        assert!(
            violations.iter().any(|v| v.contains("no admitted device")),
            "{violations:?}"
        );
    }

    #[test]
    fn the_cap_counts_bound_devices_only() {
        // Four ids, three of them naming tills this store never had. That is one real retirement,
        // not four, and refusing it would make a document unappliable for naming old hardware.
        assert_eq!(
            decide(&node(&[1, 7, 8, 9]), &[], &[device(1), device(2)]),
            Decision::Apply {
                bound: vec![device(1)],
                unbound: vec![device(7), device(8), device(9)],
            }
        );
    }

    #[test]
    fn a_store_with_nothing_paired_records_without_refusing() {
        // No bound device, so neither cap fires: there is no floor to close.
        assert_eq!(
            decide(&node(&[1]), &[], &[]),
            Decision::Apply {
                bound: vec![],
                unbound: vec![device(1)],
            }
        );
    }

    #[test]
    fn a_node_whose_inner_key_is_misspelled_is_refused_not_read_as_an_empty_list() {
        // The silent no-op. With a defaulted `device_ids`, `{"device_id": [...]}` deserialises to
        // an *empty* list, `pending` is empty, and the decision is `AlreadyDone` — a document
        // naming a stolen tablet read as a document naming nothing, with nothing in the log. The
        // field being required is what turns that into a refusal an operator can see.
        let Decision::Refused(violations) = decide(
            &json!({ "revoked_devices": { "device_id": [device(1).to_string()] } }),
            &[],
            &[device(1), device(2)],
        ) else {
            panic!("a misspelled inner key must be refused, not silently applied as nothing");
        };
        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn an_unknown_sibling_key_is_still_applied() {
        // The other side of the same coin: the config tree's forward-compatibility rule says a
        // store must apply a document carrying keys it does not understand. A future sibling of
        // `device_ids` must not turn this node into a refusal on an older build.
        assert_eq!(
            decide(
                &json!({
                    "revoked_devices": {
                        "device_ids": [device(1).to_string()],
                        "added_in_a_later_release": true
                    }
                }),
                &[],
                &[device(1), device(2)],
            ),
            Decision::Apply {
                bound: vec![device(1)],
                unbound: vec![],
            }
        );
    }

    #[test]
    fn both_caps_are_reported_when_both_are_broken() {
        // Two tills, both named. That breaks the one-device cap *and* the floor. The operator's
        // only channel for a refusal is this box's log, so naming one and not the other would cost
        // them a publish cycle to discover the second.
        let Decision::Refused(violations) = decide(&node(&[1, 2]), &[], &[device(1), device(2)])
        else {
            panic!("both caps are broken");
        };
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations.iter().any(|v| v.contains("at once")),
            "{violations:?}"
        );
        assert!(
            violations.iter().any(|v| v.contains("no admitted device")),
            "{violations:?}"
        );
    }

    #[test]
    fn the_floor_holds_when_a_roster_repeats_a_device() {
        // A cardinality comparison fails open here: `paired = [X, X]`, `bound = [X]`, lengths
        // disagree, and the store retires its last till. Set containment is what makes the rule
        // mean what it says. No shipped adapter returns a duplicate row — the contract suite
        // forbids it — but a fork's might, and the failure mode is the one the cap exists to stop.
        let Decision::Refused(violations) = decide(&node(&[1]), &[], &[device(1), device(1)])
        else {
            panic!("every paired row names the same one device, so this empties the store");
        };
        assert!(
            violations.iter().any(|v| v.contains("no admitted device")),
            "{violations:?}"
        );
    }

    #[test]
    fn the_one_device_cap_is_per_apply_by_design() {
        // Retiring four tills over four publishes is legal and intended: each is a separate
        // console act with its own audit row and its own typed-name confirmation. The cap bounds
        // what ONE document can do, which is what §6 says; it is not a lifetime quota, and the
        // floor is what stops the sequence before the store is empty.
        assert_eq!(
            decide(&node(&[1]), &[], &[device(1), device(2), device(3)]),
            Decision::Apply {
                bound: vec![device(1)],
                unbound: vec![],
            }
        );
        assert_eq!(
            decide(&node(&[1, 2]), &[device(1)], &[device(2), device(3)]),
            Decision::Apply {
                bound: vec![device(2)],
                unbound: vec![],
            }
        );
        // …and the third publish hits the floor rather than emptying the store.
        assert!(matches!(
            decide(&node(&[1, 2, 3]), &[device(1), device(2)], &[device(3)]),
            Decision::Refused(_)
        ));
    }

    #[test]
    fn a_malformed_node_is_refused_whole_rather_than_applied_in_part() {
        let Decision::Refused(violations) = decide(
            &json!({ "revoked_devices": { "device_ids": ["not-a-ulid"] } }),
            &[],
            &[device(1), device(2)],
        ) else {
            panic!("an unparseable id must be refused");
        };
        assert!(
            violations.iter().any(|v| v.contains("not-a-ulid")),
            "the reason names the entry: {violations:?}"
        );
    }

    #[test]
    fn a_node_of_the_wrong_shape_entirely_is_refused_whole() {
        let Decision::Refused(violations) = decide(
            &json!({ "revoked_devices": "everything" }),
            &[],
            &[device(1)],
        ) else {
            panic!("a node that is not an object must be refused");
        };
        assert_eq!(violations.len(), 1, "{violations:?}");
    }
}
