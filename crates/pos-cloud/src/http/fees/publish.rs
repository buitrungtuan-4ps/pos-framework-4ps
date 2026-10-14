// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/fees/publish`: a tenant's rules composed into each store's `fees` node.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::fees::PublishedFees;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use serde::Deserialize;

use super::layout::{FeeCatalog, FeeLayout};
use super::{FeesState, fee_publish_counts};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::config_tree::merge::merge_layers;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::fees::{CategoryItems, FeePlacement, FeeRule, FeeRuleStore, StoreFacts};
use crate::http::{layer_with_nodes, not_found, publish_config_nodes_with};
use crate::registry::RegistryStore;
use crate::version::records;

/// A `POST /admin/fees/publish` body: the tenant, and one store or, without one, every store.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct PublishFeesRequest {
    tenant_id: String,
    #[serde(default)]
    store_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/admin/fees/publish",
    request_body(
        description = "The tenant, and optionally one store. Without a store, every store of the \
                       tenant is republished",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Each store's rules resolved, compiled and republished, with \
                                      the same per-store outcomes as a write"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The fee rule store, the registry or the catalog is \
                                      unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fees",
)]
/// `POST /admin/fees/publish` — republishes the fees of one store, or of every store of the tenant.
///
/// A write republishes every store it reaches by itself. What it cannot see is a change elsewhere
/// that changes what a store is sent: a store given another brand, or an item added to or taken
/// out of a category a rule names, which is compiled into items when the fees are published. A
/// menu publish brings a store's fees along with it; this publishes them without one.
pub(super) async fn admin_publish_fees<F, Rg, Cat, Cfg, A, C>(
    State(state): State<FeesState<F, Rg, Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishFeesRequest>,
) -> Response
where
    F: FeeRuleStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let layout = match FeeLayout::load(&state.registry, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let stores: Vec<StoreId> = match request.store_id.as_deref().map(str::trim) {
        None | Some("") => layout.stores.iter().map(|store| store.store_id).collect(),
        Some(text) => {
            let store_id = match parse_ulid_fields([("store_id", text)]) {
                Ok([store_id]) => StoreId::new(store_id),
                Err(refusal) => return refusal,
            };
            if layout.placement(store_id).is_none() {
                return not_found("store");
            }
            vec![store_id]
        }
    };
    let catalog = match FeeCatalog::load(&state.catalog, tenant_id).await {
        Ok(catalog) => catalog,
        Err(refusal) => return refusal,
    };
    let results = publish_fees_to(&state, &layout, &stores, &catalog.categories).await;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "fee.publish",
        "tenant",
        &tenant_id.to_string(),
        None,
        Some(fee_publish_counts(&results)),
    )
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({ "stores": results })),
    )
        .into_response()
}

/// How one store's fee publish went.
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::http) struct FeePublishResult {
    store_id: String,
    /// `FEE_PUBLISH_APPLIED`, `FEE_PUBLISH_UNCHANGED`, `FEE_PUBLISH_REFUSED` or
    /// `FEE_PUBLISH_FAILED`.
    pub(super) outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_version_id: Option<String>,
    /// For a refused store: each rule it cannot apply, and why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    faults: Vec<FeeFaultView>,
}

impl FeePublishResult {
    /// An outcome with nothing more to say.
    pub(in crate::http) fn of(store_id: StoreId, outcome: &'static str) -> Self {
        Self {
            store_id: store_id.to_string(),
            outcome,
            config_version_id: None,
            faults: Vec::new(),
        }
    }
}

/// One rule a store cannot apply, as a refused publish reports it: by id, never by name.
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::http) struct FeeFaultView {
    fee_id: String,
    /// `CURRENCY_MISMATCH` or `TAX_RATE_NOT_CONFIGURED`.
    reason: &'static str,
}

/// Republishes each store's resolved rules as the `fees` node on its Store layer.
///
/// One store failing does not stop the others, and a failed one is retried with
/// `POST /admin/fees/publish`. A store whose layer already holds what it resolves to is not given
/// a new version, so a write that changes nothing for a store does not make its tills reload. A
/// store that cannot apply one of its rules is not published to at all and keeps the rules it has:
/// sending the others alone would stop charging a fee nobody removed, and sending them all would
/// fail every bill the faulted rule applies to.
///
/// The rules are read once, before the stores are written, as a settings publish reads its values.
pub(super) async fn publish_fees_to<F, Rg, Cat, Cfg, A, C>(
    state: &FeesState<F, Rg, Cat, Cfg, A, C>,
    layout: &FeeLayout,
    stores: &[StoreId],
    categories: &CategoryItems,
) -> Vec<FeePublishResult>
where
    F: FeeRuleStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let rules = match state.fees.list(layout.tenant_id).await {
        Ok(rules) => rules,
        Err(error) => {
            tracing::error!(%error, "the fee rules could not be read for a fee publish");
            return stores
                .iter()
                .map(|store_id| FeePublishResult::of(*store_id, "FEE_PUBLISH_FAILED"))
                .collect();
        }
    };
    let mut results = Vec::with_capacity(stores.len());
    for store_id in stores {
        results.push(match layout.placement(*store_id) {
            Some(placement) => {
                let fees = StoreFeeRules {
                    rules: &rules,
                    placement,
                    categories,
                };
                publish_fees_to_store(state, layout.tenant_id, *store_id, &fees).await
            }
            None => FeePublishResult::of(*store_id, "FEE_PUBLISH_FAILED"),
        });
    }
    results
}

/// A tenant's fee rules and what they are resolved and compiled against, read once before any store
/// is written, for a publish that brings a store's `fees` along with its `menu` (ADR-0159): a rule
/// naming a category is compiled into the items the category holds, and a menu publish is what
/// adds an item to a store.
pub(in crate::http) struct TenantFeeRules {
    rules: Vec<FeeRule>,
    layout: FeeLayout,
    categories: CategoryItems,
}

impl TenantFeeRules {
    /// Reads the rules, the registry and the catalog's categories, or `None`, logged, when one of
    /// them does not answer. A menu publish goes out without the store's fees then, and the store
    /// keeps the ones it has until they are published again.
    pub(in crate::http) async fn load<F, Rg, Cat>(
        fees: &F,
        registry: &Rg,
        catalog: &Cat,
        tenant_id: TenantId,
    ) -> Option<Self>
    where
        F: FeeRuleStore,
        Rg: RegistryStore,
        Cat: CatalogStore,
    {
        let rules = match fees.list(tenant_id).await {
            Ok(rules) => rules,
            Err(error) => {
                tracing::error!(
                    %error,
                    "the fee rules could not be read to publish them with a menu"
                );
                return None;
            }
        };
        let layout = FeeLayout::load(registry, tenant_id).await.ok()?;
        let items = match catalog.list_items(tenant_id).await {
            Ok(items) => records(items),
            Err(error) => {
                tracing::error!(
                    %error,
                    "the catalog could not be read to publish the fees with a menu"
                );
                return None;
            }
        };
        Some(Self {
            rules,
            layout,
            categories: CategoryItems::from_catalog(&items),
        })
    }

    /// One store's rules, or `None` for a store the tenant's registry does not list, whose rules
    /// cannot be resolved without knowing its brand.
    pub(in crate::http) fn for_store(&self, store_id: StoreId) -> Option<StoreFeeRules<'_>> {
        Some(StoreFeeRules {
            rules: &self.rules,
            placement: self.layout.placement(store_id)?,
            categories: &self.categories,
        })
    }
}

/// What a batch member's report says when its menu went out without the store's fees: the fee
/// publish's own outcome token and, for a refusal, each rule the store cannot apply and why.
pub(in crate::http) fn fees_left_behind(outcome: &str, faults: &[FeeFaultView]) -> String {
    let named: Vec<String> = faults
        .iter()
        .map(|fault| format!("{} {}", fault.fee_id, fault.reason))
        .collect();
    let why = if named.is_empty() {
        String::new()
    } else {
        format!(" ({})", named.join(", "))
    };
    format!("the menu is published, but not the store's fees, which it keeps: {outcome}{why}")
}

/// One store's fee rules as a write composes its `fees` node: the tenant's rules, where the store
/// sits, and what each category holds. All three are read before the store's tree is, so composing
/// the node needs nothing but the tree each attempt of the write reads.
pub(in crate::http) struct StoreFeeRules<'a> {
    rules: &'a [FeeRule],
    placement: FeePlacement,
    categories: &'a CategoryItems,
}

impl StoreFeeRules<'_> {
    /// The store's `fees` node on `tree` as a write will leave it with `writing` set on its Store
    /// layer: the rules are resolved, compiled and checked against what the store will hold once
    /// the write lands, and compared with the node its Store layer holds now.
    pub(in crate::http) fn on_tree(
        &self,
        tree: Option<&ConfigTreeState>,
        writing: &[(String, serde_json::Value)],
    ) -> FeesOnTree {
        let store_layer = layer_with_nodes(tree, ConfigLevel::Store, writing);
        let empty = serde_json::Value::Object(serde_json::Map::new());
        let layer = |level| tree.map_or(&empty, |tree| tree.layer(level));
        let document = merge_layers(&[
            layer(ConfigLevel::Tenant),
            layer(ConfigLevel::Brand),
            &store_layer,
            layer(ConfigLevel::Device),
        ]);
        let sent = crate::fees::for_store(
            self.rules,
            &self.placement,
            self.categories,
            &StoreFacts::from_document(&document),
        );
        if !sent.faults.is_empty() {
            return FeesOnTree::Refused(
                sent.faults
                    .iter()
                    .map(|(fee_id, fault)| FeeFaultView {
                        fee_id: fee_id.to_string(),
                        reason: fault.as_wire(),
                    })
                    .collect(),
            );
        }
        let Ok(node) = serde_json::to_value(&sent.node) else {
            return FeesOnTree::Unwritable;
        };
        let held = tree.and_then(|tree| tree.layer(ConfigLevel::Store).get(PublishedFees::NODE));
        // No node and no rule is the same answer at the edge, so it is not a change.
        if held.map_or(sent.node.fees.is_empty(), |held| *held == node) {
            FeesOnTree::Unchanged
        } else {
            FeesOnTree::Changed(node)
        }
    }
}

/// What a store's `fees` node comes to on the tree a write reads.
pub(in crate::http) enum FeesOnTree {
    /// The node to write: the store resolves to something its Store layer does not hold.
    Changed(serde_json::Value),
    /// The Store layer holds it already, or holds no node where the store runs no rule.
    Unchanged,
    /// A rule the store cannot apply, and why. Nothing is written, and the store keeps the rules
    /// it has: sending the others alone would stop charging a fee nobody removed, and sending them
    /// all would fail every bill the faulted rule applies to.
    Refused(Vec<FeeFaultView>),
    /// The node would not serialise.
    Unwritable,
}

impl FeesOnTree {
    /// The node a write sets, which is there only when it changed.
    pub(in crate::http) fn node(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Changed(node) => Some(node),
            Self::Unchanged | Self::Refused(_) | Self::Unwritable => None,
        }
    }

    /// What a batch member's report says of the store's fees, when the write leaves them behind.
    pub(in crate::http) fn note(&self) -> Option<String> {
        match self {
            Self::Changed(_) | Self::Unchanged => None,
            Self::Refused(faults) => Some(fees_left_behind("FEE_PUBLISH_REFUSED", faults)),
            Self::Unwritable => Some(fees_left_behind("FEE_PUBLISH_FAILED", &[])),
        }
    }

    /// The store's outcome, given the version the write produced, or `None` when it wrote nothing.
    pub(in crate::http) fn result(
        &self,
        store_id: StoreId,
        written: Option<ConfigVersionId>,
    ) -> FeePublishResult {
        match (self, written) {
            (Self::Changed(_), Some(version)) => FeePublishResult {
                config_version_id: Some(version.to_string()),
                ..FeePublishResult::of(store_id, "FEE_PUBLISH_APPLIED")
            },
            (Self::Unchanged, _) => FeePublishResult::of(store_id, "FEE_PUBLISH_UNCHANGED"),
            (Self::Refused(faults), _) => {
                tracing::warn!(
                    %store_id,
                    rules = faults.len(),
                    "a store cannot apply some of its fee rules, so it keeps the ones it has"
                );
                FeePublishResult {
                    faults: faults.clone(),
                    ..FeePublishResult::of(store_id, "FEE_PUBLISH_REFUSED")
                }
            }
            (Self::Unwritable, _) => {
                tracing::error!("could not serialise a store's fees node");
                FeePublishResult::of(store_id, "FEE_PUBLISH_FAILED")
            }
            (Self::Changed(_), None) => FeePublishResult::of(store_id, "FEE_PUBLISH_FAILED"),
        }
    }
}

/// Writes one store's resolved rules onto its Store layer, unless it already holds them or cannot
/// apply one of them.
///
/// The node is composed inside [`publish_config_nodes_with`]'s retry, on the tree each attempt
/// reads, so the rules are checked against the tree the write lands on: a `tax`, `locale` or
/// `menu` publish landing between a read and its write is read again, not written over. An attempt
/// that finds the store holds its rules already, or cannot apply one, abandons the write, as a
/// refusal abandons it, and what it composed is the outcome.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every node \
              publish's closure carries — and here it only abandons a write that would change \
              nothing or that the store cannot take; the outcome is what the closure composed"
)]
async fn publish_fees_to_store<F, Rg, Cat, Cfg, A, C>(
    state: &FeesState<F, Rg, Cat, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
    fees: &StoreFeeRules<'_>,
) -> FeePublishResult
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let mut composed = None;
    let written = publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        |tree| {
            let on_tree = fees.on_tree(tree, &[]);
            let nodes = on_tree
                .node()
                .map(|node| vec![(PublishedFees::NODE.to_owned(), node.clone())]);
            composed = Some(on_tree);
            nodes.ok_or_else(|| StatusCode::NO_CONTENT.into_response())
        },
    )
    .await;
    match (composed, written) {
        (Some(on_tree), Ok(version)) => on_tree.result(store_id, Some(version)),
        (Some(on_tree), Err(_)) if on_tree.node().is_none() => on_tree.result(store_id, None),
        _ => {
            tracing::warn!(
                %store_id,
                "a store's configuration could not be read for a fee publish, or refused it"
            );
            FeePublishResult::of(store_id, "FEE_PUBLISH_FAILED")
        }
    }
}
