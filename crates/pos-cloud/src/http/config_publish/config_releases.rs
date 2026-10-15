// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Config releases (`/admin/config-releases`, ADR-0125): the state, views, request bodies, router
//! and the moment a release names.

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use pos_cloud_api::response::{api_error, api_error_with_details};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use serde::{Deserialize, Serialize};

use super::release_actions::{
    admin_cancel_release, admin_list_releases, admin_read_release, admin_schedule_release,
};
use super::release_create::admin_create_release;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::config_tree::ConfigTreeStore;
use crate::http::store_groups::BatchNode;
use crate::registry::RegistryStore;
use crate::releases::{Release, ReleaseMoment, ReleaseStatus, ReleaseStore, ReleaseStoreError};
use crate::scheduling::{ScheduledPublishStatus, ScheduledPublishStore};
use crate::store_groups::StoreGroupStore;

// ---------------------------------------------------------------------------------------------
// Config releases — one decision, many writes (ADR-0125)
// ---------------------------------------------------------------------------------------------
//
// A Tết menu is not one node. It is a menu, the tax rates it prices against, the campaigns that
// discount it, the reason codes the staff void it with, and the layout that shows it — to forty
// stores, all switching on together. These routes name that set, time it, expand it, and report it.
//
// The path is `/admin/config-releases`, because `/admin` carries two kinds of release — this one,
// and the OTA artifact (signed edge binaries, R2/ADR-0088), a different thing with the same English
// name. Two routers claiming one path is an axum panic at start-up, and the collision was first
// resolved by qualifying only the newcomer; ADR-0088 Amendment 3 then moved the OTA pair to
// `/admin/ota/releases`, where §4 of that record had put it all along. Each now says which release
// it means, and this one keeps its qualifier: among config nodes it is the only release there is,
// but the prefix it shares is not.
//
// They write **no config versions of their own**. Scheduling a release expands to one
// `scheduled_publishes` row per `(node, store)` pair carrying the release's id, and ADR-0077's
// existing activator applies them (ADR-0125 §1). Everything here is bookkeeping over writes that
// already had a home — which is why the handlers live in this file beside the batch publish they
// borrow their node table and cohort rules from, rather than in a module of their own.

/// The collaborators the release routes need.
///
/// Seven, and each earns its place: `releases` holds the identity and the roll-up, `scheduled` holds
/// the pairs, `groups` expands a cohort, `registry` says which stores exist and are active,
/// `config_trees` reads each store's published `locale` for its timezone, `nodes` snapshots each node
/// once, and `admin`/`clock`/`audit` are the surface's usual three.
#[derive(Clone)]
pub(in crate::http) struct ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C> {
    pub(in crate::http) releases: Rel,
    pub(super) scheduled: Sch,
    pub(super) groups: Grp,
    pub(super) registry: Reg,
    pub(super) config_trees: Cfg,
    /// The node kinds a release may carry — the very same table a batch publish uses, so a node
    /// cannot be publishable through a release and not directly (ADR-0125 §1).
    pub(super) nodes: Arc<Vec<Arc<dyn BatchNode>>>,
    pub(super) admin: A,
    pub(in crate::http) clock: C,
    pub(in crate::http) audit: Arc<dyn AuditRecorder>,
}

/// A release on the wire.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ReleaseView {
    release_id: String,
    tenant_id: String,
    name: String,
    /// Serialised as the enum, not as `as_wire()`: `as_wire` is the **stored** token (`SCHEDULED`),
    /// and every other status on this surface reaches the console `snake_case`. Two spellings for one
    /// concept is two branches in the console's type for no gain.
    status: ReleaseStatus,
    target_group_id: Option<String>,
    /// The operator's local date, for a wall-clock release. Null for an instant release.
    wall_clock_date: Option<String>,
    wall_clock_time: Option<String>,
    /// The single instant, for a release timed in UTC. Null for a wall-clock release.
    instant_at_ms: Option<i64>,
    created_by: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl ReleaseView {
    pub(super) fn of(release: &Release) -> Self {
        let (wall_clock_date, wall_clock_time, instant_at_ms) = match &release.moment {
            Some(ReleaseMoment::WallClock { date, time }) => {
                (Some(date.clone()), Some(time.clone()), None)
            }
            Some(ReleaseMoment::Instant(at_ms)) => (None, None, Some(*at_ms)),
            None => (None, None, None),
        };
        Self {
            release_id: release.id.clone(),
            tenant_id: release.tenant_id.to_string(),
            name: release.name.clone(),
            status: release.status,
            target_group_id: release.target_group_id.clone(),
            wall_clock_date,
            wall_clock_time,
            instant_at_ms,
            created_by: release.created_by.clone(),
            created_at_ms: release.created_at_ms,
            updated_at_ms: release.updated_at_ms,
        }
    }
}

/// One `(node, store)` cell of the report — F9's whole point.
///
/// `failure` is why the pair last could not apply. A pair that failed is still `PENDING`: the
/// activator retries it, and what an operator needs meanwhile is the reason, not a status that
/// pretends the attempt never happened.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ReleasePairView {
    pub(super) pair_id: String,
    pub(super) store_id: String,
    pub(super) node: String,
    /// The same spelling `GET /admin/config/scheduled` already gives this exact row.
    pub(super) status: ScheduledPublishStatus,
    pub(super) effective_at_ms: i64,
    /// The IANA zone `effective_at_ms` was resolved against, or `None`.
    ///
    /// What turns the instant back into the sentence the operator approved. A wall-clock release is
    /// one instant per timezone (ADR-0125 §2), and a console that prints the instant without the
    /// clock it came from draws every shop's 04:00 in the reader's own time — which is the review
    /// ADR-0125 §26 rejected fire-time conversion to avoid. `None` for a release timed as a plain
    /// UTC instant, which has no per-store clock, and for pairs written before the column existed.
    pub(super) resolved_timezone: Option<String>,
    pub(super) applied_version_id: Option<String>,
    pub(super) failure: Option<String>,
}

/// A release and the grid of its pairs.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ReleaseReport {
    pub(super) release: ReleaseView,
    pub(super) pairs: Vec<ReleasePairView>,
}

/// A `POST /admin/config-releases` body: the name, the cohort, and the moment. No nodes — those arrive at
/// schedule time, with the store list, because both are snapshotted then (ADR-0125 §3).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateReleaseRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    /// The cohort this release is aimed at, recorded for the report. Optional: a release may name
    /// its stores explicitly at schedule time instead.
    #[serde(default)]
    pub(super) target_group_id: Option<String>,
    /// The operator's local date and time — "Monday 04:00, local". Converted to one instant per
    /// store at schedule time (ADR-0125 §2).
    #[serde(default)]
    wall_clock_date: Option<String>,
    #[serde(default)]
    wall_clock_time: Option<String>,
    /// A single instant instead, for a release timed in UTC. The escape hatch for a store being set
    /// up: it needs no published `locale` and is always available.
    #[serde(default)]
    instant_at_ms: Option<i64>,
}

/// One node of a release, and the arguments its plan needs — the same pair a batch publish takes.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct ReleaseNodeRequest {
    pub(in crate::http) node: String,
    #[serde(default)]
    pub(in crate::http) arguments: serde_json::Value,
}

/// A `POST /admin/config-releases/{release_id}/schedule` body.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct ScheduleReleaseRequest {
    pub(super) tenant_id: String,
    /// The nodes to carry, in any order — the activator applies them in dependency order per store.
    pub(in crate::http) nodes: Vec<ReleaseNodeRequest>,
    /// The stores, explicitly. Omitted, the release's `target_group_id` is expanded instead.
    #[serde(default)]
    pub(in crate::http) store_ids: Option<Vec<String>>,
}

/// A body carrying nothing but the tenant — what `cancel` needs, since the id is in the path.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ReleaseTenantRequest {
    pub(super) tenant_id: String,
}

/// The tenant a release read is scoped to.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ReleaseTenantQuery {
    pub(super) tenant_id: String,
}

/// Builds the releases sub-router ([ADR-0125](../../../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).
///
/// Reads sit behind [`ConsolePermission::Read`]; every write sits behind
/// [`ConsolePermission::PublishConfig`], because scheduling a release *is* scheduling publishes and
/// there is no weaker thing it could be.
#[expect(
    clippy::too_many_arguments,
    reason = "seven collaborators plus the clock and the audit recorder, and each is a seam the \
              caller chooses: the release identity, the pairs, the cohort, the registry, the config \
              tree, the node table, and the admin store. Bundling them into a struct would move the \
              same list one line up and hide which of them `main.rs` is wiring"
)]
pub fn config_release_router<Rel, Sch, Grp, Reg, Cfg, A, C>(
    releases: Rel,
    scheduled: Sch,
    groups: Grp,
    registry: Reg,
    config_trees: Cfg,
    nodes: Vec<Arc<dyn BatchNode>>,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Rel: ReleaseStore + Clone + Send + Sync + 'static,
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config-releases",
            get(admin_list_releases::<Rel, Sch, Grp, Reg, Cfg, A, C>)
                .post(admin_create_release::<Rel, Sch, Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/config-releases/{release_id}",
            get(admin_read_release::<Rel, Sch, Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/config-releases/{release_id}/schedule",
            post(admin_schedule_release::<Rel, Sch, Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/config-releases/{release_id}/cancel",
            post(admin_cancel_release::<Rel, Sch, Grp, Reg, Cfg, A, C>),
        )
        .with_state(ReleaseState {
            releases,
            scheduled,
            groups,
            registry,
            config_trees,
            nodes: Arc::new(nodes),
            admin,
            clock,
            audit,
        })
}

/// The `503` a release persistence failure becomes.
pub(in crate::http) fn release_error_response(error: &ReleaseStoreError) -> Response {
    tracing::error!(%error, "a release operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "releases are temporarily unavailable",
    )
}

/// Reads the moment from a create request, or says why it is not one.
///
/// Three columns rather than one, and the reason is the refusal that only exists because of them:
/// only an *instant* release is safe at a store whose timezone nobody has published, and collapsing
/// the two forms into one field would make that refusal unmakeable on read.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape the other route helpers carry"
)]
pub(super) fn moment_from_request(
    request: &CreateReleaseRequest,
) -> Result<Option<ReleaseMoment>, Response> {
    let wall_clock = match (&request.wall_clock_date, &request.wall_clock_time) {
        (Some(date), Some(time)) => Some(ReleaseMoment::WallClock {
            date: date.clone(),
            time: time.clone(),
        }),
        (None, None) => None,
        _ => {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a wall-clock moment needs both wall_clock_date and wall_clock_time",
                &[("wall_clock_time", "REQUIRED")],
            ));
        }
    };
    match (wall_clock, request.instant_at_ms) {
        (Some(_), Some(_)) => Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a release is timed either in each store's own clock or at one instant, not both",
            &[("instant_at_ms", "CONFLICT")],
        )),
        (Some(moment), None) => Ok(Some(moment)),
        (None, Some(at_ms)) => Ok(Some(ReleaseMoment::Instant(at_ms))),
        (None, None) => Ok(None),
    }
}
