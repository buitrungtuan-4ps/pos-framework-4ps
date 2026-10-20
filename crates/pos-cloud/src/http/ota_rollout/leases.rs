// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store lease: bump it, read it, and the receipt floor a bump carries.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_core::lease::LeaseGeneration;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::rollout::{OtaNodeWrite, as_requested, publish_ota_node};
use super::{
    BumpLeaseRequest, OtaConfigState, OtaRolloutQuery, lease_moved_response,
    lease_store_error_response, lease_undrained_response, region_refusal_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::config_publish::if_match_lease;
use crate::http::{Json, lease_bump};
use crate::lease::{LeaseBumpOutcome, LeaseStore, lease_node, resolve_region};
use crate::ota::ArtifactStore;

#[utoipa::path(
    post,
    path = "/admin/config/lease/bump",
    security(("session_cookie" = [])),
    params(
        ("If-Match" = String, Header, description = "The lease generation the decision was made \
                                                     against, or `*` for a store never issued one. \
                                                     Required"),
    ),
    request_body(
        description = "`tenant_id` and `store_id`; optionally `edge_placement` \
                       (`EDGE_PLACEMENT_IN_STORE`, `EDGE_PLACEMENT_HOSTED_BY_OPERATOR` or \
                       `EDGE_PLACEMENT_HOSTED_BY_PLATFORM`) when the store is moving, absent for a \
                       swap in place (ADR-0110); for a hosted placement, `region_country` and \
                       `region_label`, where its data will rest (ADR-0114); and \
                       `acknowledge_undrained`, the undrained generation this bump knowingly \
                       abandons. No body names a generation: the server issues it",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Issued and published: a different machine is the store now \
                                      (ADR-0108). The new generation is published as the store's \
                                      `lease` node, with the receipt floor beside it where the cloud \
                                      has one (ADR-0149), and the box on the old generation stops \
                                      installing updates. The body carries `config_version_id`. \
                                      Audited with the generation issued"),
        (status = 400, description = "tenant_id or store_id is not a ULID; edge_placement is not one \
                                      of the three; the region is incomplete, malformed or given \
                                      where no hosted placement is named; If-Match is absent or \
                                      malformed; or a value needs a recorded confirmation first", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The lease moved since you read it: re-read and retry. The \
                                      error names the current generation", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to publish the lease node lost a race with \
                                      another publish: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The previous machine still holds events this cloud has never \
                                      seen, naming the generation to acknowledge to proceed, which \
                                      abandons them; or the store's configuration would not validate \
                                      with the node", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The lease store or the configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "lease",
)]
/// `POST /admin/config/lease/bump` — issues this store's next lease generation and publishes it
/// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)).
///
/// This is the act of saying **"a different machine is the store now"**. One request does two things
/// that must not drift apart: it advances the authoritative counter in `store_lease`, and it
/// publishes the resulting number as the Store-layer `lease` node — the only rail that reaches a
/// till. The node is *derived* here, never authored: no route accepts a `lease` body, because a
/// generation a person could type would not be an authority.
///
/// A store's **first** bump lands on generation `0` and supersedes nobody — ADR-0049's "the first
/// lease a store ever issues is generation `0`" — so it is the act of putting a store under the
/// lease at all. Every bump after that supersedes whatever box holds the previous number: it stops
/// installing updates on its next config pull, and stays stopped across reboots, because the edge
/// takes its generation once and thereafter only compares.
///
/// Behind [`ConsolePermission::ManageStores`] rather than `PublishOta`: replacing the machine that
/// *is* a store is store management, and the person who does it is the person who provisions
/// hardware, not the person who runs an upgrade campaign. The audit entry names the generation
/// issued, so "who replaced this box, and when" has an answer.
///
/// # The third thing that must not drift apart
///
/// [ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md) makes *where* the store
/// runs an attribute of the store, and this route the only writer of it. A body naming
/// `edge_placement` is a **move**; a body omitting it is a swap in place, and keeps whatever the
/// store had. Either way the value is written inside the bump's own statement, so a reader can never
/// catch the two disagreeing — which is not fussiness but the difference between a console that says
/// "Offline-capable: yes" about a hosted store and one that does not.
///
/// An explicit `EDGE_PLACEMENT_UNSPECIFIED` is refused rather than accepted as "no change". On the
/// wire that token means *this message did not say*, so a caller that sends it has said nothing in a
/// field that looks like it said something; omitting the field is how you mean that, and a refusal
/// that says so beats a request that appears to move a store and does not.
pub(super) async fn admin_bump_lease<Cfg, A, C, L>(
    State(state): State<OtaConfigState<Cfg, A, C, L>>,
    headers: HeaderMap,
    Json(request): Json<BumpLeaseRequest>,
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    let context = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await?;
    let [tenant_id, store_id] = parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ])?;
    let (tenant_id, store_id) = (TenantId::new(tenant_id), StoreId::new(store_id));
    let requested_placement = lease_bump::requested_placement(request.edge_placement.as_deref())?;
    // Where the store's data will rest, decided by the same request that decides which machine
    // holds it (ADR-0114). The rule is a pure function so it can be tested without an HTTP stack;
    // here it is only turned into an envelope.
    let region = resolve_region(
        requested_placement,
        request.region_country.as_deref(),
        request.region_label.as_deref(),
    )
    .map_err(region_refusal_response)?;
    // The counter moves first. If the publish then fails the store is on a generation no till has
    // been told about, which is the safe half of the split: every box keeps the generation it holds
    // and keeps updating, and the next bump (or a re-publish) closes it. The opposite order would
    // publish a generation the authority never issued.
    //
    // The placement rides inside that same write rather than following it, so there is no interval
    // in which the store record and the lease disagree (ADR-0110).
    let expected_generation = if_match_lease(&headers)?;
    let acknowledged = request.acknowledge_undrained.map(LeaseGeneration::new);
    let bump = match state
        .config_trees
        .bump(
            tenant_id,
            store_id,
            state.clock.now(),
            requested_placement,
            region,
            acknowledged,
            expected_generation,
        )
        .await
    {
        Ok(LeaseBumpOutcome::Issued(bump)) => bump,
        // Both refusals return *before* `lease_node` and `publish_ota_node` below. That ordering is
        // the point: the comment above explains why the counter moves before the publish, and a
        // request that issued no generation must not reach a publish that would have to name one.
        Ok(LeaseBumpOutcome::VersionMismatch { current }) => {
            return Err(lease_moved_response(current));
        }
        Ok(LeaseBumpOutcome::Undrained { superseded }) => {
            return Err(lease_undrained_response(superseded));
        }
        Err(error) => return Err(lease_store_error_response(&error)),
    };
    let node = lease_node(bump.generation);
    let receipt_floor = receipt_floor(&state.config_trees, tenant_id, store_id).await;
    let companions = receipt_floor
        .map(|number| ("receipt_floor", serde_json::json!({ "number": number })))
        .into_iter()
        .collect();
    let response = publish_ota_node(
        &state,
        &context,
        tenant_id,
        store_id,
        OtaNodeWrite {
            level: ConfigLevel::Store,
            key: "lease",
            node: as_requested(node),
            action: "config.lease.bump",
            // `edge_placement` is what the store now is; `edge_placement_moved` says whether this
            // bump chose it. Without the second flag the trail cannot tell a store that was moved
            // to a hosted machine from one whose already-hosted box was swapped — the same row
            // either way, and only one of them is a change of what the store can promise.
            detail: lease_bump::bump_audit_detail(
                &bump,
                requested_placement,
                acknowledged,
                receipt_floor,
            ),
            companions,
        },
    )
    .await;
    Ok(response)
}

/// The receipt floor a lease bump publishes beside the lease (ADR-0149): the highest receipt number
/// the cloud has ingested from the store, so the box that learns it is the store has already learned
/// where its numbers must start. A log that cannot be read does not hold the takeover back: the lease
/// still publishes, without a floor, and the warning says so.
async fn receipt_floor<Cfg: LeaseStore>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Option<u64> {
    match config_trees
        .highest_receipt_number(tenant_id, store_id)
        .await
    {
        Ok(floor) => floor,
        Err(error) => {
            tracing::warn!(
                %error,
                store = %store_id,
                "could not read the highest receipt number; the lease is published without a receipt floor"
            );
            None
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/config/lease",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "`generation`, the store's authoritative lease generation, or \
                                      `null` if it has never been issued one. Read from the lease \
                                      itself, not from what the tills were told"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The lease store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "lease",
)]
/// `GET /admin/config/lease` — the store's authoritative lease generation, or `null` if it has never
/// been issued one. Behind [`ConsolePermission::Read`].
///
/// Read from the `store_lease` row, not from the published node: the row is the authority and the
/// node is its delivery, and a console that read the node would be reporting what the *tills* were
/// told rather than what is true.
pub(super) async fn admin_get_lease<Cfg, A, C, L>(
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
    match state.config_trees.current(tenant_id, store_id).await {
        Ok(current) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "generation": current.map(LeaseGeneration::value),
            })),
        )
            .into_response(),
        Err(error) => lease_store_error_response(&error),
    }
}
