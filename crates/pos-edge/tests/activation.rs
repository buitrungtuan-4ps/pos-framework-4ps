// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Edge device activation, driven without a socket (P9, ADR-0050/ADR-0053).
//!
//! The activation sub-router composes a [`CloudSync`] channel, a [`KeyVault`], and the [`Edge`] that
//! owns the log. These tests run it against a stub cloud and the in-memory fakes, exactly as the
//! shipped binary will run it against `cloud-sync-http` and the OS-keyring vault. They pin the whole
//! flow: exchange the code, store the credential, record the completion, and report the standing.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use pos_core::activation::{ActivationCode, PAYLOAD_LEN};
use pos_edge::activation_router;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::{FakeKeyVault, FakeStore};
use pos_ports::cloud_sync::{ActivationGrant, CloudSync};
use pos_ports::key_vault::{KeyVault, SecretName};
use pos_ports::{PortError, PortName, Secret};
use pos_proto::ids::{DeviceId, StoreId};
use pos_proto::text::ReleaseTag;
use pos_proto::ulid::Ulid;

/// The device the stub cloud grants when it accepts a code.
fn granted_device() -> DeviceId {
    DeviceId::new(Ulid::from_u128(0x0DE7))
}

/// A stub cloud that accepts exactly one (checksum-valid) code and grants a credential for it.
///
/// The real `CloudSync` fake accepts a fixed string that is not checksum-valid, but the edge parses
/// the code locally before the round-trip (ADR-0050 — locally checkable), so a test needs a code that
/// both parses and is the one the cloud recognises.
struct StubCloud {
    accepts: String,
}

impl CloudSync for StubCloud {
    async fn activate(&self, activation_code: &str) -> Result<ActivationGrant, PortError> {
        if activation_code == self.accepts {
            Ok(ActivationGrant {
                device_id: granted_device(),
                credential: Secret::new(b"posdev_stub_secret".to_vec()),
            })
        } else {
            Err(PortError::permission_denied(
                PortName::CloudSync,
                "the stub cloud refuses this code",
            ))
        }
    }

    async fn fetch_update(&self, _release: &ReleaseTag) -> Result<Vec<u8>, PortError> {
        Err(PortError::not_found(
            PortName::CloudSync,
            "the stub cloud publishes no releases",
        ))
    }
}

/// A checksum-valid activation code the stub cloud will accept.
fn valid_code() -> String {
    ActivationCode::from_entropy([7; PAYLOAD_LEN])
        .as_str()
        .to_owned()
}

/// Builds an edge over the fakes, a stub cloud accepting `code`, and the activation router — handing
/// back the vault and the edge so a test can inspect what the flow left behind.
fn harness(code: &str) -> (Router, Arc<FakeKeyVault>, Arc<Edge<FakeStore>>) {
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3))),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("edge composes"),
    );
    let vault = Arc::new(FakeKeyVault::new());
    let cloud = Arc::new(StubCloud {
        accepts: code.to_owned(),
    });
    let router = activation_router(Arc::clone(&edge), cloud, Arc::clone(&vault));
    (router, vault, edge)
}

/// Drives one request through the router and returns the status and body text.
async fn call(router: Router, method: &str, uri: &str, body: &str) -> (StatusCode, String) {
    let response = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).unwrap_or_default(),
    )
}

#[tokio::test]
async fn a_valid_code_activates_the_device() {
    let code = valid_code();
    let (router, vault, edge) = harness(&code);
    // Subscribe before the request, so the completion broadcast is observable.
    let mut events = edge.fanout().subscribe();

    let (status, body) = call(
        router,
        "POST",
        "/api/activate",
        &format!("{{\"code\":\"{code}\"}}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let value: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        value["device_id"].as_str(),
        Some(granted_device().to_string().as_str()),
        "the grant names the activated device"
    );

    // The credential landed in the vault — the source of truth the boot gate reads.
    let stored = vault
        .load(SecretName::DeviceCredential)
        .await
        .expect("vault readable");
    assert!(stored.is_some(), "the credential is stored");

    // The completion event was broadcast, so record_activation ran rather than silently slipping.
    assert!(
        events.try_recv().is_ok(),
        "device.activation.completed was published to the fan-out"
    );
}

#[tokio::test]
async fn the_standing_flips_to_activated() {
    let code = valid_code();
    let (router, _vault, _edge) = harness(&code);

    // Before: not activated.
    let (status, body) = call(router.clone(), "GET", "/api/activation", "").await;
    assert_eq!(status, StatusCode::OK);
    let before: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(before["activated"].as_bool(), Some(false));

    // Activate, then read the standing again.
    let (activate, _) = call(
        router.clone(),
        "POST",
        "/api/activate",
        &format!("{{\"code\":\"{code}\"}}"),
    )
    .await;
    assert_eq!(activate, StatusCode::OK);
    let (status, body) = call(router, "GET", "/api/activation", "").await;
    assert_eq!(status, StatusCode::OK);
    let after: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(after["activated"].as_bool(), Some(true));
}

#[tokio::test]
async fn a_second_activation_is_a_conflict_not_a_re_exchange() {
    let code = valid_code();
    let (router, _vault, _edge) = harness(&code);
    let body = format!("{{\"code\":\"{code}\"}}");

    let (first, _) = call(router.clone(), "POST", "/api/activate", &body).await;
    assert_eq!(first, StatusCode::OK);
    // The box already holds a credential, so a repeat is a conflict — never a re-exchange that would
    // present the now-spent code and earn a 403 (ADR-0050).
    let (second, _) = call(router, "POST", "/api/activate", &body).await;
    assert_eq!(second, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_wrong_code_is_refused_with_no_oracle() {
    // A checksum-valid code the stub cloud does not recognise: it parses locally, so it reaches the
    // cloud, which refuses it as PermissionDenied → 403.
    let accepted = valid_code();
    let other = ActivationCode::from_entropy([9; PAYLOAD_LEN])
        .as_str()
        .to_owned();
    assert_ne!(accepted, other, "two distinct valid codes");
    let (router, _vault, _edge) = harness(&accepted);
    let (status, _) = call(
        router,
        "POST",
        "/api/activate",
        &format!("{{\"code\":\"{other}\"}}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_malformed_code_is_rejected_locally() {
    let (router, vault, _edge) = harness(&valid_code());
    let (status, _) = call(router, "POST", "/api/activate", "{\"code\":\"not-a-code\"}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Nothing was stored: a malformed code never reached the cloud.
    let stored = vault
        .load(SecretName::DeviceCredential)
        .await
        .expect("vault readable");
    assert!(stored.is_none(), "a malformed code stores nothing");
}
