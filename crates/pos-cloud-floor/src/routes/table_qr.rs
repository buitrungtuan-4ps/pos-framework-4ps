// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/floor/qr`: a signed QR token for each active table (ADR-0072, ADR-0057).

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_proto::determinism::ClockSource;

use super::requests::FloorListQuery;
use super::{floor_error_response, floor_tenant_store};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::floorplan::TableStore;
use crate::qr::{TableTokenSecret, mint_table_token};
use crate::registry::EntityStatus;
use axum::Json;

// --- Table QR tokens (`/admin/floor/qr`, ADR-0072 + ADR-0057) -----------------------------------

/// The collaborators the table-QR route needs: the floor store to read a store's tables, the
/// admin/clock its session guard uses, and the signing secret the token is minted with.
#[derive(Clone)]
struct TableQrState<F, A, C> {
    floor: F,
    admin: A,
    clock: C,
    secret: TableTokenSecret,
}

/// One table's printable QR: its id, the label a host reads, and the signed token the guest's QR
/// carries. The token binds `(tenant, store, table)` — no personal data — and is the same value
/// `verify_table_token` checks on a guest order (ADR-0057).
#[derive(Debug, Clone, serde::Serialize)]
struct TableQrEntry {
    table_id: String,
    label: String,
    token: String,
}

/// The store's table QR tokens, for the console's printable sheet.
#[derive(Debug, Clone, serde::Serialize)]
struct TableQrView {
    store_id: String,
    tokens: Vec<TableQrEntry>,
}

/// Builds the table-QR sub-router ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md),
/// [ADR-0057](../../../docs/adr/0057-qr-ordering.md)).
///
/// One read, `GET /admin/floor/qr`, behind [`ConsolePermission::Read`]: mints the signed QR token for
/// each of a store's active tables so the console can print a QR sheet. Wired only when a table-token
/// secret is configured (the same gate the guest QR endpoint uses — a token no verifier would accept
/// is not worth minting). The token is not PII; it is the public value printed on the code.
pub fn table_qr_router<F, A, C>(floor: F, admin: A, clock: C, secret: TableTokenSecret) -> Router
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/floor/qr", get(admin_list_table_qr::<F, A, C>))
        .with_state(TableQrState {
            floor,
            admin,
            clock,
            secret,
        })
}

#[utoipa::path(
    get,
    path = "/admin/floor/qr",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "`store_id` and `tokens`: for each active table, `table_id`, \
                                      `label`, and `token`, the signed value its printed QR code \
                                      carries, binding the tenant, store and table and no personal \
                                      data. Served only where a table-token secret is configured"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "floor",
)]
/// Mints a signed QR token for each of a store's active tables (ADR-0072).
async fn admin_list_table_qr<F, A, C>(
    State(state): State<TableQrState<F, A, C>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let tables = match TableStore::list(&state.floor, tenant_id, store_id).await {
        Ok(tables) => tables,
        Err(error) => return floor_error_response(&error),
    };
    // Minting a QR token is a read of the floor, not a write to it, so the version each row was
    // read at (ADR-0094) is dropped rather than carried into the printable sheet.
    let tokens = tables
        .into_iter()
        .map(|table| table.record)
        .filter(|table| table.status == EntityStatus::Active)
        .map(|table| TableQrEntry {
            token: mint_table_token(&state.secret, tenant_id, store_id, table.table_id),
            table_id: table.table_id.to_string(),
            label: table.label,
        })
        .collect();
    (
        StatusCode::OK,
        Json(TableQrView {
            store_id: store_id.to_string(),
            tokens,
        }),
    )
        .into_response()
}
