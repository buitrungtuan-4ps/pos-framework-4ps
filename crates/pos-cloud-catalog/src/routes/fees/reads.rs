// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/fees` and `GET /admin/fees/effective`: the rules as written, and as a store resolves them.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::WireEnum;
use pos_proto::determinism::ClockSource;
use pos_proto::fees::PublishedFee;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::layout::{FeeCatalog, FeeLayout};
use super::write::fee_store_facts;
use super::{FeeRuleView, FeesState, FeesTenantQuery, fee_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::config_tree::ConfigTreeStore;
use crate::fees::{FeeRuleStore, StoreFault};
use crate::registry::RegistryStore;
use crate::routes::not_found;

#[utoipa::path(
    get,
    path = "/admin/fees",
    params(
        ("tenant_id" = String, Query, description = "The tenant whose rules to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "Every rule the tenant has written, at every scope: the scope \
                                      and the id it was written for, the rule with the item \
                                      categories it names, and who last wrote it when"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The fee rule store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "fees",
)]
/// `GET /admin/fees` — the rules a tenant has written, at every scope.
pub(super) async fn admin_list_fees<F, Rg, Cat, Cfg, A, C>(
    State(state): State<FeesState<F, Rg, Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<FeesTenantQuery>,
) -> Response
where
    F: FeeRuleStore + Clone + Send + Sync + 'static,
    Rg: Clone + Send + Sync + 'static,
    Cat: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.fees.list(tenant_id).await {
        Ok(rules) => {
            let views: Vec<FeeRuleView> = rules.iter().filter_map(FeeRuleView::of).collect();
            (StatusCode::OK, Json(serde_json::json!({ "rules": views }))).into_response()
        }
        Err(error) => fee_store_error_response(&error),
    }
}

/// The store whose fees to read.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct EffectiveFeesQuery {
    tenant_id: String,
    store_id: String,
}

/// One fee as one store runs it, and where its rule comes from.
#[derive(Debug, Clone, serde::Serialize)]
struct EffectiveFee {
    fee_id: String,
    /// The scope of the rule in force, and what it was written for.
    scope: &'static str,
    scope_id: String,
    /// The rule as written: the wire rule's fields, and the categories it names.
    rule: serde_json::Value,
    /// The rule as the store is sent it, its categories compiled into items.
    published: PublishedFee,
    /// Why the store cannot apply it, if it cannot.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    faults: Vec<&'static str>,
}

#[utoipa::path(
    get,
    path = "/admin/fees/effective",
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "Every fee the store runs, in the order it is sent them: the \
                                      scope and id its rule was written at, the rule as written, \
                                      the rule as the store is sent it with its categories \
                                      compiled into items, and any reason the store cannot apply \
                                      it. publishable is false when one cannot be applied, in \
                                      which case a publish leaves the store as it is"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The fee rule store, the registry, the catalog or the config \
                                      tree is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "fees",
)]
/// `GET /admin/fees/effective` — the fees one store runs, and why.
pub(super) async fn admin_effective_fees<F, Rg, Cat, Cfg, A, C>(
    State(state): State<FeesState<F, Rg, Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<EffectiveFeesQuery>,
) -> Response
where
    F: FeeRuleStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let layout = match FeeLayout::load(&state.registry, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let Some(placement) = layout.placement(store_id) else {
        return not_found("store");
    };
    let rules = match state.fees.list(tenant_id).await {
        Ok(rules) => rules,
        Err(error) => return fee_store_error_response(&error),
    };
    let catalog = match FeeCatalog::load(&state.catalog, tenant_id).await {
        Ok(catalog) => catalog,
        Err(refusal) => return refusal,
    };
    let facts = match fee_store_facts(&state.config_trees, tenant_id, store_id).await {
        Ok(facts) => facts,
        Err(refusal) => return refusal,
    };
    let fees: Vec<EffectiveFee> = crate::fees::resolve_for_store(&rules, &placement)
        .into_iter()
        .filter_map(|rule| {
            let published = crate::fees::compile(rule, &catalog.categories);
            Some(EffectiveFee {
                fee_id: published.fee_id.to_string(),
                scope: rule.scope.as_wire(),
                scope_id: rule.scope_id.clone(),
                rule: crate::fees::authored_json(rule).ok()?,
                faults: crate::fees::faults_at(&published, &facts)
                    .into_iter()
                    .map(StoreFault::as_wire)
                    .collect(),
                published,
            })
        })
        .collect();
    let publishable = fees.iter().all(|fee| fee.faults.is_empty());
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "store_id": store_id.to_string(),
            "fees": fees,
            "publishable": publishable,
        })),
    )
        .into_response()
}
