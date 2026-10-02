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
//! This module is everything about a rule that is not HTTP:
//!
//! - the record a write keeps and the store that keeps it: [`FeeRuleStore`], the seam, with
//!   [`InMemoryFeeRules`] and the PostgreSQL adapter in `store-postgres` behind it;
//! - which rules a store runs, [`resolve_for_store`];
//! - the rule a store is sent, [`compile`]: an include or exclude list may name item categories as
//!   well as items, and the categories are compiled into the items they hold when the rule is
//!   published, as the menu is compiled when it is published (ADR-0159 decision 1), so the edge
//!   needs no category model;
//! - what a store must hold for a rule to apply there, [`StoreFacts`] and [`faults_at`];
//! - the form a rule is stored and read in, [`authored_json`] and [`rule_from_authored`].
//!
//! A fee rule is pricing configuration (T2): a code, a name, a rate or an amount, the items and
//! channels it covers. Nothing here is a customer or employee identifier; who wrote a rule is a
//! console admin's id.

use core::future::Future;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;

use pos_proto::MenuBook;
use pos_proto::enums::SalesChannel;
use pos_proto::fees::{FeeItems, FeeKind, FeeTax, PublishedFee, PublishedFees};
use pos_proto::ids::{FeeId, MenuItemId, TenantId};
use pos_proto::locale::TaxRateTable;
use pos_proto::money::CurrencyCode;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::{Open, WireEnum};

use crate::catalog::{CatalogItem, ItemCategoryId};
use crate::registry::EntityStatus;

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

    /// How specific a rule at this scope is: a store's beats its brand's, which beats its
    /// tenant's.
    const fn specificity(self) -> u8 {
        match self {
            Self::Unspecified => 0,
            Self::Tenant => 1,
            Self::Brand => 2,
            Self::Store => 3,
        }
    }
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
    /// The rule, in the shape a store is sent it, with the items it names itself.
    pub rule: PublishedFee,
    /// The item categories an include or exclude list names besides its items, in id order. Each
    /// is compiled into the items it holds when the rule is published ([`compile`]); empty for a
    /// rule that counts every line.
    pub item_category_ids: Vec<ItemCategoryId>,
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

    /// Whether the rule reaches a store at `placement`: written for the store's tenant, its brand
    /// or the store itself.
    #[must_use]
    pub fn reaches(&self, placement: &FeePlacement) -> bool {
        match self.scope {
            FeeScope::Tenant => self.scope_id == placement.tenant_id,
            FeeScope::Brand => placement.brand_id.as_deref() == Some(self.scope_id.as_str()),
            FeeScope::Store => self.scope_id == placement.store_id,
            FeeScope::Unspecified => false,
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

// --- Which rules a store runs ---------------------------------------------------------------

/// Where a store sits, as far as its fee rules go: its tenant, its brand if it has one, and itself,
/// each as the text a rule's `scope_id` carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeePlacement {
    /// The store's tenant.
    pub tenant_id: String,
    /// The store's brand, or `None` for a store no brand holds.
    pub brand_id: Option<String>,
    /// The store itself.
    pub store_id: String,
}

/// The rules a store runs: for each fee, the rule written at the most specific scope that reaches
/// the store — its own, else its brand's, else its tenant's.
///
/// In fee-id order, which is the order the fees were created in, so the same rules always publish
/// as the same node and a store is not sent a new version for a list that only moved.
#[must_use]
pub fn resolve_for_store<'r>(rules: &'r [FeeRule], placement: &FeePlacement) -> Vec<&'r FeeRule> {
    let mut chosen: BTreeMap<FeeId, &'r FeeRule> = BTreeMap::new();
    for rule in rules.iter().filter(|rule| rule.reaches(placement)) {
        let more_specific = chosen
            .get(&rule.rule.fee_id)
            .is_none_or(|held| rule.scope.specificity() > held.scope.specificity());
        if more_specific {
            chosen.insert(rule.rule.fee_id, rule);
        }
    }
    chosen.into_values().collect()
}

/// Another active rule among `resolved` with `rule`'s code, if there is one. Two fees under one
/// code would both be charged and reported as one, which is how a store-level fee written as a new
/// fee, rather than as the tenant's fee overridden, charges the guest twice. A paused rule charges
/// nothing, so it shares a code with no one.
#[must_use]
pub fn shares_code_with<'r>(rule: &PublishedFee, resolved: &[&'r FeeRule]) -> Option<&'r FeeRule> {
    resolved.iter().copied().find(|other| {
        other.rule.active && other.rule.fee_id != rule.fee_id && other.rule.code == rule.code
    })
}

// --- The rule a store is sent --------------------------------------------------------------------

/// The items each category holds in the tenant's catalog: what a rule naming a category is
/// compiled against when it is published. Archived items are left out, as the menu compiler leaves
/// them out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CategoryItems {
    items: BTreeMap<ItemCategoryId, BTreeSet<MenuItemId>>,
}

impl CategoryItems {
    /// The active items of a tenant's catalog, by category.
    #[must_use]
    pub fn from_catalog<'a>(catalog: impl IntoIterator<Item = &'a CatalogItem>) -> Self {
        let mut items: BTreeMap<ItemCategoryId, BTreeSet<MenuItemId>> = BTreeMap::new();
        for item in catalog {
            if item.status != EntityStatus::Active {
                continue;
            }
            if let Some(category) = item.item_category_id {
                items.entry(category).or_default().insert(item.menu_item_id);
            }
        }
        Self { items }
    }

    /// The items `category` holds; none for a category no active item is in.
    fn of(&self, category: ItemCategoryId) -> impl Iterator<Item = MenuItemId> + '_ {
        self.items.get(&category).into_iter().flatten().copied()
    }
}

/// A rule as a store is sent it: an include or exclude list with its categories compiled into the
/// items they hold now, merged with the items the rule names itself, in id order.
///
/// An exclude list that compiles to nothing excludes nothing, so it is sent as counting every
/// line: sent empty, it would be a faulted rule the edge does not apply, and the fee would stop
/// being charged at all. An include list that compiles to nothing is sent as it is, and the edge
/// counts no line for it, which is what it says.
#[must_use]
pub fn compile(rule: &FeeRule, categories: &CategoryItems) -> PublishedFee {
    let mut published = rule.rule.clone();
    let listed = matches!(
        published.item_scope(),
        Some(FeeItems::Include | FeeItems::Exclude)
    );
    if listed {
        let mut items: BTreeSet<MenuItemId> = published.menu_item_ids.iter().copied().collect();
        for category in &rule.item_category_ids {
            items.extend(categories.of(*category));
        }
        published.menu_item_ids = items.into_iter().collect();
        if published.menu_item_ids.is_empty() && published.item_scope() == Some(FeeItems::Exclude) {
            published.item_scope = Open::from_known(FeeItems::All);
        }
    }
    published
}

/// What one store is sent, and what stops it being sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreFees {
    /// The store's resolved rules, compiled: its `fees` node.
    pub node: PublishedFees,
    /// Each rule the store cannot apply, and why. A store with any is not published to, and keeps
    /// the rules it has.
    pub faults: Vec<(FeeId, StoreFault)>,
}

/// Resolves a store's rules, compiles each, and checks each against what the store holds.
#[must_use]
pub fn for_store(
    rules: &[FeeRule],
    placement: &FeePlacement,
    categories: &CategoryItems,
    facts: &StoreFacts,
) -> StoreFees {
    let mut faults = Vec::new();
    let fees = resolve_for_store(rules, placement)
        .into_iter()
        .map(|rule| {
            let published = compile(rule, categories);
            faults.extend(
                faults_at(&published, facts)
                    .into_iter()
                    .map(|fault| (published.fee_id, fault)),
            );
            published
        })
        .collect();
    StoreFees {
        node: PublishedFees { fees },
        faults,
    }
}

// --- What a store must hold for a rule to apply ---------------------------------------------------

/// What a store's configuration says that a fee rule has to agree with: the currency it bills in,
/// the tax rates it holds, and the items on its menu.
///
/// Each is absent while the store has not been published that node, and nothing is checked against
/// a fact the store does not have: a store with no `tax` node has no menu either, because a menu
/// needs one, so it sells nothing a fee could apply to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreFacts {
    currency: Option<CurrencyCode>,
    tax_rates: Option<TaxRateTable>,
    menu: Option<MenuBook>,
}

impl StoreFacts {
    /// Reads the facts from a store's effective configuration, its layers merged: the `locale`
    /// node's currency, the `tax` node and the `menu` node. A node that is absent or does not parse
    /// is no fact, which is how the edge reads it too.
    #[must_use]
    pub fn from_document(document: &serde_json::Value) -> Self {
        Self {
            currency: document
                .get("locale")
                .and_then(|locale| locale.get("currency_code"))
                .and_then(serde_json::Value::as_str)
                .and_then(|code| CurrencyCode::parse(code).ok()),
            tax_rates: parsed(document.get("tax")),
            menu: parsed(document.get("menu")),
        }
    }

    /// Whether the store's menu lists any of `items` on one of `channels`, or on any channel when
    /// `channels` is empty, as a rule's channel list reads. `None` when the store has no menu.
    #[must_use]
    pub fn menu_lists_any(
        &self,
        items: &[MenuItemId],
        channels: &[Open<SalesChannel>],
    ) -> Option<bool> {
        let menu = self.menu.as_ref()?;
        let on = |catalog: &pos_proto::MenuCatalog| {
            items.iter().any(|item| catalog.get(*item).is_some())
        };
        Some(if channels.is_empty() {
            menu.channels().iter().any(|row| on(&row.catalog)) || on(menu.fallback())
        } else {
            known_channels(channels).any(|channel| on(menu.catalog_for(channel)))
        })
    }
}

/// A node read the way the edge reads one: written out as text and parsed back, because a money
/// amount reads only from text.
fn parsed<T: serde::de::DeserializeOwned>(value: Option<&serde_json::Value>) -> Option<T> {
    let text = serde_json::to_string(value?).ok()?;
    serde_json::from_str(&text).ok()
}

/// The channels a list names that are channels a sale is made on: not `UNSPECIFIED`, and not a
/// token this build does not know.
fn known_channels(channels: &[Open<SalesChannel>]) -> impl Iterator<Item = SalesChannel> + '_ {
    channels
        .iter()
        .filter(|channel| !channel.is_unrecognised())
        .map(Open::known)
        .filter(|channel| *channel != SalesChannel::Unspecified)
}

/// Why a store cannot apply a rule it would be sent. Each one would fail every bill the rule
/// applies to, so a store with one is not sent its rules at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreFault {
    /// An amount in a currency other than the one the store bills in. A bill is one currency, so
    /// adding it fails.
    CurrencyMismatch,
    /// A fee taxed at a class the store has no rate for on a channel the fee applies on, which
    /// fails the bill as an item of that class would.
    TaxClassNotRated,
}

impl StoreFault {
    /// The fault's token, as a refusal's detail and a publish outcome carry it.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::CurrencyMismatch => "CURRENCY_MISMATCH",
            Self::TaxClassNotRated => "TAX_RATE_NOT_CONFIGURED",
        }
    }

    /// The rule's field the fault is about.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::CurrencyMismatch => "rule.amount",
            Self::TaxClassNotRated => "rule.tax_class_id",
        }
    }
}

/// The faults `published` has at a store with `facts`. None for a paused rule, which applies to
/// nothing, and none against a fact the store does not have yet.
#[must_use]
pub fn faults_at(published: &PublishedFee, facts: &StoreFacts) -> Vec<StoreFault> {
    if !published.active {
        return Vec::new();
    }
    let mut faults = Vec::new();
    let charges_an_amount = matches!(
        published.kind(),
        Some(FeeKind::AmountPerBill | FeeKind::AmountPerUnit)
    );
    if charges_an_amount
        && let (Some(amount), Some(currency)) = (published.amount, facts.currency)
        && amount.currency_code != currency
    {
        faults.push(StoreFault::CurrencyMismatch);
    }
    if published.tax() == FeeTax::TaxClass
        && let (Some(class), Some(rates)) = (published.tax_class_id, facts.tax_rates.as_ref())
        && charged_on(published, rates)
            .into_iter()
            .any(|channel| rates.rate_for(class, channel).is_none())
    {
        faults.push(StoreFault::TaxClassNotRated);
    }
    faults
}

/// The channels a rule is charged on at a store: the ones it lists, or, when it lists none, every
/// channel the store's tax table rates, which are the channels the store bills on.
fn charged_on(published: &PublishedFee, rates: &TaxRateTable) -> BTreeSet<SalesChannel> {
    if published.channels.is_empty() {
        let table_channels: Vec<Open<SalesChannel>> = rates
            .rows()
            .iter()
            .map(|row| row.sales_channel.clone())
            .collect();
        known_channels(&table_channels).collect()
    } else {
        known_channels(&published.channels).collect()
    }
}

// --- The form a rule is stored and read in --------------------------------------------------------

/// A rule as the console writes and reads it, and as `fee_rules.doc` keeps it: the wire rule's own
/// fields, plus `item_category_ids` when the rule names a category.
///
/// # Errors
///
/// The serialiser's, which a rule built from typed fields does not meet.
pub fn authored_json(rule: &FeeRule) -> Result<serde_json::Value, serde_json::Error> {
    let mut document = serde_json::to_value(&rule.rule)?;
    if !rule.item_category_ids.is_empty()
        && let Some(fields) = document.as_object_mut()
    {
        fields.insert(
            "item_category_ids".to_owned(),
            serde_json::to_value(&rule.item_category_ids)?,
        );
    }
    Ok(document)
}

/// Reads back what [`authored_json`] wrote: the wire rule, and the categories it names.
///
/// From text, because a money amount reads only from text.
///
/// # Errors
///
/// The parser's, for text that is not a rule.
pub fn rule_from_authored(
    text: &str,
) -> Result<(PublishedFee, Vec<ItemCategoryId>), serde_json::Error> {
    /// The one field the wire rule does not have.
    #[derive(Deserialize)]
    struct Categories {
        #[serde(default)]
        item_category_ids: Vec<Ulid>,
    }
    let rule: PublishedFee = serde_json::from_str(text)?;
    let categories: Categories = serde_json::from_str(text)?;
    Ok((
        rule,
        categories
            .item_category_ids
            .into_iter()
            .map(ItemCategoryId::new)
            .collect(),
    ))
}

#[cfg(test)]
pub(crate) mod contract {
    //! The [`FeeRuleStore`] contract, run here against the in-memory store. The PostgreSQL
    //! adapter's `fee_rules` integration tests hold its SQL to the same rules on a real database,
    //! and its seam refuses what [`FeeRule::check_storable`] refuses, as this store does.

    use std::collections::BTreeMap;

    use pos_proto::fees::{FeeCode, FeeKind, PublishedFee};
    use pos_proto::ids::{FeeId, TenantId};
    use pos_proto::money::Ratio;
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

    /// A 5 % fee on every line, written at `scope` for `scope_id`, with every other field at the
    /// default the owner confirmed.
    pub(crate) fn rule(scope: FeeScope, scope_id: &str, fee_id: FeeId, code: &str) -> FeeRule {
        FeeRule {
            scope,
            scope_id: scope_id.to_owned(),
            rule: PublishedFee {
                fee_id,
                code: FeeCode::new(code),
                display_name: DisplayName::new("Service charge"),
                display_name_translations: BTreeMap::new(),
                kind: Open::from_known(FeeKind::Percent),
                rate: Ratio::percent(5).ok(),
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
            item_category_ids: Vec::new(),
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

#[cfg(test)]
mod resolving {
    //! Which rules a store runs, the rule it is sent, and what it must hold to apply it.

    use std::collections::BTreeMap;

    use pos_proto::enums::SalesChannel;
    use pos_proto::fees::{FeeCode, FeeItems, FeeKind, FeeTax, PublishedFee};
    use pos_proto::ids::{FeeId, MenuItemId, TaxClassId, TenantId};
    use pos_proto::locale::{TaxRate, TaxRateTable};
    use pos_proto::money::{CurrencyCode, Money, Ratio};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;
    use pos_proto::{MenuBook, MenuCatalog, MenuEntry};

    use super::contract::rule;
    use super::{
        CategoryItems, FeePlacement, FeeRule, FeeScope, StoreFacts, StoreFault, authored_json,
        compile, faults_at, for_store, resolve_for_store, rule_from_authored, shares_code_with,
    };
    use crate::catalog::{CatalogItem, ItemCategoryId};
    use crate::registry::EntityStatus;

    const TENANT: &str = "01J000000000000000000TENAN";
    const BRAND: &str = "01J0000000000000000000BRND";
    const OTHER_BRAND: &str = "01J000000000000000000BRND2";

    fn fee(n: u128) -> FeeId {
        FeeId::new(Ulid::from_u128(n))
    }

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(0x1000 + n))
    }

    fn class(n: u128) -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(0x2000 + n))
    }

    fn category(n: u128) -> ItemCategoryId {
        ItemCategoryId::new(Ulid::from_u128(0x3000 + n))
    }

    fn vnd(amount: i64) -> Money {
        Money::new(CurrencyCode::VND, amount)
    }

    fn placement(store: &str, brand: Option<&str>) -> FeePlacement {
        FeePlacement {
            tenant_id: TENANT.to_owned(),
            brand_id: brand.map(str::to_owned),
            store_id: store.to_owned(),
        }
    }

    /// Where each resolved rule came from, in the order the store runs them.
    fn sources(resolved: &[&FeeRule]) -> Vec<(FeeId, FeeScope, String)> {
        resolved
            .iter()
            .map(|rule| {
                (
                    rule.rule.fee_id,
                    rule.scope,
                    rule.rule.code.as_str().to_owned(),
                )
            })
            .collect()
    }

    fn with(mut rule: FeeRule, edit: impl FnOnce(&mut PublishedFee)) -> FeeRule {
        edit(&mut rule.rule);
        rule
    }

    fn catalog_item(n: u128, in_category: Option<ItemCategoryId>, active: bool) -> CatalogItem {
        CatalogItem {
            menu_item_id: item(n),
            tenant_id: TenantId::new(Ulid::from_u128(0xA)),
            name: format!("Item {n}"),
            name_translations: BTreeMap::new(),
            tax_class_id: class(1),
            item_category_id: in_category,
            item_subcategory_id: None,
            course_id: None,
            image_ref: None,
            status: if active {
                EntityStatus::Active
            } else {
                EntityStatus::Archived
            },
        }
    }

    #[test]
    fn a_store_runs_its_own_rule_over_its_brands_over_its_tenants_one_fee_at_a_time() {
        let rules = vec![
            rule(FeeScope::Tenant, TENANT, fee(1), "SERVICE"),
            rule(FeeScope::Brand, BRAND, fee(1), "SERVICE_BRAND"),
            rule(FeeScope::Store, "shop-a", fee(1), "SERVICE_SHOP"),
            rule(FeeScope::Tenant, TENANT, fee(2), "PACKAGING"),
            // Rules that reach none of the stores below.
            rule(FeeScope::Brand, OTHER_BRAND, fee(2), "PACKAGING_OTHER"),
            rule(FeeScope::Store, "shop-z", fee(3), "DELIVERY"),
            rule(FeeScope::Tenant, "another-tenant", fee(4), "ELSEWHERE"),
        ];

        let own = resolve_for_store(&rules, &placement("shop-a", Some(BRAND)));
        assert_eq!(
            sources(&own),
            vec![
                (fee(1), FeeScope::Store, "SERVICE_SHOP".to_owned()),
                (fee(2), FeeScope::Tenant, "PACKAGING".to_owned()),
            ]
        );
        let brands = resolve_for_store(&rules, &placement("shop-b", Some(BRAND)));
        assert_eq!(
            sources(&brands),
            vec![
                (fee(1), FeeScope::Brand, "SERVICE_BRAND".to_owned()),
                (fee(2), FeeScope::Tenant, "PACKAGING".to_owned()),
            ]
        );
        let no_brand = resolve_for_store(&rules, &placement("shop-c", None));
        assert_eq!(
            sources(&no_brand),
            vec![
                (fee(1), FeeScope::Tenant, "SERVICE".to_owned()),
                (fee(2), FeeScope::Tenant, "PACKAGING".to_owned()),
            ]
        );
        // A rule at no scope reaches nobody, and the order does not depend on the input's.
        let mut shuffled = rules.clone();
        shuffled.reverse();
        shuffled.push(rule(FeeScope::Unspecified, "shop-a", fee(5), "NOWHERE"));
        assert_eq!(
            sources(&resolve_for_store(
                &shuffled,
                &placement("shop-a", Some(BRAND))
            )),
            sources(&own)
        );
    }

    #[test]
    fn a_second_fee_under_the_same_code_is_found() {
        let tenant_fee = rule(FeeScope::Tenant, TENANT, fee(1), "SERVICE");
        let packaging = rule(FeeScope::Tenant, TENANT, fee(2), "PACKAGING");
        let resolved = vec![&tenant_fee, &packaging];
        let new_store_fee = rule(FeeScope::Store, "shop-a", fee(3), "SERVICE");
        assert_eq!(
            shares_code_with(&new_store_fee.rule, &resolved).map(|other| other.rule.fee_id),
            Some(fee(1))
        );
        // The same fee overridden is not a second fee, and a paused fee charges nothing.
        let override_fee = rule(FeeScope::Store, "shop-a", fee(1), "SERVICE");
        assert!(shares_code_with(&override_fee.rule, &resolved).is_none());
        let paused = with(rule(FeeScope::Tenant, TENANT, fee(1), "SERVICE"), |rule| {
            rule.active = false;
        });
        assert!(shares_code_with(&new_store_fee.rule, &[&paused, &packaging]).is_none());
    }

    #[test]
    fn a_category_compiles_into_its_active_items_merged_with_the_items_named() {
        let catalog = [
            catalog_item(1, Some(category(1)), true),
            catalog_item(2, Some(category(1)), false),
            catalog_item(3, Some(category(2)), true),
            catalog_item(4, None, true),
        ];
        let categories = CategoryItems::from_catalog(&catalog);
        let mut include = with(
            rule(FeeScope::Tenant, TENANT, fee(1), "PACKAGING"),
            |rule| {
                rule.item_scope = Open::from_known(FeeItems::Include);
                rule.menu_item_ids = vec![item(4), item(1)];
            },
        );
        include.item_category_ids = vec![category(1)];
        assert_eq!(
            compile(&include, &categories).menu_item_ids,
            vec![item(1), item(4)],
            "the category's active item, merged with the named ones, once each and in id order"
        );

        // Every line counted needs no list, so a category on such a rule compiles to nothing.
        let mut every_line = rule(FeeScope::Tenant, TENANT, fee(2), "SERVICE");
        every_line.item_category_ids = vec![category(2)];
        assert!(compile(&every_line, &categories).menu_item_ids.is_empty());
    }

    #[test]
    fn an_exclude_list_that_compiles_to_nothing_counts_every_line_and_an_include_list_none() {
        let categories = CategoryItems::from_catalog(&[catalog_item(1, Some(category(1)), false)]);
        let mut exclude = with(rule(FeeScope::Tenant, TENANT, fee(1), "SERVICE"), |rule| {
            rule.item_scope = Open::from_known(FeeItems::Exclude);
        });
        exclude.item_category_ids = vec![category(1)];
        let sent = compile(&exclude, &categories);
        assert_eq!(sent.item_scope(), Some(FeeItems::All));
        assert!(
            sent.violations().is_empty(),
            "sent as a fee on every line, not as a faulted rule the edge would drop"
        );

        let mut include = with(
            rule(FeeScope::Tenant, TENANT, fee(2), "PACKAGING"),
            |rule| {
                rule.item_scope = Open::from_known(FeeItems::Include);
            },
        );
        include.item_category_ids = vec![category(1)];
        let sent = compile(&include, &categories);
        assert_eq!(sent.item_scope(), Some(FeeItems::Include));
        assert!(
            !sent.counts_item(item(1)),
            "an include list of nothing counts no line"
        );
    }

    fn facts() -> StoreFacts {
        let menu = MenuBook::new()
            .with(
                SalesChannel::DineIn,
                MenuCatalog::new().with(MenuEntry::new(
                    item(1),
                    DisplayName::new("Pizza"),
                    Money::new(CurrencyCode::parse("JPY").expect("a currency"), 1_200),
                    class(1),
                )),
            )
            .with_fallback(MenuCatalog::new().with(MenuEntry::new(
                item(2),
                DisplayName::new("Salad"),
                Money::new(CurrencyCode::parse("JPY").expect("a currency"), 800),
                class(1),
            )));
        let tax = TaxRateTable::new().with(
            class(1),
            SalesChannel::DineIn,
            TaxRate::from_basis_points(1_000),
        );
        StoreFacts::from_document(&serde_json::json!({
            "locale": { "currency_code": "JPY", "timezone": "Asia/Tokyo" },
            "tax": serde_json::to_value(&tax).expect("encode the table"),
            "menu": serde_json::to_value(&menu).expect("encode the menu"),
        }))
    }

    #[test]
    fn a_store_cannot_apply_an_amount_in_another_currency_or_a_class_it_does_not_rate() {
        let facts = facts();
        let amount = |money: Money| {
            with(rule(FeeScope::Tenant, TENANT, fee(1), "DELIVERY"), |rule| {
                rule.kind = Open::from_known(FeeKind::AmountPerBill);
                rule.amount = Some(money);
            })
            .rule
        };
        assert_eq!(
            faults_at(&amount(vnd(15_000)), &facts),
            vec![StoreFault::CurrencyMismatch]
        );
        let yen = Money::new(CurrencyCode::parse("JPY").expect("a currency"), 300);
        assert!(faults_at(&amount(yen), &facts).is_empty());

        let taxed_at = |tax_class: TaxClassId, channels: Vec<SalesChannel>| {
            with(rule(FeeScope::Tenant, TENANT, fee(2), "SERVICE"), |rule| {
                rule.kind = Open::from_known(FeeKind::Percent);
                rule.rate = Some(Ratio::percent(5).expect("a rate"));
                rule.tax = Open::from_known(FeeTax::TaxClass);
                rule.tax_class_id = Some(tax_class);
                rule.channels = channels.into_iter().map(Open::from_known).collect();
            })
            .rule
        };
        assert!(
            faults_at(&taxed_at(class(1), Vec::new()), &facts).is_empty(),
            "every channel the store rates: dine-in, which has the class"
        );
        assert_eq!(
            faults_at(&taxed_at(class(1), vec![SalesChannel::Takeaway]), &facts),
            vec![StoreFault::TaxClassNotRated]
        );
        assert_eq!(
            faults_at(&taxed_at(class(9), Vec::new()), &facts),
            vec![StoreFault::TaxClassNotRated]
        );

        // A paused rule applies to nothing, and a store with nothing published has no fact.
        let mut paused = amount(vnd(15_000));
        paused.active = false;
        assert!(faults_at(&paused, &facts).is_empty());
        assert!(faults_at(&amount(vnd(15_000)), &StoreFacts::default()).is_empty());
    }

    #[test]
    fn a_store_with_a_faulted_rule_is_told_which() {
        let rules = vec![
            with(rule(FeeScope::Tenant, TENANT, fee(1), "DELIVERY"), |rule| {
                rule.kind = Open::from_known(FeeKind::AmountPerBill);
                rule.amount = Some(vnd(15_000));
            }),
            rule(FeeScope::Tenant, TENANT, fee(2), "SERVICE"),
        ];
        let sent = for_store(
            &rules,
            &placement("shop-a", None),
            &CategoryItems::default(),
            &facts(),
        );
        assert_eq!(sent.node.fees.len(), 2);
        assert_eq!(sent.faults, vec![(fee(1), StoreFault::CurrencyMismatch)]);
    }

    #[test]
    fn the_menu_is_read_on_the_channels_a_rule_applies_on() {
        let facts = facts();
        let dine_in = [Open::from_known(SalesChannel::DineIn)];
        let takeaway = [Open::from_known(SalesChannel::Takeaway)];
        assert_eq!(facts.menu_lists_any(&[item(1)], &[]), Some(true));
        assert_eq!(facts.menu_lists_any(&[item(2)], &[]), Some(true));
        assert_eq!(facts.menu_lists_any(&[item(1)], &dine_in), Some(true));
        assert_eq!(
            facts.menu_lists_any(&[item(1)], &takeaway),
            Some(false),
            "takeaway has no row, so it sells from the fallback"
        );
        assert_eq!(facts.menu_lists_any(&[item(2)], &takeaway), Some(true));
        assert_eq!(facts.menu_lists_any(&[item(3)], &[]), Some(false));
        assert_eq!(StoreFacts::default().menu_lists_any(&[item(1)], &[]), None);
    }

    #[test]
    fn the_stored_form_reads_back_with_its_categories() {
        let mut written = with(rule(FeeScope::Brand, BRAND, fee(1), "PACKAGING"), |rule| {
            rule.kind = Open::from_known(FeeKind::AmountPerUnit);
            rule.amount = Some(vnd(3_000));
            rule.item_scope = Open::from_known(FeeItems::Include);
            rule.menu_item_ids = vec![item(1)];
            rule.code = FeeCode::new("PACKAGING");
        });
        written.item_category_ids = vec![category(1), category(2)];
        let text = authored_json(&written)
            .expect("encode the rule")
            .to_string();
        let (rule, categories) = rule_from_authored(&text).expect("decode the rule");
        assert_eq!(rule, written.rule);
        assert_eq!(categories, written.item_category_ids);

        // A rule naming no category has no such key, and reads back with none.
        written.item_category_ids.clear();
        let document = authored_json(&written).expect("encode the rule");
        assert!(document.get("item_category_ids").is_none());
        let (_, categories) = rule_from_authored(&document.to_string()).expect("decode the rule");
        assert!(categories.is_empty());
    }
}
