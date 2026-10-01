// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Settings written once for many stores
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 3).
//!
//! A setting is a field of a published node, listed in `pos_proto::settings`. The console writes a
//! value at one scope: the tenant, a brand, a store group or one store. Each store then runs the
//! value from the most specific scope that sets one, and the default where none does. This module
//! holds the three pure steps between a written value and a store's document, and the store the
//! values live in:
//!
//! 1. [`validate`]: the setting exists, may be written at that scope, and the value is one it takes.
//! 2. [`resolve_for_store`]: which value reaches one store, and from which scope.
//! 3. [`tenant_layer_nodes`]: the nodes those values make on the store's Tenant layer.
//!
//! # Why the Tenant layer
//!
//! A setting may live on a node that a console route also publishes, such as `qr` or `stations`,
//! and every node route writes its node whole onto the Store layer. Written there, a setting would
//! be dropped by the next publish of its node, and would drop that node's other fields in turn. The
//! layers merge object by object at compile time (ADR-0033), so a field on the Tenant layer reaches
//! the store beside the Store layer's fields of the same node. A Store-layer value of the same field
//! still wins, because the Store layer is the more specific.
//!
//! # Two store groups that disagree
//!
//! A store may sit in two groups that set the same setting differently. The value written last wins,
//! and the resolved setting names the group it came from, so the console can show it.

use core::future::Future;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use pos_proto::ids::TenantId;
use pos_proto::settings::{Setting, SettingScope, ValueRefusal, register};
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::Open;

/// One value written at one scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingValue {
    /// The setting's key, `node.field`.
    pub setting_key: String,
    /// The scope the value was written at.
    pub scope: Open<SettingScope>,
    /// The tenant, brand, store group or store it was written for.
    pub scope_id: String,
    /// The value, as the node carries it.
    pub value: serde_json::Value,
    /// When it was written. Decides between two store groups that disagree.
    pub update_time: Timestamp,
}

/// Why a value cannot be written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingRefusal {
    /// The register has no setting with this key.
    #[error("there is no setting `{0}`")]
    UnknownSetting(String),
    /// The setting cannot be written at this scope.
    #[error("`{setting_key}` cannot be set at {scope}")]
    ScopeNotAllowed {
        /// The setting.
        setting_key: String,
        /// The scope it was written at.
        scope: String,
    },
    /// The value is not one the setting takes.
    #[error("`{setting_key}` takes {allowed}: {refusal}")]
    ValueNotAllowed {
        /// The setting.
        setting_key: String,
        /// What it takes, for the refusal's message.
        allowed: String,
        /// What is wrong with the value, which decides the reason the refusal names.
        refusal: ValueRefusal,
    },
}

/// Checks a value before it is stored, and returns the setting it is for.
///
/// # Errors
///
/// [`SettingRefusal`] for a key the register does not have, a scope the setting cannot be written
/// at, or a value it does not take.
pub fn validate(
    setting_key: &str,
    scope: SettingScope,
    value: &serde_json::Value,
) -> Result<Setting, SettingRefusal> {
    let setting = register()
        .into_iter()
        .find(|setting| setting.key() == setting_key)
        .ok_or_else(|| SettingRefusal::UnknownSetting(setting_key.to_owned()))?;
    if !setting.scopes.contains(&scope) {
        return Err(SettingRefusal::ScopeNotAllowed {
            setting_key: setting_key.to_owned(),
            scope: scope.to_string(),
        });
    }
    // The register's own rule, so the console's write and the resolve below cannot disagree about
    // which values a setting takes.
    if let Err(refusal) = setting.check(value) {
        return Err(SettingRefusal::ValueNotAllowed {
            setting_key: setting_key.to_owned(),
            allowed: setting.takes(),
            refusal,
        });
    }
    Ok(setting)
}

/// Where one store sits, which decides the values that reach it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorePlacement {
    /// The tenant the store belongs to.
    pub tenant_id: String,
    /// The store's brand, if it has one.
    pub brand_id: Option<String>,
    /// The active store groups the store is a member of. An archived group's values reach nobody.
    pub store_group_ids: Vec<String>,
    /// The store.
    pub store_id: String,
}

/// The value one setting resolves to for one store, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSetting {
    /// The node the value travels in.
    pub node: &'static str,
    /// The field on that node.
    pub field: &'static str,
    /// The value.
    pub value: serde_json::Value,
    /// The scope that set it.
    pub scope: SettingScope,
    /// The tenant, brand, store group or store that set it.
    pub scope_id: String,
}

/// Which written values reach one store: for each setting, the value of the most specific scope
/// that sets one — the store, then its store groups, then its brand, then the tenant.
///
/// A value for a setting the register does not have, at a scope the setting does not allow, or that
/// the setting does not take, reaches nobody: [`validate`] refuses it at write time, and this does
/// not trust that a row was written through it. A setting nothing sets is absent from the result,
/// and the store runs its default.
#[must_use]
pub fn resolve_for_store(
    values: &[SettingValue],
    placement: &StorePlacement,
) -> Vec<ResolvedSetting> {
    // How specific a value is to this store; `None` if it does not reach it.
    let rank = |scope: SettingScope, scope_id: &str| -> Option<u8> {
        match scope {
            SettingScope::Store => (scope_id == placement.store_id).then_some(0),
            SettingScope::StoreGroup => placement
                .store_group_ids
                .iter()
                .any(|group| group == scope_id)
                .then_some(1),
            SettingScope::Brand => (placement.brand_id.as_deref() == Some(scope_id)).then_some(2),
            SettingScope::Tenant => (scope_id == placement.tenant_id).then_some(3),
            SettingScope::Unspecified => None,
        }
    };

    let mut chosen: BTreeMap<String, (u8, &SettingValue, Setting)> = BTreeMap::new();
    for value in values {
        let scope = value.scope.known();
        let Some(rank) = rank(scope, &value.scope_id) else {
            continue;
        };
        let Ok(setting) = validate(&value.setting_key, scope, &value.value) else {
            continue;
        };
        // A more specific scope wins. At one rank, which only two store groups can share, the value
        // written later wins, and the lower id breaks an exact tie so the answer never depends on
        // the order the rows were read in.
        let wins = chosen
            .get(&value.setting_key)
            .is_none_or(|(held_rank, held, _)| {
                rank < *held_rank
                    || (rank == *held_rank
                        && (value.update_time > held.update_time
                            || (value.update_time == held.update_time
                                && value.scope_id < held.scope_id)))
            });
        if wins {
            chosen.insert(value.setting_key.clone(), (rank, value, setting));
        }
    }

    chosen
        .into_values()
        .map(|(_, value, setting)| ResolvedSetting {
            node: setting.node,
            field: setting.field,
            value: value.value.clone(),
            scope: value.scope.known(),
            scope_id: value.scope_id.clone(),
        })
        .collect()
}

/// The settings nodes to write onto one store's Tenant layer, given what that layer holds now.
///
/// For each node the register has a setting on, the node as the layer holds it, with every
/// registered field taken out and the resolved ones put back. The node's other fields stay: the
/// layer may carry a node written by hand at the tenant level, and settings own only their fields.
/// A node is returned when it has a field to carry or when the layer held it before, so a setting
/// somebody cleared is cleared on the store too; a node that never held a setting is not created
/// empty.
#[must_use]
pub fn tenant_layer_nodes(
    tenant_layer: &serde_json::Value,
    resolved: &[ResolvedSetting],
) -> Vec<(String, serde_json::Value)> {
    let register = register();
    let nodes: BTreeSet<&'static str> = register.iter().map(|setting| setting.node).collect();
    let mut out = Vec::new();
    for node in nodes {
        let held = tenant_layer.get(node);
        let mut fields = held
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        for setting in register.iter().filter(|setting| setting.node == node) {
            fields.remove(setting.field);
        }
        for resolved in resolved.iter().filter(|resolved| resolved.node == node) {
            fields.insert(resolved.field.to_owned(), resolved.value.clone());
        }
        if held.is_some() || !fields.is_empty() {
            out.push((node.to_owned(), serde_json::Value::Object(fields)));
        }
    }
    out
}

/// Persists and reads a tenant's setting values.
///
/// Every method is tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every
/// other cloud table. A value is keyed by its setting, its scope and its scope id, so writing one
/// again replaces it. Nothing cites a value by id, so a value nobody wants any more is deleted.
pub trait SettingsStore {
    /// Every value a tenant has written, in key, scope and scope-id order.
    fn list(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<SettingValue>, SettingsStoreError>> + Send;

    /// Writes a value, replacing the one at the same setting, scope and scope id.
    fn put(
        &self,
        tenant_id: TenantId,
        value: &SettingValue,
    ) -> impl Future<Output = Result<(), SettingsStoreError>> + Send;

    /// Removes the value at one setting, scope and scope id. Removing one that is not there is not
    /// an error; the answer says whether there was one.
    fn delete(
        &self,
        tenant_id: TenantId,
        setting_key: &str,
        scope: SettingScope,
        scope_id: &str,
    ) -> impl Future<Output = Result<bool, SettingsStoreError>> + Send;
}

/// A failure of the settings store itself: the database is unreachable, or a stored row could not
/// be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the settings store failed: {0}")]
pub struct SettingsStoreError(String);

impl SettingsStoreError {
    /// Wraps a message, for the server's log.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::ids::TenantId;
    use pos_proto::settings::SettingScope;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;
    use serde_json::json;

    use super::{
        SettingRefusal, SettingValue, SettingsStore, SettingsStoreError, StorePlacement,
        resolve_for_store, tenant_layer_nodes, validate,
    };

    const KEY: &str = "shift.no_shift_selling";
    const REFUSE: &str = "NO_SHIFT_SELLING_REFUSE";
    const ALLOW: &str = "NO_SHIFT_SELLING_ALLOW";

    fn at(milliseconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(milliseconds).expect("a valid instant")
    }

    fn value(scope: SettingScope, scope_id: &str, token: &str, written: i64) -> SettingValue {
        SettingValue {
            setting_key: KEY.to_owned(),
            scope: Open::from_known(scope),
            scope_id: scope_id.to_owned(),
            value: json!(token),
            update_time: at(written),
        }
    }

    fn placement() -> StorePlacement {
        StorePlacement {
            tenant_id: "tenant".to_owned(),
            brand_id: Some("brand".to_owned()),
            store_group_ids: vec!["north".to_owned(), "south".to_owned()],
            store_id: "store".to_owned(),
        }
    }

    fn resolved_token(values: &[SettingValue]) -> Option<(String, SettingScope, String)> {
        let resolved = resolve_for_store(values, &placement());
        let only = resolved.first()?;
        Some((
            only.value.as_str()?.to_owned(),
            only.scope,
            only.scope_id.clone(),
        ))
    }

    #[test]
    fn a_value_is_checked_against_the_register() {
        assert!(validate(KEY, SettingScope::Store, &json!(REFUSE)).is_ok());
        assert_eq!(
            validate("shift.no_such_thing", SettingScope::Store, &json!(REFUSE)),
            Err(SettingRefusal::UnknownSetting(
                "shift.no_such_thing".to_owned()
            ))
        );
        assert!(matches!(
            validate(KEY, SettingScope::Store, &json!("NO_SHIFT_SELLING_LATER")),
            Err(SettingRefusal::ValueNotAllowed { .. })
        ));
        assert!(matches!(
            validate(KEY, SettingScope::Store, &json!(true)),
            Err(SettingRefusal::ValueNotAllowed { .. })
        ));
        assert!(matches!(
            validate(KEY, SettingScope::Unspecified, &json!(REFUSE)),
            Err(SettingRefusal::ScopeNotAllowed { .. })
        ));
    }

    #[test]
    fn the_most_specific_scope_wins() {
        let tenant = value(SettingScope::Tenant, "tenant", ALLOW, 1);
        let brand = value(SettingScope::Brand, "brand", REFUSE, 2);
        let group = value(SettingScope::StoreGroup, "north", ALLOW, 3);
        let store = value(SettingScope::Store, "store", REFUSE, 4);

        assert_eq!(
            resolved_token(std::slice::from_ref(&tenant)),
            Some((ALLOW.to_owned(), SettingScope::Tenant, "tenant".to_owned()))
        );
        assert_eq!(
            resolved_token(&[tenant.clone(), brand.clone()]),
            Some((REFUSE.to_owned(), SettingScope::Brand, "brand".to_owned()))
        );
        assert_eq!(
            resolved_token(&[tenant.clone(), brand.clone(), group.clone()]),
            Some((
                ALLOW.to_owned(),
                SettingScope::StoreGroup,
                "north".to_owned()
            ))
        );
        // The order values are listed in does not matter.
        assert_eq!(
            resolved_token(&[store, group, brand, tenant]),
            Some((REFUSE.to_owned(), SettingScope::Store, "store".to_owned()))
        );
    }

    #[test]
    fn of_two_groups_that_disagree_the_value_written_last_wins() {
        let north = value(SettingScope::StoreGroup, "north", REFUSE, 10);
        let south = value(SettingScope::StoreGroup, "south", ALLOW, 20);
        assert_eq!(
            resolved_token(&[north.clone(), south.clone()]),
            Some((
                ALLOW.to_owned(),
                SettingScope::StoreGroup,
                "south".to_owned()
            ))
        );
        assert_eq!(
            resolved_token(&[south, north]),
            Some((
                ALLOW.to_owned(),
                SettingScope::StoreGroup,
                "south".to_owned()
            ))
        );
    }

    #[test]
    fn a_value_for_another_store_brand_group_or_tenant_does_not_reach_this_one() {
        let others = [
            value(SettingScope::Store, "another store", REFUSE, 1),
            value(SettingScope::StoreGroup, "west", REFUSE, 1),
            value(SettingScope::Brand, "another brand", REFUSE, 1),
            value(SettingScope::Tenant, "another tenant", REFUSE, 1),
        ];
        assert_eq!(resolved_token(&others), None, "the store runs its default");
    }

    #[test]
    fn a_row_the_register_would_refuse_reaches_nobody() {
        let unknown = SettingValue {
            setting_key: "shift.no_such_thing".to_owned(),
            ..value(SettingScope::Store, "store", REFUSE, 1)
        };
        let bad_value = value(SettingScope::Store, "store", "NO_SHIFT_SELLING_LATER", 1);
        let unknown_scope = SettingValue {
            scope: Open::parse("SETTING_SCOPE_PLANET"),
            ..value(SettingScope::Store, "store", REFUSE, 1)
        };
        assert_eq!(resolved_token(&[unknown, bad_value, unknown_scope]), None);
    }

    #[test]
    fn the_tenant_layer_gains_a_setting_and_keeps_what_else_it_holds() {
        let layer = json!({ "shift": { "a_field_written_by_hand": 7 }, "menu": { "kept": true } });
        let resolved = resolve_for_store(
            &[value(SettingScope::Brand, "brand", REFUSE, 1)],
            &placement(),
        );
        let nodes = tenant_layer_nodes(&layer, &resolved);
        assert_eq!(
            nodes,
            vec![(
                "shift".to_owned(),
                json!({ "a_field_written_by_hand": 7, "no_shift_selling": REFUSE })
            )],
            "only the settings nodes are written; `menu` is not touched"
        );
    }

    #[test]
    fn a_cleared_setting_is_cleared_on_the_tenant_layer() {
        let layer = json!({ "shift": { "no_shift_selling": REFUSE } });
        assert_eq!(
            tenant_layer_nodes(&layer, &[]),
            vec![("shift".to_owned(), json!({}))],
            "an empty node reads as the defaults at the edge"
        );
    }

    #[test]
    fn a_node_nothing_sets_is_not_created() {
        assert!(tenant_layer_nodes(&json!({}), &[]).is_empty());
        assert!(tenant_layer_nodes(&serde_json::Value::Null, &[]).is_empty());
    }

    /// An in-memory `SettingsStore`, pinning the seam's contract for the real adapter: a write
    /// replaces the value at its key, a delete of an absent value is no error, and one tenant never
    /// sees another's values.
    #[derive(Default)]
    struct FakeSettings {
        rows: Mutex<Vec<(TenantId, SettingValue)>>,
    }

    fn same_slot(a: &SettingValue, b: &SettingValue) -> bool {
        a.setting_key == b.setting_key && a.scope == b.scope && a.scope_id == b.scope_id
    }

    impl SettingsStore for FakeSettings {
        async fn list(&self, tenant_id: TenantId) -> Result<Vec<SettingValue>, SettingsStoreError> {
            let rows = self.rows.lock().expect("lock");
            let mut mine: Vec<SettingValue> = rows
                .iter()
                .filter(|(owner, _)| *owner == tenant_id)
                .map(|(_, value)| value.clone())
                .collect();
            mine.sort_by(|a, b| {
                (&a.setting_key, a.scope.as_wire(), &a.scope_id).cmp(&(
                    &b.setting_key,
                    b.scope.as_wire(),
                    &b.scope_id,
                ))
            });
            Ok(mine)
        }

        async fn put(
            &self,
            tenant_id: TenantId,
            value: &SettingValue,
        ) -> Result<(), SettingsStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            rows.retain(|(owner, held)| !(*owner == tenant_id && same_slot(held, value)));
            rows.push((tenant_id, value.clone()));
            Ok(())
        }

        async fn delete(
            &self,
            tenant_id: TenantId,
            setting_key: &str,
            scope: SettingScope,
            scope_id: &str,
        ) -> Result<bool, SettingsStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            let before = rows.len();
            rows.retain(|(owner, held)| {
                !(*owner == tenant_id
                    && held.setting_key == setting_key
                    && held.scope.known() == scope
                    && held.scope_id == scope_id)
            });
            Ok(rows.len() < before)
        }
    }

    #[test]
    fn a_written_value_replaces_the_last_at_its_scope_and_stays_with_its_tenant() {
        pos_fakes::executor::run_ready(async {
            let store = FakeSettings::default();
            let tenant = TenantId::new(Ulid::from_u128(1));
            let other = TenantId::new(Ulid::from_u128(2));
            store
                .put(tenant, &value(SettingScope::Store, "store", REFUSE, 1))
                .await
                .expect("put");
            store
                .put(tenant, &value(SettingScope::Store, "store", ALLOW, 2))
                .await
                .expect("put again");
            let listed = store.list(tenant).await.expect("list");
            assert_eq!(listed.len(), 1, "the second write replaced the first");
            assert_eq!(
                listed.first().map(|held| held.value.clone()),
                Some(json!(ALLOW))
            );
            assert!(store.list(other).await.expect("list").is_empty());

            assert!(
                store
                    .delete(tenant, KEY, SettingScope::Store, "store")
                    .await
                    .expect("delete")
            );
            assert!(
                !store
                    .delete(tenant, KEY, SettingScope::Store, "store")
                    .await
                    .expect("delete again"),
                "deleting what is not there is no error"
            );
        });
    }
}
