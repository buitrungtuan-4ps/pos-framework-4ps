// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/einvoice-symbol` and `POST /admin/config/einvoice-symbol/allocate`: HQ gives
//! one store the symbol it numbers its e-invoices under, by hand or from its country's rule
//! ([ADR-0225](../../../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md)
//! decision 2, [ADR-0240](../../../../../docs/adr/0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)).
//!
//! A Vietnamese store seeded to issue with every settle refuses to settle until it has a symbol, so
//! this is how a store gets one. It is not a setting: a symbol names one issuing point and is never
//! inherited from a brand or a tenant, so it is written only on the Store layer, beside the
//! `einvoice` node's settings, which it leaves as they are. Every symbol given is recorded in the
//! tenant's ledger, and a symbol the ledger gives another store is refused.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_country::CountryModule;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::einvoice::{InvoiceSymbol, PublishedEInvoice};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::audit::AuditRecorder;
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::einvoice::symbols::{AllocateError, SymbolLedger, SymbolRecord, SymbolSource, allocate};
use crate::http::locale_node::published_country;
use crate::http::{Json, PublishedConfig};
use crate::lease::LeaseStore;

/// What the symbol routes need: the trees, the session guard, the clock, the trail and the ledger.
#[derive(Clone)]
pub(super) struct EInvoiceSymbolState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    ledger: Arc<dyn SymbolLedger>,
}

/// Builds the e-invoice symbol sub-router (ADR-0240): the hand route and the allocate route.
pub fn einvoice_symbol_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    ledger: Arc<dyn SymbolLedger>,
) -> Router
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/einvoice-symbol",
            put(admin_publish_einvoice_symbol::<Cfg, A, C>),
        )
        .route(
            "/admin/config/einvoice-symbol/allocate",
            post(admin_allocate_einvoice_symbol::<Cfg, A, C>),
        )
        .with_state(EInvoiceSymbolState {
            config_trees,
            admin,
            clock,
            audit,
            ledger,
        })
}

/// A `PUT /admin/config/einvoice-symbol` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PublishEInvoiceSymbolRequest {
    tenant_id: String,
    store_id: String,
    /// The symbol, such as `1C26MAA`: the shape the store's country writes.
    symbol: String,
}

/// A `POST /admin/config/einvoice-symbol/allocate` body: the store to give its next symbol.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct AllocateEInvoiceSymbolRequest {
    tenant_id: String,
    store_id: String,
}

/// The symbol allocated and the config version that published it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct AllocatedEInvoiceSymbol {
    /// The symbol the store now numbers under, such as `1C26MAA`.
    symbol: String,
    /// The config version that published it.
    config_version_id: String,
}

#[utoipa::path(
    put,
    path = "/admin/config/einvoice-symbol",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        content = PublishEInvoiceSymbolRequest, description = "`tenant_id` and `store_id`, then the `symbol` the store numbers its e-invoices under, in its country's shape (ADR-0225 decision 2)",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = PublishedConfig, description = "Recorded in the tenant's symbol ledger and published as the `symbol` of the \
                                      store's `einvoice` node on its Store layer, the node's settings kept. Audited with the symbol"),
        (status = 400, description = "tenant_id or store_id is not a ULID, or the symbol is not one the store's country \
                                      writes (`INVALID_FORMAT` on `symbol`)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The ledger gives the symbol to another store of the tenant \
                                      (`E_INVOICE_SYMBOL_IN_USE` on `symbol`, ADR-0240 decision 2)", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race \
                                      with another publish: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The store's configuration would not validate with the node", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store or the ledger is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// HQ publishes the symbol one store numbers its e-invoices under. Checked against the store's
/// country where it has published one, else against the shape every country shares, and against
/// the tenant's ledger.
pub(super) async fn admin_publish_einvoice_symbol<Cfg, A, C>(
    State(state): State<EInvoiceSymbolState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishEInvoiceSymbolRequest>,
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let (context, tenant_id, store_id) =
        admitted(&state, &headers, &request.tenant_id, &request.store_id).await?;
    let symbol = InvoiceSymbol::parse(&request.symbol).ok_or_else(not_a_symbol)?;
    let countries = crate::countries::registry();
    let module = store_country(&state, &countries, tenant_id, store_id).await?;
    if module.is_some_and(|module| !module.is_valid_invoice_symbol(symbol.as_str())) {
        return Err(not_a_symbol());
    }
    let wanted = SymbolRecord {
        symbol: symbol.as_str().to_owned(),
        store_id: Some(store_id.to_string()),
        lease_generation: current_generation(&state, tenant_id, store_id).await,
        source: SymbolSource::Manual,
        recorded_at_ms: state.clock.now().as_milliseconds_since_epoch(),
    };
    let held = state
        .ledger
        .record(tenant_id, wanted)
        .await
        .map_err(|_| service_unavailable("e-invoice symbol ledger"))?;
    if !held.is_held_by(store_id) {
        return Err(symbol_in_use());
    }
    let id = publish_symbol(&state, &context, tenant_id, store_id, &held, "publish").await?;
    Ok((
        StatusCode::OK,
        Json(PublishedConfig {
            config_version_id: id,
        }),
    )
        .into_response())
}

#[utoipa::path(
    post,
    path = "/admin/config/einvoice-symbol/allocate",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        content = AllocateEInvoiceSymbolRequest, description = "The store to give the next free symbol of its country's rule (ADR-0240 decision 3)",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = AllocatedEInvoiceSymbol, description = "The next symbol of the current year the tenant's ledger does not hold, \
                                      recorded for the store and published as its `einvoice` node's `symbol`. Audited"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "Every symbol of the year's rule is in the ledger \
                                      (`E_INVOICE_SYMBOL_EXHAUSTED` on `symbol`)", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race \
                                      with another publish: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The store's country has no allocation rule, or the store has published no country: \
                                      type the symbol by hand (`E_INVOICE_SYMBOL_NO_RULE` on `symbol`)", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store or the ledger is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// HQ gives one store the next free symbol of its country's rule (ADR-0240 decision 3).
pub(super) async fn admin_allocate_einvoice_symbol<Cfg, A, C>(
    State(state): State<EInvoiceSymbolState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<AllocateEInvoiceSymbolRequest>,
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let (context, tenant_id, store_id) =
        admitted(&state, &headers, &request.tenant_id, &request.store_id).await?;
    let countries = crate::countries::registry();
    let module = store_country(&state, &countries, tenant_id, store_id).await?;
    let now = state.clock.now();
    let generation = current_generation(&state, tenant_id, store_id).await;
    let record = allocate(
        state.ledger.as_ref(),
        module,
        tenant_id,
        Some(store_id),
        generation,
        utc_year(now),
        now,
    )
    .await
    .map_err(|error| allocate_refusal(&error))?;
    let id = publish_symbol(&state, &context, tenant_id, store_id, &record, "allocate").await?;
    Ok((
        StatusCode::OK,
        Json(AllocatedEInvoiceSymbol {
            symbol: record.symbol,
            config_version_id: id,
        }),
    )
        .into_response())
}

/// The session's context and the two ids, or the refusal.
async fn admitted<Cfg, A, C>(
    state: &EInvoiceSymbolState<Cfg, A, C>,
    headers: &HeaderMap,
    tenant_id: &str,
    store_id: &str,
) -> Result<(AdminContext, TenantId, StoreId), Response>
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = require_permission(
        &state.admin,
        &state.clock,
        headers,
        ConsolePermission::PublishConfig,
    )
    .await?;
    let [tenant_id, store_id] =
        parse_ulid_fields([("tenant_id", tenant_id), ("store_id", store_id)])?;
    Ok((context, TenantId::new(tenant_id), StoreId::new(store_id)))
}

/// The store's published country's module, or `None` where it has published none.
async fn store_country<'r, Cfg, A, C>(
    state: &EInvoiceSymbolState<Cfg, A, C>,
    countries: &'r pos_country::CountryRegistry,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<Option<&'r dyn CountryModule>, Response>
where
    Cfg: ConfigTreeStore,
{
    let tree = state
        .config_trees
        .load(tenant_id, store_id)
        .await
        .map_err(|_| service_unavailable("configuration"))?;
    Ok(tree
        .as_ref()
        .and_then(|tree| published_country(&tree.record))
        .and_then(|code| countries.get(code)))
}

/// The store's lease generation, where it has one and it can be read; the ledger records it as a
/// fact about the allocation and nothing depends on it.
async fn current_generation<Cfg: LeaseStore, A, C>(
    state: &EInvoiceSymbolState<Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Option<i64> {
    let current = state.config_trees.current(tenant_id, store_id).await;
    current
        .ok()
        .flatten()
        .and_then(|generation| i64::try_from(generation.value()).ok())
}

/// Publishes `record`'s symbol on the store's `einvoice` node and audits it as
/// `config.einvoice_symbol.<verb>`.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response, the shape publish_config_nodes_with takes"
)]
async fn publish_symbol<Cfg, A, C>(
    state: &EInvoiceSymbolState<Cfg, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    store_id: StoreId,
    record: &SymbolRecord,
    verb: &str,
) -> Result<String, Response>
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let id = pos_cloud_config::publish::publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        |tree| Ok(vec![symbol_node(tree, &record.symbol)]),
    )
    .await?;
    pos_cloud_api::console::audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(tenant_id),
        &format!("config.einvoice_symbol.{verb}"),
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({
            "symbol": record.symbol,
            "source": record.source.as_str(),
        })),
    )
    .await;
    Ok(id.to_string())
}

/// The store's `einvoice` node with `symbol` set and its settings kept.
pub(crate) fn symbol_node(
    tree: Option<&crate::config_tree::ConfigTreeState>,
    symbol: &str,
) -> (String, serde_json::Value) {
    let mut node = tree
        .and_then(|tree| tree.layer(ConfigLevel::Store).get(PublishedEInvoice::NODE))
        .filter(|node| node.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    node["symbol"] = serde_json::json!(symbol);
    (PublishedEInvoice::NODE.to_owned(), node)
}

/// The calendar year of `now` in UTC: the year a symbol is allocated for (ADR-0240 decision 3).
pub(crate) fn utc_year(now: Timestamp) -> i16 {
    let days = now.as_milliseconds_since_epoch().div_euclid(86_400_000);
    // Civil-from-days (the proleptic Gregorian calendar), as the submitter's day count inverts.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400 + i64::from(month_from_march >= 10);
    i16::try_from(year).unwrap_or(i16::MAX)
}

fn allocate_refusal(error: &AllocateError) -> Response {
    match error {
        AllocateError::NoRule => api_error_with_details(
            ErrorStatus::Unprocessable,
            "the store's country has no symbol allocation rule: type the symbol by hand",
            &[("symbol", "E_INVOICE_SYMBOL_NO_RULE")],
        ),
        AllocateError::Exhausted => api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "every symbol of the year's rule is already in the tenant's ledger",
            &[("symbol", "E_INVOICE_SYMBOL_EXHAUSTED")],
        ),
        AllocateError::Ledger(_) => service_unavailable("e-invoice symbol ledger"),
    }
}

fn symbol_in_use() -> Response {
    api_error_with_details(
        ErrorStatus::FailedPrecondition,
        "the symbol is given to another store of the tenant",
        &[("symbol", "E_INVOICE_SYMBOL_IN_USE")],
    )
}

fn not_a_symbol() -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        "the symbol is not one the store's country writes",
        &[("symbol", "INVALID_FORMAT")],
    )
}

#[cfg(test)]
mod tests {
    use pos_proto::time::Timestamp;

    use super::utc_year;

    #[test]
    fn the_year_of_an_instant_is_its_utc_year() {
        for (millis, year) in [
            (0, 1970),
            (1_767_225_599_999, 2025),
            (1_767_225_600_000, 2026),
            (1_798_761_599_999, 2026),
            (1_798_761_600_000, 2027),
        ] {
            let now = Timestamp::from_milliseconds_since_epoch(millis).expect("an instant");
            assert_eq!(utc_year(now), year, "{millis}");
        }
    }
}
