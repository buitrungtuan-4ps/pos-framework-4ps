// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which cell this is (`GET /admin/cell`): the `[cell]` section of `cloud.toml` as the console reads
//! it ([ADR-0179](../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C1). Read-only:
//! a cell is changed in its configuration file and by a deploy, never from the console.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_proto::determinism::ClockSource;

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::cell::Cell;

/// This cell as the console reads it. Fixed when the server starts; the same for every tenant. It
/// holds no personal data and no secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct CellView {
    /// The cell id (`vn1`), or `default` when `cloud.toml` has no `[cell]` section.
    cell_id: String,
    /// The country alias (`vn`), when one is configured.
    #[schema(required = true)]
    country_alias: Option<String>,
    /// The country this cell's tenants are homed by (`VN`). Null when no `[cell]` section is
    /// configured.
    #[schema(required = true)]
    home_country: Option<String>,
    /// The origin this cell answers on. Null when no `[cell]` section is configured.
    #[schema(required = true)]
    base_url: Option<String>,
    /// The country this cell's servers run in. Null when no `[cell]` section is configured.
    #[schema(required = true)]
    runs_in: Option<String>,
    /// Whether the servers run in a different country from the home country. A warning, never a
    /// refusal.
    runs_abroad: bool,
    /// Whether `cloud.toml` carries a `[cell]` section. False is level 0: the one and only cell.
    configured: bool,
}

impl From<&Cell> for CellView {
    fn from(cell: &Cell) -> Self {
        Self {
            cell_id: cell.cell_id.clone(),
            country_alias: cell.country_alias.clone(),
            home_country: cell.home_country.map(|code| code.as_str().to_owned()),
            base_url: cell.base_url.clone(),
            runs_in: cell.runs_in.map(|code| code.as_str().to_owned()),
            runs_abroad: cell.runs_abroad(),
            configured: cell.configured,
        }
    }
}

#[derive(Clone)]
struct CellState<A, C> {
    view: Arc<CellView>,
    admin: A,
    clock: C,
}

/// Builds the `GET /admin/cell` sub-router over the cell resolved at boot, behind
/// [`ConsolePermission::Read`].
pub fn cell_router<A, C>(cell: &Cell, admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/cell", get(admin_get_cell::<A, C>))
        .with_state(CellState {
            view: Arc::new(CellView::from(cell)),
            admin,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/admin/cell",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CellView, description = "Which cell this cloud is (ADR-0179): \
                                      `cell_id`, the optional `country_alias`, `home_country`, \
                                      `base_url`, the country it `runs_in`, whether that differs \
                                      from the home country (`runs_abroad`), and whether \
                                      `cloud.toml` has a `[cell]` section at all (`configured`). \
                                      Fixed when the server starts"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "cell",
)]
/// A console admin reads which cell this cloud is.
async fn admin_get_cell<A, C>(State(state): State<CellState<A, C>>, headers: HeaderMap) -> Response
where
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
    (StatusCode::OK, Json(state.view.as_ref().clone())).into_response()
}

#[cfg(test)]
mod tests {
    use pos_proto::locale::CountryCode;

    use super::CellView;
    use crate::cell::Cell;

    #[test]
    fn the_default_cell_reads_as_unconfigured() {
        let view = serde_json::to_value(CellView::from(&Cell::single())).expect("serialises");
        assert_eq!(
            view,
            serde_json::json!({
                "cell_id": "default",
                "country_alias": null,
                "home_country": null,
                "base_url": null,
                "runs_in": null,
                "runs_abroad": false,
                "configured": false,
            })
        );
    }

    #[test]
    fn a_configured_cell_reads_every_field() {
        let cell = Cell {
            cell_id: "vn1".to_owned(),
            country_alias: Some("vn".to_owned()),
            home_country: Some(CountryCode::VN),
            base_url: Some("https://vn1.example.com".to_owned()),
            runs_in: Some(CountryCode::JP),
            configured: true,
        };
        let view = serde_json::to_value(CellView::from(&cell)).expect("serialises");
        assert_eq!(
            view,
            serde_json::json!({
                "cell_id": "vn1",
                "country_alias": "vn",
                "home_country": "VN",
                "base_url": "https://vn1.example.com",
                "runs_in": "JP",
                "runs_abroad": true,
                "configured": true,
            })
        );
    }
}
