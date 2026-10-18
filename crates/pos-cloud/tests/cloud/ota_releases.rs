// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin` release surface (ADR-0088): uploading and fetching a release, its installer, and
//! refusing to promote one the cloud does not host.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::auth::apikey::Scope;
use pos_cloud::release_source::{
    FetchedArtifact, FetchedRelease, ReleaseSource, ReleaseSourceError,
};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeBlobs, FakeConfigTrees, FakeKeys, FakeOtaArtifacts, FakeRollups, FakeWebhooks,
    TEST_TARGET, admin_cookie, app_all, artifact_body, clock, get_with_cookie, issue_store_key,
    json_body, ota_app_hosting, post_json_bearer, post_with_cookie, provisioned_admin,
    put_with_cookie, role_session_cookie, store_id, tenant,
};

// ---------------------------------------------------------------------------------------------
// R2's other half: putting a release into the cloud, and refusing to promote one that is not there
// ---------------------------------------------------------------------------------------------

/// A well-formed minisign signature line: the 74-byte blob (`algorithm ∥ key_id ∥ ed25519`) as
/// base64, which is what line 2 of a `.minisig` holds. The bytes are arbitrary — the cloud never
/// verifies, it only checks the shape ([ADR-0088](../../../../docs/adr/0088-ota-artifact-hosting.md)).
fn minisig_line() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(minisig_bytes())
}

/// The same 74 bytes, raw — what the fetch path carries, having done the base64 decode itself.
fn minisig_bytes() -> Vec<u8> {
    let mut blob = Vec::with_capacity(74);
    blob.extend_from_slice(b"ED");
    blob.extend_from_slice(&[7_u8; 8]);
    blob.extend_from_slice(&[9_u8; 64]);
    blob
}

/// The `/admin` release router plus the store-facing artifact route, over one shared object store and
/// one shared registry — so a test can upload and then download the same release, which is the whole
/// point of the pair.
fn release_app(admin: FakeAdmin) -> (axum::Router, FakeKeys) {
    release_app_with_source(admin, Some(FakeReleaseSource::publishes_nothing()))
}

/// The same three routers over one shared object store and registry, with the forge scripted.
///
/// All three doors in one app on purpose: what the cloud *holds* has to be identical whether an
/// artifact was uploaded or fetched, and the only way to assert that is to serve both to the same
/// store-facing route (ADR-0088 Amendment 4).
fn release_app_with_source<S>(admin: FakeAdmin, source: Option<S>) -> (axum::Router, FakeKeys)
where
    S: ReleaseSource + Clone + Send + Sync + 'static,
{
    let blobs = FakeBlobs::default();
    let releases = FakeOtaArtifacts::default();
    let keys = FakeKeys::default();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let router = http::router(app)
        .merge(http::ota_release_admin_router(
            blobs.clone(),
            releases.clone(),
            admin.clone(),
            clock(),
            Arc::new(NoopAuditRecorder),
        ))
        .merge(http::ota_release_fetch_router(
            blobs.clone(),
            releases.clone(),
            admin,
            clock(),
            Arc::new(NoopAuditRecorder),
            source,
        ))
        .merge(http::ota_artifact_router(
            keys.clone(),
            clock(),
            blobs,
            releases,
        ));
    (router, keys)
}

/// What the scripted forge answers. Three shapes, because three are what an operator meets: the
/// release is there, it is not, or the credentials are refused.
#[derive(Clone)]
enum ReleaseScript {
    Holds {
        tag: String,
        targets: Vec<String>,
        binary: Vec<u8>,
    },
    Missing,
    Unauthorized,
}

/// A [`ReleaseSource`] that answers from a script rather than a network, so the route's own
/// behaviour — permission, admission, idempotence, the refusals — is tested without a socket.
#[derive(Clone)]
struct FakeReleaseSource {
    script: ReleaseScript,
}

impl FakeReleaseSource {
    fn holding(tag: &str, targets: &[&str], binary: &[u8]) -> Self {
        Self {
            script: ReleaseScript::Holds {
                tag: tag.to_owned(),
                targets: targets.iter().map(|target| (*target).to_owned()).collect(),
                binary: binary.to_vec(),
            },
        }
    }

    fn publishes_nothing() -> Self {
        Self {
            script: ReleaseScript::Missing,
        }
    }

    fn refuses_the_credentials() -> Self {
        Self {
            script: ReleaseScript::Unauthorized,
        }
    }
}

impl ReleaseSource for FakeReleaseSource {
    async fn fetch(&self, release: &str) -> Result<FetchedRelease, ReleaseSourceError> {
        match &self.script {
            ReleaseScript::Holds {
                tag,
                targets,
                binary,
            } => Ok(FetchedRelease {
                tag: tag.clone(),
                artifacts: targets
                    .iter()
                    .map(|target| FetchedArtifact {
                        target: pos_cloud::ota::TargetTriple::parse(target)
                            .expect("a valid triple"),
                        binary: binary.clone(),
                        signature: minisig_bytes(),
                        asset_name: format!("pos-edge-{tag}-{target}.bin"),
                    })
                    .collect(),
            }),
            ReleaseScript::Missing => Err(ReleaseSourceError::NoSuchRelease {
                candidates: vec![format!("v{release}"), release.to_owned()],
            }),
            ReleaseScript::Unauthorized => Err(ReleaseSourceError::Unauthorized),
        }
    }
}

/// Asks the cloud to go and get a release.
fn fetch_request(release: &str, cookie: &str) -> Request<Body> {
    post_with_cookie(
        "/admin/ota/releases/fetch",
        &serde_json::json!({ "release": release }),
        cookie,
    )
}

/// Builds the upload request: the bare executable as the body, the signature line in its header.
fn upload_request(
    release: &str,
    arch: &str,
    body: Vec<u8>,
    minisig: &str,
    cookie: &str,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/admin/ota/releases?release={release}&arch={arch}"))
        .header("content-type", "application/octet-stream")
        .header("x-pos-minisig", minisig)
        .header("cookie", cookie)
        .body(Body::from(body))
        .expect("build the upload")
}

/// The pair joined: what `/admin` stores is byte-for-byte what a store downloads, signature included.
/// Until this passed, the serve route had no production producer at all — only a test wrote the rows
/// it reads.
#[tokio::test]
async fn an_uploaded_release_is_what_a_store_downloads() {
    let (router, keys) = release_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let binary = b"the next pos-edge".to_vec();

    let uploaded = router
        .clone()
        .oneshot(upload_request(
            "1.5.0",
            TEST_TARGET,
            binary.clone(),
            &minisig_line(),
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(uploaded.status(), StatusCode::CREATED);

    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);
    let served = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id()),
            &artifact_body("1.5.0", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the download");
    assert_eq!(served.status(), StatusCode::OK);
    let signature_header = served
        .headers()
        .get("x-pos-artifact-signature")
        .and_then(|value| value.to_str().ok())
        .expect("the signature header")
        .to_owned();
    let downloaded = axum::body::to_bytes(served.into_body(), usize::MAX)
        .await
        .expect("read the body");
    assert_eq!(downloaded.as_ref(), binary.as_slice());
    // The header is the hex of the same 74 bytes the upload's base64 carried.
    assert_eq!(signature_header.len(), 74 * 2);
    assert!(signature_header.starts_with("4544"), "\"ED\" in hex");
}

/// Uploading is a `console.ota.publish` action, not something any signed-in admin can do: a release
/// artifact is what every store in a ring will execute.
#[tokio::test]
async fn an_upload_without_the_ota_permission_is_refused() {
    let admin = provisioned_admin();
    let (router, _keys) = release_app(admin.clone());
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-upload").await;

    let refused = router
        .oneshot(upload_request(
            "1.5.0",
            TEST_TARGET,
            b"x".to_vec(),
            &minisig_line(),
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

/// The three request-shape refusals, each naming its own field. The cloud does not verify the
/// signature — but a value that could never *be* a signature is refused while a human is watching,
/// rather than at every box in the ring hours later.
#[tokio::test]
async fn a_malformed_upload_is_refused_naming_the_field() {
    let (router, _keys) = release_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let good = minisig_line();

    for (release, arch, body, minisig, field) in [
        (
            "1.5.0",
            TEST_TARGET,
            b"x".to_vec(),
            "not base64!!",
            "x-pos-minisig",
        ),
        (
            "1.5.0",
            TEST_TARGET,
            b"x".to_vec(),
            "c2hvcnQ=",
            "x-pos-minisig",
        ),
        ("1.5.0", TEST_TARGET, Vec::new(), good.as_str(), "body"),
        (
            "../etc",
            TEST_TARGET,
            b"x".to_vec(),
            good.as_str(),
            "release",
        ),
        // Uppercase: a triple is `[a-z0-9_-]`, and unlike a space this is URI-legal.
        (
            "1.5.0",
            "X86_64-Unknown-Linux",
            b"x".to_vec(),
            good.as_str(),
            "arch",
        ),
    ] {
        let refused = router
            .clone()
            .oneshot(upload_request(release, arch, body, minisig, &cookie))
            .await
            .expect("route the upload");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "release={release} arch={arch} minisig={minisig}"
        );
        let body = json_body(refused).await;
        assert_eq!(
            body["error"]["details"][0]["field"], field,
            "the refusal names {field}"
        );
    }
}

/// The Windows build the one-file installer is served from — the target the release workflow builds.
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

/// Asks for a store's one-file installer.
fn installer_request(release: &str, query: &str, cookie: &str) -> Request<Body> {
    get_with_cookie(
        &format!("/admin/ota/releases/{release}/installer?{query}"),
        cookie,
    )
}

/// The console's download: a hosted Windows release, byte for byte, under the name the edge reads as
/// "install this store, dialling this cloud" (ADR-0140, ADR-0141). The bytes are untouched — that is
/// what keeps their signature verifying — so the store and the cloud travel only in the name.
#[tokio::test]
async fn a_hosted_windows_release_downloads_as_the_stores_installer() {
    let (router, _keys) = release_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let binary = b"the windows pos-edge".to_vec();
    let uploaded = router
        .clone()
        .oneshot(upload_request(
            "1.5.0",
            WINDOWS_TARGET,
            binary.clone(),
            &minisig_line(),
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(uploaded.status(), StatusCode::CREATED);

    // No `arch`: the Windows build is the default, because it is the only kind of installer.
    let served = router
        .oneshot(installer_request(
            "1.5.0",
            &format!("store_id={}&cloud=Cloud.Example.com:8443", store_id()),
            &cookie,
        ))
        .await
        .expect("route the download");
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(
        served
            .headers()
            .get("content-disposition")
            .and_then(|value| value.to_str().ok()),
        Some(
            format!(
                "attachment; filename=\"pos-edge-setup_cloud.example.com@8443_{}.exe\"",
                store_id()
            )
            .as_str()
        )
    );
    let downloaded = axum::body::to_bytes(served.into_body(), usize::MAX)
        .await
        .expect("read the body");
    assert_eq!(downloaded.as_ref(), binary.as_slice());
}

/// Every refusal names its field, and each is one a person at the console can act on: a store id
/// that is not one, a cloud address the edge could not read back out of a file name, and a Linux
/// build — which installs with the generated script instead.
#[tokio::test]
async fn an_installer_request_is_refused_naming_the_field() {
    let (router, _keys) = release_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let store = store_id();

    for (release, query, field) in [
        (
            "1.5.0!",
            format!("store_id={store}&cloud=pos.example.vn"),
            "release",
        ),
        ("1.5.0", "cloud=pos.example.vn".to_owned(), "store_id"),
        (
            "1.5.0",
            "store_id=nope&cloud=pos.example.vn".to_owned(),
            "store_id",
        ),
        ("1.5.0", format!("store_id={store}"), "cloud"),
        (
            "1.5.0",
            format!("store_id={store}&cloud=pos_example.vn"),
            "cloud",
        ),
        (
            "1.5.0",
            format!("store_id={store}&cloud=pos.example.vn:0"),
            "cloud",
        ),
        (
            "1.5.0",
            format!("store_id={store}&cloud=pos.example.vn&arch={TEST_TARGET}"),
            "arch",
        ),
    ] {
        let refused = router
            .clone()
            .oneshot(installer_request(release, &query, &cookie))
            .await
            .expect("route the request");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "{release} {query}"
        );
        let body = json_body(refused).await;
        assert_eq!(
            body["error"]["details"][0]["field"], field,
            "{release} {query} names {field}"
        );
    }
}

/// A release the cloud holds only for Linux has no installer: a `404` that says what to do, rather
/// than a file that would never have run.
#[tokio::test]
async fn a_release_not_hosted_for_windows_has_no_installer() {
    let (router, _keys) = release_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let uploaded = router
        .clone()
        .oneshot(upload_request(
            "1.5.0",
            TEST_TARGET,
            b"the linux pos-edge".to_vec(),
            &minisig_line(),
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(uploaded.status(), StatusCode::CREATED);

    let missing = router
        .oneshot(installer_request(
            "1.5.0",
            &format!("store_id={}&cloud=pos.example.vn", store_id()),
            &cookie,
        ))
        .await
        .expect("route the request");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let body = json_body(missing).await;
    assert_eq!(body["error"]["details"][0]["field"], "release");
}

/// Reading what the cloud hosts is a console read, and a signed-out request is not one.
#[tokio::test]
async fn an_installer_needs_a_console_session() {
    let (router, _keys) = release_app(provisioned_admin());
    let refused = router
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/admin/ota/releases/1.5.0/installer?store_id={}&cloud=pos.example.vn",
                    store_id()
                ))
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the request");
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
}

/// Re-running a release step is not an error, and changing a release's bytes is: a version a ring has
/// already installed has to keep meaning the same thing.
#[tokio::test]
async fn re_uploading_is_idempotent_but_changing_the_bytes_is_refused() {
    let (router, _keys) = release_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let minisig = minisig_line();

    let first = router
        .clone()
        .oneshot(upload_request(
            "1.5.0",
            TEST_TARGET,
            b"same".to_vec(),
            &minisig,
            &cookie,
        ))
        .await
        .expect("route the first upload");
    assert_eq!(first.status(), StatusCode::CREATED);

    let again = router
        .clone()
        .oneshot(upload_request(
            "1.5.0",
            TEST_TARGET,
            b"same".to_vec(),
            &minisig,
            &cookie,
        ))
        .await
        .expect("route the repeat");
    assert_eq!(
        again.status(),
        StatusCode::OK,
        "identical bytes are a no-op"
    );

    let changed = router
        .oneshot(upload_request(
            "1.5.0",
            TEST_TARGET,
            b"different".to_vec(),
            &minisig,
            &cookie,
        ))
        .await
        .expect("route the changed upload");
    assert_eq!(changed.status(), StatusCode::CONFLICT);
}

/// The button behind step 5 of the release runbook. What the cloud holds afterwards has to be
/// byte-for-byte what an upload would have put there — same blobs, same rows — because a store
/// downloads through one route either way (ADR-0088 Amendment 4).
#[tokio::test]
async fn a_fetched_release_is_what_a_store_downloads() {
    let binary = b"the next pos-edge, fetched".to_vec();
    let (router, keys) = release_app_with_source(
        provisioned_admin(),
        Some(FakeReleaseSource::holding(
            "v1.6.0",
            &[TEST_TARGET, "aarch64-unknown-linux-gnu"],
            &binary,
        )),
    );
    let cookie = admin_cookie(&router).await;

    let fetched = router
        .clone()
        .oneshot(fetch_request("1.6.0", &cookie))
        .await
        .expect("route the fetch");
    assert_eq!(fetched.status(), StatusCode::OK);
    let body = json_body(fetched).await;
    assert_eq!(body["release"], "1.6.0", "recorded under the bare spelling");
    assert_eq!(
        body["tag"], "v1.6.0",
        "and reports the forge's own spelling"
    );
    assert_eq!(
        body["artifacts"].as_array().map(Vec::len),
        Some(2),
        "every target the release holds is hosted in one action — the whole point of the button"
    );
    assert_eq!(body["artifacts"][0]["outcome"], "recorded");

    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);
    let served = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id()),
            &artifact_body("1.6.0", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the download");
    assert_eq!(served.status(), StatusCode::OK);
    let downloaded = axum::body::to_bytes(served.into_body(), usize::MAX)
        .await
        .expect("read the body");
    assert_eq!(downloaded.as_ref(), binary.as_slice());
}

/// Pressing the button twice is not an error: the digest is the same, so the second answer says the
/// cloud already held it rather than pretending to work.
#[tokio::test]
async fn fetching_the_same_release_twice_reports_it_already_hosted() {
    let (router, _keys) = release_app_with_source(
        provisioned_admin(),
        Some(FakeReleaseSource::holding(
            "v1.6.0",
            &[TEST_TARGET],
            b"same",
        )),
    );
    let cookie = admin_cookie(&router).await;

    let first = router
        .clone()
        .oneshot(fetch_request("1.6.0", &cookie))
        .await
        .expect("route the first fetch");
    assert_eq!(
        json_body(first).await["artifacts"][0]["outcome"],
        "recorded"
    );

    let again = router
        .oneshot(fetch_request("1.6.0", &cookie))
        .await
        .expect("route the repeat");
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(
        json_body(again).await["artifacts"][0]["outcome"],
        "already_hosted"
    );
}

/// Fetching is a `console.ota.publish` action, exactly like uploading: it decides what every store in
/// a ring will execute, and the door it came through does not change that.
#[tokio::test]
async fn a_fetch_without_the_ota_permission_is_refused() {
    let admin = provisioned_admin();
    let (router, _keys) = release_app_with_source(
        admin.clone(),
        Some(FakeReleaseSource::holding("v1.6.0", &[TEST_TARGET], b"x")),
    );
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-fetch").await;

    let refused = router
        .oneshot(fetch_request("1.6.0", &cookie))
        .await
        .expect("route the fetch");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

/// The two refusals an operator actually meets, each naming what to go and look at: the version they
/// typed, or the token in `cloud.toml`.
#[tokio::test]
async fn a_fetch_that_finds_nothing_says_which_tags_it_tried() {
    let (missing, _keys) = release_app_with_source(
        provisioned_admin(),
        Some(FakeReleaseSource::publishes_nothing()),
    );
    let cookie = admin_cookie(&missing).await;
    let refused = missing
        .oneshot(fetch_request("9.9.9", &cookie))
        .await
        .expect("route the fetch");
    assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    let body = json_body(refused).await;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("v9.9.9") && message.contains("9.9.9"),
        "both spellings are named, so a fork that tags differently learns what was looked for: \
         {message}"
    );

    let (unauthorized, _keys) = release_app_with_source(
        provisioned_admin(),
        Some(FakeReleaseSource::refuses_the_credentials()),
    );
    let cookie = admin_cookie(&unauthorized).await;
    let refused = unauthorized
        .oneshot(fetch_request("1.6.0", &cookie))
        .await
        .expect("route the fetch");
    // `FailedPrecondition`, which this tree answers as `409`: the request is well-formed and the
    // version may well exist — what is not in place is the cloud's own credential, and no retry
    // mends that until a human edits `cloud.toml`.
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(refused).await["error"]["details"][0]["field"],
        "release_source.token",
        "the refusal points at the credential, not at the version"
    );
}

/// A release page with no signed executable on it — every asset an archive or a checksum — is a
/// refusal rather than a silent success, because a silent success is a ring that fetches nothing.
#[tokio::test]
async fn a_release_holding_no_signed_executable_is_refused() {
    let (router, _keys) = release_app_with_source(
        provisioned_admin(),
        Some(FakeReleaseSource::holding("v1.6.0", &[], b"")),
    );
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(fetch_request("1.6.0", &cookie))
        .await
        .expect("route the fetch");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(refused).await["error"]["details"][0]["field"],
        "release"
    );
}

/// A cloud with no `[release_source]` says so, rather than answering the bare `404` an unmerged
/// route would — which a console cannot tell apart from "that version was never released".
#[tokio::test]
async fn a_fetch_with_no_release_source_configured_names_the_missing_block() {
    let (router, _keys) = release_app_with_source(provisioned_admin(), None::<FakeReleaseSource>);
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(fetch_request("1.6.0", &cookie))
        .await
        .expect("route the fetch");
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "release_source");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("/admin/ota/releases"),
        "and points at the door that needs no forge at all"
    );
}

/// The regression for the overwrite the immutability rule was supposed to prevent.
///
/// Both blobs used to be written *before* the registry was asked, so re-sending a hosted release
/// with different bytes replaced the executable and its signature and only then answered `409`: the
/// registry kept naming the old digest while the object store held the new pair, and an edge would
/// have verified the new signature over the new bytes and installed them under a version string that
/// already meant something else. The `409` alone never caught it — what catches it is downloading
/// afterwards.
#[tokio::test]
async fn a_refused_change_leaves_the_hosted_artifact_untouched() {
    let (router, keys) = release_app(provisioned_admin());
    let cookie = admin_cookie(&router).await;
    let minisig = minisig_line();

    let first = router
        .clone()
        .oneshot(upload_request(
            "1.7.0",
            TEST_TARGET,
            b"the promoted bytes".to_vec(),
            &minisig,
            &cookie,
        ))
        .await
        .expect("route the first upload");
    assert_eq!(first.status(), StatusCode::CREATED);

    let changed = router
        .clone()
        .oneshot(upload_request(
            "1.7.0",
            TEST_TARGET,
            b"a different build entirely".to_vec(),
            &minisig,
            &cookie,
        ))
        .await
        .expect("route the changed upload");
    assert_eq!(changed.status(), StatusCode::CONFLICT);

    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);
    let served = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id()),
            &artifact_body("1.7.0", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the download");
    assert_eq!(served.status(), StatusCode::OK);
    let downloaded = axum::body::to_bytes(served.into_body(), usize::MAX)
        .await
        .expect("read the body");
    assert_eq!(
        downloaded.as_ref(),
        b"the promoted bytes".as_slice(),
        "a store downloads what the registry says it hosts, not what the refused upload carried"
    );
}

/// The promote guard. Publishing a rollout for a version the cloud does not host used to succeed and
/// then fail invisibly — every store in the ring fetching a `404`, which means "install nothing", so
/// the fleet would sit still with nothing in any log explaining why.
#[tokio::test]
async fn promoting_a_version_the_cloud_does_not_host_is_refused() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app_hosting(provisioned_admin(), config_trees, &[]);
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(put_with_cookie(
            "/admin/config/ota",
            &serde_json::json!({
                "tenant_id": tenant().to_string(),
                "store_id": store_id().to_string(),
                "target_version": "9.9.9",
                "min_ring": "canary",
                "rollout_percent": 10,
                "signing_key_id": "0101010101010101",
                "revoked_key_ids": [],
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "target_version");
}
