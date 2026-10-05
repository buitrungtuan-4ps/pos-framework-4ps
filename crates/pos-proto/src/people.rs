// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `permissions` config node: who may sign in at a store, and what each person may do
//! ([ADR-0070](../../../docs/adr/0070-people-and-access.md),
//! [ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 10).
//!
//! The cloud compiles it from the tenant's people, roles and assignments, and the edge applies it as
//! the roster it authorises sign-ins against. Until this type the two sides each kept a private
//! struct for the same document, so a field one side added was a field the other could silently
//! drop. Now the cloud writes this type and the edge reads it.
//!
//! # Personal data
//!
//! The node is configuration, not an event, so the `NoPii` barrier on event payloads does not reach
//! it — and it does carry personal data: a person's name and staff code (T1, Decree 13/2023) and the
//! Argon2id hash of their PIN. The name is here because the till shows it, the hash because the till
//! verifies a PIN offline (ADR-0030). The console never returns the hash, and returns the name and
//! code only to a role holding `console.people.read` (ADR-0158 decision 9).

use serde::{Deserialize, Serialize};

/// One member of staff as the node carries them.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedStaffMember {
    /// The employee a sign-in under [`Self::code`] acts as, a ULID string.
    ///
    /// Text rather than an `EmployeeId`, so that one malformed id costs that one person their
    /// sign-in rather than failing the whole roster: the edge refuses to sign the code in instead of
    /// acting as an identity it made up (ADR-0084).
    #[serde(default)]
    pub id: Option<String>,
    /// The staff code a person types to sign in.
    pub code: String,
    /// The person's name, which the till shows.
    #[serde(default)]
    pub name: String,
    /// The `pos-core` permission ids the person holds **directly**, sorted and without duplicates:
    /// they act on these alone. An id this release does not know grants nothing.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// The permission ids the person may exercise only when another person, who holds the
    /// permission directly, approves that one act with their code and PIN
    /// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
    /// decision 4). Sorted, without duplicates, and none of them also in
    /// [`Self::permissions`].
    ///
    /// Read only where the store decides with each person's own set
    /// ([`PublishedPermissions::enforced`]). Skipped from the wire when empty, so a node with none
    /// reads, and is written, exactly as before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions_with_approval: Vec<String>,
    /// How much the person may discount before it needs a manager, in the currency's minor unit.
    ///
    /// Absent when no ceiling is configured, which the edge reads as zero. Skipped from the wire
    /// when absent, so a node published before the field existed reads the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount_ceiling_minor: Option<i64>,
    /// The Argon2id PHC hash of the person's PIN, or `None` when no PIN is set, in which case the
    /// person cannot sign in. Never the PIN itself.
    #[serde(default)]
    pub pin_phc: Option<String>,
}

impl core::fmt::Debug for PublishedStaffMember {
    /// Never prints the name, the code or the PIN hash, so a member logged by mistake leaks none of
    /// them (AGENTS.md: no personal data in logs).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PublishedStaffMember")
            .field("id", &self.id)
            .field("permissions", &self.permissions)
            .field("permissions_with_approval", &self.permissions_with_approval)
            .field("discount_ceiling_minor", &self.discount_ceiling_minor)
            .field("has_pin", &self.pin_phc.is_some())
            .finish_non_exhaustive()
    }
}

/// The `permissions` node: one store's staff.
///
/// Two layers write it, and the merge puts them together: the people compiler writes [`Self::staff`]
/// on the store's Store layer, and the settings compile writes [`Self::enforced`] on its Tenant
/// layer ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
///
/// # A node without a `staff` key
///
/// A node with no `staff` key at all is the switch alone: a store the people publish has not
/// reached, or one whose people node a rollback took away. The edge sets [`Self::enforced`] from it
/// and keeps the roster it holds, because an absent list says nothing about who works there. A
/// `staff` list that is present, even empty, is the roster, and replaces the one held. Both read the
/// same through this type, whose `staff` defaults to empty, so the edge asks the raw node whether the
/// key is there. An edge from before 0.14.1 does not ask, and replaces the roster with an empty one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPermissions {
    /// The store the node authorises staff for, a ULID string. The edge does not read it; it is
    /// there so a node lifted out of its tree still says whose it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
    /// Whether the store decides every command with the signed-in person's own permissions
    /// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
    /// decision 1 and Rollout).
    ///
    /// `false`, which a node without the field reads as, keeps the store-wide set: every
    /// permission granted, and a PIN-flagged one asking for a holder's PIN, as before. It is the
    /// setting `permissions.enforced` (`docs/configuration.md`), resolved by the cloud and written
    /// on the store's Tenant layer, beside the staff the people compiler publishes, and a temporary
    /// one: once every store runs with it on, a later change removes it. Skipped from the wire when
    /// `false`, so a node written before it existed is byte-identical.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub enforced: bool,
    /// The store's staff, sorted by code so two compiles of the same state are byte-identical. A
    /// node with no `staff` key reads as empty here and keeps the edge's roster: see the type's
    /// documentation.
    #[serde(default)]
    pub staff: Vec<PublishedStaffMember>,
}

impl PublishedPermissions {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "permissions";
}

#[cfg(test)]
mod tests {
    use super::{PublishedPermissions, PublishedStaffMember};

    fn member() -> PublishedStaffMember {
        PublishedStaffMember {
            id: Some("01J0000000000000000000EMPL".to_owned()),
            code: "C01".to_owned(),
            name: "Alice".to_owned(),
            permissions: vec!["billing.discount.apply".to_owned()],
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
            pin_phc: Some("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA".to_owned()),
        }
    }

    #[test]
    fn the_node_reads_as_the_cloud_has_always_written_it() {
        // The shape the cloud's compiler emitted before this type: `store_id`, and per member `id`,
        // `code`, `name`, `permissions`, `pin_phc` (null when unset) and no ceiling key when none.
        let text = r#"{
            "store_id": "01J0000000000000000000STOR",
            "staff": [
                {
                    "id": "01J0000000000000000000EMPL",
                    "code": "C01",
                    "name": "Alice",
                    "permissions": ["billing.discount.apply"],
                    "pin_phc": "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"
                },
                { "id": "01J0000000000000000000EMP2", "code": "C02", "name": "Bao", "permissions": [], "pin_phc": null }
            ]
        }"#;
        let node: PublishedPermissions = serde_json::from_str(text).expect("the node parses");
        assert_eq!(node.staff.len(), 2);
        assert_eq!(node.staff.first(), Some(&member()));
        assert_eq!(node.staff.get(1).and_then(|bao| bao.pin_phc.clone()), None);
    }

    #[test]
    fn a_compiled_node_round_trips_byte_for_byte() {
        let node = PublishedPermissions {
            store_id: Some("01J0000000000000000000STOR".to_owned()),
            enforced: false,
            staff: vec![member()],
        };
        let text = serde_json::to_string(&node).expect("serialise");
        assert_eq!(
            text,
            r#"{"store_id":"01J0000000000000000000STOR","staff":[{"id":"01J0000000000000000000EMPL","code":"C01","name":"Alice","permissions":["billing.discount.apply"],"pin_phc":"$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"}]}"#,
            "no ceiling key when there is none, and the order the compiler always wrote"
        );
        let back: PublishedPermissions = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, node);
    }

    #[test]
    fn a_member_with_only_a_code_and_a_field_from_a_newer_release_still_reads() {
        let node: PublishedPermissions =
            serde_json::from_str(r#"{ "staff": [{ "code": "C03", "badge_colour": "red" }] }"#)
                .expect("the node parses");
        let member = node.staff.first().expect("one member");
        assert_eq!(member.code, "C03");
        assert_eq!(member.id, None);
        assert!(
            member.permissions.is_empty(),
            "nothing is granted by default"
        );
    }

    #[test]
    fn approval_modes_and_the_rollout_switch_ride_the_wire_when_set() {
        let node = PublishedPermissions {
            store_id: None,
            enforced: true,
            staff: vec![PublishedStaffMember {
                permissions_with_approval: vec!["sales.line.void_fired".to_owned()],
                ..member()
            }],
        };
        let text = serde_json::to_string(&node).expect("serialise");
        assert!(text.contains(r#""enforced":true"#));
        assert!(text.contains(r#""permissions_with_approval":["sales.line.void_fired"]"#));
        let back: PublishedPermissions = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, node);
    }

    #[test]
    fn a_node_without_the_switch_is_not_enforced_and_grants_nothing_with_approval() {
        let node: PublishedPermissions = serde_json::from_str(
            r#"{ "staff": [{ "code": "C04", "permissions": ["sales.line.add"] }] }"#,
        )
        .expect("the node parses");
        assert!(!node.enforced, "an absent switch keeps the store-wide set");
        let member = node.staff.first().expect("one member");
        assert!(member.permissions_with_approval.is_empty());
    }

    #[test]
    fn debug_prints_no_name_code_or_hash() {
        let printed = format!("{:?}", member());
        assert!(!printed.contains("Alice"));
        assert!(!printed.contains("C01"));
        assert!(!printed.contains("argon2id"));
        assert!(printed.contains("has_pin: true"));
    }
}
