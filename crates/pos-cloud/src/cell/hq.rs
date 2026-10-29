// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/hq/rollups`: the daily rollups of the stores an HQ reads in other cells
//! ([ADR-0179](../../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C10).
//!
//! Behind `console.data.read`, like this cell's own rollups. It reads every source `[federation]`
//! names through the other cells' public `/v1` API and answers per store: its rollups, or why there
//! are none. **Only aggregates cross**: daily event counts, no personal data. No key is ever
//! answered: a source is named by its cell id. The whole read is bounded by [`READ_BUDGET`], after
//! which the stores not yet read say so instead of holding the console open.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use core::time::Duration;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::api_error_with_details;
use pos_cloud_reports::types::DailyRollup;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;

use super::federation::https::is_date;
use super::federation::{FederatedStore, FederationSection, RollupFetcher, RollupWindow, federate};
use super::signed::CellDirectory;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;

/// How long one HQ read may take in all, across every cell and store.
pub(crate) const READ_BUDGET: Duration = Duration::from_secs(30);

/// The HQ read as the console sees it.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct HqRollupsView {
    /// Whether `cloud.toml` has a `[federation]` section with any source.
    configured: bool,
    /// One answer per configured store, in the order configured.
    stores: Vec<HqStoreView>,
}

/// One store in another cell, and its rollups or why there are none.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct HqStoreView {
    /// The cell the store is in.
    cell_id: String,
    /// The store's ULID in that cell.
    store_id: String,
    /// Its daily rollups, oldest day first: counts only. Null when they could not be read.
    #[schema(required = true)]
    rollups: Option<Vec<DailyRollup>>,
    /// Why they could not be read. Null when they were.
    #[schema(required = true)]
    error: Option<String>,
}

impl From<FederatedStore> for HqStoreView {
    fn from(store: FederatedStore) -> Self {
        Self {
            cell_id: store.cell_id,
            store_id: store.store_id,
            rollups: store.rollups,
            error: store.error,
        }
    }
}

/// The window, as the console sends it.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct HqRollupsQuery {
    /// The first business date, `YYYY-MM-DD`, inclusive.
    from: Option<String>,
    /// The last business date, `YYYY-MM-DD`, inclusive.
    to: Option<String>,
}

#[derive(Clone)]
struct HqState<F, A, C> {
    directory: Arc<CellDirectory>,
    federation: Arc<FederationSection>,
    fetcher: F,
    admin: A,
    clock: C,
}

/// Builds the `GET /admin/hq/rollups` sub-router.
pub fn hq_rollups_router<F, A, C>(
    directory: Arc<CellDirectory>,
    federation: Option<FederationSection>,
    fetcher: F,
    admin: A,
    clock: C,
) -> Router
where
    F: RollupFetcher + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/hq/rollups", get(admin_hq_rollups::<F, A, C>))
        .with_state(HqState {
            directory,
            federation: Arc::new(federation.unwrap_or_default()),
            fetcher,
            admin,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/admin/hq/rollups",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(HqRollupsQuery),
    responses(
        (status = 200, body = HqRollupsView, description = "Whether `[federation]` names any \
                                      source (`configured`), and per configured store in another \
                                      cell its `cell_id`, `store_id`, and its daily `rollups` \
                                      (counts only, no personal data) or the `error` saying why \
                                      there are none. Read through each cell's public /v1 API \
                                      within 30 seconds in all (ADR-0179 C10)"),
        (status = 400, description = "from or to is not a YYYY-MM-DD date", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "cell",
)]
/// A console admin reads the daily rollups of the stores this HQ reads in other cells.
async fn admin_hq_rollups<F, A, C>(
    State(state): State<HqState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<HqRollupsQuery>,
) -> Response
where
    F: RollupFetcher + Clone + Send + Sync + 'static,
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
    for (field, date) in [("from", &query.from), ("to", &query.to)] {
        if date.as_deref().is_some_and(|date| !is_date(date)) {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "from and to are YYYY-MM-DD business dates",
                &[(field, "INVALID_FORMAT")],
            );
        }
    }
    let window = RollupWindow {
        from: query.from,
        to: query.to,
    };
    let stores = federate(
        &state.directory,
        &state.federation,
        &state.fetcher,
        &window,
        tokio::time::Instant::now() + READ_BUDGET,
    )
    .await;
    let view = HqRollupsView {
        configured: !state.federation.sources.is_empty(),
        stores: stores.into_iter().map(HqStoreView::from).collect(),
    };
    (StatusCode::OK, Json(view)).into_response()
}
