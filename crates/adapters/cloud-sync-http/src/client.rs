// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`CloudSync`] adapter over an [`HttpTransport`]
//! ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)).
//!
//! Everything here is pure but for the one `await` on the transport: build the request body, map the
//! cloud's HTTP status to the right [`PortError`] status. The status mapping is the load-bearing part
//! — a caller branches on the status, so a refused code that surfaced as anything but
//! [`PortError::permission_denied`] would be a wrong retry policy.

use pos_ports::cloud_sync::{ActivationGrant, CloudSync, SignedArtifact, UpdateReport};
use pos_ports::signer::Signature;
use pos_ports::{PortError, PortName, Secret};
use pos_proto::ids::{DeviceId, StoreId};
use pos_proto::text::ReleaseTag;

use crate::wire::{HttpResponse, HttpTransport};

/// The port this adapter serves, for every [`PortError`] it raises.
const PORT: PortName = PortName::CloudSync;

/// The cloud route the activation exchange posts to ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
const ACTIVATE_PATH: &str = "/activate";

/// The cloud route the OTA artifact fetch posts to
/// ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 1).
///
/// **On `/sync`, not `/internal`.** ADR-0054 pinned this at `/internal/ota/artifact` when `/internal`
/// was believed to be the store-facing surface. It is not: `deploy/Caddyfile.d/site.caddy` answers
/// `404` to every `/internal/*` request from outside the box, and a store reaches its cloud through
/// that proxy — so the pinned path was unreachable by its only caller. The rule, from that amendment:
/// a route a store calls belongs on `/sync`; `/internal` is for callers on the box's own network.
fn artifact_path(store_id: StoreId) -> String {
    format!("/sync/stores/{store_id}/artifact")
}

/// The response header carrying the artifact's detached signature, as lowercase hex
/// ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)). Named after the existing
/// `X-Pos-Webhook-Signature` convention. It rides a header rather than the body so the body stays
/// the raw artifact: a JSON envelope would mean encoding tens of megabytes to move a few hundred
/// bytes.
const SIGNATURE_HEADER: &str = "X-Pos-Artifact-Signature";

/// The cloud route an update report posts to ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// store-scoped for the same reason [`artifact_path`] is — and for one more.
///
/// [ADR-0097](../../../docs/adr/0097-internal-route-authentication.md) recorded that a fleet-wide
/// shared secret could never make the `/internal` report *attributable*: that route read `tenant_id`
/// and `store_id` out of the body, so it believed the caller's claim about which store it was. On
/// `/sync` the cloud takes the tenant from the scoped key and the store from the path. The ADR said
/// the move was owed "when store-originated reporting gains a real caller"; this is that caller.
fn report_path(store_id: StoreId) -> String {
    format!("/sync/stores/{store_id}/report")
}

/// The [`CloudSync`] adapter: the edge's request/response channel to its cloud, over an
/// [`HttpTransport`].
#[derive(Debug, Clone)]
pub struct HttpCloudSync<T> {
    transport: T,
    store_id: StoreId,
    arch: &'static str,
}

impl<T> HttpCloudSync<T> {
    /// Wraps a transport as a [`CloudSync`] channel for one store, on one architecture.
    ///
    /// Both are properties of the **box**, not of any single call, so they are constructor state
    /// rather than parameters on [`CloudSync::fetch_update`]. That also keeps the port unchanged: the
    /// store id became part of the path when the store-called routes moved to `/sync`, and `arch` is
    /// the additive body field [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
    /// Correction 2 added — neither is something a caller should have to remember to pass.
    #[must_use]
    pub const fn new(transport: T, store_id: StoreId, arch: &'static str) -> Self {
        Self {
            transport,
            store_id,
            arch,
        }
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

/// The artifact-fetch request body: the release to fetch, and the architecture to fetch it for.
///
/// `arch` is additive per [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Correction 2.
/// R1's workflow cross-compiles two targets, so a request without one cannot say which binary it
/// means — and guessing hands an `aarch64` box an `x86_64` executable that fails its self-test
/// *after* the install, which is the expensive place to find out.
#[derive(serde::Serialize)]
struct FetchRequest<'a> {
    release: &'a str,
    arch: &'a str,
}

/// The update-report request body: the version the store now runs, and its self-test outcome.
///
/// No `tenant_id` and no `store_id` any more. The `/internal` shape carried both, which is exactly
/// what made a report un-attributable ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md));
/// on `/sync` the tenant comes from the scoped key the transport already attaches and the store from
/// the path. Dropping them from the body is not a courtesy — leaving them would mean the wire still
/// *offered* a store id the cloud must be careful to ignore.
#[derive(serde::Serialize)]
struct ReportRequest<'a> {
    installed: &'a str,
    /// Omitted entirely when the store has never self-tested (ADR-0078 Amendment 1), rather than
    /// sent as `null`: the cloud's field is `#[serde(default)]`, so an absent field and a `null` mean
    /// the same thing there, and omitting keeps a never-self-tested box distinguishable from a failed
    /// one.
    #[serde(skip_serializing_if = "Option::is_none")]
    self_test_passed: Option<bool>,
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

    async fn fetch_update(&self, release: &ReleaseTag) -> Result<SignedArtifact, PortError> {
        let body = serde_json::to_vec(&FetchRequest {
            release: release.as_str(),
            arch: self.arch,
        })
        .map_err(|error| {
            PortError::internal(PORT, format!("encoding the fetch request failed: {error}"))
        })?;
        let response = self
            .transport
            .post_json(&artifact_path(self.store_id), body)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_fetch(response)
    }

    async fn report(&self, report: &UpdateReport) -> Result<(), PortError> {
        let body = serde_json::to_vec(&ReportRequest {
            installed: report.installed.as_str(),
            self_test_passed: report.self_test_passed,
        })
        .map_err(|error| {
            PortError::internal(PORT, format!("encoding the update report failed: {error}"))
        })?;
        let response = self
            .transport
            .post_json(&report_path(report.store), body)
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
fn parse_fetch(response: HttpResponse) -> Result<SignedArtifact, PortError> {
    match response.status {
        200..=299 => {
            // The body is the raw artifact and the signature rides a header
            // ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)), so a 2xx with no
            // signature is bytes with nothing to judge them. Unusable, and reported as retryable:
            // a proxy stripping the header or a cloud mid-deploy is the likely cause, and the edge
            // should back off rather than treat it as terminal. It is never permission to install.
            let encoded = response.header(SIGNATURE_HEADER).ok_or_else(|| {
                PortError::unavailable(
                    PORT,
                    format!("the cloud served an artifact with no {SIGNATURE_HEADER}"),
                )
            })?;
            let signature = decode_hex(encoded).ok_or_else(|| {
                PortError::unavailable(
                    PORT,
                    format!("the cloud's {SIGNATURE_HEADER} is not lowercase hex"),
                )
            })?;
            Ok(SignedArtifact {
                bytes: response.body,
                signature: Signature::new(signature),
            })
        }
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

/// Decodes lowercase hex, or `None` if `text` is not an even run of hex digits.
///
/// Hand-rolled for the same reason `pos_ports::TokenDigest` hand-rolls its own: adding a base64 or
/// hex crate to this adapter would be a new third-party dependency, which `docs/adr/README.md` makes
/// an ADR-first change. Hex over base64 costs about seventy extra bytes on a signature of a few
/// hundred — against an artifact of tens of megabytes, which is why ADR-0092's base64 was corrected
/// to hex rather than a dependency being added to honour it.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let mut digits = text.chars();
    while let (Some(high), Some(low)) = (digits.next(), digits.next()) {
        bytes.push((nibble(high)? << 4) | nibble(low)?);
    }
    Some(bytes)
}

/// The four bits one lowercase hex character stands for.
fn nibble(character: char) -> Option<u8> {
    match character {
        '0'..='9' => Some(character as u8 - b'0'),
        'a'..='f' => Some(character as u8 - b'a' + 10),
        _ => None,
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
    use super::{
        HttpResponse, SIGNATURE_HEADER, decode_hex, parse_activate, parse_fetch, parse_report,
    };
    use pos_proto::ids::DeviceId;
    use pos_proto::ulid::Ulid;

    fn response(status: u16, body: &[u8]) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_vec(),
            ..HttpResponse::default()
        }
    }

    /// A 2xx artifact response carrying `signature` in the header, hex-encoded as the cloud will.
    ///
    /// The header name is deliberately lowercased here, because that is how a real wire delivers it
    /// while the constant is spelled in mixed case.
    fn signed_response(body: &[u8], signature: &[u8]) -> HttpResponse {
        // The encoder side, only needed by these tests: production only ever decodes.
        const fn nibble_of(value: u8) -> char {
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
        let mut hex = String::with_capacity(signature.len() * 2);
        for byte in signature {
            hex.push(nibble_of(byte >> 4));
            hex.push(nibble_of(*byte));
        }
        HttpResponse {
            status: 200,
            body: body.to_vec(),
            headers: vec![(SIGNATURE_HEADER.to_ascii_lowercase(), hex)],
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
    fn a_fetched_artifact_comes_back_intact_with_its_signature() {
        let fetched = parse_fetch(signed_response(b"signed-artifact-bytes", b"sig"))
            .expect("an artifact and its signature");
        assert_eq!(fetched.bytes, b"signed-artifact-bytes");
        assert_eq!(fetched.signature.as_bytes(), b"sig");
    }

    #[test]
    fn a_two_hundred_with_no_signature_header_is_unavailable_not_an_artifact() {
        // The failure that matters: bytes with nothing to judge them. It must not come back as a
        // successful fetch, because the next thing that happens to a successful fetch is `apply`.
        let error = parse_fetch(response(200, b"signed-artifact-bytes"))
            .expect_err("an unsigned artifact is not a usable answer");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Unavailable);
    }

    #[test]
    fn a_signature_header_that_is_not_hex_is_refused() {
        let mut malformed = signed_response(b"artifact", b"sig");
        malformed.headers = vec![(SIGNATURE_HEADER.to_ascii_lowercase(), "not-hex".to_owned())];
        let error = parse_fetch(malformed).expect_err("a malformed signature is refused");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Unavailable);
    }

    #[test]
    fn the_signature_header_is_matched_case_insensitively() {
        // HTTP header names are case-insensitive, so a cloud, a proxy, or a fork's own test stub may
        // send any casing. Reading it case-sensitively would turn a valid artifact into a fetch
        // failure for every store at once.
        let mut shouting = signed_response(b"artifact", b"\x01\x02");
        shouting.headers = vec![("X-POS-ARTIFACT-SIGNATURE".to_owned(), "0102".to_owned())];
        let fetched = parse_fetch(shouting).expect("any casing is the same header");
        assert_eq!(fetched.signature.as_bytes(), &[0x01, 0x02]);
    }

    #[test]
    fn hex_decoding_rejects_what_is_not_a_whole_run_of_hex_digits() {
        assert_eq!(decode_hex("00ff"), Some(vec![0x00, 0xff]));
        assert_eq!(decode_hex(""), None, "an empty signature is no signature");
        assert_eq!(decode_hex("abc"), None, "an odd length is not whole bytes");
        assert_eq!(
            decode_hex("00FF"),
            None,
            "uppercase is not the agreed encoding"
        );
        assert_eq!(decode_hex("zz"), None);
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
