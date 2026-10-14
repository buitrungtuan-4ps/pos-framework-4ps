// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Fleet liveness (`/admin/fleet`, ADR-0068): every store's last contact, versions and region.
//!
//! The state, the views it shares, the router and the store-error helper are here.

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use regions::admin_acknowledge_region;
use stores::{admin_fleet_store, admin_list_fleet};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::config_tree::ConfigTreeStore;
use crate::fleet::{FleetStore, FleetStoreError};
use crate::lease::RegionAcknowledgementStore;

mod regions;
mod stores;
mod views;

// The handler the console's OpenAPI document names.
pub(crate) use regions::__path_admin_acknowledge_region;

// --- Fleet liveness (`/admin/fleet`, ADR-0068) --------------------------------------------------

/// A store is **online** if its most recent contact is within this window of now. Liveness is captured
/// on every config pull ([ADR-0033](../../../docs/adr/0033-config-tree.md)) and on the edge's
/// heartbeat ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 2), so this threshold is a
/// few of those cycles of slack: one dropped pull or heartbeat must not flap a healthy store to
/// offline. Owned here, at the read, because online/offline is derived — never stored (ADR-0068).
const FLEET_ONLINE_THRESHOLD_MS: i64 = 180_000;

/// The collaborators the fleet routes need, stated independently of [`CloudApp`]: the fleet read
/// model, plus the admin and clock every session guard (and the online-at-read derivation) uses. Like
/// [`RegistryState`], it carries its own state and is merged into the main router.
#[derive(Clone)]
struct FleetState<F, A, C, Cfg, Ack> {
    fleet: F,
    admin: A,
    clock: C,
    /// The store's own configuration, for the one thing the fleet read model cannot answer: which
    /// country the store publishes ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Used by the **single-store** read only, which does exactly one `load` for the one store it
    /// already names. The list route deliberately does not: a comparison there would be one
    /// config-tree load per row — work that grows with the fleet, which `AGENTS.md` §2's
    /// bounded-work discipline and [ADR-0098](../../../docs/adr/0098-paged-admin-reads.md) both
    /// refuse. The tempting shortcut is worse than the read it saves: copying `country_code` onto
    /// the registry row would make a second copy of a versioned node, and a config rollback can
    /// move the node backwards while the copy keeps asserting the old value — a silent disagreement
    /// about a store's country, which is the exact failure ADR-0114 exists to prevent.
    config_trees: Cfg,
    /// A store's recorded answer to its region difference
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)) — read by the
    /// single-store read beside the comparison it answers, and written by the acknowledge route.
    acknowledgements: Ack,
    /// The trail the acknowledge route writes to. The row holds *what is currently answered*; this
    /// holds every answer ever given, with the person against it, and nothing overwrites it.
    audit: Arc<dyn AuditRecorder>,
}

/// A recorded answer to a store's region difference, as the console sees it
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// The two country codes ride along rather than being left implicit: the console renders them in
/// the sentence it shows ("data in SG, store registered in VN — acknowledged"), and a reader of the
/// payload can see *what* was answered without inferring it from the fields beside it.
///
/// No `acknowledged_by`. The row does not carry one — the audit action
/// `store.edge_placement.acknowledge` does, and it is the copy that survives the next answer. The
/// console links to the store's audit tab rather than showing a name this read cannot honestly give.
#[derive(Debug, Clone, serde::Serialize)]
struct RegionAcknowledgementView {
    /// The country the store was registered in when the answer was written.
    profile_country: String,
    /// The country its data was resting in when the answer was written.
    region_country: String,
    /// Why the difference is correct, as somebody typed it. Carried verbatim, never parsed.
    reason: String,
    /// When it was written, in milliseconds since the epoch — the same representation every other
    /// instant on this read uses.
    acknowledged_at_ms: i64,
}

/// One bound print agent as the fleet console sees it
/// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
///
/// Two opaque identifiers and a duration, carried verbatim from what the store reported. No document,
/// no line, no name: nothing a ticket *says* helps diagnose a stalled agent, so nothing a ticket says
/// crosses this wire either.
#[derive(Debug, Clone, serde::Serialize)]
struct FleetPrintAgent {
    /// The terminal a console admin created and a manager bound at the till.
    agent_device_id: String,
    /// The paired device answering for it — the box an operator has to walk to.
    paired_device_id: String,
    /// How long the oldest still-unacknowledged job has waited, or `null` when nothing is waiting.
    /// `null` is the healthy answer and is deliberately not zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    oldest_unacknowledged_secs: Option<u64>,
}

/// Builds the fleet-liveness sub-router ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 3).
///
/// Two reads, both behind [`ConsolePermission::Read`] (every console role, so Ops and Viewer see the
/// fleet) and both naming their tenant the admin-is-global way — a `?tenant_id=` query
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): the whole fleet, and one
/// store's detail. Online/offline is derived here at read time, so the answer is always current
/// without any background sweep.
pub fn fleet_router<F, A, C, Cfg, Ack>(
    fleet: F,
    admin: A,
    clock: C,
    config_trees: Cfg,
    acknowledgements: Ack,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/fleet", get(admin_list_fleet::<F, A, C, Cfg, Ack>))
        .route(
            "/admin/fleet/{store_id}",
            get(admin_fleet_store::<F, A, C, Cfg, Ack>),
        )
        // Under `/admin/stores` because it is a fact about a store, and served from *this* router
        // because answering it needs both halves of the comparison: the region, which lives on the
        // lease behind `FleetStore`, and the published country, which lives in the config tree.
        // `registry_router` holds neither, and giving it both to keep the paths together would put
        // two store handles in a router whose whole payload is the registry record.
        .route(
            "/admin/stores/{store_id}/region-acknowledgement",
            post(admin_acknowledge_region::<F, A, C, Cfg, Ack>),
        )
        .with_state(FleetState {
            fleet,
            admin,
            clock,
            config_trees,
            acknowledgements,
            audit,
        })
}

/// Maps a fleet read failure to a retryable `503`, logging the detail rather than leaking it.
fn fleet_error_response(error: &FleetStoreError) -> Response {
    tracing::error!(%error, "a fleet read failed");
    api_error(ErrorStatus::Unavailable, "the fleet service is unavailable")
}
