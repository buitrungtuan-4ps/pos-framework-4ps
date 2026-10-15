// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/export/items` and `/admin/catalog/import/items/*`: a tenant's items as a file.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error, csv_download_response, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
use serde::Deserialize;

use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogItem, CatalogStore, ItemCategoryId, ItemSubcategoryId};
use crate::routes::RegistryTenantQuery;
use crate::version::{UpdateOutcome, Versioned};
use crate::{export, import};

/// A super-admin exports a tenant's catalog items as CSV ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5). Behind [`ConsolePermission::ManageCatalog`] and audited — the entry records who exported
/// which domain and how many rows, never the row contents. No price is exported (prices are per-channel
/// placements, a deferred T2 export); this is the item master only.
pub(super) async fn admin_export_items<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // The version each row was read at is for a writer (ADR-0094); an export is a read, so the
    // records go out as they always did.
    let items: Vec<CatalogItem> = match state.catalog.list_items(tenant_id).await {
        Ok(items) => items.into_iter().map(|item| item.record).collect(),
        Err(error) => return catalog_error_response(&error),
    };
    let Ok(body) = export::items_csv(&items) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "catalog.export_items",
        "catalog_export",
        &tenant_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": "items", "rows": items.len() })),
    )
    .await;
    csv_download_response("items.csv", body)
}

/// A super-admin creates an item; the id is minted here and returned once in the created record.
/// A tenant query on the item-import routes — the same shape the export takes.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ItemImportQuery {
    tenant_id: String,
}

/// Reads the tenant's items and the ids a row may reference, so the parser can check a foreign key
/// where the operator can see which row broke.
async fn item_import_inputs<Cat>(
    catalog: &Cat,
    tenant_id: TenantId,
) -> Result<
    (
        Vec<Versioned<CatalogItem>>,
        Vec<TaxClassId>,
        Vec<ItemCategoryId>,
        Vec<ItemSubcategoryId>,
    ),
    Response,
>
where
    Cat: CatalogStore,
{
    let items = catalog
        .list_items(tenant_id)
        .await
        .map_err(|error| catalog_error_response(&error))?;
    let tax_classes = catalog
        .list_tax_classes(tenant_id)
        .await
        .map_err(|error| catalog_error_response(&error))?
        .into_iter()
        .map(|row| row.record.tax_class_id)
        .collect();
    let categories = catalog
        .list_item_categories(tenant_id)
        .await
        .map_err(|error| catalog_error_response(&error))?
        .into_iter()
        .map(|row| row.record.item_category_id)
        .collect();
    let subcategories = catalog
        .list_item_subcategories(tenant_id)
        .await
        .map_err(|error| catalog_error_response(&error))?
        .into_iter()
        .map(|row| row.record.item_subcategory_id)
        .collect();
    Ok((items, tax_classes, categories, subcategories))
}

/// Writes one accepted row, answering with the reason it could not be written rather than throwing
/// it away — a row that fails at the write is reported like a row that failed to parse.
///
/// `Err` is reserved for what stops the whole import: only the entropy failure, because an id that
/// cannot be minted is not a property of this row and would be the same answer for every one after.
async fn write_item_upsert<Cat, C>(
    catalog: &Cat,
    clock: &C,
    tenant_id: TenantId,
    upsert: import::ItemUpsert,
) -> Result<Option<String>, Response>
where
    Cat: CatalogStore,
    C: ClockSource,
{
    match upsert {
        import::ItemUpsert::Create(new_item) => {
            let Some(menu_item_id) =
                mint_ulid(clock.now().as_milliseconds_since_epoch()).map(MenuItemId::new)
            else {
                return Err(catalog_entropy_unavailable());
            };
            let record = CatalogItem {
                menu_item_id,
                tenant_id,
                name: new_item.name,
                // A created item has no per-locale names: the format cannot carry them, and
                // inventing one from the default caption would put an English word in the
                // Vietnamese column and call it a translation.
                name_translations: std::collections::BTreeMap::new(),
                tax_class_id: new_item.tax_class_id,
                item_category_id: new_item.item_category_id,
                item_subcategory_id: new_item.item_subcategory_id,
                // The import format carries no course, the same way it carries no translations.
                // An item imported onto no course is an item the operator then puts on one, which
                // is better than guessing a service sequence from a spreadsheet column that is not
                // there.
                course_id: None,
                image_ref: new_item.image_ref,
                status: new_item.status,
            };
            Ok(match catalog.create_item(&record).await {
                Ok(_) => None,
                Err(error) => Some(error.to_string()),
            })
        }
        import::ItemUpsert::Update { item, expected } => {
            Ok(match catalog.update_item(&item, &expected).await {
                Ok(UpdateOutcome::Updated(_)) => None,
                Ok(UpdateOutcome::VersionMismatch) => {
                    Some("the item changed while the import was being reviewed".to_owned())
                }
                Ok(UpdateOutcome::NotFound) => {
                    Some("the item was deleted while the import was being reviewed".to_owned())
                }
                Err(error) => Some(error.to_string()),
            })
        }
    }
}

/// `POST /admin/catalog/import/items/dry-run` — what an item-master CSV **would** do, row by row,
/// writing nothing (roadmap-v3 **F15**, [ADR-0075](../../../docs/adr/0075-media-and-file-rail.md)).
///
/// The file is the one `GET /admin/catalog/export/items` writes, which is what makes the pair a
/// round-trip: export the master, edit it in a spreadsheet, bring it back. It carries no price —
/// a price is a per-channel placement — so neither the upload nor the report an operator reads can
/// carry one in clear text.
///
/// Behind [`ConsolePermission::ManageCatalog`] and not audited, because it changes nothing.
pub(super) async fn admin_import_items_dry_run<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ItemImportQuery>,
    body: axum::body::Bytes,
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
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let (items, tax_classes, categories, subcategories) =
        match item_import_inputs(&state.catalog, tenant_id).await {
            Ok(inputs) => inputs,
            Err(refusal) => return refusal,
        };
    match import::parse_items_csv(
        &body,
        tenant_id,
        &items,
        &tax_classes,
        &categories,
        &subcategories,
    ) {
        Ok((_, report)) => (StatusCode::OK, Json(report)).into_response(),
        Err(error) => api_error(ErrorStatus::InvalidArgument, error.to_string()),
    }
}

/// `POST /admin/catalog/import/items` — the confirm after a dry run: the valid rows are written and
/// the rejected ones are reported, never written.
///
/// # Why a row can be applied and reported as applied, and the next one still fail
///
/// The rows are separate records and are written one at a time, so this is not a transaction and
/// does not pretend to be. What it does instead is report per row: a row whose write was refused
/// comes back as a rejection naming what happened, so re-importing the same file after fixing it is
/// safe — the rows that already landed are updates at their new version, and the rest are retried.
/// Wrapping the whole file in one transaction would mean one bad row discarding an operator's whole
/// afternoon, which is the behaviour the dry run exists to make unnecessary.
///
/// An update is conditional on the version the dry run read, so a row somebody else changed in
/// between is refused rather than silently overwritten
/// ([ADR-0094](../../../docs/adr/0094-optimistic-concurrency-on-admin.md)).
///
/// Behind [`ConsolePermission::ManageCatalog`] and audited — the row counts, never the contents.
pub(super) async fn admin_import_items_apply<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ItemImportQuery>,
    body: axum::body::Bytes,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let (items, tax_classes, categories, subcategories) =
        match item_import_inputs(&state.catalog, tenant_id).await {
            Ok(inputs) => inputs,
            Err(refusal) => return refusal,
        };
    let (upserts, mut report) = match import::parse_items_csv(
        &body,
        tenant_id,
        &items,
        &tax_classes,
        &categories,
        &subcategories,
    ) {
        Ok(parsed) => parsed,
        Err(error) => return api_error(ErrorStatus::InvalidArgument, error.to_string()),
    };

    // The report's rows are in file order and only the accepted ones have an upsert, in that same
    // order — so walking the two together lets a failed write turn its own row into a rejection
    // without disturbing anybody else's verdict.
    let mut applying = report
        .rows
        .iter_mut()
        .filter(|row| !matches!(row.action, import::RowAction::Reject { .. }));
    for upsert in upserts {
        let Some(row) = applying.next() else {
            break;
        };
        let failure = match write_item_upsert(&state.catalog, &state.clock, tenant_id, upsert).await
        {
            Ok(failure) => failure,
            Err(refusal) => return refusal,
        };
        if let Some(reason) = failure {
            // Downgrade the row's verdict in place: it was counted as a create or an update, and it
            // is neither. The totals follow below so the headline matches the rows under it.
            match row.action {
                import::RowAction::Create => report.create_count -= 1,
                import::RowAction::Update => report.update_count -= 1,
                import::RowAction::Reject { .. } => {}
            }
            row.action = import::RowAction::Reject { reason };
            report.reject_count += 1;
        }
    }

    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "catalog.items.import",
        "catalog_item",
        &tenant_id.to_string(),
        None,
        Some(serde_json::json!({
            "created": report.create_count,
            "updated": report.update_count,
            "rejected": report.reject_count,
        })),
    )
    .await;
    (StatusCode::OK, Json(report)).into_response()
}
