// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/ota/releases/{release}/installer`: a hosted release as one store's installer.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_ports::blob_store::BlobStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::StoreId;
use serde::Deserialize;

use super::artifacts::read_one_artifact_blob;
use super::release_upload::OtaReleaseAdminState;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::ota::{
    ArtifactKind, ArtifactStore, TargetTriple, WINDOWS_TARGET, artifact_key, installer_file_name,
    validate_release_tag,
};

/// Which store a one-file installer is for and which cloud it dials — both written into its file
/// name — and, optionally, which Windows build.
///
/// Every field is optional to serde so that a missing one is refused in the API's own envelope,
/// naming the field, rather than as axum's bare query rejection.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct InstallerQuery {
    #[serde(default)]
    store_id: Option<String>,
    /// The cloud as a browser shows it: `host` or `host:port`.
    #[serde(default)]
    cloud: Option<String>,
    /// A Windows target triple; absent means [`WINDOWS_TARGET`].
    #[serde(default)]
    arch: Option<String>,
}

/// Checks everything about an installer request that can be judged from the request alone, and
/// composes the file name.
///
/// # Errors
///
/// The [`Response`] to send: a `400` naming `release`, `store_id`, `cloud` or `arch`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape the other route helpers carry"
)]
fn parse_installer_request(
    release: &str,
    query: &InstallerQuery,
) -> Result<(TargetTriple, String), Response> {
    if let Err(error) = validate_release_tag(release) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("release: {error}"),
            &[("release", "not usable as a storage key")],
        ));
    }
    let [store_id] = parse_ulid_fields([("store_id", query.store_id.as_deref().unwrap_or(""))])?;
    let file_name =
        installer_file_name(query.cloud.as_deref().unwrap_or(""), StoreId::new(store_id)).map_err(
            |error| {
                api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    format!("cloud: {error}"),
                    &[("cloud", "cannot be written into the installer's name")],
                )
            },
        )?;
    let target =
        TargetTriple::parse(query.arch.as_deref().unwrap_or(WINDOWS_TARGET)).map_err(|error| {
            api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("arch: {error}"),
                &[("arch", "not a target triple")],
            )
        })?;
    if !target.is_windows() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "arch: {target} is not a Windows build — the one-file installer is Windows-only, \
                 and a Linux store runs the install-pos-edge.sh the console generates"
            ),
            &[("arch", "not a Windows target")],
        ));
    }
    Ok((target, file_name))
}

/// `GET /admin/ota/releases/{release}/installer?store_id=&cloud=` — a hosted release's Windows
/// executable, named as that store's one-file installer
/// ([ADR-0141](../../../docs/adr/0141-the-console-hands-out-the-installer-by-name.md)).
///
/// **The bytes are the release's own.** Nothing is appended or rewritten: the store and the cloud
/// travel in the file *name*, which is what lets the minisign signature over these bytes keep
/// verifying, and what makes the file safe to hand out — the name carries a store id and a host,
/// neither of them a secret, and the credential still arrives only through activation.
///
/// **`console.data.read`**, like the list beside it: this reads what the cloud hosts. It writes
/// nothing and provisions nothing — a box it installs is unactivated until somebody with
/// `console.devices.manage` mints it a code.
///
/// **The cloud's host comes from the caller**, not from the `Host` header. The console passes the
/// address its own browser is showing, the same value it writes into every generated installer;
/// behind a reverse proxy the `Host` a handler sees can be the upstream's, and a file naming
/// `127.0.0.1` would install a store that dials itself.
///
/// Buffered, as the store-facing serve route is ([`read_artifact_blobs`]): a download is a person
/// at a console, one file at a time.
pub(super) async fn admin_download_installer<B, L, A, C>(
    State(state): State<OtaReleaseAdminState<B, L, A, C>>,
    headers: HeaderMap,
    Path(release): Path<String>,
    Query(query): Query<InstallerQuery>,
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
    let (target, file_name) = match parse_installer_request(&release, &query) {
        Ok(parsed) => parsed,
        Err(refusal) => return refusal,
    };
    match state.releases.find_artifact(&release, &target).await {
        Ok(Some(_hosted)) => {}
        Ok(None) => {
            return api_error_with_details(
                ErrorStatus::NotFound,
                format!(
                    "release {release} is not hosted for {target} — put it in the cloud from the \
                     OTA screen first"
                ),
                &[("release", "not hosted for this target")],
            );
        }
        Err(error) => {
            tracing::error!(%error, "reading the release registry for an installer failed");
            return service_unavailable("the release registry");
        }
    }
    let Ok(key) = artifact_key(&release, &target, ArtifactKind::Binary) else {
        // Unreachable: the tag validated and the triple parsed, and those are the only inputs.
        return api_error(
            ErrorStatus::Internal,
            "the release's storage key could not be built",
        );
    };
    let bytes = match read_one_artifact_blob(&state.blobs, &key, "artifact").await {
        Ok(bytes) => bytes,
        Err(refusal) => return refusal,
    };
    (
        [
            (CONTENT_TYPE, "application/octet-stream".to_owned()),
            // The name is composed only from a validated host, port and ULID, so it holds nothing a
            // quoted-string would need escaped.
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
        ],
        bytes,
    )
        .into_response()
}
