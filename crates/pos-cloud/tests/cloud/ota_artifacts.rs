// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The OTA artifact a store downloads (`/sync/stores/{store_id}/artifact`).

use axum::http::StatusCode;
use pos_cloud::auth::apikey::Scope;
use tower::ServiceExt;

use crate::{TEST_TARGET, artifact_body, artifact_fixture, post_json, post_json_bearer, store_id};

#[tokio::test]
async fn a_store_fetches_the_artifact_and_its_signature() {
    let (router, token, _) =
        artifact_fixture(b"the-binary", b"\xde\xad\xbe\xef", &[Scope::ReadConfig]);
    let response = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("1.4.0", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-pos-artifact-signature")
            .and_then(|value| value.to_str().ok()),
        Some("deadbeef"),
        "the signature rides a header as lowercase hex, and the body stays the raw artifact"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    assert_eq!(body.as_ref(), b"the-binary");
}

#[tokio::test]
async fn a_key_without_read_config_cannot_fetch_an_artifact() {
    // The owner's 2026-09-04 call was `read_config`, which every provisioned box already carries.
    // A key holding some *other* scope is still refused: the route is not open to any valid key.
    let (router, token, _) = artifact_fixture(b"the-binary", b"\x01", &[Scope::ReadRollups]);
    let response = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("1.4.0", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_unauthenticated_artifact_fetch_is_refused() {
    let (router, _, _) = artifact_fixture(b"the-binary", b"\x01", &[Scope::ReadConfig]);
    let response = router
        .oneshot(post_json(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("1.4.0", TEST_TARGET),
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_release_the_cloud_does_not_host_is_a_not_found() {
    // The adapter maps this onto `PortError::not_found` and installs nothing — it is an ordinary
    // answer, not a fault.
    let (router, token, _) = artifact_fixture(b"the-binary", b"\x01", &[Scope::ReadConfig]);
    let response = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("v9.9.9", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_architecture_the_cloud_hosts_nothing_for_is_a_not_found() {
    // A well-formed triple the registry has no artifact for is the same answer as an unknown
    // release: nothing to install. Deliberately *not* a validation refusal against a hardcoded list
    // — a fork that cross-compiles beyond the two targets R1 builds records artifacts for them, and
    // the registry stays the authority on what exists.
    let (router, token, _) = artifact_fixture(b"the-binary", b"\x01", &[Scope::ReadConfig]);
    let response = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("1.4.0", "sparc64-unknown-linux-gnu"),
            &token,
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_architecture_that_could_not_name_a_target_is_a_validation_refusal() {
    // The shape check, which is a different failure from "not hosted" and says so: the field is
    // named, so a caller can tell a typo from a release that was never built.
    let (router, token, _) = artifact_fixture(b"the-binary", b"\x01", &[Scope::ReadConfig]);
    let response = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("1.4.0", "X86_64 Linux!"),
            &token,
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("arch"),
        "the refusal names the field that was wrong, got: {text}"
    );
}

#[tokio::test]
async fn a_blob_that_does_not_match_its_recorded_digest_is_not_served() {
    // The registry's sha256 is an integrity check against a truncated upload or a corrupted blob.
    // Serving bytes the cloud can already tell are not the ones it recorded would push the failure
    // onto the store, which would refuse them at signature verification anyway — after downloading
    // thirty megabytes.
    let (router, token, blobs) = artifact_fixture(b"the-binary", b"\x01", &[Scope::ReadConfig]);
    let target = pos_cloud::ota::TargetTriple::parse(TEST_TARGET).expect("a valid triple");
    blobs.put_now(
        pos_cloud::ota::artifact_key("1.4.0", &target, pos_cloud::ota::ArtifactKind::Binary)
            .expect("a valid key")
            .as_str(),
        b"tampered",
    );
    let response = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("1.4.0", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn a_recorded_release_whose_blob_is_missing_is_not_a_not_found() {
    // The registry says it exists and the object store disagrees: that is the cloud's own
    // inconsistency. A `404` would tell the edge there is nothing to install and hide the fault.
    let (router, token, blobs) = artifact_fixture(b"the-binary", b"\x01", &[Scope::ReadConfig]);
    let target = pos_cloud::ota::TargetTriple::parse(TEST_TARGET).expect("a valid triple");
    blobs
        .objects
        .lock()
        .expect("the blob map is not poisoned")
        .remove(
            pos_cloud::ota::artifact_key("1.4.0", &target, pos_cloud::ota::ArtifactKind::Binary)
                .expect("a valid key")
                .as_str(),
        );
    let response = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/artifact", store_id().as_ulid()),
            &artifact_body("1.4.0", TEST_TARGET),
            &token,
        ))
        .await
        .expect("route the fetch");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
