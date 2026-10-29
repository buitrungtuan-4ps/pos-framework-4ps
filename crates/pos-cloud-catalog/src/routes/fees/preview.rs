// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/fees/preview`: the rules being edited, applied to a sample bill.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error_with_details, parse_ulid_fields, service_unavailable, ulid_refusal,
};
use pos_proto::determinism::ClockSource;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{FeeId, MenuItemId, StoreId, TenantId};
use pos_proto::money::Money;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use pos_proto::{ErrorStatus, WireEnum};
use serde::Deserialize;

use super::layout::{FeeCatalog, FeeLayout};
use super::rule::{FeeRuleBody, build_fee_rule, parse_fee_scope};
use super::write::fee_store_facts;
use super::{FeesState, fee_store_error_response};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::config_tree::ConfigTreeStore;
use crate::fees::{FeePlacement, FeeRule, FeeRuleStore};
use crate::registry::RegistryStore;
use crate::routes::{FieldRefusal, enum_refusal, not_found};

/// The most rules one preview takes: the rule being edited and the handful beside it, bounded so one
/// request cannot ask the cloud to resolve an unbounded list.
const MOST_PREVIEW_RULES: usize = 20;

/// The most units one sample line takes.
const MOST_SAMPLE_UNITS: i64 = 999;

/// A `POST /admin/fees/preview` body: a store, the rules as they would be written, and the lines of
/// a sample bill.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PreviewFeesRequest {
    tenant_id: String,
    store_id: String,
    /// The channel the sample bill's order comes in on, which picks the menu's prices, the tax
    /// rate and the fees that apply. Dine-in when absent.
    #[serde(default)]
    sales_channel: Option<Open<SalesChannel>>,
    /// Rules as a `PUT` would send them, each at its scope, in place of what is written there. A rule
    /// with no `fee_id` is a fee not created yet.
    #[serde(default)]
    rules: Vec<PreviewFeeRule>,
    /// The sample bill's lines.
    lines: Vec<SampleLineRequest>,
}

/// One rule a preview writes in place, as a `PUT` body carries it.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct PreviewFeeRule {
    scope: String,
    scope_id: String,
    rule: FeeRuleBody,
}

/// One sample line: an item on the store's menu, and how many.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct SampleLineRequest {
    menu_item_id: String,
    quantity: i64,
}

/// A sample bill as the console draws it: each line, each fee with its tax, the tax per class, and
/// what the guest would pay.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SampleBillView {
    store_id: String,
    #[schema(value_type = SalesChannel)]
    sales_channel: &'static str,
    lines: Vec<SampleLineView>,
    subtotal: Money,
    fee_lines: Vec<SampleFeeLineView>,
    /// Every fee together, as the bill's one service-charge figure carries it.
    service_charge: Money,
    tax_lines: Vec<SampleTaxLineView>,
    tax_total: Money,
    rounding_adjustment: Money,
    total_due: Money,
}

/// A sample line, priced from the store's menu.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SampleLineView {
    menu_item_id: String,
    display_name: DisplayName,
    quantity: i64,
    unit_price: Money,
    line_total: Money,
    tax_class_id: String,
}

/// A fee the sample bill charges, with its tax.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SampleFeeLineView {
    fee_id: String,
    code: String,
    display_name: DisplayName,
    amount: Money,
    tax: Money,
    /// The parts of the fee taxed at each class, with each part's tax.
    class_shares: Vec<SampleFeeShareView>,
}

/// The part of a fee taxed at one class.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SampleFeeShareView {
    tax_class_id: String,
    amount: Money,
    tax: Money,
}

/// The tax at one class, on the base the fees joined.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct SampleTaxLineView {
    tax_class_id: String,
    taxable_base: Money,
    rate_basis_points: u32,
    tax: Money,
}

impl SampleBillView {
    pub(in crate::routes) fn of(
        store_id: StoreId,
        channel: SalesChannel,
        units: &[i64],
        bill: crate::fees::SampleBill,
    ) -> Self {
        let totals = bill.totals;
        Self {
            store_id: store_id.to_string(),
            sales_channel: channel.as_wire(),
            lines: bill
                .lines
                .into_iter()
                .zip(units)
                .map(|(line, quantity)| SampleLineView {
                    menu_item_id: line.menu_item_id.to_string(),
                    display_name: line.display_name,
                    quantity: *quantity,
                    unit_price: line.unit_price,
                    line_total: line.line_total,
                    tax_class_id: line.tax_class_id.to_string(),
                })
                .collect(),
            subtotal: totals.subtotal,
            fee_lines: totals
                .fee_lines
                .into_iter()
                .map(|fee| SampleFeeLineView {
                    fee_id: fee.fee_id.to_string(),
                    code: fee.code.as_str().to_owned(),
                    display_name: fee.display_name,
                    amount: fee.amount,
                    tax: fee.tax,
                    class_shares: fee
                        .class_shares
                        .into_iter()
                        .map(|share| SampleFeeShareView {
                            tax_class_id: share.tax_class_id.to_string(),
                            amount: share.amount,
                            tax: share.tax,
                        })
                        .collect(),
                })
                .collect(),
            service_charge: totals.service_charge,
            tax_lines: totals
                .tax_lines
                .into_iter()
                .map(|line| SampleTaxLineView {
                    tax_class_id: line.tax_class_id.to_string(),
                    taxable_base: line.taxable_base,
                    rate_basis_points: line.rate_basis_points,
                    tax: line.tax,
                })
                .collect(),
            tax_total: totals.tax_total,
            rounding_adjustment: totals.rounding_adjustment,
            total_due: totals.total_due,
        }
    }
}

#[utoipa::path(
    post,
    path = "/admin/fees/preview",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.reports.revenue"))),
    request_body(
        content = PreviewFeesRequest, description = "The tenant, a store, the sales channel (dine-in when absent), the rules as \
                       they would be written — each with its scope and scope_id and the rule as a \
                       PUT sends it, its fee_id absent for a fee not created yet — and the sample \
                       bill's lines, each a menu_item_id from the store's menu and a quantity of \
                       1 to 999. From 1 to 50 lines, and at most 20 rules",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = SampleBillView, description = "The sample bill as the store would assemble it with those \
                                      rules in place: each line priced from its menu, each fee \
                                      charged with its tax and the parts of it taxed at each \
                                      class, the tax per class, the cash rounding and the total \
                                      due. Nothing is written or published"),
        (status = 400, description = "An id is not a ULID, the channel or a scope is not one, a \
                                      rule is refused as a write would refuse it, or there are no \
                                      lines, too many lines or rules, or a quantity out of range", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no such store, brand or store a rule is for", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "The store could not assemble the bill: it has no locale, \
                                      tax table or menu published (NOT_PUBLISHED), a line is not \
                                      on its menu for the channel (NOT_ON_MENU), not available \
                                      (NOT_AVAILABLE) or has no tax rate (TAX_RATE_NOT_CONFIGURED), \
                                      or a rule it would run cannot apply there", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The fee rule store, the registry, the catalog or the config \
                                      tree is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "fees",
)]
/// `POST /admin/fees/preview` — a sample bill at one store with rules in place, before they are
/// written ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md): "the console previews a
/// sample bill before publishing").
///
/// A read: it writes nothing and publishes nothing, so it stands behind the fee reads' permission.
/// It resolves the store's rules with the ones sent in place, compiles them as a publish would, and
/// assembles the bill with [`crate::fees::sample_bill`], which prices each line and adds it up with
/// `pos_core`'s own functions, from the store's own published menu, tax table and rounding.
pub(super) async fn admin_preview_fees<F, Rg, Cat, Cfg, A, C>(
    State(state): State<FeesState<F, Rg, Cat, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PreviewFeesRequest>,
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
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let read = match read_preview_request(&request) {
        Ok(read) => read,
        Err(refusal) => return refusal,
    };
    let (tenant_id, store_id, channel) = (read.tenant_id, read.store_id, read.channel);
    let (layout, placement, catalog) = match store_fee_inputs(&state, tenant_id, store_id).await {
        Ok(inputs) => inputs,
        Err(refusal) => return refusal,
    };
    let rules = match rules_with_previews(
        &state,
        &context,
        &layout,
        &catalog,
        tenant_id,
        &request.rules,
    )
    .await
    {
        Ok(rules) => rules,
        Err(refusal) => return refusal,
    };
    let facts = match fee_store_facts(&state.config_trees, tenant_id, store_id).await {
        Ok(facts) => facts,
        Err(refusal) => return refusal,
    };
    let sent = crate::fees::for_store(&rules, &placement, &catalog.categories, &facts);
    if !sent.faults.is_empty() {
        return store_faults_refusal(&sent.faults);
    }
    let bill = crate::fees::sample_bill(&facts, channel, &read.lines, &sent.node);
    sample_bill_answer(store_id, channel, &request.lines, bill)
}

/// The tenant's fee layout, the store's place in it, and the catalog the rules compile against.
async fn store_fee_inputs<F, Rg, Cat, Cfg, A, C>(
    state: &FeesState<F, Rg, Cat, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<(FeeLayout, FeePlacement, FeeCatalog), Response>
where
    Rg: RegistryStore,
    Cat: CatalogStore,
{
    let layout = FeeLayout::load(&state.registry, tenant_id).await?;
    let Some(placement) = layout.placement(store_id) else {
        return Err(not_found("store"));
    };
    let catalog = FeeCatalog::load(&state.catalog, tenant_id).await?;
    Ok((layout, placement, catalog))
}

/// The sample bill as the console draws it, or the `422` naming what the store could not price.
fn sample_bill_answer(
    store_id: StoreId,
    channel: SalesChannel,
    lines: &[SampleLineRequest],
    bill: Result<crate::fees::SampleBill, crate::fees::SampleBillError>,
) -> Response {
    match bill {
        Ok(bill) => {
            let units: Vec<i64> = lines.iter().map(|line| line.quantity).collect();
            (
                StatusCode::OK,
                Json(SampleBillView::of(store_id, channel, &units, bill)),
            )
                .into_response()
        }
        Err(error) => sample_bill_refusal(&error),
    }
}

/// What a preview reads from its request before it reads any store.
struct PreviewRequest {
    tenant_id: TenantId,
    store_id: StoreId,
    channel: SalesChannel,
    lines: Vec<crate::fees::SampleLine>,
}

/// The ids, the channel (dine-in when absent), the rule count and the sample lines, refused in
/// that order.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn read_preview_request(request: &PreviewFeesRequest) -> Result<PreviewRequest, Response> {
    let [tenant_id, store_id] = parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ])?;
    let channel = match &request.sales_channel {
        None => SalesChannel::DineIn,
        Some(channel) if channel.is_unspecified() => {
            return Err(enum_refusal(
                "sales_channel",
                SalesChannel::ALL
                    .iter()
                    .filter(|channel| **channel != SalesChannel::Unspecified)
                    .map(|channel| channel.as_wire()),
            ));
        }
        Some(channel) => channel.known(),
    };
    if request.rules.len() > MOST_PREVIEW_RULES {
        return Err(
            FieldRefusal::new("a preview takes at most 20 rules", "rules", "OUT_OF_RANGE")
                .into_response(),
        );
    }
    let lines = parse_sample_lines(&request.lines)?;
    Ok(PreviewRequest {
        tenant_id: TenantId::new(tenant_id),
        store_id: StoreId::new(store_id),
        channel,
        lines,
    })
}

/// The tenant's written rules with each previewed rule in the place of the one at its slot, checked
/// against the tenant's `layout` and `catalog` as a write checks it.
async fn rules_with_previews<F, Rg, Cat, Cfg, A, C>(
    state: &FeesState<F, Rg, Cat, Cfg, A, C>,
    context: &AdminContext,
    layout: &FeeLayout,
    catalog: &FeeCatalog,
    tenant_id: TenantId,
    previews: &[PreviewFeeRule],
) -> Result<Vec<FeeRule>, Response>
where
    F: FeeRuleStore,
    C: ClockSource,
{
    let mut rules = state
        .fees
        .list(tenant_id)
        .await
        .map_err(|error| fee_store_error_response(&error))?;
    for preview in previews {
        let rule = previewed_rule(state, context, layout, catalog, preview)?;
        rules.retain(|held| held.slot() != rule.slot());
        rules.push(rule);
    }
    Ok(rules)
}

/// The `422` for rules the store cannot apply: each fee and its fault in the message, each
/// distinct fault once in the details.
fn store_faults_refusal(faults: &[(FeeId, crate::fees::StoreFault)]) -> Response {
    let message: Vec<String> = faults
        .iter()
        .map(|(fee_id, fault)| {
            format!(
                "fee {fee_id} cannot apply at the store: {}",
                fault.as_wire()
            )
        })
        .collect();
    let details: BTreeSet<(&str, &str)> = faults
        .iter()
        .map(|(_, fault)| ("rules", fault.as_wire()))
        .collect();
    let details: Vec<(&str, &str)> = details.into_iter().collect();
    api_error_with_details(ErrorStatus::Unprocessable, message.join("; "), &details)
}

/// One rule a preview writes in place, checked as a write checks it. A rule with no `fee_id` is
/// given one for the preview alone.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn previewed_rule<F, Rg, Cat, Cfg, A, C>(
    state: &FeesState<F, Rg, Cat, Cfg, A, C>,
    context: &AdminContext,
    layout: &FeeLayout,
    catalog: &FeeCatalog,
    preview: &PreviewFeeRule,
) -> Result<FeeRule, Response>
where
    C: ClockSource,
{
    let scope = parse_fee_scope(&preview.scope)?;
    let fee_id = if let Some(text) = preview.rule.fee_id.as_deref() {
        let [id] = parse_ulid_fields([("rule.fee_id", text)])?;
        FeeId::new(id)
    } else {
        let Some(id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()) else {
            tracing::error!("could not read OS entropy to mint a fee id for a preview");
            return Err(service_unavailable("fees"));
        };
        FeeId::new(id)
    };
    let (rule, item_category_ids) = build_fee_rule(&preview.rule, fee_id)?;
    let scope_id = layout.scope_target(scope, &preview.scope_id)?;
    catalog.check_references(&rule, &item_category_ids)?;
    Ok(FeeRule {
        scope,
        scope_id,
        rule,
        item_category_ids,
        update_time: state.clock.now(),
        updated_by: context.admin.id.clone(),
    })
}

/// Reads a sample bill's lines: from 1 to [`crate::fees::MOST_SAMPLE_LINES`], each an item id and
/// from 1 to [`MOST_SAMPLE_UNITS`] units.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn parse_sample_lines(
    lines: &[SampleLineRequest],
) -> Result<Vec<crate::fees::SampleLine>, Response> {
    let out_of_range =
        |message: &'static str| FieldRefusal::new(message, "lines", "OUT_OF_RANGE").into_response();
    if lines.is_empty() || lines.len() > crate::fees::MOST_SAMPLE_LINES {
        return Err(out_of_range("a sample bill has from 1 to 50 lines"));
    }
    lines
        .iter()
        .map(|line| {
            let Ok(id) = line.menu_item_id.trim().parse::<Ulid>() else {
                return Err(ulid_refusal(&["lines"]));
            };
            if !(1..=MOST_SAMPLE_UNITS).contains(&line.quantity) {
                return Err(out_of_range("a sample line has from 1 to 999 units"));
            }
            let quantity = pos_proto::quantity::Quantity::from_whole(line.quantity)
                .map_err(|_overflow| out_of_range("a sample line has from 1 to 999 units"))?;
            Ok(crate::fees::SampleLine {
                menu_item_id: MenuItemId::new(id),
                quantity,
            })
        })
        .collect()
}

/// The `422` for a sample bill the store could not assemble, naming what it lacks or the line it
/// cannot price.
fn sample_bill_refusal(error: &crate::fees::SampleBillError) -> Response {
    use crate::fees::SampleBillError;
    use pos_core::menu::RepriceError;

    let (field, reason) = match error {
        SampleBillError::NotPublished(_) => ("store_id", "NOT_PUBLISHED"),
        SampleBillError::Line(RepriceError::UnknownItem(_)) => ("lines", "NOT_ON_MENU"),
        SampleBillError::Line(RepriceError::Unavailable(_)) => ("lines", "NOT_AVAILABLE"),
        SampleBillError::Line(RepriceError::MissingRate { .. }) => {
            ("lines", "TAX_RATE_NOT_CONFIGURED")
        }
        SampleBillError::Line(_) | SampleBillError::Assemble(_) => ("lines", "INVALID_VALUE"),
    };
    api_error_with_details(
        ErrorStatus::Unprocessable,
        error.to_string(),
        &[(field, reason)],
    )
}
