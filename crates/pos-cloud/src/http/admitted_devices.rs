// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/stores/{store_id}/devices/admitted`: the devices a store has admitted (ADR-0118 §4).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{ConfigTenantQuery, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_cloud_reports::refusal::rollup_error_response;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::dashboard::{RollupStore, admitted_devices};
use crate::http::{CloudApp, Json};

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/devices/admitted",
    params(
        ("store_id" = String, Path, description = "The store whose roster to read (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that store belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's admitted devices, still-admitted first and \
                                      newest admission first within each group. Each entry carries \
                                      the edge-minted local device id, when it was admitted, the \
                                      paired device whose signed-in manager minted the code (absent \
                                      for the code a box announces at boot), and when it was \
                                      retired if it has been. A retired device keeps its row. \
                                      Empty for a store that has admitted nothing, and for one \
                                      whose rollup has not been projected yet"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The rollup store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "devices",
)]
/// Which devices a store has admitted, which it has retired, and when
/// ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
///
/// The read that makes `device.admission.granted` and `device.admission.revoked` a feature rather
/// than two events with no reader — the state the tree already had one of, and which that record
/// named as the thing not to repeat. Before this the console's picture of a store's tills was zero
/// rows, however many tablets the store had actually paired.
///
/// **T3**, so `console.data.read` — the same standing the fleet detail beside it takes. The roster
/// carries device identifiers and instants: no money, and **no employee**, because who admitted a
/// device is recorded on the store's own disk and reaches no cloud (ADR-0118 §5). The tenant is
/// named explicitly, as on every `/admin` read, because the console admin is global.
///
/// Unwindowed and unpaged: a store's roster is a handful of rows, at the rate it commissions tills.
pub(in crate::http) async fn admin_admitted_devices<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    match admitted_devices(&app.rollups, tenant_id, store_id).await {
        Ok(devices) => (StatusCode::OK, Json(devices)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}
