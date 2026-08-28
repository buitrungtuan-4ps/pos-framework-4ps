// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`CloudSync`] adapter over an [`HttpTransport`]
//! ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)).
//!
//! Everything here is pure but for the one `await` on the transport: build the request body, map the
//! cloud's HTTP status to the right [`PortError`] status. The status mapping is the load-bearing part
//! — a caller branches on the status, so a refused code that surfaced as anything but
//! [`PortError::permission_denied`] would be a wrong retry policy.

use pos_ports::cloud_sync::{ActivationGrant, CloudSync, UpdateReport};
use pos_ports::{PortError, PortName, Secret};
use pos_proto::ids::DeviceId;
use pos_proto::text::ReleaseTag;

use crate::wire::{HttpResponse, HttpTransport};

/// The port this adapter serves, for every [`PortError`] it raises.
const PORT: PortName = PortName::CloudSync;

/// The cloud route the activation exchange posts to ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
const ACTIVATE_PATH: &str = "/activate";

/// The cloud route the OTA artifact fetch posts to ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)).
const ARTIFACT_PATH: &str = "/internal/ota/artifact";

/// The cloud route an update report posts to ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)).
const REPORT_PATH: &str = "/internal/ota/report";

/// The [`CloudSync`] adapter: the edge's request/response channel to its cloud, over an
/// [`HttpTransport`].
#[derive(Debug, Clone)]
pub struct HttpCloudSync<T> {
    transport: T,
}

impl<T> HttpCloudSync<T> {
    /// Wraps a transport as a [`CloudSync`] channel.
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }
}

/// The activation request body: the operator's code, in any casing or spacing.
#[derive(serde::Serialize)]
struct ActivateRequest<'a> {
    code: &'a str,
}

/// The activation response body: the device the credential authenticates as, and the credential.
#[derive(serde::Deserialize)]
struct ActivateResponse {
    device_id: String,
    credential: String,
}

/// The artifact-fetch request body: the release to fetch.
#[derive(serde::Serialize)]
struct FetchRequest<'a> {
    release: &'a str,
}

/// The update-report request body: which store, the version it now runs, and its self-test outcome
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)). The `/internal` route is
/// trusted-network, so the identity rides in the body as `/internal/reconcile` does.
#[derive(serde::Serialize)]
struct ReportRequest<'a> {
    tenant_id: String,
    store_id: String,
    installed: &'a str,
    self_test_passed: bool,
}

impl<T: HttpTransport> CloudSync for HttpCloudSync<T> {
    async fn activate(&self, activation_code: &str) -> Result<ActivationGrant, PortError> {
        let body = serde_json::to_vec(&ActivateRequest {
            code: activation_code,
        })
        .map_err(|error| {
            PortError::internal(
                PORT,
                format!("encoding the activation request failed: {error}"),
            )
        })?;
        let response = self
            .transport
            .post_json(ACTIVATE_PATH, body)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_activate(&response)
    }

    async fn fetch_update(&self, release: &ReleaseTag) -> Result<Vec<u8>, PortError> {
        let body = serde_json::to_vec(&FetchRequest {
            release: release.as_str(),
        })
        .map_err(|error| {
            PortError::internal(PORT, format!("encoding the fetch request failed: {error}"))
        })?;
        let response = self
            .transport
            .post_json(ARTIFACT_PATH, body)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_fetch(response)
    }

    async fn report(&self, report: &UpdateReport) -> Result<(), PortError> {
        let body = serde_json::to_vec(&ReportRequest {
            tenant_id: report.tenant.to_string(),
            store_id: report.store.to_string(),
            installed: report.installed.as_str(),
            self_test_passed: report.self_test_passed,
        })
        .map_err(|error| {
            PortError::internal(PORT, format!("encoding the update report failed: {error}"))
        })?;
        let response = self
            .transport
            .post_json(REPORT_PATH, body)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_report(&response)
    }
}

/// Maps an activation response to a grant, or the right refusal.
///
/// `2xx` is a grant (a body that does not parse, or names a non-ULID device, is the cloud breaking
/// its own contract — [`internal`](PortError::internal)); `400` is a malformed code
/// ([`invalid_argument`](PortError::invalid_argument)); `403` is a refusal
/// ([`permission_denied`](PortError::permission_denied), with no oracle — spent, revoked, and unknown
/// are one status); anything else is retryable ([`unavailable`](PortError::unavailable)).
fn parse_activate(response: &HttpResponse) -> Result<ActivationGrant, PortError> {
    match response.status {
        200..=299 => {
            let parsed: ActivateResponse =
                serde_json::from_slice(&response.body).map_err(|error| {
                    PortError::internal(
                        PORT,
                        format!("the cloud's activation response did not parse: {error}"),
                    )
                })?;
            let device_id = parsed.device_id.parse::<DeviceId>().map_err(|_ignored| {
                PortError::internal(PORT, "the cloud returned a device_id that is not a ULID")
            })?;
            if parsed.credential.is_empty() {
                return Err(PortError::internal(
                    PORT,
                    "the cloud returned an empty credential",
                ));
            }
            Ok(ActivationGrant {
                device_id,
                credential: Secret::new(parsed.credential.into_bytes()),
            })
        }
        400 => Err(PortError::invalid_argument(
            PORT,
            "the cloud rejected the activation code as malformed",
        )),
        403 => Err(PortError::permission_denied(
            PORT,
            "the cloud refused the activation code",
        )),
        other => Err(PortError::unavailable(
            PORT,
            format!("the cloud returned HTTP {other} for activation"),
        )),
    }
}

/// Maps an artifact response to bytes, or the right absence.
///
/// `2xx` is the signed artifact; `404` is an unpublished release
/// ([`not_found`](PortError::not_found), so the caller installs nothing rather than empty bytes);
/// anything else is retryable ([`unavailable`](PortError::unavailable)).
fn parse_fetch(response: HttpResponse) -> Result<Vec<u8>, PortError> {
    match response.status {
        200..=299 => Ok(response.body),
        404 => Err(PortError::not_found(
            PORT,
            "the cloud publishes no such release",
        )),
        other => Err(PortError::unavailable(
            PORT,
            format!("the cloud returned HTTP {other} for the artifact"),
        )),
    }
}

/// Maps an update-report response to acceptance, or the right refusal.
///
/// `2xx` is accepted (the report is telemetry, so an empty body is success); `400` is a malformed
/// report ([`invalid_argument`](PortError::invalid_argument)); anything else is retryable
/// ([`unavailable`](PortError::unavailable)) — a report the cloud never saw is dropped, never a
/// reason to undo an install.
fn parse_report(response: &HttpResponse) -> Result<(), PortError> {
    match response.status {
        200..=299 => Ok(()),
        400 => Err(PortError::invalid_argument(
            PORT,
            "the cloud rejected the update report as malformed",
        )),
        other => Err(PortError::unavailable(
            PORT,
            format!("the cloud returned HTTP {other} for the update report"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpResponse, parse_activate, parse_fetch, parse_report};
    use pos_proto::ids::DeviceId;
    use pos_proto::ulid::Ulid;

    fn response(status: u16, body: &[u8]) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_vec(),
        }
    }

    #[test]
    fn a_granted_activation_parses_the_device_and_credential() {
        let device = DeviceId::new(Ulid::from_u128(0x0DE7));
        let json = format!(r#"{{"device_id":"{device}","credential":"posdev_abc_secret"}}"#);
        let grant = parse_activate(&response(201, json.as_bytes())).expect("a grant");
        assert_eq!(grant.device_id, device);
        assert_eq!(grant.credential.expose(), b"posdev_abc_secret");
    }

    #[test]
    fn a_403_is_permission_denied_no_oracle() {
        let error = parse_activate(&response(403, b"activation refused")).expect_err("refused");
        assert_eq!(error.status(), pos_proto::ErrorStatus::PermissionDenied);
    }

    #[test]
    fn a_400_is_invalid_argument() {
        let error = parse_activate(&response(400, b"malformed")).expect_err("malformed");
        assert_eq!(error.status(), pos_proto::ErrorStatus::InvalidArgument);
    }

    #[test]
    fn a_503_activation_is_unavailable() {
        let error = parse_activate(&response(503, b"down")).expect_err("down");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Unavailable);
    }

    #[test]
    fn a_grant_with_an_empty_credential_is_an_internal_contract_breach() {
        let device = DeviceId::new(Ulid::from_u128(0x0DE7));
        let json = format!(r#"{{"device_id":"{device}","credential":""}}"#);
        let error = parse_activate(&response(201, json.as_bytes())).expect_err("empty credential");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Internal);
    }

    #[test]
    fn a_fetched_artifact_comes_back_intact() {
        let bytes = parse_fetch(response(200, b"signed-artifact-bytes")).expect("bytes");
        assert_eq!(bytes, b"signed-artifact-bytes");
    }

    #[test]
    fn an_unpublished_release_is_not_found() {
        let error = parse_fetch(response(404, b"no such release")).expect_err("not found");
        assert_eq!(error.status(), pos_proto::ErrorStatus::NotFound);
    }

    #[test]
    fn a_502_artifact_is_unavailable() {
        let error = parse_fetch(response(502, b"bad gateway")).expect_err("bad gateway");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Unavailable);
    }

    #[test]
    fn a_2xx_report_is_accepted() {
        parse_report(&response(204, b"")).expect("a well-formed report is accepted");
    }

    #[test]
    fn a_400_report_is_invalid_argument() {
        let error = parse_report(&response(400, b"malformed")).expect_err("malformed");
        assert_eq!(error.status(), pos_proto::ErrorStatus::InvalidArgument);
    }

    #[test]
    fn a_503_report_is_unavailable() {
        let error = parse_report(&response(503, b"down")).expect_err("down");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Unavailable);
    }
}
