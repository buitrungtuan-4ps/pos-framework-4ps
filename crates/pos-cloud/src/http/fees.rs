// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Fees: the rules a tenant authors under `/admin/fees`, their preview on a sample bill, and their
//! publish to each store's `fees` node ([ADR-0159](../../../../docs/adr/0159-a-fee-is-configuration.md)).
//!
//! The state, the router and the helpers every handler shares are here; the handlers are in a
//! module per job.

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use pos_cloud_api::response::service_unavailable;
use pos_proto::WireEnum;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::FeeId;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::catalog::CatalogStore;
use crate::config_tree::ConfigTreeStore;
use crate::fees::{FeeRule, FeeRuleStore, FeeRuleStoreError, FeeScope};
use crate::registry::RegistryStore;

mod delete;
mod layout;
mod preview;
mod publish;
mod reads;
mod rule;
mod write;

use delete::admin_delete_fee;
use preview::admin_preview_fees;
use publish::admin_publish_fees;
use reads::{admin_effective_fees, admin_list_fees};
use write::{admin_create_fee, admin_put_fee};

// What the menu publish and the batch fan-out use of a fee publish, and the handlers the console's
// OpenAPI document names.
pub(crate) use delete::__path_admin_delete_fee;
pub(crate) use preview::__path_admin_preview_fees;
pub(crate) use publish::__path_admin_publish_fees;
pub(super) use publish::{FeePublishResult, TenantFeeRules, fees_left_behind};
pub(crate) use reads::{__path_admin_effective_fees, __path_admin_list_fees};
pub(crate) use write::{__path_admin_create_fee, __path_admin_put_fee};

// --- Fees (`/admin/fees`, ADR-0159 decision 1) ---------------------------------------------------

/// The collaborators the fee routes need: the rules; the registry that says which stores a rule
/// reaches; the catalog a rule's items, categories and tax class are checked and compiled
/// against; the config trees each store's `fees` node is written onto; and the admin, clock and
/// audit every route carries.
#[derive(Clone)]
struct FeesState<F, Rg, Cat, Cfg, A, C> {
    fees: F,
    registry: Rg,
    catalog: Cat,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the fee sub-router ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)
/// decision 1).
///
/// A rule is written once, at the tenant, a brand or one store, and every store it reaches is
/// republished in the same request: each store's rules are resolved, per fee the most specific
/// scope's ([`crate::fees::resolve_for_store`]), compiled ([`crate::fees::compile`]) and written
/// whole as the `fees` node on its Store layer, as the `integrations` node is.
///
/// `POST /admin/fees/preview` assembles a sample bill at one store with rules in place before they
/// are written, and writes nothing.
///
/// A rule's rate and amount are prices (T2), so the reads, the preview among them, are behind
/// [`ConsolePermission::ReadRevenue`], as a menu's placements are (production-readiness S5). A
/// write publishes, so it is behind [`ConsolePermission::PublishConfig`], like every node publish.
pub fn fees_router<F, Rg, Cat, Cfg, A, C>(
    fees: F,
    registry: Rg,
    catalog: Cat,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    F: FeeRuleStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/fees",
            get(admin_list_fees::<F, Rg, Cat, Cfg, A, C>)
                .post(admin_create_fee::<F, Rg, Cat, Cfg, A, C>)
                .put(admin_put_fee::<F, Rg, Cat, Cfg, A, C>)
                .delete(admin_delete_fee::<F, Rg, Cat, Cfg, A, C>),
        )
        .route(
            "/admin/fees/effective",
            get(admin_effective_fees::<F, Rg, Cat, Cfg, A, C>),
        )
        .route(
            "/admin/fees/publish",
            post(admin_publish_fees::<F, Rg, Cat, Cfg, A, C>),
        )
        .route(
            "/admin/fees/preview",
            post(admin_preview_fees::<F, Rg, Cat, Cfg, A, C>),
        )
        .with_state(FeesState {
            fees,
            registry,
            catalog,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// The tenant a fee read is for.
#[derive(Debug, Clone, Deserialize)]
struct FeesTenantQuery {
    tenant_id: String,
}

/// One rule as the console lists it.
#[derive(Debug, Clone, serde::Serialize)]
struct FeeRuleView {
    /// `FEE_SCOPE_TENANT`, `_BRAND` or `_STORE`.
    scope: &'static str,
    /// The tenant, brand or store it was written for.
    scope_id: String,
    /// The rule as written: the wire rule's fields, and the categories it names.
    rule: serde_json::Value,
    update_time: pos_proto::Timestamp,
    /// The console admin who last wrote it, by id.
    updated_by: String,
}

impl FeeRuleView {
    pub(in crate::http) fn of(rule: &FeeRule) -> Option<Self> {
        Some(Self {
            scope: rule.scope.as_wire(),
            scope_id: rule.scope_id.clone(),
            rule: crate::fees::authored_json(rule).ok()?,
            update_time: rule.update_time,
            updated_by: rule.updated_by.clone(),
        })
    }
}

/// What the trail keeps of a fee write or publish: how many stores it reached and how each went.
/// Counts only — never a rule's code, name, rate or amount, which the rule store keeps.
fn fee_publish_counts(results: &[FeePublishResult]) -> serde_json::Value {
    let count = |outcome: &str| {
        results
            .iter()
            .filter(|result| result.outcome == outcome)
            .count()
    };
    serde_json::json!({
        "stores": results.len(),
        "applied": count("FEE_PUBLISH_APPLIED"),
        "unchanged": count("FEE_PUBLISH_UNCHANGED"),
        "refused": count("FEE_PUBLISH_REFUSED"),
        "failed": count("FEE_PUBLISH_FAILED"),
    })
}

/// What the audit trail names a rule by: its fee, its scope and the id it was written for.
fn fee_audit_id(fee_id: FeeId, scope: FeeScope, scope_id: &str) -> String {
    format!("{fee_id} {scope} {scope_id}")
}

/// The `503` for a fee rule store that did not answer.
fn fee_store_error_response(error: &FeeRuleStoreError) -> Response {
    tracing::error!(%error, "the fee rule store failed");
    service_unavailable("fees")
}
