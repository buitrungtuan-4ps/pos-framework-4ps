// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /.well-known/pos-cell`: which cell this is, and, for `?tenant=<slug>`, which cell is that
//! tenant's home ([ADR-0179](../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C3).
//!
//! Public, because the one who asks holds no credential yet: a device being set up resolves its
//! cell here once and then stores its `cloud_url`, and an integrator or a browser asks where a
//! tenant lives. It answers from memory, from the `[cell]` section and the signed directory this
//! cell holds, and it says nothing a directory does not already say: no personal data, no secret.
//! It never proxies and never redirects; the front door (C5) redirects.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::response::{api_error, api_error_with_details};
use pos_proto::ErrorStatus;

use crate::cell::directory::is_slug;
use crate::cell::signed::{CellDirectory, DirectoryMode, TenantHome};

/// How long a client may reuse an answer, in seconds. Short, because a tenant that moves cell is
/// moved by a new directory, and a cached answer is a stale directory (ADR-0179).
const MAX_AGE_SECS: u32 = 60;

/// The path, fixed by ADR-0179.
const WELL_KNOWN_CELL_PATH: &str = "/.well-known/pos-cell";

/// This cell, as anyone may know it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct WellKnownCell {
    /// This cell's id (`vn1`), or `default` for a cloud with no `[cell]` section.
    cell_id: String,
    /// The origin this cell answers on. Null when it is not configured.
    #[schema(required = true)]
    base_url: Option<String>,
    /// This cell's country alias (`vn`), when one is configured.
    #[schema(required = true)]
    country_alias: Option<String>,
    /// The country this cell's tenants are homed by. Null when it is not configured.
    #[schema(required = true)]
    home_country: Option<String>,
    /// The version of the signed directory this cell answers from. Null when it holds none.
    #[schema(required = true)]
    directory_version: Option<u64>,
    /// The asked-for tenant's home. Present only when `tenant` was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    tenant: Option<TenantHomeView>,
}

/// A tenant's home cell.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct TenantHomeView {
    /// The tenant slug asked about.
    tenant_slug: String,
    /// The tenant's home cell id.
    cell_id: String,
    /// That cell's origin. Null only when it is this cell and its base URL is not configured.
    #[schema(required = true)]
    base_url: Option<String>,
    /// Whether that cell is this one.
    here: bool,
}

impl TenantHomeView {
    fn new(tenant_slug: &str, home: TenantHome) -> Self {
        Self {
            tenant_slug: tenant_slug.to_owned(),
            cell_id: home.cell_id,
            base_url: home.base_url,
            here: home.here,
        }
    }
}

/// The query: an optional tenant slug.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct WellKnownQuery {
    /// A tenant slug: 1 to 63 lower-case letters, digits or `-`, not starting or ending with `-`.
    tenant: Option<String>,
}

/// Builds the public `GET /.well-known/pos-cell` sub-router over this cell's directory holder.
pub fn well_known_cell_router(directory: Arc<CellDirectory>) -> Router {
    Router::new()
        .route(WELL_KNOWN_CELL_PATH, get(well_known_cell))
        .with_state(directory)
}

#[utoipa::path(
    get,
    path = "/.well-known/pos-cell",
    params(WellKnownQuery),
    security(),
    extensions(("x-public-reason" = json!("asked before any credential exists, by a box being set up or by anyone finding a tenant's cell; it says only what the PII-free cell directory says (ADR-0179)"))),
    responses(
        (status = 200, body = WellKnownCell, description = "This cell: `cell_id`, `base_url`, \
                                      `country_alias`, `home_country` and the \
                                      `directory_version` it answers from. With `tenant`, also \
                                      `tenant`: the tenant's home `cell_id`, that cell's \
                                      `base_url`, and whether it is `here`. Never a redirect. May \
                                      be cached for 60 seconds"),
        (status = 400, description = "`tenant` is not a tenant slug", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The directory this cell holds does not list the tenant", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "This cell is configured for a signed directory and holds none yet, so it can name no tenant's home; ask again later", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "cells",
)]
/// Anyone asks which cell this is and, optionally, where a tenant is homed.
async fn well_known_cell(
    State(directory): State<Arc<CellDirectory>>,
    Query(query): Query<WellKnownQuery>,
) -> Response {
    let cell = directory.cell();
    let mut answer = WellKnownCell {
        cell_id: cell.cell_id.clone(),
        base_url: cell.base_url.clone(),
        country_alias: cell.country_alias.clone(),
        home_country: cell.home_country.map(|code| code.as_str().to_owned()),
        directory_version: directory.held().map(|held| held.directory_version),
        tenant: None,
    };
    if let Some(slug) = query.tenant.as_deref() {
        if !is_slug(slug) {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "tenant is not a tenant slug: 1 to 63 lower-case letters, digits or '-'",
                &[("tenant", "MALFORMED")],
            );
        }
        if directory.mode() == DirectoryMode::Missing {
            return api_error(
                ErrorStatus::Unavailable,
                "this cell holds no signed cell directory yet, so it cannot say where a tenant is",
            );
        }
        let Some(home) = directory.tenant_home(slug) else {
            return api_error_with_details(
                ErrorStatus::NotFound,
                "the cell directory does not list this tenant",
                &[("tenant", "NOT_LISTED")],
            );
        };
        answer.tenant = Some(TenantHomeView::new(slug, home));
    }
    let mut response = Json(answer).into_response();
    if let Ok(value) = HeaderValue::from_str(&format!("public, max-age={MAX_AGE_SECS}")) {
        response.headers_mut().insert(header::CACHE_CONTROL, value);
    }
    response
}

#[cfg(test)]
mod tests;
