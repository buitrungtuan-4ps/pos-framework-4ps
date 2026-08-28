// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `CloudSync` contract suite, against `HttpCloudSync` over a stub transport.
//!
//! `docs/roadmap.md` P2's exit criterion is *"every port has a contract suite; every implementation
//! passes it"*. This is the `cloud-sync-http` half. The stub transport reproduces the cloud's exact
//! status/body responses (ADR-0054), so the suite checks the adapter's whole branching behaviour —
//! the request it sends and the [`PortError`](pos_ports::PortError) it maps each status to — in the
//! fast pull-request gate, with no socket. The real TLS path (`TlsHttpTransport`) is exercised in the
//! gated integration lane and the soak, the split ADR-0038 already drew for the webhook sender.

// The whole file is test scaffolding. `allow-expect-in-tests` in clippy.toml scopes to `#[test]` and
// `#[cfg(test)]`, which does not reach an integration test's module-level helpers, so the stub cloud
// and the runtime it drives are allowed to expect and to take the trait's owned body by value here.
#![allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    reason = "test scaffolding: a stub cloud whose own JSON is malformed is an unrecoverable \
              test-setup fault, not a contract failure; and the HttpTransport method takes its body \
              by owned Vec, which the stub only needs to read"
)]

use core::future::Future;

use cloud_sync_http::{HttpCloudSync, HttpResponse, HttpTransport, TransportError};
use pos_contract_tests::harness::{CloudSyncHarness, Setup};
use pos_ports::UpdateReport;
use pos_proto::ids::{DeviceId, StoreId, TenantId};
use pos_proto::text::ReleaseTag;
use pos_proto::ulid::Ulid;

/// The one activation code the stub cloud accepts.
const VALID_CODE: &str = "AAAA-AAAA-AAAA";
/// The one release the stub cloud publishes.
const KNOWN_RELEASE: &str = "v1.2.3";
/// The bytes the stub cloud serves for [`KNOWN_RELEASE`].
const ARTIFACT: &[u8] = b"fake-signed-update-artifact";

/// The device the accepted code grants.
fn granted_device() -> DeviceId {
    DeviceId::new(Ulid::from_u128(0x0DE7))
}

/// A stub cloud: it speaks the exact wire the real cloud does (ADR-0050, ADR-0054), so the adapter's
/// request-shaping and status mapping are what the suite exercises.
struct StubCloud;

impl HttpTransport for StubCloud {
    async fn post_json(&self, path: &str, body: Vec<u8>) -> Result<HttpResponse, TransportError> {
        // Parsing the body proves the adapter sent the field the cloud expects (`code` / `release`).
        let request: serde_json::Value =
            serde_json::from_slice(&body).expect("the adapter sends a JSON request body");
        let response = match path {
            "/activate" => {
                let code = request
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if code == VALID_CODE {
                    let payload = serde_json::json!({
                        "device_id": granted_device().to_string(),
                        "credential": "posdev_stub_secret",
                    });
                    HttpResponse {
                        status: 201,
                        body: serde_json::to_vec(&payload).expect("the stub encodes its own JSON"),
                    }
                } else {
                    HttpResponse {
                        status: 403,
                        body: b"activation refused".to_vec(),
                    }
                }
            }
            "/internal/ota/artifact" => {
                let release = request
                    .get("release")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if release == KNOWN_RELEASE {
                    HttpResponse {
                        status: 200,
                        body: ARTIFACT.to_vec(),
                    }
                } else {
                    HttpResponse {
                        status: 404,
                        body: b"no such release".to_vec(),
                    }
                }
            }
            "/internal/ota/report" => {
                // A well-formed report is accepted; parsing proves the adapter sent the fields the
                // cloud expects.
                let has_fields = request.get("store_id").is_some()
                    && request.get("installed").is_some()
                    && request.get("self_test_passed").is_some();
                HttpResponse {
                    status: if has_fields { 204 } else { 400 },
                    body: Vec::new(),
                }
            }
            other => HttpResponse {
                status: 404,
                body: format!("the stub cloud has no route {other}").into_bytes(),
            },
        };
        Ok(response)
    }
}

/// The harness the shared suite runs against: `HttpCloudSync` over the stub cloud.
struct HttpHarness;

impl CloudSyncHarness for HttpHarness {
    type Channel = HttpCloudSync<StubCloud>;

    async fn fresh(&self) -> Setup<Self::Channel> {
        Ok(HttpCloudSync::new(StubCloud))
    }

    fn valid_code(&self) -> String {
        VALID_CODE.to_owned()
    }

    fn granted_device(&self) -> DeviceId {
        granted_device()
    }

    fn known_release(&self) -> ReleaseTag {
        ReleaseTag::new(KNOWN_RELEASE)
    }

    fn update_bytes(&self) -> Vec<u8> {
        ARTIFACT.to_vec()
    }

    fn sample_report(&self) -> UpdateReport {
        UpdateReport {
            tenant: TenantId::new(Ulid::from_u128(0x7E5A)),
            store: StoreId::new(Ulid::from_u128(0x570E)),
            installed: ReleaseTag::new(KNOWN_RELEASE),
            self_test_passed: true,
        }
    }
}

/// Drives a future to completion. The stub is immediately ready and touches no socket, so a
/// current-thread runtime is all the suite needs.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

mod cloud_sync {
    use super::{HttpHarness, block_on};
    pos_contract_tests::cloud_sync_suite!(HttpHarness, block_on);
}
