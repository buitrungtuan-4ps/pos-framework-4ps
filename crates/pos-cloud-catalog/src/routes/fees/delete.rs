// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `DELETE /admin/fees`: a rule removed, and the stores it reached republished.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::StoresPublished;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{FeeId, TenantId};
use serde::Deserialize;

use super::layout::{FeeCatalog, FeeLayout};
use super::publish::FeePublishResult;
use super::publish::publish_fees_to;
use super::rule::parse_fee_scope;
use super::write::{FeeWrite, check_fee_write};
use super::{FeesState, fee_audit_id, fee_publish_counts, fee_store_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::config_tree::ConfigTreeStore;
use crate::fees::{FeeRule, FeeRuleSlot, FeeRuleStore};
use crate::registry::RegistryStore;
use crate::routes::not_found;

/// The rule a `DELETE /admin/fees` removes.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct DeleteFeeQuery {
    tenant_id: String,
    scope: String,
    scope_id: String,
    fee_id: String,
}

#[utoipa::path(
    delete,
    path = "/admin/fees",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
        ("scope" = String, Query, description = "FEE_SCOPE_TENANT, _BRAND or _STORE"),
        ("scope_id" = String, Query, description = "The tenant, brand or store the rule was written for"),
        ("fee_id" = String, Query, description = "The fee"),
    ),
    responses(
        (status = 200, body = StoresPublished<FeePublishResult>, description = "Removed, and every store it reached republished, with the \
                                      same per-store outcomes as a write. A store that ran it runs \
                                      the fee's broader rule if there is one, and otherwise no \
                                      longer charges it"),
        (status = 400, description = "An id is not a ULID or the scope is not a scope", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "No rule is written there", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "A store would then run the fee's broader rule, and could not \
                                      apply it, or runs another fee with its code", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The fee rule store, the registry, the catalog or the config \
                                      tree is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "fees",
)]
/// `DELETE /admin/fees` — removes a rule at one scope and republishes every store it reached.
pub(super) async fn admin_delete_fee<F, Rg, Cat, Cfg, A, C>(
    State(state): State<FeesState<F, Rg, Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<DeleteFeeQuery>,
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
    let (tenant_id, scope_id, fee_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("scope_id", &query.scope_id),
        ("fee_id", &query.fee_id),
    ]) {
        Ok([tenant_id, scope_id, fee_id]) => (
            TenantId::new(tenant_id),
            scope_id.to_string(),
            FeeId::new(fee_id),
        ),
        Err(refusal) => return refusal,
    };
    let scope = match parse_fee_scope(&query.scope) {
        Ok(scope) => scope,
        Err(refusal) => return refusal,
    };
    let slot = FeeRuleSlot {
        scope,
        scope_id: &scope_id,
        fee_id,
    };
    let before = match state.fees.list(tenant_id).await {
        Ok(rules) => rules,
        Err(error) => return fee_store_error_response(&error),
    };
    if !before.iter().any(|held| held.slot() == slot) {
        return not_found("fee rule");
    }
    let after: Vec<FeeRule> = before
        .iter()
        .filter(|held| held.slot() != slot)
        .cloned()
        .collect();
    let layout = match FeeLayout::load(&state.registry, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let catalog = match FeeCatalog::load(&state.catalog, tenant_id).await {
        Ok(catalog) => catalog,
        Err(refusal) => return refusal,
    };
    let reached = layout.reached_by(scope, &scope_id);
    let write = FeeWrite {
        before: &before,
        after: &after,
        written: None,
    };
    if let Err(refusal) =
        check_fee_write(&state, &layout, &reached, &write, &catalog.categories).await
    {
        return refusal;
    }
    if let Err(error) = state.fees.delete(tenant_id, slot).await {
        return fee_store_error_response(&error);
    }
    let results = publish_fees_to(&state, &layout, &reached, &catalog.categories).await;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "fee.delete",
        "fee_rule",
        &fee_audit_id(fee_id, scope, &scope_id),
        None,
        Some(fee_publish_counts(&results)),
    )
    .await;
    (StatusCode::OK, Json(StoresPublished { stores: results })).into_response()
}
