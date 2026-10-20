// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What publishing a node would change, and `/admin/config/preview` (roadmap-v3 F11).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::{Deserialize, Serialize};

use super::load_tree_for_write;
use super::redaction::staff_redacted_for;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::merge::{diff, merge_layers};
use crate::config_tree::validate::ConfigValidator;
use crate::config_tree::{CapabilityValidator, ConfigTreeState, ConfigTreeStore};
use crate::http::store_groups::{BatchNode, unknown_batch_node};
use crate::http::{Json, unprocessable_violations};

/// What publishing a candidate node *would* change, computed without minting a version or saving:
/// the RFC 7386 merge patch from the store's current effective document to the candidate, so the
/// console can show a real before/after before anyone commits ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::http) struct ConfigPreview {
    /// The version the diff is computed against — the store's current version — or `null` when the
    /// store has no published configuration yet (the candidate is then entirely new).
    pub(in crate::http) from_version_id: Option<String>,
    /// The RFC 7386 merge patch a publish would apply to the effective document. An empty object
    /// means the candidate composes to exactly today's effective document.
    pub(in crate::http) diff: serde_json::Value,
    /// True when `diff` is empty — nothing would change, so there is nothing to publish.
    pub(in crate::http) unchanged: bool,
}

/// The dry-run behind a node publish: composes the store's effective document with `node_key` set to
/// `node_value` on the Store layer (index 2) and returns the merge patch from the store's current
/// effective document to that candidate. Mints no version and saves nothing — the tree is composed in
/// memory and dropped.
///
/// The candidate is validated with exactly the [`CapabilityValidator`] a real publish uses, so
/// `Err` carries the violations a publish would reject it with — the preview reports them verbatim
/// rather than a bland "invalid". `diff` mirrors the delta an up-to-date store would receive on the
/// next sync: current published effective → candidate effective.
pub(in crate::http) fn preview_config_node(
    state_before: Option<&ConfigTreeState>,
    node_key: &str,
    node_value: serde_json::Value,
) -> Result<ConfigPreview, Vec<String>> {
    preview_config_nodes(state_before, &[(node_key.to_owned(), node_value)])
}

/// The same dry-run over *several* nodes at once, which is what a compiled plan hands back.
///
/// Eleven of the thirteen batchable nodes compile to a single `(key, value)`, but the shape
/// [`BatchNodePlan::compile_for`] returns is a list, and a preview that took only the first would
/// quietly under-report a plan that writes two. Setting them all on the Store layer before composing
/// also gives the honest answer for a plan whose nodes interact — the validator sees the candidate
/// the publish would actually write, not one node of it.
///
/// That includes what QR ordering's switch makes follow it: a candidate that sets the switch, the
/// `qr` node or the `channels` list is previewed with the nodes [`crate::qr_ordering::align`] adds,
/// exactly as [`publish_config_nodes_with`] would write them.
fn preview_config_nodes(
    state_before: Option<&ConfigTreeState>,
    nodes: &[(String, serde_json::Value)],
) -> Result<ConfigPreview, Vec<String>> {
    use serde_json::Value;
    let nodes = crate::qr_ordering::align(state_before, nodes.to_vec());
    let empty = || Value::Object(serde_json::Map::new());
    // The four authored layers as stored (all empty when the store has no tree yet).
    let layers: [Value; 4] = state_before.map_or_else(
        || [empty(), empty(), empty(), empty()],
        |s| s.layers.clone(),
    );
    // The effective document the store currently holds — what a sync delta is computed against.
    let (from_version_id, current) = state_before
        .and_then(|s| s.history.last())
        .map_or((None, empty()), |v| {
            (Some(v.id.to_string()), v.effective.clone())
        });

    // Set each node on the Store layer, leaving its other keys intact, exactly as the publish route
    // does before composing.
    let mut store_layer = layers[2].clone();
    if !store_layer.is_object() {
        store_layer = empty();
    }
    if let Value::Object(map) = &mut store_layer {
        for (node_key, node_value) in nodes {
            map.insert(node_key, node_value);
        }
    }

    let candidate = merge_layers(&[&layers[0], &layers[1], &store_layer, &layers[3]]);
    CapabilityValidator.validate(&candidate)?;

    let patch = diff(&current, &candidate);
    let unchanged = matches!(&patch, Value::Object(map) if map.is_empty());
    Ok(ConfigPreview {
        from_version_id,
        diff: patch,
        unchanged,
    })
}

// --- The generic publish preview (`/admin/config/preview`, roadmap-v3 F11) -----------------------

/// The collaborators the generic preview needs: the node registry it compiles through, the config
/// trees it diffs against, and the admin/clock every `/admin` route authenticates with.
///
/// No `AuditRecorder`, and that is the point of the route: a preview mints no version, writes
/// nothing and changes nothing, so there is nothing for the trail to record. The permission is
/// still [`ConsolePermission::PublishConfig`] — seeing what a publish *would* do to a store is the
/// same disclosure as doing it, and the compiled document can carry prices. Staff are where that
/// stops being true, so the route redacts them ([`admin_preview_config_node`]).
#[derive(Clone)]
struct ConfigPreviewState<Cfg, A, C> {
    config_trees: Cfg,
    nodes: Vec<Arc<dyn BatchNode>>,
    admin: A,
    clock: C,
}

/// A `POST /admin/config/preview` body — the same `(node, arguments)` pair a batch publish takes,
/// aimed at one store instead of a cohort.
#[derive(Debug, Clone, Deserialize)]
struct PreviewNodeRequest {
    tenant_id: String,
    store_id: String,
    /// Which node to compile — one of the thirteen the registry knows.
    node: String,
    /// That node's own arguments: the single-store publish body minus the `(tenant, store)`.
    /// Nodes compiled from tenant-wide authoring take none, and an object is accepted and ignored.
    #[serde(default)]
    arguments: serde_json::Value,
}

/// What a preview answers with: the node it compiled, and the diff a publish of it would apply.
#[derive(Debug, Clone, Serialize)]
struct NodePreview {
    /// Echoed back so a console that fired several previews can tell the answers apart.
    node: String,
    /// The version the diff is computed against, or `null` when the store has no tree yet.
    from_version_id: Option<String>,
    /// The RFC 7386 merge patch the publish would apply to the store's effective document, with its
    /// staff redacted for the caller's role ([`admin_preview_config_node`]).
    diff: serde_json::Value,
    /// True when nothing would change — there is nothing to publish.
    unchanged: bool,
}

#[utoipa::path(
    post,
    path = "/admin/config/preview",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id`, `store_id`, `node` (one of the nodes a batch publish can \
                       compile), and `arguments`, that node's own publish body without the tenant \
                       and store. Nothing is written",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "What publishing the node would change: `node`; \
                                      `from_version_id`, the version the diff is against, or `null` \
                                      for a store never published to; `diff`, the merge patch \
                                      (RFC 7386) the publish would apply; and `unchanged`. The node \
                                      is compiled by the publish's own code, so the preview cannot \
                                      differ from it. The diff never shows a staff PIN hash, and \
                                      shows a name or staff code only to a role holding \
                                      console.people.read. Not audited"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID, node is not one \
                                      the batch publish knows, or the node's own publish would \
                                      refuse the arguments — with that publish's own words", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The publish would be refused, with its violations verbatim", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store or a store the node compiles from is \
                                      unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// `POST /admin/config/preview` — what publishing this node to this store *would* change.
///
/// # Why this exists when `/admin/config/campaigns/preview` already did
///
/// It did, for one node out of sixteen. Every other publish button in the console committed
/// blind: the operator pressed **Publish** and found out afterwards, from the store, whether the
/// document was what they meant (roadmap-v3 **F11**). Writing fifteen more preview routes by hand
/// would have been fifteen more chances for a preview to compile its node differently from the
/// publish it previews — a dry run that lies is worse than none.
///
/// So this route compiles through [`BatchNode`], the same registry ADR-0122's batch publish fans
/// out, which delegates to the very `*_nodes` function each single-store route calls. A preview
/// cannot drift from its publish because it is not a second copy of it. The nodes the registry does
/// not carry have no preview here, and deliberately: `locale` and `store_profile` are per-store by
/// definition (ADR-0122 §4), so there is no tenant-wide document to compile.
///
/// # Whom the diff names
///
/// A `permissions` preview carries the store's roster: each member's name, staff code and PIN
/// hash. Publishing that node shows nobody any of them, and `PublishConfig` is Ops's permission
/// too, so the diff gets a config read's treatment ([`staff_redacted_for`]): never a PIN hash, and
/// a name or code only for a role holding [`ConsolePermission::ReadPeople`]
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)). Every
/// node's preview is redacted, not only that one's: a store with layers and no published version
/// has nothing to diff against, so the diff of any node is the whole document, staff included.
///
/// It is redacted *after* it is computed, never before. Diffing redacted documents would answer
/// `unchanged: true` to a PIN reset or a rename, and an operator told that nothing would change
/// would not publish the change the till is waiting for.
///
/// Mints no version, saves nothing, audits nothing. `422` carries the violations a real publish
/// would refuse the candidate with, verbatim.
async fn admin_preview_config_node<Cfg, A, C>(
    State(state): State<ConfigPreviewState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PreviewNodeRequest>,
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
    let Some(node) = state
        .nodes
        .iter()
        .find(|candidate| candidate.key() == request.node)
    else {
        return unknown_batch_node(&state.nodes, &request.node);
    };

    // Compile exactly as the publish would: plan the tenant-wide half, then the store's own. Both
    // refusals are the publish's own words — an unpublishable instruction should read the same
    // whether it was discovered by previewing or by pressing the button.
    let plan = match node.plan(tenant_id, &request.arguments).await {
        Ok(plan) => plan,
        Err(refusal) => return refusal,
    };
    let mut compiled = match plan.compile_for(store_id).await {
        Ok(compiled) => compiled,
        Err(refusal) => return refusal,
    };

    let loaded = match load_tree_for_write(&state.config_trees, tenant_id, store_id).await {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    // And what rides along with it, as the publish composes it on this tree: a menu's `fees`.
    let _left_behind = plan.compose_on(store_id, loaded.state.as_ref(), &mut compiled);
    match preview_config_nodes(loaded.state.as_ref(), &compiled) {
        Ok(preview) => (
            StatusCode::OK,
            Json(NodePreview {
                node: request.node,
                from_version_id: preview.from_version_id,
                diff: staff_redacted_for(&preview.diff, context.admin.role),
                unchanged: preview.unchanged,
            }),
        )
            .into_response(),
        Err(violations) => unprocessable_violations(&violations),
    }
}

/// The one-route router for the generic preview. Separate from the publish routers because it holds
/// the node registry and none of them do — and because a route that writes nothing should not be
/// reachable only through a state that carries an audit recorder.
pub fn config_preview_router<Cfg, A, C>(
    config_trees: Cfg,
    nodes: Vec<Arc<dyn BatchNode>>,
    admin: A,
    clock: C,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/preview",
            post(admin_preview_config_node::<Cfg, A, C>),
        )
        .with_state(ConfigPreviewState {
            config_trees,
            nodes,
            admin,
            clock,
        })
}
