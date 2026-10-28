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
use crate::cell::signed::{CellDirectory, DirectoryMode};

/// Which kind of answer the cell gives about where a tenant is (ADR-0179 C2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub(crate) enum CellDirectoryMode {
    /// No directory key is configured: this is the only cell, home to every tenant.
    #[serde(rename = "CELL_DIRECTORY_SELF")]
    SelfOnly,
    /// A directory key is configured and no directory has been accepted yet: this cell answers for
    /// no tenant until one is.
    #[serde(rename = "CELL_DIRECTORY_MISSING")]
    Missing,
    /// A signed directory is held.
    #[serde(rename = "CELL_DIRECTORY_SIGNED")]
    Signed,
}

impl From<DirectoryMode> for CellDirectoryMode {
    fn from(mode: DirectoryMode) -> Self {
        match mode {
            DirectoryMode::SelfOnly => Self::SelfOnly,
            DirectoryMode::Missing => Self::Missing,
            DirectoryMode::Signed => Self::Signed,
        }
    }
}

/// This cell as the console reads it: the `[cell]` section, fixed when the server starts, and the
/// directory it answers from now. The same for every tenant. It holds no personal data and no
/// secret.
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
    /// Which kind of answer the cell gives about where a tenant is.
    directory_mode: CellDirectoryMode,
    /// The version of the signed directory held. Null unless one is.
    #[schema(required = true)]
    directory_version: Option<u64>,
    /// Why the last directory offered was refused, until one is accepted. The directory held, if
    /// any, is kept.
    #[schema(required = true)]
    directory_refusal: Option<String>,
}

impl CellView {
    /// The view of a cell and the directory it holds now.
    fn of(directory: &CellDirectory) -> Self {
        let mut view = Self::from(directory.cell());
        view.directory_mode = directory.mode().into();
        view.directory_version = directory.held().map(|held| held.directory_version);
        view.directory_refusal = directory.last_refusal();
        view
    }
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
            directory_mode: CellDirectoryMode::SelfOnly,
            directory_version: None,
            directory_refusal: None,
        }
    }
}

#[derive(Clone)]
struct CellState<A, C> {
    directory: Arc<CellDirectory>,
    admin: A,
    clock: C,
}

/// Builds the `GET /admin/cell` sub-router over the cell resolved at boot and the directory it
/// holds, behind [`ConsolePermission::Read`].
pub fn cell_router<A, C>(directory: Arc<CellDirectory>, admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/cell", get(admin_get_cell::<A, C>))
        .with_state(CellState {
            directory,
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
                                      `cloud.toml` has a `[cell]` section at all (`configured`), \
                                      all fixed when the server starts; and the directory it \
                                      answers from now: `directory_mode`, the signed \
                                      `directory_version` held, and why the last directory \
                                      offered was refused (`directory_refusal`)"),
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
    (StatusCode::OK, Json(CellView::of(&state.directory))).into_response()
}

#[cfg(test)]
mod tests {
    use pos_proto::locale::CountryCode;
    use serde_json::json;

    use super::CellView;
    use crate::cell::Cell;
    use crate::cell::directory::tests::vn1;
    use crate::cell::signed::{CellDirectory, parse_public_key};
    use crate::cell::testkit::{V3, V5_FORGED, public_key_line};

    fn view(directory: &CellDirectory) -> serde_json::Value {
        serde_json::to_value(CellView::of(directory)).expect("serialises")
    }

    #[test]
    fn the_default_cell_reads_as_unconfigured_and_self() {
        assert_eq!(
            view(&CellDirectory::single(Cell::single())),
            json!({
                "cell_id": "default",
                "country_alias": null,
                "home_country": null,
                "base_url": null,
                "runs_in": null,
                "runs_abroad": false,
                "configured": false,
                "directory_mode": "CELL_DIRECTORY_SELF",
                "directory_version": null,
                "directory_refusal": null,
            })
        );
    }

    #[test]
    fn a_configured_cell_reads_every_field() {
        let cell = Cell {
            runs_in: Some(CountryCode::JP),
            ..vn1()
        };
        assert_eq!(
            view(&CellDirectory::single(cell)),
            json!({
                "cell_id": "vn1",
                "country_alias": "vn",
                "home_country": "VN",
                "base_url": "https://vn1.example.com",
                "runs_in": "JP",
                "runs_abroad": true,
                "configured": true,
                "directory_mode": "CELL_DIRECTORY_SELF",
                "directory_version": null,
                "directory_refusal": null,
            })
        );
    }

    #[test]
    fn the_directory_mode_version_and_refusal_are_read_live() {
        let key = parse_public_key(public_key_line()).expect("the key parses");
        let directory = CellDirectory::trusting(vn1(), key);
        assert_eq!(view(&directory)["directory_mode"], "CELL_DIRECTORY_MISSING");

        assert!(directory.offer(V3.document, V3.minisig).is_ok());
        let signed = view(&directory);
        assert_eq!(signed["directory_mode"], "CELL_DIRECTORY_SIGNED");
        assert_eq!(signed["directory_version"], 3);
        assert_eq!(signed["directory_refusal"], serde_json::Value::Null);

        assert!(
            directory
                .offer(V5_FORGED.document, V5_FORGED.minisig)
                .is_err()
        );
        let refused = view(&directory);
        assert_eq!(
            refused["directory_version"], 3,
            "the directory held is kept"
        );
        assert!(
            refused["directory_refusal"]
                .as_str()
                .is_some_and(|why| why.contains("does not verify"))
        );
    }
}
