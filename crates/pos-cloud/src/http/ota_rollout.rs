// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The OTA rollout levers (`/admin/config/ota` and the lease, ADR-0078): rollouts, placement, the
//! store lease and handovers.
//!
//! The state, request bodies, router and the refusals the lease routes share are here.

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use handovers::{admin_retire_handover, admin_settle_handover};
use leases::{admin_bump_lease, admin_get_lease};
use placement::{admin_get_placement, admin_publish_placement};
use pos_cloud_api::response::{api_error_with_details, service_unavailable};
use pos_core::lease::LeaseGeneration;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use rollout::{admin_get_rollout, admin_halt_rollout, admin_publish_rollout};
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::config_tree::ConfigTreeStore;
use crate::lease::{LeaseStore, LeaseStoreError, RegionRefusal};
use crate::ota::ArtifactStore;

mod handovers;
mod leases;
mod placement;
mod rollout;

// --- OTA rollout levers (`/admin/config/ota`, ADR-0078, Track O3) --------------------------------

/// The collaborators the OTA rollout routes need: the config-tree store the `fleet_update` node is
/// written onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct OtaConfigState<Cfg, A, C, L> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    releases: L,
}

/// A `PUT /admin/config/ota` body: the `(tenant, store)` and the rollout to publish. There is no
/// `halted` field — a fresh publish is live; the kill switch is `POST /admin/config/ota/halt`.
#[derive(Debug, Clone, Deserialize)]
struct PublishRolloutRequest {
    tenant_id: String,
    store_id: String,
    target_version: String,
    min_ring: String,
    rollout_percent: u8,
    signing_key_id: String,
    #[serde(default)]
    revoked_key_ids: Vec<String>,
}

/// A `POST /admin/config/ota/halt` body: the `(tenant, store)` whose rollout to halt (`true`) or
/// resume (`false`) — the kill switch, without re-typing the whole rollout.
#[derive(Debug, Clone, Deserialize)]
struct HaltRolloutRequest {
    tenant_id: String,
    store_id: String,
    halted: bool,
}

/// A `GET /admin/config/ota?tenant_id=&store_id=` query: which store's published rollout to read.
/// Also serves `GET /admin/config/ota/placement` — the same `(tenant, store)` question about the
/// other OTA node.
#[derive(Debug, Clone, Deserialize)]
struct OtaRolloutQuery {
    tenant_id: String,
    store_id: String,
}

/// A `POST /admin/config/lease/bump` body: the `(tenant, store)` whose next generation to issue, and
/// optionally where the machine taking it runs
/// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// `edge_placement` absent means "replace the machine where the store already is" — ADR-0003's swap,
/// and every bump this route served before the field existed, which is why it defaults rather than
/// being required. Present, it means the store is **moving**, and this is the only route in the tree
/// that writes the value.
#[derive(Debug, Clone, Deserialize)]
struct BumpLeaseRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    edge_placement: Option<String>,
    /// Where in the world the machine taking this generation is — its ISO 3166-1 alpha-2 country
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Required together with `region_label` when `edge_placement` names a hosted mode, and refused
    /// on any other bump: an in-store machine is in the shop, and a bump that names no placement is
    /// a swap in place, so a region on either would be a region edited without moving the store.
    #[serde(default)]
    region_country: Option<String>,
    /// The place a person recognises — `ap-southeast-1`, `Ho Chi Minh City`, `the rack in Sakai`.
    ///
    /// Stored verbatim and never parsed by anything. It is free text an admin typed, so the
    /// console's helper text asks for a place and not a person; it is never logged and never enters
    /// an event payload.
    #[serde(default)]
    region_label: Option<String>,
    /// The undrained generation this bump is knowingly abandoning, or absent.
    ///
    /// It names the value already sitting in `superseded_generation` — *not* the generation this
    /// bump is about to displace. So an acknowledged bump does not clear the column; it rolls it
    /// forward, and the fact that the older generation was abandoned survives in the audit trail,
    /// which is why the entry carries both numbers.
    #[serde(default)]
    acknowledge_undrained: Option<u64>,
}

/// A `PUT /admin/config/ota/placement` body: the `(tenant, store)` and where in the rollout it sits —
/// its ring, and the stable canary bucket that fixes its place in the fleet ramp.
#[derive(Debug, Clone, Deserialize)]
struct PublishPlacementRequest {
    tenant_id: String,
    store_id: String,
    ring: String,
    canary_bucket: u8,
}

/// Builds the OTA rollout sub-router ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md), O3).
///
/// The first-class levers that replace hand-editing a `fleet_update` node: `PUT /admin/config/ota`
/// publishes a rollout from typed fields, `POST /admin/config/ota/halt` flips its kill switch, and
/// `GET /admin/config/ota` reads the currently-published rollout. The writes compose the `fleet_update`
/// node through the same config tree and the same `CapabilityValidator` (its `ota_violations`) the
/// generic publish used, so a malformed rollout is a `422` with the exact violations. Writes are behind
/// [`ConsolePermission::PublishOta`](crate::auth::console_rbac::ConsolePermission::PublishOta) and audited; the read is behind [`ConsolePermission::Read`](crate::auth::console_rbac::ConsolePermission::Read).
///
/// `PUT`/`GET /admin/config/ota/placement` are the other half of the rollout decision. A rollout says
/// *which* devices are eligible; a placement says *where this store sits* — its ring and its stable
/// canary bucket. A store with no placement is placed nowhere, and `decide_rollout` finds it eligible
/// for nothing: safe, but it never updates. Both halves have to be authored for a fleet to move
/// ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md),
/// [ADR-0052](../../../docs/adr/0052-ota-rollout-config.md)).
pub fn ota_config_router<Cfg, A, C, L>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    releases: L,
) -> Router
where
    Cfg: ConfigTreeStore + LeaseStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/ota",
            get(admin_get_rollout::<Cfg, A, C, L>).put(admin_publish_rollout::<Cfg, A, C, L>),
        )
        .route(
            "/admin/config/ota/halt",
            post(admin_halt_rollout::<Cfg, A, C, L>),
        )
        .route(
            "/admin/config/ota/placement",
            get(admin_get_placement::<Cfg, A, C, L>).put(admin_publish_placement::<Cfg, A, C, L>),
        )
        // The lease (ADR-0108) is not an OTA lever, and it sits here anyway: this router already owns
        // the load→set-node→publish→audit machinery a store-scoped config node needs, and the lease's
        // whole delivery mechanism is that node. It is deliberately not a `PUT` beside the two above
        // — there is nothing to author, only a counter to advance.
        .route("/admin/config/lease", get(admin_get_lease::<Cfg, A, C, L>))
        .route(
            "/admin/config/lease/bump",
            post(admin_bump_lease::<Cfg, A, C, L>),
        )
        // The two ways a handover ends by hand. Neither publishes a config node — nothing about
        // them changes what a till is told — so they sit beside the bump rather than going through
        // `publish_ota_node`, and audit themselves directly.
        .route(
            "/admin/config/lease/settle",
            post(admin_settle_handover::<Cfg, A, C, L>),
        )
        .route(
            "/admin/config/lease/retire",
            post(admin_retire_handover::<Cfg, A, C, L>),
        )
        .with_state(OtaConfigState {
            config_trees,
            admin,
            clock,
            audit,
            releases,
        })
}

/// `412` for a bump whose `If-Match` does not match the row
/// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
///
/// `current` is read *after* the refusal, so it can already be stale by the time it is rendered.
/// That is fine and deliberate: it is for the message, never for a decision. The decision was the
/// write's own `WHERE`, which cannot race.
fn lease_moved_response(current: Option<LeaseGeneration>) -> Response {
    api_error_with_details(
        ErrorStatus::FailedPrecondition,
        "the store's lease has moved since you read it; re-read and retry",
        &[(
            "if-match",
            &current.map_or_else(
                || "NO_LEASE_ISSUED".to_owned(),
                |generation| generation.value().to_string(),
            ),
        )],
    )
}

/// `422` for a bump that would abandon a machine still holding events
/// ([ADR-0096](../../../docs/adr/0096-unprocessable-status.md)).
///
/// You do not move a store off a machine whose events are still on it — not without a person
/// saying, by name and in the trail, that those events are being abandoned. The detail names the
/// generation the caller has to acknowledge to proceed.
fn lease_undrained_response(superseded: LeaseGeneration) -> Response {
    api_error_with_details(
        ErrorStatus::Unprocessable,
        "this store's previous machine still holds events this cloud has never seen; acknowledge \
         that generation to move the store anyway, which abandons them",
        &[("acknowledge_undrained", &superseded.value().to_string())],
    )
}

/// Turns a region refusal into its [AIP-193](../../../docs/naming-and-api.md) envelope
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Extracted from [`admin_bump_lease`] rather than inlined: five messages is most of a screen, and
/// the handler it sits in already carries the generation, the placement, the acknowledgement and
/// the publish.
///
/// Each message names what the field accepts, not only what it rejected — the same rule the
/// placement's own refusal one screen up follows, and the difference between an admin who fixes the
/// form and one who guesses at it.
fn region_refusal_response(refusal: RegionRefusal) -> Response {
    let message = match refusal {
        RegionRefusal::Incomplete => {
            "a hosted edge placement needs both region_country and region_label — where a store's \
             data rests is recorded, not inferred"
        }
        RegionRefusal::NotForPlacement => {
            "a region belongs only to a hosted edge placement: an in-store machine is in the shop, \
             and a bump that names no edge_placement is a swap in place. To move a hosted store's \
             region, name its edge_placement again alongside the new one"
        }
        RegionRefusal::CountryShape => {
            "region_country must be an ISO 3166-1 alpha-2 code — two letters, such as SG or JP"
        }
        RegionRefusal::LabelEmpty => "region_label must name a place",
        RegionRefusal::LabelTooLong => {
            "region_label must be at most 64 characters — it names a place for a person to \
             recognise, not an address"
        }
    };
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        message,
        &[(refusal.field(), "INVALID_VALUE")],
    )
}

/// Turns a lease-store fault into the same `503` every other store outage returns. There is no
/// caller-fixable case: a bump takes no value and a read takes no filter, so anything that fails
/// here is the database, not the request.
fn lease_store_error_response(error: &LeaseStoreError) -> Response {
    tracing::error!(%error, "the lease store failed");
    service_unavailable("the lease store is unavailable")
}
