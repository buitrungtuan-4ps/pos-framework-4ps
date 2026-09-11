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
use pos_core::lease::{LeaseGeneration, LeaseStanding};
use pos_edge::activation_router;
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::{FakeKeyVault, FakeStore};
use pos_ports::cloud_sync::{ActivationGrant, CloudSync, SignedArtifact, UpdateReport};
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

    async fn fetch_update(&self, _release: &ReleaseTag) -> Result<SignedArtifact, PortError> {
        Err(PortError::not_found(
            PortName::CloudSync,
            "the stub cloud publishes no releases",
        ))
    }

    async fn report(&self, _report: &UpdateReport) -> Result<(), PortError> {
        // Activation tests never report; accept it so the stub satisfies the port.
        Ok(())
    }

    async fn archive_key(&self, _store: StoreId) -> Result<String, PortError> {
        // Activation tests never archive. `unavailable` rather than a made-up key, so a future
        // test that reaches here fails loudly instead of sealing under a stub's invention.
        Err(PortError::unavailable(
            PortName::CloudSync,
            "the stub cloud stores no archives",
        ))
    }

    async fn upload_archive(
        &self,
        _store: StoreId,
        _taken_at: pos_proto::time::Timestamp,
        _archive: &[u8],
    ) -> Result<(), PortError> {
        Err(PortError::unavailable(
            PortName::CloudSync,
            "the stub cloud stores no archives",
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
    harness_with_origins(code, &[])
}

/// The same harness with a store that has published `origins`
/// ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
fn harness_with_origins(
    code: &str,
    published: &[&str],
) -> (Router, Arc<FakeKeyVault>, Arc<Edge<FakeStore>>) {
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
    let origins = Arc::new(pos_edge::origins::Origins::new());
    origins
        .replace(published)
        .expect("the test's origins are valid");
    // No lease: the tests below are about the code exchange, and a store the cloud has never
    // issued a lease to has nothing to forget (ADR-0123). The one case that *is* about the lease
    // builds its own router below.
    let router = activation_router(Arc::clone(&edge), cloud, Arc::clone(&vault), None, &origins);
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

/// The origin a store published for a second front-end.
const PUBLISHED_ORIGIN: &str = "https://shell.example.com";

/// A CORS preflight for `uri` from [`PUBLISHED_ORIGIN`], returning the `Allow-Origin` it was granted.
async fn preflighted(router: Router, method: &str, uri: &str) -> Option<String> {
    let response = router
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri(uri)
                .header(header::HOST, "till.local:8080")
                .header(header::ORIGIN, PUBLISHED_ORIGIN)
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, method)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test]
async fn the_standing_route_is_reachable_cross_origin_and_the_exchange_is_not() {
    // `GET /api/activation` is the first call any front-end makes: `App.tsx`'s `onMount` runs it on
    // every boot, ahead of pairing, and routes the operator to `/setup` when the box is not
    // activated. Leaving it same-origin-only would make a second origin's very first request fail —
    // and fail *softly*, because that call is wrapped in a `.catch`.
    let (router, _vault, _edge) = harness_with_origins(&valid_code(), &[PUBLISHED_ORIGIN]);
    assert_eq!(
        preflighted(router, "GET", "/api/activation")
            .await
            .as_deref(),
        Some(PUBLISHED_ORIGIN)
    );

    // `POST /api/activate` mints a machine credential into the box's OS keyring from a code on the
    // store's setup sheet (ADR-0086). There is no cross-origin actor in that story: an operator
    // activates at the `/setup` screen the box itself serves. Pinned because the two routes are
    // layered separately, and `Router::layer` covering "everything added so far" would silently
    // pull this one in if they were ever merged back together.
    let (router, _vault, _edge) = harness_with_origins(&valid_code(), &[PUBLISHED_ORIGIN]);
    assert_eq!(preflighted(router, "POST", "/api/activate").await, None);
}

/// Activating forgets a lease generation this box inherited from a copied `store.sqlite`
/// ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
///
/// The scenario is the one `docs/guides/bring-a-store-online.md` actually tells an operator to
/// perform: the shop's machine dies, the operator copies its `store.sqlite` onto the replacement to
/// recover events the old box never published, and the console bumps the store's lease. Take-once
/// then binds the *dead* box's generation to the new machine's disk — so without this, ADR-0123's
/// gate refuses to seat a table on the machine that just replaced the broken one, which is exactly
/// the "fires wrongly" failure ADR-0108 declined to risk.
#[tokio::test]
async fn activating_forgets_a_lease_generation_inherited_from_a_copied_disk() {
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3))),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("edge composes"),
    );
    let store_id = edge.store_id();
    // The inherited row: this "disk" already holds generation 4, and the cloud has bumped to 5.
    let authority = pos_edge::InMemoryLease::new();
    let lease = Arc::new(pos_edge::LeaseWatch::new(
        Arc::new(pos_edge::StoreLease::new(authority, store_id)),
        Arc::clone(edge.lease()),
    ));
    lease.settle(Some(LeaseGeneration::new(4))).await;
    lease.settle(Some(LeaseGeneration::new(5))).await;
    assert_eq!(
        edge.lease().get(),
        LeaseStanding::Superseded,
        "the premise: a replacement built from a copied disk reads itself as the dead box"
    );

    let code = valid_code();
    let vault = Arc::new(FakeKeyVault::new());
    let origins = Arc::new(pos_edge::origins::Origins::new());
    let router = activation_router(
        Arc::clone(&edge),
        Arc::new(StubCloud {
            accepts: code.clone(),
        }),
        Arc::clone(&vault),
        Some(Arc::clone(&lease)),
        &origins,
    );

    let (status, _body) = call(
        router,
        "POST",
        "/api/activate",
        &format!(r#"{{"code":"{code}"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Both halves: the till stops refusing straight away…
    assert_eq!(
        edge.lease().get(),
        LeaseStanding::Active,
        "a freshly activated box must not be refusing to seat tables while it waits for a pull"
    );
    // …and the durable row is gone, so the next pull takes the store's *current* generation as a
    // first sight rather than re-superseding the box.
    lease.settle(Some(LeaseGeneration::new(5))).await;
    assert_eq!(
        edge.lease().get(),
        LeaseStanding::Active,
        "forgetting only the cell would let the next config pull undo the activation"
    );
}
