// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store's device placement: the `device_ota` node, written and read.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::rollout::{OtaNodeWrite, as_requested, publish_ota_node};
use super::{OtaConfigState, OtaRolloutQuery, PublishPlacementRequest};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::http::config_publish::strip_tree_version;
use crate::http::{Json, config_store_error_response};
use crate::lease::LeaseStore;
use crate::ota::ArtifactStore;

/// Composes a `device_ota` node onto a store's **Device** layer and publishes it — the placement half
/// of the rollout decision ([ADR-0052](../../../docs/adr/0052-ota-rollout-config.md)). Same
/// load→merge→publish→version shape as the rollout lever, on a different layer and key, so the store's
/// other Device-level settings survive. The config tree's `CapabilityValidator` checks the node before
/// it commits, so a ring that names no ring or a bucket past 99 is a `422` carrying the reasons.
///
/// The placement is per **store**, not per terminal, despite the key's name and the Device layer it
/// sits on: a config tree is keyed by `StoreId` and its Device layer is one document the store's
/// terminals share, so they all take the same ring and bucket. That is the granularity the delivery
/// mechanism has, and it is also the granularity a shop wants — a counter running two releases at once
/// is worse than a counter a week behind. ADR-0052 Correction 1 records this against that ADR's
/// original "per-device" wording.
pub(super) async fn admin_publish_placement<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<PublishPlacementRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishOta,
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
    let node = serde_json::json!({
        "ring": request.ring,
        "canary_bucket": request.canary_bucket,
    });
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Device,
            key: "device_ota",
            node: as_requested(node.clone()),
            action: "config.ota.placement",
            detail: node,
            companions: Vec::new(),
        },
    )
    .await
}

/// Reads a store's published placement — the `device_ota` node the store runs, its layers composed
/// so a placement a rollback restored reads as it runs — or `null` if the store has never been
/// placed. Behind [`ConsolePermission::Read`].
pub(super) async fn admin_get_placement<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Query(query): Query<OtaRolloutQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    match state
        .config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(state_before) => {
            let placement = state_before
                .as_ref()
                .map(ConfigTreeState::effective)
                .and_then(|document| document.get("device_ota").cloned())
                .unwrap_or(serde_json::Value::Null);
            (StatusCode::OK, Json(placement)).into_response()
        }
        Err(error) => config_store_error_response(&error),
    }
}
