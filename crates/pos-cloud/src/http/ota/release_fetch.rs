// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/ota/releases/fetch`: the cloud fetches the pair the release workflow already published.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details};
use pos_ports::blob_store::BlobStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use serde::{Deserialize, Serialize};

use super::release_upload::admit_release_artifact;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::Json;
use crate::ota::{ArtifactStore, RecordOutcome, validate_release_tag};
use crate::release_source::{ReleaseSource, ReleaseSourceError};

// ---------------------------------------------------------------------------------------------
// The other door: the cloud fetches the pair the release workflow already published
// ---------------------------------------------------------------------------------------------

/// The collaborators the release-fetch route needs: everything the upload needs, plus the forge.
#[derive(Clone)]
struct OtaReleaseFetchState<B, L, A, C, S> {
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    source: Option<S>,
}

/// A `POST /admin/ota/releases/fetch` body: which version to go and get.
///
/// `release` is spelled the way the registry keys it and a rollout's `target_version` names it —
/// bare, without a leading `v` ([ADR-0088](../../../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2). Which tag that is on the forge is the source's business, and it tries both.
#[derive(Debug, Clone, Deserialize)]
struct FetchReleaseRequest {
    release: String,
}

/// One artifact the fetch put in the cloud.
#[derive(Debug, Clone, Serialize)]
struct FetchedArtifactResponse {
    arch: String,
    size_bytes: i64,
    sha256: String,
    /// `recorded` for a new artifact, `already_hosted` for one the cloud already held with these
    /// exact bytes — so a re-run reads as a no-op rather than as work.
    outcome: &'static str,
    /// The asset the bytes came from, so the console can say what it took.
    asset: String,
}

/// Builds the `/admin` release-fetch sub-router — the door that does not need the operator to hold
/// the bytes ([ADR-0088](../../../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 4).
///
/// Merged wherever `[artifacts]` is, and `source` is `None` when no `[release_source]` is
/// configured. That is the opposite posture from the *store-facing* artifact route, deliberately:
/// there, absence is a meaningful answer an edge reads as "install nothing", and a route answering
/// `503` would be worse than none. Here the caller is a console with a person at it, and a bare
/// `404` from an unmerged route is indistinguishable from "that version was never released" — so
/// the route exists and says which block of `cloud.toml` is missing.
///
/// Either way the upload route is unaffected: it needs no outbound network and no credential, and
/// is what `docs/release-runbook.md` documents as the way in when this one cannot be used.
pub fn ota_release_fetch_router<B, L, A, C, S>(
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    source: Option<S>,
) -> Router
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    S: ReleaseSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/ota/releases/fetch",
            post(admin_fetch_ota_release::<B, L, A, C, S>),
        )
        .with_state(OtaReleaseFetchState {
            blobs,
            releases,
            admin,
            clock,
            audit,
            source,
        })
}

/// Turns a fetch failure into the refusal an operator can act on.
///
/// Each one names the thing to go and look at: the version they typed, the token in `cloud.toml`,
/// the asset that is wrong, or the network. A fetch failure is never fatal to a release — the upload
/// route is the way past every one of them.
fn release_fetch_refusal(error: &ReleaseSourceError) -> Response {
    match error {
        ReleaseSourceError::NoSuchRelease { candidates } => api_error_with_details(
            ErrorStatus::NotFound,
            format!(
                "the release source publishes no release as {} — check the version, or that the \
                 release workflow finished",
                candidates.join(" or ")
            ),
            &[("release", "no release is published under either spelling")],
        ),
        ReleaseSourceError::Unauthorized => api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "the release source refused the cloud's credentials — a private repository needs \
             release_source.token in cloud.toml, and an expired one reads the same way",
            &[("release_source.token", "absent, expired, or not permitted")],
        ),
        ReleaseSourceError::TooLarge { asset, size_bytes } => api_error_with_details(
            ErrorStatus::Unprocessable,
            format!("{asset} is {size_bytes} bytes, past what the cloud will host"),
            &[("release", "an asset is larger than the cloud hosts")],
        ),
        ReleaseSourceError::Malformed { asset, reason } => api_error_with_details(
            ErrorStatus::Unprocessable,
            format!("{asset}: {reason}"),
            &[("release", "an asset is not what it claims to be")],
        ),
        ReleaseSourceError::Unavailable(reason) => api_error(
            ErrorStatus::Unavailable,
            format!("the release source: {reason}"),
        ),
    }
}

/// A super-admin points the cloud at a release, and it fetches every pair the forge holds for it.
///
/// The button behind `docs/release-runbook.md` step 5: instead of downloading six files and running
/// three `curl`s with a console cookie, the operator names the version. What the cloud *holds*
/// afterwards is identical either way — the same blobs, the same rows, the same immutability rule —
/// because both doors go through [`admit_release_artifact`].
///
/// Not a transaction. Each target is admitted on its own, so a refusal on the third leaves the first
/// two hosted; that is the same grain the upload has always had (one call per target), and the
/// refusal names which target stopped it.
async fn admin_fetch_ota_release<B, L, A, C, S>(
    State(state): State<OtaReleaseFetchState<B, L, A, C, S>>,
    headers: HeaderMap,
    Json(request): Json<FetchReleaseRequest>,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    S: ReleaseSource + Clone + Send + Sync + 'static,
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
    let Some(source) = state.source.as_ref() else {
        return api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "this cloud has no release source configured, so it cannot go and get a release: set \
             [release_source] in cloud.toml, or send the signed pair to POST /admin/ota/releases \
             (docs/release-runbook.md step 5)",
            &[("release_source", "not configured in cloud.toml")],
        );
    };
    let release = request.release.trim().to_owned();
    if let Err(error) = validate_release_tag(&release) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        );
    }
    let fetched = match source.fetch(&release).await {
        Ok(fetched) => fetched,
        Err(error) => {
            tracing::warn!(%error, %release, "fetching a release from the release source failed");
            return release_fetch_refusal(&error);
        }
    };
    if fetched.artifacts.is_empty() {
        return api_error_with_details(
            ErrorStatus::Unprocessable,
            format!(
                "{} carries no OTA pair: the cloud installs the bare executable \
                 (pos-edge-{}-<target>.bin, or .exe on Windows) with its .minisig beside it, and \
                 neither the archive nor the checksums are that",
                fetched.tag, fetched.tag
            ),
            &[("release", "the release holds no signed executable")],
        );
    }

    let mut recorded = Vec::with_capacity(fetched.artifacts.len());
    for artifact in fetched.artifacts {
        let asset = artifact.asset_name.clone();
        let (admitted, outcome) = match admit_release_artifact(
            &state.blobs,
            &state.releases,
            &state.clock,
            &release,
            artifact.target,
            &artifact.binary,
            &artifact.signature,
        )
        .await
        {
            Ok(admitted) => admitted,
            Err(refusal) => return refusal,
        };
        recorded.push(FetchedArtifactResponse {
            arch: admitted.target.to_string(),
            size_bytes: admitted.size_bytes,
            sha256: admitted.sha256,
            outcome: match outcome {
                RecordOutcome::Recorded => "recorded",
                RecordOutcome::AlreadyRecorded => "already_hosted",
            },
            asset,
        });
    }
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        None,
        "release.fetch",
        "release_artifact",
        &release,
        None,
        Some(serde_json::json!({
            "tag": fetched.tag,
            "artifacts": recorded,
        })),
    )
    .await;
    Json(serde_json::json!({
        "release": release,
        "tag": fetched.tag,
        "artifacts": recorded,
    }))
    .into_response()
}
