// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Over-the-air updates: a store's OTA report, the artifact it downloads, and the `/admin` release
//! surface that hosts what it downloads (ADR-0078, ADR-0088).
//!
//! The report and artifact states, request bodies and routers are here.

use artifacts::serve_ota_artifact;
use axum::Router;
use axum::routing::post;
use pos_ports::blob_store::BlobStore;
use pos_proto::determinism::ClockSource;
use report::{ingest_ota_report, receive_store_report};
use serde::Deserialize;

use crate::auth::apikey::ApiKeyStore;
use crate::config::InternalSecret;
use crate::fleet::OtaReportStore;
use crate::ota::ArtifactStore;

mod artifacts;
mod installer;
mod release_fetch;
mod release_upload;
mod report;

pub use release_fetch::ota_release_fetch_router;
pub use release_upload::ota_release_admin_router;

// The handlers the console's OpenAPI document names.
pub(crate) use installer::__path_admin_download_installer;
pub(crate) use release_fetch::__path_admin_fetch_ota_release;
pub(crate) use release_upload::{__path_admin_list_ota_release, __path_admin_upload_ota_release};

/// The state the OTA-report ingest carries: the write seam and the server clock that stamps the
/// report's arrival instant.
#[derive(Clone)]
struct OtaReportState<R, C, K> {
    reports: R,
    clock: C,
    /// The key store the **store-facing** route authenticates against. The `/internal` route beside
    /// it does not use this: it is trusted-network and guarded by the shared secret instead.
    keys: K,
    /// The `/internal` shared secret (ADR-0097). Guarded at the handler like the other two, for one
    /// shape across all three, even though this router carries only the one route.
    internal_shared_secret: Option<InternalSecret>,
}

/// An edge's OTA report body ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)): which store
/// reported, the version it is now running, and whether the post-install self-test passed. Identity is
/// in the body because `/internal` is trusted-network and unauthenticated, exactly like `/internal/reconcile`.
#[derive(Debug, Clone, Deserialize)]
struct OtaReportRequest {
    /// The tenant the store belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store reporting (a 26-character ULID).
    store_id: String,
    /// The release the store is now running (a version string, e.g. `v1.2.3`).
    installed: String,
    /// Whether the post-install self-test passed, absent when the store has never self-tested
    /// (ADR-0078 Amendment 1). Optional on the wire, so an edge built before the amendment posts the
    /// same body and its `true`/`false` is read unchanged.
    #[serde(default)]
    self_test_passed: Option<bool>,
}

/// What a **store** reports about itself ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// on the `/sync` route that replaced the `/internal` one for store-originated reporting
/// ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)).
///
/// No `tenant_id` and no `store_id`: the tenant comes from the scoped key and the store from the
/// path. That is the entire point of the move — the `/internal` shape took both in the body, so it
/// trusted the caller's claim about which store it was, and a shared secret could change *who could
/// reach* the route without making a report attributable.
#[derive(Debug, Clone, Deserialize)]
struct StoreReportRequest {
    /// The release the store is now running.
    installed: String,
    /// Whether the post-install self-test passed; absent when the store has never self-tested
    /// (ADR-0078 Amendment 1).
    #[serde(default)]
    self_test_passed: Option<bool>,
}

/// Builds the OTA-report ingest sub-router ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// stated independently of [`CloudApp`](crate::http::CloudApp) like [`reconcile_router`](crate::http::reconcile_router).
///
/// `POST /internal/ota/report` records the version a store is running and its last self-test outcome
/// onto the fleet-liveness read model, so the cloud can see rollout-ring progress. Internal,
/// private-network, and absent from the public OpenAPI, exactly like `/internal/ingest` and
/// `/internal/reconcile`; the server stamps the arrival instant from its own clock.
pub fn ota_report_router<R, C, K>(
    reports: R,
    clock: C,
    keys: K,
    internal_shared_secret: Option<InternalSecret>,
) -> Router
where
    R: OtaReportStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/report",
            post(receive_store_report::<R, C, K>),
        )
        .route("/internal/ota/report", post(ingest_ota_report::<R, C, K>))
        .with_state(OtaReportState {
            reports,
            clock,
            keys,
            internal_shared_secret,
        })
}

/// The response header carrying an artifact's detached signature, as lowercase hex
/// ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)).
///
/// It must name the same header as `cloud-sync-http`'s `SIGNATURE_HEADER` — the spellings differ in
/// case only, which HTTP treats as the same name. The edge refuses an
/// artifact whose signature header is missing, so a rename on one side alone stops every update in
/// the fleet — loudly, but only once a rollout reaches a real box.
const ARTIFACT_SIGNATURE_HEADER: &str = "x-pos-artifact-signature";

/// The collaborators the artifact route needs: the key store and clock that authenticate a store, the
/// object store the bytes live in, and the registry that says a release exists.
#[derive(Clone)]
struct ArtifactState<K, C, B, L> {
    keys: K,
    clock: C,
    blobs: B,
    releases: L,
}

/// What a store asks for: a release tag, and the architecture it runs.
///
/// `arch` is the additive field [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Correction 2 added. R1's workflow cross-compiles two targets, so a request without one cannot say
/// which binary it means — and guessing would hand an `aarch64` box an `x86_64` executable that fails
/// its self-test after the install, which is the expensive place to find out.
#[derive(Debug, Clone, Deserialize)]
struct ArtifactRequest {
    release: String,
    arch: String,
}

/// Builds the OTA artifact sub-router, stated independently of [`CloudApp`](crate::http::CloudApp)
/// ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 1).
///
/// Like [`device_router`](crate::http::device_router), it carries its own state and is merged into the main router rather than
/// adding more `CloudApp` generics for two collaborators one route uses.
///
/// **On `/sync`, not `/internal`.** [ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)
/// pinned this at `/internal/ota/artifact` when `/internal` was believed to be the store-facing
/// surface. It is not: the proxy answers `404` to every `/internal/*` request from outside the box,
/// and a store reaches its cloud through that proxy, so a handler there would be unreachable by its
/// only caller. Amendment 1 moves it here, where the tenant comes from the scoped key rather than a
/// body field.
pub fn ota_artifact_router<K, C, B, L>(keys: K, clock: C, blobs: B, releases: L) -> Router
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/artifact",
            post(serve_ota_artifact::<K, C, B, L>),
        )
        .with_state(ArtifactState {
            keys,
            clock,
            blobs,
            releases,
        })
}
