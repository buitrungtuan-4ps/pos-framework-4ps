// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The authored nodes a batch publish compiles per store: tax, campaigns, inventory, reason codes,
//! the menu, permissions and the floor, and the set of every node a binary offers.

use std::sync::Arc;

use axum::response::Response;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::fees::PublishedFees;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::batch_node::{BatchNode, BatchNodePlan, BoxFuture, SameEverywhere, batch_arguments};
use super::settings_nodes::{
    CapabilitiesBatchNode, ChannelsBatchNode, OriginsBatchNode, QrBatchNode, TenderBatchNode,
    VendorPoliciesBatchNode,
};
use crate::campaigns::CampaignStore;
use crate::catalog::{CatalogStore, MenuId};
use crate::config_tree::ConfigTreeState;
use crate::fees::FeeRuleStore;
use crate::floorplan::{AreaStore, RoutingRuleStore, StationStore, TableStore};
use crate::http::campaigns::campaigns_nodes;
use crate::http::catalog::menu_nodes;
use crate::http::config_nodes::tax_nodes;
use crate::http::fees::{FeePublishOutcome, TenantFeeRules, fees_left_behind};
use crate::http::floor::floor_nodes;
use crate::http::inventory::inventory_nodes;
use crate::http::people::permissions_nodes;
use crate::http::reason_codes::reason_code_nodes;
use crate::inventory::InventoryStore;
use crate::people::{AssignmentStore, EmployeeStore, RoleTemplateStore};
use crate::reason_codes::ReasonCodeStore;
use crate::registry::RegistryStore;
use crate::tax::TaxRateStore;

/// The `tax` node: the tenant's authored rate table (ADR-0074).
struct TaxBatchNode<Tax> {
    tax_rates: Tax,
}

impl<Tax> BatchNode for TaxBatchNode<Tax>
where
    Tax: TaxRateStore + Send + Sync + 'static,
{
    fn key(&self) -> &'static str {
        "tax"
    }

    fn audit_action(&self) -> &'static str {
        "config.tax.publish"
    }

    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        _arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let (nodes, rates) = tax_nodes(&self.tax_rates, tenant_id).await?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "rates": rates }),
                nodes,
            }
            .boxed())
        })
    }
}

/// The `campaigns` node: the tenant's authored promotions (ADR-0077).
struct CampaignsBatchNode<Camp> {
    campaigns: Camp,
}

impl<Camp> BatchNode for CampaignsBatchNode<Camp>
where
    Camp: CampaignStore + Send + Sync + 'static,
{
    fn key(&self) -> &'static str {
        "campaigns"
    }

    fn audit_action(&self) -> &'static str {
        "config.campaigns.publish"
    }

    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        _arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let (nodes, campaigns) = campaigns_nodes(&self.campaigns, tenant_id).await?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "campaigns": campaigns }),
                nodes,
            }
            .boxed())
        })
    }
}

/// The `inventory` node: the tenant's ingredients, recipes and suppliers (ADR-0079).
struct InventoryBatchNode<Inv> {
    inventory: Inv,
}

impl<Inv> BatchNode for InventoryBatchNode<Inv>
where
    Inv: InventoryStore + Send + Sync + 'static,
{
    fn key(&self) -> &'static str {
        "inventory"
    }

    fn audit_action(&self) -> &'static str {
        "config.inventory.publish"
    }

    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        _arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let (nodes, summary) = inventory_nodes(&self.inventory, tenant_id).await?;
            Ok(SameEverywhere {
                summary: serde_json::to_value(summary).unwrap_or(serde_json::Value::Null),
                nodes,
            }
            .boxed())
        })
    }
}

/// The `reason_codes` node: the managed list a void or discount must cite (ADR-0115).
struct ReasonCodesBatchNode<R> {
    reason_codes: R,
}

impl<R> BatchNode for ReasonCodesBatchNode<R>
where
    R: ReasonCodeStore + Send + Sync + 'static,
{
    fn key(&self) -> &'static str {
        "reason_codes"
    }

    fn audit_action(&self) -> &'static str {
        "config.reason_codes.publish"
    }

    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        _arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let (nodes, summary) = reason_code_nodes(&self.reason_codes, tenant_id).await?;
            Ok(SameEverywhere {
                summary: serde_json::to_value(summary).unwrap_or(serde_json::Value::Null),
                nodes,
            }
            .boxed())
        })
    }
}

// The prerequisite table moved to `config_tree` when releases became its third reader: a release has
// to apply `tax` before `menu` as well as refuse a `menu` without one, and a table beside any one of
// its readers is a table the other two can drift from ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §6).
// [`BatchNode::prerequisites`] explains the four candidate rules that were examined and deliberately
// not adopted.
use crate::config_tree::prerequisites_for;

/// The `menu` node (and the `layout` node it moves with): a compiled price book (ADR-0066).
///
/// The one node with prerequisites. A menu published to a store whose tree has no `tax` produces a
/// shop that boots, syncs, shows the menu, takes the order — and then raises
/// `DomainError::TaxRateNotConfigured` at the payment screen, with nothing before that saying a
/// word (ADR-0122 §7). `locale` is the same shape of trap one layer down: without it the store has
/// no currency, timezone or business-date cutoff to price and close against.
///
/// Its write brings each member's `fees` node along, composed on the member's tree, as the
/// single-store route does: the fee rules and the registry are what that is resolved from.
struct MenuBatchNode<Cat, Fee, Reg> {
    catalog: Cat,
    fee_rules: Fee,
    registry: Reg,
}

/// [`MenuBatchNode`]'s plan: one book for every member, and the tenant's fee rules each member's
/// `fees` node is composed from, on its own tree, when its write lands.
struct MenuBatchPlan {
    nodes: Vec<(String, serde_json::Value)>,
    summary: serde_json::Value,
    /// `None` when the rules, the registry or the catalog did not answer: the menu still goes out,
    /// and each member's report says its fees did not.
    fees: Option<TenantFeeRules>,
}

impl BatchNodePlan for MenuBatchPlan {
    fn summary(&self) -> serde_json::Value {
        self.summary.clone()
    }

    fn compile_for(
        &self,
        _store_id: StoreId,
    ) -> BoxFuture<'_, Result<Vec<(String, serde_json::Value)>, Response>> {
        Box::pin(core::future::ready(Ok(self.nodes.clone())))
    }

    fn compose_on(
        &self,
        store_id: StoreId,
        tree: Option<&ConfigTreeState>,
        nodes: &mut Vec<(String, serde_json::Value)>,
    ) -> Option<String> {
        let Some(fees) = self.fees.as_ref().and_then(|fees| fees.for_store(store_id)) else {
            return Some(fees_left_behind(FeePublishOutcome::Failed, &[]));
        };
        let on_tree = fees.on_tree(tree, nodes);
        if let Some(node) = on_tree.node() {
            nodes.push((PublishedFees::NODE.to_owned(), node.clone()));
        }
        on_tree.note()
    }
}

/// A `menu` batch's arguments: which menu to compile.
#[derive(Debug, Clone, Deserialize)]
struct MenuBatchArguments {
    menu_id: String,
}

impl<Cat, Fee, Reg> BatchNode for MenuBatchNode<Cat, Fee, Reg>
where
    Cat: CatalogStore + Send + Sync + 'static,
    Fee: FeeRuleStore + Send + Sync + 'static,
    Reg: RegistryStore + Send + Sync + 'static,
{
    fn key(&self) -> &'static str {
        "menu"
    }

    fn audit_action(&self) -> &'static str {
        "catalog.publish"
    }

    fn prerequisites(&self) -> &'static [&'static str] {
        prerequisites_for(self.key())
    }

    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let parsed: MenuBatchArguments = batch_arguments(self.key(), arguments)?;
            let menu_id = match parse_ulid_fields([("menu_id", &parsed.menu_id)]) {
                Ok([menu_id]) => MenuId::new(menu_id),
                Err(refusal) => return Err(refusal),
            };
            let nodes = menu_nodes(&self.catalog, tenant_id, menu_id).await?;
            let fees =
                TenantFeeRules::load(&self.fee_rules, &self.registry, &self.catalog, tenant_id)
                    .await;
            Ok(Box::new(MenuBatchPlan {
                nodes,
                summary: serde_json::json!({ "menu_id": menu_id.to_string() }),
                fees,
            }) as Box<dyn BatchNodePlan + 'a>)
        })
    }
}

/// The `permissions` node: each member's own roster, compiled per store (ADR-0070).
struct PermissionsBatchNode<P> {
    people: P,
}

/// [`PermissionsBatchNode`]'s plan — it holds no compiled document, because there is not one.
struct PermissionsBatchPlan<'a, P> {
    people: &'a P,
    tenant_id: TenantId,
}

impl<P> BatchNodePlan for PermissionsBatchPlan<'_, P>
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Send + Sync,
{
    fn summary(&self) -> serde_json::Value {
        serde_json::json!({ "compiled": "per-store" })
    }

    fn compile_for(
        &self,
        store_id: StoreId,
    ) -> BoxFuture<'_, Result<Vec<(String, serde_json::Value)>, Response>> {
        Box::pin(async move {
            permissions_nodes(self.people, self.tenant_id, store_id)
                .await
                .map(|(nodes, _staff)| nodes)
        })
    }
}

impl<P> BatchNode for PermissionsBatchNode<P>
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Send + Sync + 'static,
{
    fn key(&self) -> &'static str {
        "permissions"
    }

    fn audit_action(&self) -> &'static str {
        "permissions.publish"
    }

    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        _arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(core::future::ready(Ok(Box::new(PermissionsBatchPlan {
            people: &self.people,
            tenant_id,
        })
            as Box<dyn BatchNodePlan + 'a>)))
    }
}

/// The `floor` and `stations` nodes: each member's own plan, compiled per store (ADR-0072).
struct FloorBatchNode<F> {
    floor: F,
}

/// [`FloorBatchNode`]'s plan — per store, for the same reason as [`PermissionsBatchPlan`].
struct FloorBatchPlan<'a, F> {
    floor: &'a F,
    tenant_id: TenantId,
}

impl<F> BatchNodePlan for FloorBatchPlan<'_, F>
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Send + Sync,
{
    fn summary(&self) -> serde_json::Value {
        serde_json::json!({ "compiled": "per-store" })
    }

    fn compile_for(
        &self,
        store_id: StoreId,
    ) -> BoxFuture<'_, Result<Vec<(String, serde_json::Value)>, Response>> {
        Box::pin(async move {
            floor_nodes(self.floor, self.tenant_id, store_id)
                .await
                .map(|(nodes, _summary)| nodes)
        })
    }
}

impl<F> BatchNode for FloorBatchNode<F>
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Send + Sync + 'static,
{
    fn key(&self) -> &'static str {
        "floor"
    }

    fn audit_action(&self) -> &'static str {
        "floor.publish"
    }

    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        _arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(core::future::ready(Ok(Box::new(FloorBatchPlan {
            floor: &self.floor,
            tenant_id,
        })
            as Box<dyn BatchNodePlan + 'a>)))
    }
}

/// Every node kind a batch can publish, in the order the console offers them.
///
/// The one place the thirteen are assembled. `main.rs` calls this with the same store handles the
/// single-store routers already hold, so a node cannot be wired into a batch without being wired
/// into its own route first — and the three ADR-0122 §4 excludes (`locale`, `store_profile`, the
/// generic level write) are absent by simply never appearing here.
///
/// The fee rules and the registry are the menu's: a menu publish brings each store's `fees` node
/// along, as the single-store route does.
#[expect(
    clippy::too_many_arguments,
    reason = "one authoring seam per node kind, plus the two a menu's fees are resolved from; \
              bundling them would move the same list one line up and hide which store `main.rs` \
              hands to which node"
)]
pub fn batch_nodes<Cat, Tax, Camp, Inv, R, P, F, Fee, Reg>(
    catalog: Cat,
    tax_rates: Tax,
    campaigns: Camp,
    inventory: Inv,
    reason_codes: R,
    people: P,
    floor: F,
    fee_rules: Fee,
    registry: Reg,
) -> Vec<Arc<dyn BatchNode>>
where
    Cat: CatalogStore + Send + Sync + 'static,
    Tax: TaxRateStore + Send + Sync + 'static,
    Camp: CampaignStore + Send + Sync + 'static,
    Inv: InventoryStore + Send + Sync + 'static,
    R: ReasonCodeStore + Send + Sync + 'static,
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Send + Sync + 'static,
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Send + Sync + 'static,
    Fee: FeeRuleStore + Send + Sync + 'static,
    Reg: RegistryStore + Send + Sync + 'static,
{
    vec![
        Arc::new(MenuBatchNode {
            catalog,
            fee_rules,
            registry,
        }),
        Arc::new(TaxBatchNode { tax_rates }),
        Arc::new(CampaignsBatchNode { campaigns }),
        Arc::new(InventoryBatchNode { inventory }),
        Arc::new(ReasonCodesBatchNode { reason_codes }),
        Arc::new(PermissionsBatchNode { people }),
        Arc::new(FloorBatchNode { floor }),
        Arc::new(CapabilitiesBatchNode),
        Arc::new(ChannelsBatchNode),
        Arc::new(TenderBatchNode),
        Arc::new(OriginsBatchNode),
        Arc::new(QrBatchNode),
        Arc::new(VendorPoliciesBatchNode),
    ]
}
