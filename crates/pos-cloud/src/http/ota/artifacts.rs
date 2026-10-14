// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/sync/stores/{store_id}/artifact`: the signed artifact a store downloads.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::response::{api_error, parse_ulid_fields, service_unavailable};
use pos_ports::blob_store::{BlobKey, BlobStore};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::StoreId;

use super::{ARTIFACT_SIGNATURE_HEADER, ArtifactRequest, ArtifactState};
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::http::{hex_digest, hex_encode, not_found};
use crate::ota::{ArtifactKind, ArtifactStore, TargetTriple, artifact_key, validate_release_tag};

/// Reads a release's two blobs — the executable and its raw detached signature.
///
/// Buffered, not streamed: [`BlobStore::get`] returns a `Vec<u8>`, so a 30 MB artifact is held in
/// memory for the length of the response. [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// flags that deliberately — a streaming `get` is a port change with its own contract-suite work and
/// belongs to the performance wave. Tolerable for a ring of a few boxes, wrong for a fleet-wide push.
///
/// # Errors
///
/// The [`Response`] to send. A blob that is **absent** while the registry says the release exists is
/// the cloud's own inconsistency: not the caller's mistake, and not something a retry mends. So it is
/// `Internal` rather than a `404` (which would tell the edge there is nothing to install and hide the
/// fault) or a `503` (which would ask it to try again forever). A backend that cannot be *reached* is
/// the retryable case, and stays `503`.
async fn read_artifact_blobs<B: BlobStore>(
    blobs: &B,
    binary_key: &BlobKey,
    signature_key: &BlobKey,
) -> Result<(Vec<u8>, Vec<u8>), Response> {
    let bytes = read_one_artifact_blob(blobs, binary_key, "artifact").await?;
    // Refused rather than served: bytes with no signature are bytes the edge has nothing to judge
    // by, and it would be right to refuse them anyway.
    let signature = read_one_artifact_blob(blobs, signature_key, "signature").await?;
    Ok((bytes, signature))
}

/// One blob, or the refusal to send. See [`read_artifact_blobs`] for why an absent blob is
/// `Internal`; `part` names which half is missing so the log and the message say the same thing.
pub(super) async fn read_one_artifact_blob<B: BlobStore>(
    blobs: &B,
    key: &BlobKey,
    part: &str,
) -> Result<Vec<u8>, Response> {
    match blobs.get(key).await {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => {
            tracing::error!(
                key = key.as_str(),
                part,
                "a recorded release is missing a blob"
            );
            Err(api_error(
                ErrorStatus::Internal,
                format!("the release is recorded but its {part} is missing"),
            ))
        }
        Err(error) => {
            tracing::warn!(%error, key = key.as_str(), part, "reading a release blob failed");
            Err(service_unavailable("artifact store"))
        }
    }
}

/// `POST /sync/stores/{store_id}/artifact` — hands a store the signed release bytes it decided to
/// install ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)).
///
/// **The cloud is a dumb host.** It stores bytes an operator uploaded and hands them back; it never
/// signs, and it never verifies the minisign signature. The edge verifies against a trust anchor
/// baked into its own binary ([ADR-0047](../../../docs/adr/0047-minisign-verification.md),
/// [ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)), so a compromised cloud, a swapped
/// blob or a spoofed host can make an update *fail* — never make a box install code.
///
/// **`read_config`, the owner's call (2026-09-04).** Every provisioned box already carries that
/// scope, so the OTA path works with no re-provisioning; a dedicated `fetch_update` scope would be
/// cleaner and would cost a visit to every live store before a single update could ship. The scope
/// is here to stop the VPS being an open binary-distribution host, not to keep a signed artifact
/// secret.
///
/// The signature rides `X-Pos-Artifact-Signature` as lowercase hex and the body stays the raw
/// artifact: a JSON envelope would mean encoding tens of megabytes to carry a few hundred bytes. The
/// blob holds **raw** signature bytes — ADR-0092 Correction 2 put minisign's base64 decode at the
/// `/admin` upload, once, under operator supervision, so the edge never sees base64 for a signature.
pub(super) async fn serve_ota_artifact<K, C, B, L>(
    State(state): State<ArtifactState<K, C, B, L>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<ArtifactRequest>,
) -> Response
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    L: ArtifactStore + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadConfig) {
        return forbidden.into_response();
    }
    // A release is fleet-wide, so there is no per-store row to isolate here — but the key presenting
    // itself is still a *store's* credential, and one store must not fetch a release under another
    // store's name (it is how the OTA progress read model attributes an install). So the id is parsed
    // and then held to the grant, exactly as the config pull beside it does (S1).
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    if let Err(error) = validate_release_tag(&request.release) {
        return api_error(ErrorStatus::InvalidArgument, format!("release: {error}"));
    }
    // A *shape* check only. `TargetTriple::parse` says whether this could name a target, not
    // whether the cloud hosts one for it — a fork that cross-compiles beyond the two targets R1
    // builds records artifacts for them, and the registry stays the authority on what exists. So a
    // well-formed triple with nothing recorded falls through to the `404` below, exactly like an
    // unknown release tag; only a triple that could never name anything is refused here.
    let target = match TargetTriple::parse(&request.arch) {
        Ok(target) => target,
        Err(error) => return api_error(ErrorStatus::InvalidArgument, format!("arch: {error}")),
    };
    let artifact = match state
        .releases
        .find_artifact(&request.release, &target)
        .await
    {
        Ok(Some(artifact)) => artifact,
        // Not an error: a store asking for a release this cloud does not host is told so, and
        // installs nothing. The adapter maps this onto `PortError::not_found`.
        Ok(None) => return not_found("release artifact"),
        Err(error) => {
            tracing::warn!(%error, "reading the release registry failed");
            return service_unavailable("release registry");
        }
    };
    let (Ok(binary_key), Ok(signature_key)) = (
        artifact_key(&request.release, &target, ArtifactKind::Binary),
        artifact_key(&request.release, &target, ArtifactKind::Signature),
    ) else {
        // Unreachable: the tag validated above and the triple parsed, and those are the only two
        // inputs. Reported rather than unwrapped, because a panic here would take the process down
        // for a case the type system nearly rules out but does not.
        return api_error(
            ErrorStatus::Internal,
            "the release's storage key could not be built",
        );
    };
    let (bytes, signature) =
        match read_artifact_blobs(&state.blobs, &binary_key, &signature_key).await {
            Ok(pair) => pair,
            Err(refusal) => return refusal,
        };
    // The registry's digest is an integrity check against a truncated upload or a corrupted blob, and
    // is deliberately not a trust boundary — only the minisign signature makes an artifact safe to
    // install, and only the edge verifies it. Checking it here stops the cloud shipping bytes it can
    // already tell are not the ones it recorded. `Internal`, not `Unavailable`: a corrupt blob does
    // not fix itself on retry, and telling the edge to back off and try again would be a lie.
    let digest = hex_digest(&bytes);
    if digest != artifact.sha256 {
        tracing::error!(
            key = binary_key.as_str(),
            recorded = artifact.sha256,
            found = digest,
            "an artifact blob does not match its recorded digest"
        );
        return api_error(
            ErrorStatus::Internal,
            "the stored artifact does not match its recorded digest",
        );
    }
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    match HeaderValue::from_str(&hex_encode(&signature)) {
        Ok(value) => {
            headers.insert(ARTIFACT_SIGNATURE_HEADER, value);
        }
        Err(error) => {
            // Unreachable: hex is header-safe by construction. Refused rather than served, because a
            // 2xx without the signature is bytes the edge has nothing to judge by.
            tracing::error!(%error, "the artifact signature could not become a header value");
            return api_error(
                ErrorStatus::Internal,
                "the artifact signature could not be sent",
            );
        }
    }
    response
}
