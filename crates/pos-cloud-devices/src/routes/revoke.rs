// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/stores/{store_id}/devices/revoke`: a store's device retired remotely (ADR-0118 §6).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{DeviceId, StoreId, TenantId};
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use pos_cloud_config::publish::publish_config_nodes_with;
use pos_cloud_config::route::PublishedConfig;

// --- Device revocation (`/admin/stores/{id}/devices/revoke`, ADR-0118 §6) -----------------------

/// The collaborators the device-revoke route needs: the config-tree store the deny-list is appended
/// to, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct DeviceRevokeState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Retire one till on a store: the tenant it belongs to and the store-minted local device id.
///
/// `local_device_id` is the id from `GET /admin/stores/{id}/devices/admitted` — the store's own,
/// **not** a console `Device.device_id`. Nothing joins those two identity spaces, so an id taken
/// from the wrong list names a device the store has never had, and the store records it as done
/// without retiring anything.
#[derive(Debug, Clone, Deserialize)]
struct RevokeDeviceRequest {
    tenant_id: String,
    local_device_id: String,
}

/// Builds the device-revoke sub-router
/// ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §6).
///
/// Its own router rather than a route on the config publishes, because it takes
/// `console.stores.manage` rather than the `PublishConfig` those share.
pub fn device_revoke_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/stores/{store_id}/devices/revoke",
            post(admin_revoke_device::<Cfg, A, C>),
        )
        .with_state(DeviceRevokeState {
            config_trees,
            admin,
            clock,
            audit,
        })
}

#[utoipa::path(
    post,
    path = "/admin/stores/{store_id}/devices/revoke",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.stores.manage"))),
    params(
        ("store_id" = String, Path, description = "The store the till belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The deny-list now names the device, as of the config version \
                                      returned. The store carries it out on its next config pull — \
                                      within thirty seconds for a store with a working link, and on \
                                      its next boot otherwise — and the admitted-devices roster \
                                      then shows the device retired. Re-revoking a device already \
                                      on the list succeeds and adds nothing"),
        (status = 400, description = "tenant_id, store_id or local_device_id is absent or not a \
                                      ULID, or this store's deny-list is already at its cap", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 409, description = "Concurrent config publishes kept winning the race", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "The composed node did not validate", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The config tree is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "devices",
)]
/// Retires one till on a store the console cannot otherwise reach
/// ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §6).
///
/// **Appends to the `revoked_devices` node, never replaces it**, and the append happens *inside* the
/// conditional-write retry (see [`publish_config_nodes_with`]). Two admins each retiring a different
/// till therefore cannot lose each other's entry — which for a deny-list would mean a device left
/// trading that somebody was told had been retired.
///
/// **There is no un-revoke route, and that is the design.** The list is monotone: retiring a device
/// deletes its pairing row on the box, and no document can put it back. A till that legitimately
/// returns is a *fresh pairing* with a fresh id, which the list never names.
///
/// **A `200` is not proof the device is off.** It means the instruction is published. The store
/// applies it on its next pull and may refuse it whole — its blast-radius caps are the only place
/// the roster is authoritative — and it has no channel to say so. Read the admitted-devices roster
/// to see the retirement land; if it does not, the box's own log says why (ADR-0117).
///
/// `console.stores.manage` (Owner/Admin) rather than the `PublishConfig` norm that includes Ops. The
/// same standing as the lease bump beside it on the Fleet screen, and for the same reason: this is
/// not a day-to-day publish, it takes a terminal out of service, and a mistake can leave a shop
/// unable to sell.
///
/// The trail records the store and the device id. No employee: who at the console did this is the
/// audit row's actor, and no *store* employee is involved at all (ADR-0118 §5).
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — it is the refusal this \
              route returns, and boxing it would buy nothing but a deref at every other call site"
)]
async fn admin_revoke_device<Cfg, A, C>(
    State(state): State<DeviceRevokeState<Cfg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<RevokeDeviceRequest>,
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
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id, device_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &store_id),
        ("local_device_id", &request.local_device_id),
    ]) {
        Ok([tenant_id, store_id, device_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            DeviceId::new(device_id),
        ),
        Err(refusal) => return refusal,
    };
    let listed = device_id.to_string();
    let id = match publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        |before| append_revoked_device(before, &listed),
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.devices.revoke",
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({ "local_device_id": listed })),
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

/// Composes the `revoked_devices` node from the one the store runs plus `listed`.
///
/// The list is read from the tree's layers composed ([`ConfigTreeState::effective`]) and written,
/// with `listed` added, onto the Store layer as before. Read from the Store layer alone it was wrong
/// after a rollback, which restores a version onto the Tenant layer and empties the Store layer: the
/// append wrote a list of `listed` alone, and since a list replaces when layers merge, the store
/// stopped refusing every device it had retired.
///
/// Called on every attempt of the conditional-write retry, so the merge always sees the tree as just
/// read. Three properties, all load-bearing:
///
/// - **Append only.** Every id already there survives, including ones this admin has never seen.
/// - **Idempotent.** An id already on the list re-publishes the same list; the operator gets a `200`
///   and a version rather than a duplicate entry the store would then refuse as malformed.
/// - **Capped.** The list never shrinks and rides every config pull, so it needs a ceiling; refusing
///   here names the same number the store would.
///
/// An unreadable existing node is treated as absent rather than refused. A node that cannot be read
/// as a list of strings cannot be preserved, so preserving it is not on offer — and refusing would
/// leave an operator unable to retire a lost tablet because of a hand-written document they may not
/// even be able to see. The publish that follows re-validates the composed node, so what lands is
/// well-formed either way.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the route returns, the \
              same shape every other route helper carries"
)]
pub(super) fn append_revoked_device(
    before: Option<&ConfigTreeState>,
    listed: &str,
) -> Result<Vec<(String, serde_json::Value)>, Response> {
    let mut ids: Vec<String> = before
        .map(ConfigTreeState::effective)
        .as_ref()
        .and_then(|document| document.get("revoked_devices"))
        .and_then(|node| node.get("device_ids"))
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if !ids.iter().any(|held| held == listed) {
        if ids.len() >= pos_core::device_revocation::MAX_REVOKED_DEVICES {
            return Err(api_error(
                ErrorStatus::InvalidArgument,
                format!(
                    "this store's device deny-list already holds {} ids, which is its cap",
                    pos_core::device_revocation::MAX_REVOKED_DEVICES
                ),
            ));
        }
        ids.push(listed.to_owned());
    }
    Ok(vec![(
        "revoked_devices".to_owned(),
        serde_json::json!({ "device_ids": ids }),
    )])
}
