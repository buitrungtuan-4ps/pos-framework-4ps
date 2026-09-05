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
use pos_ports::{Signature, UpdateReport};
use pos_proto::ids::{DeviceId, StoreId};
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

/// The store this harness speaks for.
///
/// Named once and used both to build the adapter (which puts it in the path) and to build the report
/// the suite hands it, so the two cannot drift into a test that passes for the wrong reason.
fn reporting_store() -> StoreId {
    StoreId::new(Ulid::from_u128(0x570E))
}

/// The detached signature the stub serves beside [`ARTIFACT`]. Opaque bytes: this suite proves the
/// *wire* carries a signature and the adapter decodes it, not that Ed25519 works.
const ARTIFACT_SIGNATURE: &[u8] = b"stub-detached-signature";

/// Lowercase hex, matching what the cloud will put in the header.
///
/// Pushed a nibble at a time rather than through `format!`, which the workspace lints reject for
/// appending to a `String` — and which would allocate once per byte for no reason.
fn encode_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(nibble(byte >> 4));
        hex.push(nibble(*byte));
    }
    hex
}

/// One lowercase hex character for the low four bits of `value`.
///
/// A match rather than a lookup table, because the backbone crates forbid `indexing_slicing` and
/// this crate inherits the same lint — the same shape `pos_ports::device_registry` uses.
const fn nibble(value: u8) -> char {
    match value & 0x0f {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
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
                        ..HttpResponse::default()
                    }
                } else {
                    HttpResponse {
                        status: 403,
                        body: b"activation refused".to_vec(),
                        ..HttpResponse::default()
                    }
                }
            }
            // Store-scoped, not `/internal` (ADR-0088 Amendment 1). Matched by shape so the stub
            // asserts the *family* the real cloud serves rather than one hard-coded store id — a
            // path that is not under `/sync/stores/` falls through to the 404 arm below, which is
            // what would catch a regression back to `/internal`.
            path if path.starts_with("/sync/stores/") && path.ends_with("/artifact") => {
                let release = request
                    .get("release")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                // `arch` is required (ADR-0088 Correction 2). Enforced here rather than ignored:
                // without it the cloud cannot tell which of R1's two cross-compiled binaries the
                // box means, and the wrong one fails its self-test only after installing.
                let arch = request
                    .get("arch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if release == KNOWN_RELEASE && !arch.is_empty() {
                    // The wire shape ADR-0092 fixes: the artifact is the raw body and its detached
                    // signature rides a header as lowercase hex. The header name is spelled in
                    // mixed case here on purpose — the wire delivers it lowercased, and the
                    // adapter's lookup has to be case-insensitive like HTTP itself.
                    HttpResponse {
                        status: 200,
                        body: ARTIFACT.to_vec(),
                        headers: vec![(
                            "X-Pos-Artifact-Signature".to_owned(),
                            encode_hex(ARTIFACT_SIGNATURE),
                        )],
                    }
                } else {
                    HttpResponse {
                        status: 404,
                        body: b"no such release".to_vec(),
                        ..HttpResponse::default()
                    }
                }
            }
            path if path.starts_with("/sync/stores/") && path.ends_with("/report") => {
                // A well-formed report is accepted; parsing proves the adapter sent the fields the
                // cloud expects. `self_test_passed` is deliberately NOT required (ADR-0078
                // Amendment 1): the real route reads it as `#[serde(default)]`, so a store that has
                // never self-tested omits it and still reports which binary it runs. This stub
                // required it until the amendment, which is what the suite's new case caught.
                //
                // And the body must *not* carry a store id any more (ADR-0097's delivery note): the
                // store is in the path and the tenant is in the key. A body field would be a store
                // id the cloud has to remember to ignore, which is the field somebody wires back up
                // in two years — so its absence is asserted, not merely unused.
                let has_fields = request.get("installed").is_some()
                    && request.get("store_id").is_none()
                    && request.get("tenant_id").is_none();
                // When the verdict *is* sent it must be a boolean, not a string or a number — the
                // shape the cloud will deserialize into `Option<bool>`.
                let verdict_well_formed = request
                    .get("self_test_passed")
                    .is_none_or(serde_json::Value::is_boolean);
                HttpResponse {
                    status: if has_fields && verdict_well_formed {
                        204
                    } else {
                        400
                    },
                    body: Vec::new(),
                    ..HttpResponse::default()
                }
            }
            other => HttpResponse {
                status: 404,
                body: format!("the stub cloud has no route {other}").into_bytes(),
                ..HttpResponse::default()
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
        Ok(HttpCloudSync::new(
            StubCloud,
            reporting_store(),
            "x86_64-unknown-linux-gnu",
        ))
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

    fn update_signature(&self) -> Signature {
        Signature::new(ARTIFACT_SIGNATURE.to_vec())
    }

    fn sample_report(&self) -> UpdateReport {
        UpdateReport {
            store: reporting_store(),
            installed: ReleaseTag::new(KNOWN_RELEASE),
            self_test_passed: Some(true),
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
