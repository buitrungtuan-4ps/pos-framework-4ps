// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/ota/releases`: put a signed release in, and read what is hosted.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details, service_unavailable};
use pos_ports::blob_store::{BlobKey, BlobStore};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use serde::{Deserialize, Serialize};

use super::installer::admin_download_installer;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{Json, hex_digest};
use crate::ota::{
    ArtifactKind, ArtifactStore, ArtifactStoreError, MAX_ARTIFACT_BYTES, MINISIGN_SIGNATURE_LEN,
    RecordOutcome, ReleaseArtifact, TargetTriple, admit_artifact, artifact_key,
    validate_release_tag,
};

// ---------------------------------------------------------------------------------------------
// The `/admin` release surface: putting an artifact in, and reading what is hosted
// ---------------------------------------------------------------------------------------------

/// The header the upload carries the signature in: the **first base64 line** of the `.minisig` file
/// minisign wrote (line 2 of the file — line 1 is the untrusted comment).
///
/// Deliberately a different name from [`ARTIFACT_SIGNATURE_HEADER`], which the *serve* route answers
/// with in lowercase hex. Same bytes, two encodings, two directions: reusing one name for both would
/// make a mis-encoded upload look like a valid download header.
const UPLOAD_SIGNATURE_HEADER: &str = "x-pos-minisig";

/// The collaborators the `/admin` release routes need: the object store the bytes go to, the registry
/// that records them, and the admin/clock/audit trio every console write carries.
#[derive(Clone)]
pub(super) struct OtaReleaseAdminState<B, L, A, C> {
    pub(super) blobs: B,
    pub(super) releases: L,
    pub(super) admin: A,
    pub(super) clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Which release and target the uploaded bytes are.
///
/// `release` is spelled the way a rollout's `target_version` and the binary's own version spell it —
/// bare, without a leading `v` ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2). The uploader passes the string it will promote.
#[derive(Debug, Clone, Deserialize)]
struct ReleaseUploadQuery {
    release: String,
    arch: String,
}

/// One hosted artifact, as the console reads it.
#[derive(Debug, Clone, Serialize)]
struct HostedArtifactResponse {
    arch: String,
    size_bytes: i64,
    sha256: String,
    recorded_at_ms: i64,
}

/// Builds the `/admin` release sub-router — the half that puts an artifact *into* the cloud, and
/// hands a Windows store its one-file installer back out of it.
///
/// Merged only when `[artifacts]` is configured, exactly like [`ota_artifact_router`]: without an
/// object store there is nowhere for the bytes to go, and a route that always answers `503` is worse
/// than a route that is honestly absent.
///
/// `POST /admin/ota/releases?release=&arch=` takes the **bare executable** as its body and minisign's
/// signature line in [`UPLOAD_SIGNATURE_HEADER`]. The bare executable, not the release tarball,
/// because `UpdateInstaller::apply` writes the bytes it is handed as the next binary — so the
/// signature the edge checks has to cover *these* bytes ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Amendment 2). Unpacking a tarball server-side would hand `apply` bytes nobody signed.
pub fn ota_release_admin_router<B, L, A, C>(
    blobs: B,
    releases: L,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/ota/releases",
            post(admin_upload_ota_release::<B, L, A, C>),
        )
        .route(
            "/admin/ota/releases/{release}",
            get(admin_list_ota_release::<B, L, A, C>),
        )
        .route(
            "/admin/ota/releases/{release}/installer",
            get(admin_download_installer::<B, L, A, C>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_ARTIFACT_BYTES))
        .with_state(OtaReleaseAdminState {
            blobs,
            releases,
            admin,
            clock,
            audit,
        })
}

/// Decodes minisign's base64 signature line into the raw 74-byte blob the edge verifies against.
///
/// [ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md) Correction 2 put the decode here: every
/// type in the tree holds raw signature bytes, minisign's on-disk form is base64, and the boundary
/// where one becomes the other is this upload. The edge never sees base64.
///
/// # Errors
///
/// The [`Response`] to send — a `400` naming the header, for a value that is not base64 or does not
/// decode to a minisign blob's length.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape the other route helpers carry"
)]
fn decode_minisig_line(headers: &HeaderMap) -> Result<Vec<u8>, Response> {
    let Some(line) = headers
        .get(UPLOAD_SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the upload must carry minisign's signature line",
            &[(
                UPLOAD_SIGNATURE_HEADER,
                "absent — send the second line of the .minisig file",
            )],
        ));
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(line.trim()) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "minisign's signature line is not valid base64",
            &[(UPLOAD_SIGNATURE_HEADER, "not base64")],
        ));
    };
    if bytes.len() != MINISIGN_SIGNATURE_LEN {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "that is not a minisign signature",
            &[(
                UPLOAD_SIGNATURE_HEADER,
                "a minisign signature decodes to seventy-four bytes",
            )],
        ));
    }
    Ok(bytes)
}

/// Writes an artifact's two blobs, signature first.
///
/// The order is the contract, not a preference: the serve route treats "the registry says this
/// release exists but a blob does not" as the cloud's own inconsistency and answers `500`. Recording
/// the row only after **both** blobs are stored means that state is never reachable through this
/// route — a half-finished upload leaves no row, and the release simply does not exist yet.
///
/// # Errors
///
/// The [`Response`] to send: `503`, because an object store that cannot be written is a dependency
/// being down, and the same upload retried later is the right thing for the caller to do.
async fn store_artifact_blobs<B: BlobStore>(
    blobs: &B,
    binary_key: &BlobKey,
    signature_key: &BlobKey,
    binary: &[u8],
    signature: &[u8],
) -> Result<(), Response> {
    for (key, bytes, part) in [
        (signature_key, signature, "signature"),
        (binary_key, binary, "binary"),
    ] {
        if let Err(error) = blobs.put(key, bytes).await {
            tracing::error!(%error, part, key = key.as_str(), "storing a release blob failed");
            return Err(service_unavailable("the release artifact store"));
        }
    }
    Ok(())
}

/// Checks everything about an upload that can be judged from the request alone: that the release and
/// target could be storage keys, that the body is not empty, and that the signature header decodes to
/// a minisign blob.
///
/// Separated from the handler so that "is this request well-formed" and "did storing it work" stay
/// legible as two steps — and because every refusal here is a `400` naming its field, while every
/// failure after it is a dependency being down.
///
/// # Errors
///
/// The [`Response`] to send: a `400` naming `release`, `arch`, `body`, or the signature header.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape the other route helpers carry"
)]
fn parse_release_upload(
    query: &ReleaseUploadQuery,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(TargetTriple, Vec<u8>), Response> {
    if let Err(error) = validate_release_tag(&query.release) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        ));
    }
    let target = TargetTriple::parse(&query.arch).map_err(|error| {
        api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("arch: {error}"),
            &[("arch", "not a target triple")],
        )
    })?;
    if body.is_empty() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the upload body is the release executable, and it is empty",
            &[("body", "empty")],
        ));
    }
    let signature = decode_minisig_line(headers)?;
    Ok((target, signature))
}

/// Puts one artifact's bytes in the object store and its row in the registry.
///
/// Shared by both intake paths — the upload and the release fetch
/// ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 4) — so that whichever door
/// an artifact arrives through, the same immutability rule, the same keys and the same write order
/// apply. Only the audit entry differs, and that stays in the handlers where the source is known.
///
/// **Immutability is checked before anything is written.** The first version of the upload wrote both
/// blobs and *then* asked the registry, so re-sending a release tag with different bytes overwrote
/// the stored executable and its signature and only afterwards answered `409`: the registry kept the
/// old digest while the object store held the new pair, and an edge fetching that release would have
/// verified the new signature over the new bytes and installed them happily under a version string
/// that already meant something else. Reading first closes that, and the write order ADR-0088 relies
/// on — signature, binary, then the row — is unchanged for a genuinely new artifact.
///
/// Identical bytes still write their blobs before answering "already hosted": the digest says the
/// content is the same, so the write is a no-op except in the one case where it is a repair (a blob
/// lost under a row that survived).
///
/// # Errors
///
/// The [`Response`] to send: `409` for a release already hosted with different bytes, `503` for an
/// object store or registry that could not be reached, `500` if the keys could not be composed.
pub(super) async fn admit_release_artifact<B, L, C>(
    blobs: &B,
    releases: &L,
    clock: &C,
    release: &str,
    target: TargetTriple,
    binary: &[u8],
    signature: &[u8],
) -> Result<(ReleaseArtifact, RecordOutcome), Response>
where
    B: BlobStore,
    L: ArtifactStore,
    C: ClockSource,
{
    let (Ok(binary_key), Ok(signature_key)) = (
        artifact_key(release, &target, ArtifactKind::Binary),
        artifact_key(release, &target, ArtifactKind::Signature),
    ) else {
        return Err(api_error(
            ErrorStatus::Internal,
            "could not compose the artifact keys",
        ));
    };
    let artifact = ReleaseArtifact {
        release: release.to_owned(),
        target,
        size_bytes: i64::try_from(binary.len()).unwrap_or(i64::MAX),
        sha256: hex_digest(binary),
        recorded_at: clock.now(),
    };
    let stored = match releases
        .find_artifact(&artifact.release, &artifact.target)
        .await
    {
        Ok(stored) => stored,
        Err(error) => {
            tracing::error!(%error, "reading the release registry before an intake failed");
            return Err(service_unavailable("the release registry"));
        }
    };
    let admitted = admit_artifact(
        stored.as_ref().map(|found| found.sha256.as_str()),
        &artifact,
    )
    .map_err(already_hosted_refusal)?;
    store_artifact_blobs(blobs, &binary_key, &signature_key, binary, signature).await?;
    if admitted == RecordOutcome::AlreadyRecorded {
        return Ok((artifact, RecordOutcome::AlreadyRecorded));
    }
    match releases.record_artifact(&artifact).await {
        Ok(outcome) => Ok((artifact, outcome)),
        // The read above closes the ordinary case; this stays the authority for two intakes racing.
        Err(error @ ArtifactStoreError::Immutable { .. }) => Err(already_hosted_refusal(error)),
        Err(error @ ArtifactStoreError::Unavailable(_)) => {
            tracing::error!(%error, "recording a release artifact failed");
            Err(service_unavailable("the release registry"))
        }
    }
}

/// The `409` a release already hosted with different bytes earns.
///
/// A version a ring may have installed has to keep meaning the same thing, or a rollback target
/// stops being a known quantity ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)).
fn already_hosted_refusal(error: ArtifactStoreError) -> Response {
    match error {
        ArtifactStoreError::Immutable { release, target } => api_error_with_details(
            ErrorStatus::AlreadyExists,
            format!("release {release} for {target} is already hosted with different bytes"),
            &[("release", "already hosted with a different digest")],
        ),
        // `admit_artifact` returns only `Immutable`, and the caller maps `Unavailable` itself; this
        // arm exists because the type says it can, not because a path reaches it.
        ArtifactStoreError::Unavailable(reason) => {
            tracing::error!(%reason, "the release registry was unavailable");
            service_unavailable("the release registry")
        }
    }
}

/// A super-admin uploads one release artifact: the bare executable, plus its minisign signature.
///
/// Idempotent by [`admit_artifact`]'s rule — re-uploading the same release/target with identical
/// bytes answers `200` rather than failing, so a re-run of a release step does not wedge; different
/// bytes for a release a ring may already have installed are refused, because a version that can
/// change under a fleet is not a version.
async fn admin_upload_ota_release<B, L, A, C>(
    State(state): State<OtaReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ReleaseUploadQuery>,
    body: axum::body::Bytes,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
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
    let (target, signature) = match parse_release_upload(&query, &headers, &body) {
        Ok(parsed) => parsed,
        Err(refusal) => return refusal,
    };
    let (artifact, outcome) = match admit_release_artifact(
        &state.blobs,
        &state.releases,
        &state.clock,
        &query.release,
        target,
        &body,
        &signature,
    )
    .await
    {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        None,
        "release.upload",
        "release_artifact",
        &format!("{}/{}", artifact.release, artifact.target),
        None,
        Some(serde_json::json!({
            "arch": artifact.target.as_str(),
            "size_bytes": artifact.size_bytes,
            "sha256": artifact.sha256,
        })),
    )
    .await;
    let status = match outcome {
        RecordOutcome::Recorded => StatusCode::CREATED,
        RecordOutcome::AlreadyRecorded => StatusCode::OK,
    };
    (
        status,
        Json(serde_json::json!({
            "release": artifact.release,
            "arch": artifact.target.as_str(),
            "size_bytes": artifact.size_bytes,
            "sha256": artifact.sha256,
        })),
    )
        .into_response()
}

/// Which targets a release is hosted for — what an operator checks before promoting it, and what the
/// promote guard consults on their behalf.
async fn admin_list_ota_release<B, L, A, C>(
    State(state): State<OtaReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Path(release): Path<String>,
) -> Response
where
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
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
    if let Err(error) = validate_release_tag(&release) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        );
    }
    match state.releases.list_artifacts(&release).await {
        Ok(artifacts) => Json(serde_json::json!({
            "release": release,
            "artifacts": artifacts
                .into_iter()
                .map(|artifact| HostedArtifactResponse {
                    arch: artifact.target.to_string(),
                    size_bytes: artifact.size_bytes,
                    sha256: artifact.sha256,
                    recorded_at_ms: artifact.recorded_at.as_milliseconds_since_epoch(),
                })
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "listing a release's artifacts failed");
            service_unavailable("the release registry")
        }
    }
}
