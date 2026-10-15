// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Publishing, halting and reading a store's OTA rollout.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::{HaltRolloutRequest, OtaConfigState, OtaRolloutQuery, PublishRolloutRequest};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::http::config_publish::{publish_config_nodes_with, strip_tree_version};
use crate::http::{Json, PublishedConfig, config_store_error_response};
use crate::lease::LeaseStore;
use crate::ota::{ArtifactStore, validate_release_tag};

/// Refuses a rollout whose `target_version` the cloud does not host
/// ([ADR-0088](../../../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 2).
///
/// Without this, the console's happy path was: publish a typo, and every store in the ring fetches,
/// gets a `404`, and stays on the old version. A `404` on the artifact route means "install nothing",
/// so that failure is silent by design — the fleet just never moves, and no log says why. The check
/// turns it into a `422` at the moment a human is looking at the field they got wrong, and lists what
/// *is* hosted so they can see the spelling they meant.
///
/// It reads the registry, not the object store, so it also works on a deployment with no
/// `[artifacts]` block: such a cloud can never upload, therefore hosts nothing, therefore cannot
/// promote — the correct posture for one that ships no edge releases.
///
/// # Errors
///
/// The [`Response`] to send: `422` naming `target_version` when nothing is hosted for it, or `503`
/// when the registry could not be read (refusing to publish is right either way — a rollout is not
/// worth guessing about).
async fn require_hosted_release<L: ArtifactStore>(
    releases: &L,
    target_version: &str,
) -> Result<(), Response> {
    if validate_release_tag(target_version).is_err() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "target_version: not usable as a storage key, so no artifact could be hosted for it",
            &[("target_version", "not usable as a storage key")],
        ));
    }
    match releases.list_artifacts(target_version).await {
        Ok(hosted) if hosted.is_empty() => Err(api_error_with_details(
            ErrorStatus::Unprocessable,
            format!(
                "no release artifact is hosted for {target_version}, so every store in the ring \
                 would fetch nothing — put one there first (POST /admin/ota/releases)"
            ),
            &[("target_version", "no artifact hosted for this version")],
        )),
        Ok(_hosted) => Ok(()),
        Err(error) => {
            tracing::error!(%error, "checking hosted release artifacts failed");
            Err(service_unavailable("the release registry"))
        }
    }
}

/// Composes a `fleet_update` node onto a store's Store layer and publishes it — the same
/// load→merge→publish→version shape as the campaigns/tax node publishes, so the other Store-level keys
/// survive. The node is validated by the config tree's `CapabilityValidator` before it commits.
pub(super) async fn admin_publish_rollout<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<PublishRolloutRequest>,
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
    if let Err(refusal) = require_hosted_release(&state.releases, &request.target_version).await {
        return refusal;
    }
    // A fresh publish is live: `halted` is omitted so it defaults false in the node.
    let node = serde_json::json!({
        "target_version": request.target_version,
        "min_ring": request.min_ring,
        "rollout_percent": request.rollout_percent,
        "signing_key_id": request.signing_key_id,
        "revoked_key_ids": request.revoked_key_ids,
    });
    let audit_detail = serde_json::json!({
        "target_version": request.target_version,
        "min_ring": request.min_ring,
        "rollout_percent": request.rollout_percent,
    });
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "fleet_update",
            node: as_requested(node),
            action: "config.ota.publish",
            detail: audit_detail,
            companions: Vec::new(),
        },
    )
    .await
}

/// Flips the kill switch on a store's published rollout: loads the `fleet_update` the store runs, sets
/// `halted`, and re-publishes it on the Store layer — preserving the rest of the rollout, so an
/// operator halts a bad rollout (or resumes a paused one) without re-typing the target, ring, and
/// key. `400` if the store has no rollout to halt.
///
/// The rollout is read from the tree's layers composed ([`ConfigTreeState::effective`]): a rollback
/// restores a version onto the Tenant layer and empties the Store layer, and a halt that read the
/// Store layer alone refused a store running the restored rollout.
///
/// It is read from the tree the halt writes over ([`halted_rollout`], composed inside the
/// conditional write's retry), so a rollout published while the halt is in flight is the one it
/// halts. The halt used to read the tree once, before the publish read it again to write, and wrote
/// the rollout it had read first, halted, over the newer one: the kill switch's own write was a lost
/// update.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every node \
              publish's closure carries — and it is the halt's own refusal, which abandons the write"
)]
pub(super) async fn admin_halt_rollout<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<HaltRolloutRequest>,
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
    let halted = request.halted;
    publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "fleet_update",
            node: |tree: Option<&ConfigTreeState>| halted_rollout(tree, halted),
            action: "config.ota.halt",
            detail: serde_json::json!({ "halted": halted }),
            companions: Vec::new(),
        },
    )
    .await
}

/// The rollout `tree` runs, its layers composed, with the kill switch set to `halted` and the rest
/// of it kept; or the `400` a store with no published rollout earns.
///
/// Called with the tree as each attempt of the write reads it, so the refusal is decided on the tree
/// the halt would write over, and a refusal abandons the write before anything is audited.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
fn halted_rollout(
    tree: Option<&ConfigTreeState>,
    halted: bool,
) -> Result<serde_json::Value, Response> {
    let Some(mut node) = tree
        .map(ConfigTreeState::effective)
        .and_then(|document| document.get("fleet_update").cloned())
    else {
        return Err(api_error(
            ErrorStatus::InvalidArgument,
            "the store has no published rollout to halt",
        ));
    };
    if let serde_json::Value::Object(map) = &mut node {
        map.insert("halted".to_owned(), serde_json::Value::Bool(halted));
    }
    Ok(node)
}

/// A node the request names in full — a rollout, a placement, a lease — for [`OtaNodeWrite::node`].
/// Published as it is on every attempt, because nothing in it depends on what the store holds.
#[expect(
    clippy::result_large_err,
    reason = "the closure's Err is an axum Response by design — the shape every node publish's \
              closure carries — and this one never takes the Err branch at all"
)]
pub(super) fn as_requested(
    node: serde_json::Value,
) -> impl FnMut(Option<&ConfigTreeState>) -> Result<serde_json::Value, Response> {
    move |_| Ok(node.clone())
}

/// One OTA node write: where it lands and how the trail names it. Grouped because the five travel
/// together and mean nothing apart — a `key` on the wrong `level` is exactly the mistake
/// [`ConfigTreeState::layer`] exists to prevent, so they are chosen at one place.
pub(super) struct OtaNodeWrite<'a, F> {
    /// The config layer the node belongs on: Store for a rollout, Device for a placement.
    pub(super) level: ConfigLevel,
    /// The node's key in that layer — `fleet_update` or `device_ota`.
    pub(super) key: &'a str,
    /// The value to set, composed on the tree as each attempt of the write reads it: a node the
    /// request names in full ignores the tree ([`as_requested`]), and one that changes what the store
    /// already holds reads it there ([`halted_rollout`]). `Err` refuses the write.
    pub(super) node: F,
    /// The audit action id.
    pub(super) action: &'a str,
    /// The audit detail, which carries no personal data — a rollout is fleet configuration.
    pub(super) detail: serde_json::Value,
    /// Nodes published in the **same** version beside `node`, so no pull can see one without the
    /// other. Only the lease bump has one: the receipt floor a replacement box must number above
    /// (ADR-0149).
    pub(super) companions: Vec<(&'a str, serde_json::Value)>,
}

/// The shared load→set-node→publish→save→audit tail behind every OTA lever.
///
/// The node is composed inside [`publish_config_nodes_with`]'s retry, on the tree each attempt reads,
/// so a lever that changes what the store holds changes what it holds when the write lands. A
/// refusal from the node abandons the write, and nothing is audited.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every node \
              publish's closure carries — and it passes on the node's own refusal unchanged"
)]
pub(super) async fn publish_ota_node<Cfg, A, C, L, F>(
    state: &OtaConfigState<Cfg, A, C, L>,
    context: &AdminContext,
    tenant_id: TenantId,
    store_id: StoreId,
    write: OtaNodeWrite<'_, F>,
) -> Response
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    F: FnMut(Option<&ConfigTreeState>) -> Result<serde_json::Value, Response>,
{
    let OtaNodeWrite {
        level,
        key,
        mut node,
        action,
        detail,
        companions,
    } = write;
    let id = match publish_config_nodes_with(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        level,
        |tree| {
            let mut nodes = vec![(key.to_owned(), node(tree)?)];
            nodes.extend(
                companions
                    .iter()
                    .map(|(companion, value)| ((*companion).to_owned(), value.clone())),
            );
            Ok(nodes)
        },
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(tenant_id),
        action,
        "store",
        &store_id.to_string(),
        None,
        Some(detail),
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

/// Reads a store's currently-published rollout — the `fleet_update` node the store runs, its layers
/// composed so a rollout a rollback restored reads as it runs — or `null` if none is published.
/// Behind [`ConsolePermission::Read`].
pub(super) async fn admin_get_rollout<Cfg, A, C, L>(
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
            let rollout = state_before
                .as_ref()
                .map(ConfigTreeState::effective)
                .and_then(|document| document.get("fleet_update").cloned())
                .unwrap_or(serde_json::Value::Null);
            (StatusCode::OK, Json(rollout)).into_response()
        }
        Err(error) => config_store_error_response(&error),
    }
}
