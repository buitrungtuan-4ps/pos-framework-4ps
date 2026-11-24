// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/einvoice-symbol`: HQ gives one store the symbol it numbers its e-invoices
//! under ([ADR-0225](../../../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md)
//! decision 2).
//!
//! A Vietnamese store seeded to issue with every settle refuses to settle until it has a symbol, so
//! this is how a pilot store gets one. It is not a setting: a symbol names one issuing point and is
//! never inherited from a brand or a tenant, so it is written only on the Store layer, beside the
//! `einvoice` node's settings, which it leaves as they are. Allocating a symbol per lease generation
//! automatically is still to come.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::einvoice::{InvoiceSymbol, PublishedEInvoice};
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::ConfigChannelsState;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::locale_node::published_country;
use crate::http::{Json, PublishedConfig};

/// A `PUT /admin/config/einvoice-symbol` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PublishEInvoiceSymbolRequest {
    tenant_id: String,
    store_id: String,
    /// The symbol, such as `1C26MAA`: the shape the store's country writes.
    symbol: String,
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
        (status = 200, body = PublishedConfig, description = "Published as the `symbol` of the store's `einvoice` node on its Store layer, \
                                      the node's settings kept. Audited with the symbol"),
        (status = 400, description = "tenant_id or store_id is not a ULID, or the symbol is not one the store's country \
                                      writes (`INVALID_FORMAT` on `symbol`)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race \
                                      with another publish: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The store's configuration would not validate with the node", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// HQ publishes the symbol one store numbers its e-invoices under. Checked against the store's
/// country where it has published one, else against the shape every country shares.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response, the shape publish_config_nodes_with takes"
)]
pub(super) async fn admin_publish_einvoice_symbol<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishEInvoiceSymbolRequest>,
) -> Response
where
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let Some(symbol) = InvoiceSymbol::parse(&request.symbol) else {
        return not_a_symbol();
    };
    let countries = crate::countries::registry();
    let published = pos_cloud_config::publish::publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        |tree| {
            let country = tree.and_then(published_country);
            let module = country.and_then(|code| countries.get(code));
            if module.is_some_and(|module| !module.is_valid_invoice_symbol(symbol.as_str())) {
                return Err(not_a_symbol());
            }
            let mut node = tree
                .and_then(|tree| tree.layer(ConfigLevel::Store).get(PublishedEInvoice::NODE))
                .filter(|node| node.is_object())
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            node["symbol"] = serde_json::json!(symbol.as_str());
            Ok(vec![(PublishedEInvoice::NODE.to_owned(), node)])
        },
    )
    .await;
    match published {
        Ok(id) => {
            pos_cloud_api::console::audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "config.einvoice_symbol.publish",
                "store",
                &store_id.to_string(),
                None,
                Some(serde_json::json!({ "symbol": symbol.as_str() })),
            )
            .await;
            (
                StatusCode::OK,
                Json(PublishedConfig {
                    config_version_id: id.to_string(),
                }),
            )
                .into_response()
        }
        Err(refusal) => refusal,
    }
}

fn not_a_symbol() -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        "the symbol is not one the store's country writes",
        &[("symbol", "INVALID_FORMAT")],
    )
}
