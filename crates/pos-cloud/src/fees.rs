// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's fee rules, authored once for many stores
//! ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 1).
//!
//! A fee rule is one charge a bill may add — a service charge, a packaging fee per box, a delivery
//! fee — in the wire shape the edge reads, [`PublishedFee`]. The console writes a rule at one scope:
//! the tenant, one brand or one store. Rules merge by `fee_id` down the tree, so a brand's or a
//! store's rule with the same id replaces the tenant's for its stores, and each store is sent its
//! resolved list whole, as the `fees` node.
//!
//! This module is the record a write keeps and the store that keeps it: [`FeeRuleStore`], the
//! seam, with [`InMemoryFeeRules`] and the PostgreSQL adapter in `store-postgres` behind it.
//! Nothing writes a rule yet: validation, resolution and publishing come with the console's fee
//! routes.
//!
//! A fee rule is pricing configuration (T2): a code, a name, a rate or an amount, the items and
//! channels it covers. Nothing here is a customer or employee identifier; who wrote a rule is a
//! console admin's id.

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_proto::fees::PublishedFee;
use pos_proto::ids::{FeeId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::WireEnum;

/// The most rules the in-memory store keeps for one tenant: an explicit bound, as every in-memory
/// structure here has. Far above what any tenant authors — a store charges a handful of fees — and
/// low enough that a runaway test cannot fill memory.
pub const IN_MEMORY_RULES_PER_TENANT: usize = 4_096;

/// The most tenants the in-memory store keeps rules for, bounded for the same reason.
pub const IN_MEMORY_TENANTS: usize = 1_024;

pos_proto::wire_enum! {
    /// Where a fee rule is written: every store of the tenant, every store of one brand, or one
    /// store.
    ///
    /// The tree's own levels below the device ([ADR-0033](../../../docs/adr/0033-config-tree.md)),
    /// so a rule is inherited the way a vendor connection is. Store groups are not a scope for fees:
    /// ADR-0159 does not name them. `FEE_SCOPE_UNSPECIFIED` is the zero value every wire enum has
    /// (`docs/naming-and-api.md` §3.3), and no rule is written at it.
    FeeScope, prefix = "FEE_SCOPE";
    /// Every store the tenant runs. The scope id is the tenant's id.
    Tenant = "TENANT",
    /// Every store of one brand. The scope id is the brand's id.
    Brand = "BRAND",
    /// One store. The scope id is the store's id.
    Store = "STORE",
}

impl FeeScope {
    /// The scopes a rule is written at, broadest first: every one but `FEE_SCOPE_UNSPECIFIED`.
    pub const WRITABLE: [Self; 3] = [Self::Tenant, Self::Brand, Self::Store];
}

/// Where one rule sits: its scope, the id of that scope, and the rule's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeRuleSlot<'a> {
    /// The scope the rule was written at.
    pub scope: FeeScope,
    /// The tenant, brand or store the rule was written for.
    pub scope_id: &'a str,
    /// The rule's stable id.
    pub fee_id: FeeId,
}

/// One fee rule as a tenant wrote it, at one scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeRule {
    /// The scope it was written at.
    pub scope: FeeScope,
    /// The tenant, brand or store it was written for.
    pub scope_id: String,
    /// The rule, in the shape a store is sent it.
    pub rule: PublishedFee,
    /// When it was last written, by the cloud's clock.
    pub update_time: Timestamp,
    /// The console admin who last wrote it, by id. Never a name.
    pub updated_by: String,
}

impl FeeRule {
    /// Where the rule sits.
    #[must_use]
    pub fn slot(&self) -> FeeRuleSlot<'_> {
        FeeRuleSlot {
            scope: self.scope,
            scope_id: &self.scope_id,
            fee_id: self.rule.fee_id,
        }
    }

    /// Refuses a rule no store may keep: one at `FEE_SCOPE_UNSPECIFIED`, which reaches no store.
    /// Every adapter checks it before it writes, so none holds a rule another could not read back.
    ///
    /// # Errors
    ///
    /// [`FeeRuleStoreError`] when the rule's scope is not one of [`FeeScope::WRITABLE`].
    pub fn check_storable(&self) -> Result<(), FeeRuleStoreError> {
        if FeeScope::WRITABLE.contains(&self.scope) {
            Ok(())
        } else {
            Err(FeeRuleStoreError::new(
                "a fee rule is written at the tenant, a brand or a store, never at \
                 FEE_SCOPE_UNSPECIFIED",
            ))
        }
    }
}

/// A fee-rule store that could not be reached, that holds a rule it cannot read back, or that
/// refused a write: a rule at no scope, or one past the in-memory store's bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct FeeRuleStoreError {
    message: String,
}

impl FeeRuleStoreError {
    /// An error carrying `message`, which never names a person.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Persists and reads a tenant's fee rules.
///
/// Every method is tenant-scoped; the `store-postgres` adapter is RLS-isolated by tenant like every
/// other cloud table. A rule is keyed by its scope, its scope id and its fee id, so writing one
/// again replaces it. Nothing cites a row by anything else, so a rule nobody wants any more is
/// deleted: a store's rule deleted hands the store back its brand's or its tenant's.
pub trait FeeRuleStore {
    /// Every rule a tenant has written, in scope, scope-id and fee-id order: the wire tokens' and
    /// the ids' own text order, so two adapters list one tenant's rules the same way.
    fn list(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<FeeRule>, FeeRuleStoreError>> + Send;

    /// Writes a rule, replacing the one in the same slot. A rule
    /// [`check_storable`](FeeRule::check_storable) refuses is refused, and nothing is written.
    fn put(
        &self,
        tenant_id: TenantId,
        rule: &FeeRule,
    ) -> impl Future<Output = Result<(), FeeRuleStoreError>> + Send;

    /// Removes the rule in one slot, and says whether there was one. Removing one that is not there
    /// is not an error.
    fn delete(
        &self,
        tenant_id: TenantId,
        slot: FeeRuleSlot<'_>,
    ) -> impl Future<Output = Result<bool, FeeRuleStoreError>> + Send;
}

/// The key a rule sits under in [`InMemoryFeeRules`]: the order [`FeeRuleStore::list`] promises.
type InMemoryKey = (String, String, String);

/// The in-memory [`FeeRuleStore`], for tests and for code that runs without a database. The
/// contract suite holds it to the seam's rules, the ones the PostgreSQL adapter's integration tests
/// hold that adapter to. Bounded at [`IN_MEMORY_TENANTS`] tenants of
/// [`IN_MEMORY_RULES_PER_TENANT`] rules each.
#[derive(Clone, Debug, Default)]
pub struct InMemoryFeeRules {
    rows: Arc<Mutex<BTreeMap<TenantId, BTreeMap<InMemoryKey, FeeRule>>>>,
}

impl InMemoryFeeRules {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn key(slot: FeeRuleSlot<'_>) -> InMemoryKey {
        (
            slot.scope.as_wire().to_owned(),
            slot.scope_id.to_owned(),
            slot.fee_id.to_string(),
        )
    }
}

impl FeeRuleStore for InMemoryFeeRules {
    async fn list(&self, tenant_id: TenantId) -> Result<Vec<FeeRule>, FeeRuleStoreError> {
        let rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(rows
            .get(&tenant_id)
            .map(|tenant| tenant.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn put(&self, tenant_id: TenantId, rule: &FeeRule) -> Result<(), FeeRuleStoreError> {
        rule.check_storable()?;
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        if !rows.contains_key(&tenant_id) && rows.len() >= IN_MEMORY_TENANTS {
            return Err(FeeRuleStoreError::new(format!(
                "the in-memory fee-rule store holds rules for at most {IN_MEMORY_TENANTS} tenants"
            )));
        }
        let tenant = rows.entry(tenant_id).or_default();
        let key = Self::key(rule.slot());
        if !tenant.contains_key(&key) && tenant.len() >= IN_MEMORY_RULES_PER_TENANT {
            return Err(FeeRuleStoreError::new(format!(
                "the in-memory fee-rule store holds at most {IN_MEMORY_RULES_PER_TENANT} rules for \
                 a tenant"
            )));
        }
        tenant.insert(key, rule.clone());
        Ok(())
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        slot: FeeRuleSlot<'_>,
    ) -> Result<bool, FeeRuleStoreError> {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tenant) = rows.get_mut(&tenant_id) else {
            return Ok(false);
        };
        let removed = tenant.remove(&Self::key(slot)).is_some();
        // A tenant with no rule left no longer counts against the bound.
        if tenant.is_empty() {
            rows.remove(&tenant_id);
        }
        Ok(removed)
    }
}

#[cfg(test)]
pub(crate) mod contract {
    //! The [`FeeRuleStore`] contract, run here against the in-memory store. The PostgreSQL
    //! adapter's `fee_rules` integration tests hold its SQL to the same rules on a real database,
    //! and its seam refuses what [`FeeRule::check_storable`] refuses, as this store does.

    use std::collections::BTreeMap;

    use pos_proto::fees::{FeeCode, PublishedFee};
    use pos_proto::ids::{FeeId, TenantId};
    use pos_proto::text::DisplayName;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{FeeRule, FeeRuleStore, FeeScope};

    fn tenant(n: u128) -> TenantId {
        TenantId::new(Ulid::from_u128(n))
    }

    fn fee(n: u128) -> FeeId {
        FeeId::new(Ulid::from_u128(n))
    }

    fn at(milliseconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(milliseconds).expect("a valid instant")
    }

    /// A rule with only what every rule needs, written at `scope` for `scope_id`.
    pub(crate) fn rule(scope: FeeScope, scope_id: &str, fee_id: FeeId, code: &str) -> FeeRule {
        FeeRule {
            scope,
            scope_id: scope_id.to_owned(),
            rule: PublishedFee {
                fee_id,
                code: FeeCode::new(code),
                display_name: DisplayName::new("Service charge"),
                display_name_translations: BTreeMap::new(),
                kind: Open::default(),
                rate: None,
                amount: None,
                channels: Vec::new(),
                item_scope: Open::default(),
                menu_item_ids: Vec::new(),
                base_discounted: true,
                base_tax_inclusive: false,
                tax: Open::default(),
                tax_class_id: None,
                waivable: false,
                active: true,
            },
            update_time: at(1_000),
            updated_by: "admin-1".to_owned(),
        }
    }

    /// Runs every rule of the contract against `store`, which must start empty.
    pub(crate) async fn holds(store: &impl FeeRuleStore) {
        let owner = tenant(0xA);
        let other = tenant(0xB);
        let shop = "01J0000000000000000000STOR";

        // A write, and a second write to the same slot, which replaces the first.
        let first = rule(FeeScope::Store, shop, fee(1), "SERVICE");
        store.put(owner, &first).await.expect("write");
        let replaced = FeeRule {
            update_time: at(2_000),
            updated_by: "admin-2".to_owned(),
            ..rule(FeeScope::Store, shop, fee(1), "SERVICE_2")
        };
        store.put(owner, &replaced).await.expect("write again");
        assert_eq!(
            store.list(owner).await.expect("list"),
            vec![replaced.clone()],
            "the second write replaced the first, rule, time and author together"
        );

        // The same fee at another scope is a rule of its own, and the list is in scope order.
        let tenant_wide = rule(FeeScope::Tenant, &owner.to_string(), fee(1), "SERVICE");
        store
            .put(owner, &tenant_wide)
            .await
            .expect("write at another scope");
        let brand = rule(FeeScope::Brand, "brand-1", fee(2), "PACKAGING");
        store.put(owner, &brand).await.expect("write a brand's");
        assert_eq!(
            store.list(owner).await.expect("list"),
            vec![brand.clone(), replaced.clone(), tenant_wide.clone()],
            "in scope, scope-id and fee-id order"
        );

        // Another tenant reads nothing of it.
        assert!(store.list(other).await.expect("list").is_empty());

        // A rule at no scope is refused, and nothing is written.
        assert!(
            store
                .put(owner, &rule(FeeScope::Unspecified, shop, fee(3), "NOWHERE"))
                .await
                .is_err()
        );
        assert_eq!(
            store.list(owner).await.expect("list"),
            vec![brand.clone(), replaced.clone(), tenant_wide.clone()],
            "the refused rule is not kept"
        );

        // A delete says whether there was a rule, and removes only its own slot.
        assert!(store.delete(owner, replaced.slot()).await.expect("delete"));
        assert!(
            !store
                .delete(owner, replaced.slot())
                .await
                .expect("delete again"),
            "deleting what is not there is no error"
        );
        assert!(
            !store.delete(other, brand.slot()).await.expect("delete"),
            "another tenant cannot delete it"
        );
        assert_eq!(
            store.list(owner).await.expect("list"),
            vec![brand, tenant_wide]
        );
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::{FeeId, TenantId};
    use pos_proto::ulid::Ulid;

    use pos_proto::wire_enum::WireEnum;

    use super::contract::{holds, rule};
    use super::{
        FeeRuleStore, FeeScope, IN_MEMORY_RULES_PER_TENANT, IN_MEMORY_TENANTS, InMemoryFeeRules,
    };

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build a current-thread runtime")
            .block_on(future)
    }

    #[test]
    fn the_in_memory_store_holds_the_contract() {
        block_on(holds(&InMemoryFeeRules::new()));
    }

    #[test]
    fn the_in_memory_store_refuses_a_rule_past_its_bound_and_still_replaces_one() {
        block_on(async {
            let store = InMemoryFeeRules::new();
            let owner = TenantId::new(Ulid::from_u128(0xA));
            for n in 0..IN_MEMORY_RULES_PER_TENANT {
                let fee_id = FeeId::new(Ulid::from_u128(n as u128 + 1));
                store
                    .put(owner, &rule(FeeScope::Store, "shop", fee_id, "FEE"))
                    .await
                    .expect("within the bound");
            }
            let one_more = FeeId::new(Ulid::from_u128(0xFFFF_FFFF));
            assert!(
                store
                    .put(owner, &rule(FeeScope::Store, "shop", one_more, "FEE"))
                    .await
                    .is_err()
            );
            // Replacing a rule it holds is not growing it.
            let first = FeeId::new(Ulid::from_u128(1));
            store
                .put(owner, &rule(FeeScope::Store, "shop", first, "RENAMED"))
                .await
                .expect("a replacement is not a new rule");
        });
    }

    #[test]
    fn the_in_memory_store_refuses_a_tenant_past_its_bound_and_still_serves_the_others() {
        block_on(async {
            let store = InMemoryFeeRules::new();
            let fee_id = FeeId::new(Ulid::from_u128(1));
            for n in 0..IN_MEMORY_TENANTS {
                let tenant_id = TenantId::new(Ulid::from_u128(n as u128 + 1));
                store
                    .put(tenant_id, &rule(FeeScope::Store, "shop", fee_id, "FEE"))
                    .await
                    .expect("within the bound");
            }
            let one_more = TenantId::new(Ulid::from_u128(0xFFFF_FFFF));
            assert!(
                store
                    .put(one_more, &rule(FeeScope::Store, "shop", fee_id, "FEE"))
                    .await
                    .is_err()
            );
            assert!(store.list(one_more).await.expect("list").is_empty());
            // A tenant it holds still writes.
            let first = TenantId::new(Ulid::from_u128(1));
            store
                .put(first, &rule(FeeScope::Brand, "brand", fee_id, "FEE"))
                .await
                .expect("a tenant it holds is not a new tenant");
        });
    }

    #[test]
    fn a_scope_reads_back_from_its_token_and_no_rule_is_written_at_unspecified() {
        for scope in FeeScope::ALL {
            assert_eq!(FeeScope::from_wire(scope.as_wire()), Some(*scope));
        }
        assert_eq!(FeeScope::Brand.as_wire(), "FEE_SCOPE_BRAND");
        assert_eq!(FeeScope::Unspecified.as_wire(), "FEE_SCOPE_UNSPECIFIED");
        assert_eq!(FeeScope::from_wire("FEE_SCOPE_STORE_GROUP"), None);
        assert_eq!(FeeScope::from_wire("SETTING_SCOPE_STORE"), None);
        assert!(!FeeScope::WRITABLE.contains(&FeeScope::Unspecified));
        assert_eq!(FeeScope::WRITABLE.len() + 1, FeeScope::ALL.len());
    }
}
