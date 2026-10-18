// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST` and `PUT /admin/fees`: a rule written, checked against every store it reaches, and republished.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::fees::{FeeItems, PublishedFee};
use pos_proto::ids::{FeeId, StoreId, TenantId};

use super::layout::{FeeCatalog, FeeLayout};
use super::publish::publish_fees_to;
use super::rule::{FeeRuleRequest, build_fee_rule, parse_fee_scope};
use super::{FeesState, fee_audit_id, fee_publish_counts, fee_store_error_response};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::fees::{CategoryItems, FeeRule, FeeRuleStore, StoreFacts, StoreFault};
use crate::registry::RegistryStore;
use crate::routes::{FieldRefusal, load_tree_for_write};

#[utoipa::path(
    post,
    path = "/admin/fees",
    request_body(
        description = "The tenant, the scope (FEE_SCOPE_TENANT, _BRAND or _STORE), the id it is \
                       for, and the rule without a fee_id: code, display_name and its \
                       translations, kind (FEE_KIND_PERCENT with a rate, or FEE_KIND_AMOUNT_PER_BILL \
                       or _PER_UNIT with an amount), channels, item_scope with menu_item_ids and \
                       item_category_ids, base_discounted, base_tax_inclusive, tax (with a \
                       tax_class_id for FEE_TAX_TAX_CLASS), waivable and active",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created with a minted fee_id, and every store it reaches \
                                      republished. The body names the fee_id and lists each store \
                                      with FEE_PUBLISH_APPLIED and the new config version, \
                                      FEE_PUBLISH_UNCHANGED, FEE_PUBLISH_REFUSED with the rules it \
                                      cannot apply, or FEE_PUBLISH_FAILED"),
        (status = 400, description = "An id is not a ULID, the scope is not a scope, a field the \
                                      rule needs is absent or out of range, a token is not one \
                                      the field takes, a fee_id was sent, the rule names an item, \
                                      category or tax class the catalog does not have, or a \
                                      tenant-scope rule names another tenant", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no such brand or store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "A store the rule takes effect at could not apply it: an \
                                      amount in another currency than the store's, or a tax class \
                                      it has no rate for; or the store already runs another fee \
                                      with this code; or an item list names nothing on the menu of \
                                      any store it takes effect at", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The fee rule store, the registry, the catalog or the config \
                                      tree is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "fees",
)]
/// `POST /admin/fees` — writes a new fee, minting its id, and republishes every store it reaches.
pub(super) async fn admin_create_fee<F, Rg, Cat, Cfg, A, C>(
    State(state): State<FeesState<F, Rg, Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<FeeRuleRequest>,
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
    if request.rule.fee_id.is_some() {
        return FieldRefusal::new(
            "a new fee's id is minted here; write an existing fee with PUT",
            "rule.fee_id",
            "INVALID_VALUE",
        )
        .into_response();
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(fee_id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(FeeId::new)
    else {
        tracing::error!("could not read OS entropy to mint a fee id");
        return service_unavailable("fees");
    };
    write_fee_rule(&state, &context, tenant_id, fee_id, &request, true).await
}

#[utoipa::path(
    put,
    path = "/admin/fees",
    request_body(
        description = "As a POST, with the rule's fee_id: the fee to write at this scope. Written \
                       where it is, the rule replaces it; written at a narrower scope, it \
                       overrides the broader one for the stores that scope reaches",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Written, and every store it reaches republished, with the \
                                      same per-store outcomes as a POST"),
        (status = 400, description = "As a POST, or the fee_id is absent", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no such brand or store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "As a POST", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The fee rule store, the registry, the catalog or the config \
                                      tree is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "fees",
)]
/// `PUT /admin/fees` — writes a fee at one scope and republishes every store it reaches.
pub(super) async fn admin_put_fee<F, Rg, Cat, Cfg, A, C>(
    State(state): State<FeesState<F, Rg, Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<FeeRuleRequest>,
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
    let Some(fee_text) = request.rule.fee_id.as_deref() else {
        return FieldRefusal::new(
            "rule.fee_id is required: a PUT writes the fee it names",
            "rule.fee_id",
            "REQUIRED",
        )
        .into_response();
    };
    let (tenant_id, fee_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("rule.fee_id", fee_text)]) {
            Ok([tenant_id, fee_id]) => (TenantId::new(tenant_id), FeeId::new(fee_id)),
            Err(refusal) => return refusal,
        };
    write_fee_rule(&state, &context, tenant_id, fee_id, &request, false).await
}

/// The write `POST` and `PUT` share: checks the rule, checks it against every store it would take
/// effect at, keeps it, republishes every store it reaches, and records it.
async fn write_fee_rule<F, Rg, Cat, Cfg, A, C>(
    state: &FeesState<F, Rg, Cat, Cfg, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    fee_id: FeeId,
    request: &FeeRuleRequest,
    minted: bool,
) -> Response
where
    F: FeeRuleStore,
    Rg: RegistryStore,
    Cat: CatalogStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let scope = match parse_fee_scope(&request.scope) {
        Ok(scope) => scope,
        Err(refusal) => return refusal,
    };
    let (rule, item_category_ids) = match build_fee_rule(&request.rule, fee_id) {
        Ok(built) => built,
        Err(refusal) => return refusal,
    };
    let layout = match FeeLayout::load(&state.registry, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let scope_id = match layout.scope_target(scope, &request.scope_id) {
        Ok(scope_id) => scope_id,
        Err(refusal) => return refusal,
    };
    let catalog = match FeeCatalog::load(&state.catalog, tenant_id).await {
        Ok(catalog) => catalog,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = catalog.check_references(&rule, &item_category_ids) {
        return refusal;
    }
    let written = FeeRule {
        scope,
        scope_id,
        rule,
        item_category_ids,
        update_time: state.clock.now(),
        updated_by: context.admin.id.clone(),
    };
    let before = match state.fees.list(tenant_id).await {
        Ok(rules) => rules,
        Err(error) => return fee_store_error_response(&error),
    };
    let replaced = before.iter().any(|held| held.slot() == written.slot());
    let mut after: Vec<FeeRule> = before
        .iter()
        .filter(|held| held.slot() != written.slot())
        .cloned()
        .collect();
    after.push(written.clone());
    let reached = layout.reached_by(scope, &written.scope_id);
    let write = FeeWrite {
        before: &before,
        after: &after,
        written: Some(&written),
    };
    if let Err(refusal) =
        check_fee_write(state, &layout, &reached, &write, &catalog.categories).await
    {
        return refusal;
    }
    if let Err(error) = state.fees.put(tenant_id, &written).await {
        return fee_store_error_response(&error);
    }
    let results = publish_fees_to(state, &layout, &reached, &catalog.categories).await;
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(tenant_id),
        if replaced && !minted {
            "fee.update"
        } else {
            "fee.create"
        },
        "fee_rule",
        &fee_audit_id(fee_id, scope, &written.scope_id),
        None,
        Some(fee_publish_counts(&results)),
    )
    .await;
    (
        if minted {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(serde_json::json!({ "fee_id": fee_id.to_string(), "stores": results })),
    )
        .into_response()
}

/// What a write changes: the tenant's rules before and after it, and the rule it writes, which a
/// delete has none of.
pub(super) struct FeeWrite<'a> {
    pub(super) before: &'a [FeeRule],
    pub(super) after: &'a [FeeRule],
    pub(super) written: Option<&'a FeeRule>,
}

/// Checks what a write would change at each store it reaches, before the write is kept.
///
/// At each store, the rules in force before and after the write are resolved, and every rule in
/// force after it that was not before — the rule written, or for a delete the broader rule it
/// uncovers — must be one the store can apply ([`crate::fees::faults_at`]) and must not share its
/// code with another fee the store runs. A written include or exclude list must also name an item
/// on the menu of at least one store it takes effect at, where any of them has a menu: ADR-0159's
/// check of an `items` list that names nothing on the store's menu.
///
/// A rule already in force that a later change to the store left unable to apply is not this
/// write's to refuse: the publish leaves that store as it is, and says why.
pub(super) async fn check_fee_write<F, Rg, Cat, Cfg, A, C>(
    state: &FeesState<F, Rg, Cat, Cfg, A, C>,
    layout: &FeeLayout,
    stores: &[StoreId],
    write: &FeeWrite<'_>,
    categories: &CategoryItems,
) -> Result<(), Response>
where
    Cfg: ConfigTreeStore,
{
    let mut problems = FeeWriteProblems::default();
    for store_id in stores {
        let Some(placement) = layout.placement(*store_id) else {
            continue;
        };
        let was = crate::fees::resolve_for_store(write.before, &placement);
        let now = crate::fees::resolve_for_store(write.after, &placement);
        let changed: Vec<&FeeRule> = now
            .iter()
            .copied()
            .filter(|rule| !was.contains(rule))
            .collect();
        if changed.is_empty() {
            continue;
        }
        let facts = fee_store_facts(&state.config_trees, layout.tenant_id, *store_id).await?;
        for rule in changed {
            let published = crate::fees::compile(rule, categories);
            problems.note(
                *store_id,
                &published,
                &now,
                &facts,
                write.written == Some(rule),
            );
        }
    }
    problems.into_result(write.written.is_none())
}

/// What [`check_fee_write`] found, gathered across the stores a write reaches.
#[derive(Default)]
struct FeeWriteProblems {
    /// Each store-and-rule fault, as a sentence naming them by id.
    faults: Vec<String>,
    /// The `details` the refusal carries, each once.
    details: BTreeSet<(&'static str, &'static str)>,
    /// Whether a store the written list takes effect at has a menu, and whether one lists an item.
    menus: Option<bool>,
}

impl FeeWriteProblems {
    /// The most store-and-rule sentences a refusal spells out; the rest are counted.
    const SENTENCES: usize = 5;

    /// Notes what is wrong with `published`, in force at `store_id` among `now`.
    pub(in crate::routes) fn note(
        &mut self,
        store_id: StoreId,
        published: &PublishedFee,
        now: &[&FeeRule],
        facts: &StoreFacts,
        written: bool,
    ) {
        for fault in crate::fees::faults_at(published, facts) {
            self.details.insert((fault.field(), fault.as_wire()));
            self.faults.push(format!(
                "fee {} cannot apply at store {store_id}: {}",
                published.fee_id,
                match fault {
                    StoreFault::CurrencyMismatch => "its amount is in another currency",
                    StoreFault::TaxClassNotRated => "its tax class has no rate there",
                }
            ));
        }
        if published.active
            && let Some(other) = crate::fees::shares_code_with(published, now)
        {
            self.details.insert(("rule.code", "ALREADY_EXISTS"));
            self.faults.push(format!(
                "fee {} at store {store_id} has the code of fee {}, which the store also runs; \
                 override that fee instead",
                published.fee_id, other.rule.fee_id
            ));
        }
        let listed = matches!(
            published.item_scope(),
            Some(FeeItems::Include | FeeItems::Exclude)
        );
        if written
            && listed
            && published.active
            && let Some(lists) = facts.menu_lists_any(&published.menu_item_ids, &published.channels)
        {
            self.menus = Some(self.menus.unwrap_or(false) || lists);
        }
    }

    /// The `422` for what was found, or nothing. A delete's refusal names `fee_id`, since the rule
    /// it is about is not in the request.
    #[expect(
        clippy::result_large_err,
        reason = "the Err is an axum Response by design — it is the refusal the caller returns"
    )]
    fn into_result(mut self, deleting: bool) -> Result<(), Response> {
        if self.menus == Some(false) {
            self.details.insert(("rule.menu_item_ids", "NOT_ON_MENU"));
            self.faults.push(
                "the item list names nothing on the menu of any store the fee takes effect at"
                    .to_owned(),
            );
        }
        if self.faults.is_empty() {
            return Ok(());
        }
        let more = self.faults.len().saturating_sub(Self::SENTENCES);
        let mut message: Vec<String> = self.faults.into_iter().take(Self::SENTENCES).collect();
        if more > 0 {
            message.push(format!("and {more} more"));
        }
        let details: Vec<(&str, &str)> = self
            .details
            .iter()
            .map(|(field, reason)| (if deleting { "fee_id" } else { *field }, *reason))
            .collect();
        Err(api_error_with_details(
            ErrorStatus::Unprocessable,
            message.join("; "),
            &details,
        ))
    }
}

/// What a store's configuration holds that a fee must agree with; nothing for a store with no
/// configuration yet.
pub(super) async fn fee_store_facts<Cfg: ConfigTreeStore>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<StoreFacts, Response> {
    let loaded = load_tree_for_write(config_trees, tenant_id, store_id).await?;
    Ok(loaded.state.map_or_else(StoreFacts::default, |tree| {
        StoreFacts::from_document(
            &ConfigTree::from_state(store_id, CapabilityValidator, tree).effective(),
        )
    }))
}
