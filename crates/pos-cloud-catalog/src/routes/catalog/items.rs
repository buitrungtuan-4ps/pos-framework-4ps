// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/items`: list, create and update a tenant's items.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, ulid_refusal};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{CourseId, MenuItemId, TaxClassId, TenantId};
use pos_proto::ulid::Ulid;

use super::requests::{CreateItemRequest, UpdateItemRequest, clean_name_translations};
use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{
    CatalogItem, CatalogStore, ItemCategoryId, ItemListFilter, ItemSort, ItemSubcategoryId,
};
use crate::media::MediaId;
use crate::registry::EntityStatus;
use crate::routes::{
    ItemListQuery, entity_status_refusal, enum_refusal, if_match, not_found, paged_ok,
    parse_entity_status, parse_optional_ulid, parse_page, present_param, version_mismatch,
    versioned_created, versioned_ok,
};
use crate::version::UpdateOutcome;

/// Parses a list of item ids from a request body; `Err` if any entry is not a ULID.
pub(super) fn parse_item_id_list(values: &[String]) -> Result<Vec<MenuItemId>, ()> {
    values
        .iter()
        .map(|text| text.parse::<Ulid>().map(MenuItemId::new).map_err(|_| ()))
        .collect()
}

/// A super-admin lists a tenant's items.
pub(super) async fn admin_list_items<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ItemListQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit (ADR-0098). This route has the most
    // consumers of any list in the console and only one of them is a table: the menu compiler and
    // the item pickers need every item, or a menu compiles without whatever fell off the page. That
    // is the reason an absent `?limit=` can never come to mean a default page size.
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        // `q`/`sort`/`order` shape a *page*. Accepting them on the whole-set read and ignoring them
        // would answer a different question than the caller asked and say nothing about it — so the
        // route names the one parameter that is missing instead.
        for (field, value) in [
            ("q", query.q.as_deref()),
            ("sort", query.sort.as_deref()),
            ("order", query.order.as_deref()),
        ] {
            if present_param(value).is_some() {
                return page_shaping_needs_a_limit_refusal(field);
            }
        }
        return match state.catalog.list_items(tenant_id).await {
            Ok(items) => (StatusCode::OK, Json(items)).into_response(),
            Err(error) => catalog_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    let filter = match item_list_filter(
        query.q.as_deref(),
        query.sort.as_deref(),
        query.order.as_deref(),
    ) {
        Ok(filter) => filter,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .list_items_page(tenant_id, page, &filter)
        .await
    {
        Ok(read) => paged_ok(read, page),
        Err(error) => catalog_error_response(&error),
    }
}

/// Reads `?q=`, `?sort=` and `?order=` into an [`ItemListFilter`], refusing a token outside the
/// route's own closed sets.
///
/// The refusal names the field and lists what it accepts (ADR-0096's shape), so a caller that sent
/// `?sort=price` is told `sort` must be `newest`, `name`, or `status` rather than being handed the
/// default order and left to wonder why nothing moved.
///
/// A blank or whitespace `q` is *absence*, not a search for nothing: clearing a search box sends
/// `?q=`, and the one reading of that which is never right is "match only items whose name contains
/// the empty string", which is every item — the same answer as no search, arrived at by accident.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal is built where the field names are \
              known, exactly as the other query parsers on this router do"
)]
fn item_list_filter(
    search: Option<&str>,
    sort: Option<&str>,
    order: Option<&str>,
) -> Result<ItemListFilter, Response> {
    let search = present_param(search).map(str::to_owned);
    let sort = match present_param(sort) {
        None => ItemSort::default(),
        Some(token) => match ItemSort::from_token(token) {
            Some(sort) => sort,
            None => return Err(enum_refusal("sort", ItemSort::tokens().iter().copied())),
        },
    };
    let descending = match present_param(order) {
        // Absent is ascending — the same reading `?order=asc` has, so a caller that omits the
        // parameter and one that spells out the default get the same page.
        None | Some("asc") => false,
        Some("desc") => true,
        Some(_unknown) => return Err(enum_refusal("order", ["asc", "desc"])),
    };
    Ok(ItemListFilter {
        search,
        sort,
        descending,
    })
}

/// The refusal for a page-shaping parameter sent to a read that was not asked to page.
///
/// Its own sentence rather than
/// [`offset_without_limit_refusal`](pos_cloud_api::paged::offset_without_limit_refusal)'s, because
/// the fix differs: an `offset` without a `limit` is a caller that forgot half a page, while a `q`
/// without one is a caller expecting a filtered *set* — a thing this API does not offer, and will
/// not, because the whole-set read exists for consumers that need every row.
#[must_use]
pub fn page_shaping_needs_a_limit_refusal(field: &str) -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        format!(
            "{field} shapes a page and needs a limit: without one this read returns every row, \
             unfiltered and in its own order"
        ),
        &[(field, "MISSING_DEPENDENT_FIELD")],
    )
}

pub(super) async fn admin_create_item<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateItemRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, tax_class_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("tax_class_id", &request.tax_class_id),
    ]) {
        Ok([tenant_id, tax_class_id]) => (TenantId::new(tenant_id), TaxClassId::new(tax_class_id)),
        Err(refusal) => return refusal,
    };
    let Ok(item_category_id) =
        parse_optional_ulid(request.item_category_id.as_deref(), ItemCategoryId::new)
    else {
        return ulid_refusal(&["item_category_id"]);
    };
    let Ok(course_id) = parse_optional_ulid(request.course_id.as_deref(), CourseId::new) else {
        return ulid_refusal(&["course_id"]);
    };
    let Ok(item_subcategory_id) = parse_optional_ulid(
        request.item_subcategory_id.as_deref(),
        ItemSubcategoryId::new,
    ) else {
        return ulid_refusal(&["item_subcategory_id"]);
    };
    let Ok(image_ref) = parse_optional_ulid(request.image_ref.as_deref(), MediaId::new) else {
        return ulid_refusal(&["image_ref"]);
    };
    let Some(menu_item_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MenuItemId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = CatalogItem {
        menu_item_id,
        tenant_id,
        name: request.name,
        name_translations: clean_name_translations(request.name_translations),
        tax_class_id,
        item_category_id,
        item_subcategory_id,
        course_id,
        image_ref,
        status: EntityStatus::Active,
    };
    match state.catalog.create_item(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_item.create",
                "menu_item",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames an item, sets its tax class and/or status.
pub(super) async fn admin_update_item<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_item_id): Path<String>,
    Json(request): Json<UpdateItemRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (menu_item_id, tenant_id, tax_class_id) = match parse_ulid_fields([
        ("menu_item_id", &menu_item_id),
        ("tenant_id", &request.tenant_id),
        ("tax_class_id", &request.tax_class_id),
    ]) {
        Ok([menu_item_id, tenant_id, tax_class_id]) => (
            MenuItemId::new(menu_item_id),
            TenantId::new(tenant_id),
            TaxClassId::new(tax_class_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let Ok(item_category_id) =
        parse_optional_ulid(request.item_category_id.as_deref(), ItemCategoryId::new)
    else {
        return ulid_refusal(&["item_category_id"]);
    };
    let Ok(course_id) = parse_optional_ulid(request.course_id.as_deref(), CourseId::new) else {
        return ulid_refusal(&["course_id"]);
    };
    let Ok(item_subcategory_id) = parse_optional_ulid(
        request.item_subcategory_id.as_deref(),
        ItemSubcategoryId::new,
    ) else {
        return ulid_refusal(&["item_subcategory_id"]);
    };
    let Ok(image_ref) = parse_optional_ulid(request.image_ref.as_deref(), MediaId::new) else {
        return ulid_refusal(&["image_ref"]);
    };
    let record = CatalogItem {
        menu_item_id,
        tenant_id,
        name: request.name,
        name_translations: clean_name_translations(request.name_translations),
        tax_class_id,
        item_category_id,
        item_subcategory_id,
        course_id,
        image_ref,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_item(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_item.update",
                "menu_item",
                &record.menu_item_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("item"),
        Err(error) => catalog_error_response(&error),
    }
}
