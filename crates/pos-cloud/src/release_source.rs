// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Fetching a signed release pair from the forge that published it
//! ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 4).
//!
//! R1's workflow builds `pos-edge` for three targets, signs each with minisign, and publishes the
//! artifacts to a release. [`crate::ota`] is the cloud's registry of what it holds; the `/admin`
//! upload is one way to fill it. This module is the other: the cloud reads the release's asset list,
//! downloads each bare executable and the `.minisig` beside it, and hands the pair to the same
//! admission path the upload uses.
//!
//! # What this is not
//!
//! ADR-0088 rejected **pointing the edge at the forge**, and that stands: a shop's install path must
//! not depend on a third party being reachable from the shop's network. Nothing here touches it. A
//! store still fetches from `POST /sync/stores/{store_id}/artifact`, served by the cloud. What moves
//! is an *operator* step — from a laptop holding a console cookie to the box that is going to serve
//! the bytes anyway.
//!
//! # The cloud still judges nothing
//!
//! It moves bytes it was pointed at. The signature is carried along and stored, never checked here:
//! the edge verifies it against the anchor baked into its own binary
//! ([ADR-0047](../../../docs/adr/0047-minisign-verification.md),
//! [ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)) before staging anything. The one
//! check this module makes on a signature is its **length**, which is a shape check that catches a
//! truncated or mis-pasted file at the moment a human is watching rather than at every box in the
//! ring hours later.
//!
//! # Why the transport is written out rather than borrowed
//!
//! [`crate::webhook::transport`] already dials over the same `hyper`/`rustls` stack, but it is a
//! `POST` of a signed body to one vetted address with no redirect handling — every one of those
//! differs here. What *is* shared is the property that matters: the destination is vetted with
//! [`crate::webhook::ssrf`] and then dialed **at the address that vet approved**, on every hop. A
//! release asset download lands on a signed URL at a host the forge chooses, so the redirect target
//! is third-party input and is treated as such.

use core::fmt;
use core::future::Future;
use core::time::Duration;
use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Limited};
use hyper::Request;
use hyper::header::{ACCEPT, AUTHORIZATION, HOST, LOCATION, USER_AGENT};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use crate::ota::{MAX_ARTIFACT_BYTES, MINISIGN_SIGNATURE_LEN, TargetTriple};
use crate::webhook::ssrf::vet_blocking;

/// How many redirects one download follows before giving up.
///
/// A release asset is served through exactly one redirect today — the API hands back a signed URL at
/// an object host. Five leaves room for a forge that adds a hop without letting a redirect loop
/// become an infinite fetch.
const MAX_REDIRECTS: usize = 5;

/// The largest release document the forge may answer a metadata request with.
///
/// A release with a few dozen assets is a few kilobytes of JSON; 4 MiB is far past any honest
/// answer and small enough that a misconfigured base URL pointing at something huge cannot fill the
/// box's memory.
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// The largest `.minisig` file accepted. Minisign writes four short lines; 8 KiB is generous.
const MAX_SIGNATURE_FILE_BYTES: usize = 8 * 1024;

/// How long a metadata request gets.
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one asset download gets. A tens-of-megabytes binary over a modest uplink, with room to
/// spare; past this the fetch is reported as unreachable rather than left holding the console.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// What every request to the forge identifies itself as. GitHub's REST API refuses a request with
/// no `User-Agent` outright, so this is required rather than polite.
const USER_AGENT_VALUE: &str = "pos-cloud";

/// The stem every release artifact's file name starts with, matching R1's workflow.
const ARTIFACT_STEM: &str = "pos-edge";

/// The extensions the **bare executable** is published under: `.bin` for the Linux targets, `.exe`
/// for Windows. Read from the asset list rather than composed, because a composed name would have to
/// know this mapping — the exact drift ADR-0088 Amendment 2 was written about.
const BINARY_EXTENSIONS: [&str; 2] = [".bin", ".exe"];

/// The suffix minisign gives a detached signature.
const SIGNATURE_EXTENSION: &str = ".minisig";

// ---------------------------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------------------------

/// One artifact a release holds: the bare executable, and the detached signature beside it.
#[derive(Clone)]
pub struct FetchedArtifact {
    /// The target the binary was compiled for, parsed out of the asset's name.
    pub target: TargetTriple,
    /// The executable itself — the bytes `UpdateInstaller::apply` will write as the next binary.
    pub binary: Vec<u8>,
    /// The raw minisign signature, already decoded from the `.minisig` file's base64 line.
    pub signature: Vec<u8>,
    /// The asset's file name, recorded in the audit entry so the trail says what was taken.
    pub asset_name: String,
}

// Hand-written so a 30 MB binary never lands in a log line; the crate warns on a missing `Debug`, and
// deriving one here would make any `?artifact` in a `tracing` call a memory and legibility hazard.
impl fmt::Debug for FetchedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FetchedArtifact")
            .field("target", &self.target)
            .field("binary_bytes", &self.binary.len())
            .field("asset_name", &self.asset_name)
            .finish_non_exhaustive()
    }
}

/// What a release holds, as the cloud read it.
#[derive(Debug, Clone)]
pub struct FetchedRelease {
    /// The tag the forge publishes it under, in the forge's own spelling (e.g. `v1.2.3`).
    pub tag: String,
    /// Every OTA pair found, ordered by target.
    pub artifacts: Vec<FetchedArtifact>,
}

/// Why a release could not be fetched.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseSourceError {
    /// No release is published under any spelling of the version that was asked for.
    #[error("no release is published as {}", candidates.join(" or "))]
    NoSuchRelease {
        /// Every tag that was tried, so the refusal teaches a fork that tags differently.
        candidates: Vec<String>,
    },
    /// The forge refused the cloud's credentials — a missing token for a private repository, or an
    /// expired one.
    #[error("the release source refused the cloud's credentials")]
    Unauthorized,
    /// An asset is larger than the cloud will host.
    #[error("{asset} is {size_bytes} bytes, past what the cloud will host")]
    TooLarge {
        /// The asset's file name.
        asset: String,
        /// Its size as the forge reported it.
        size_bytes: u64,
    },
    /// An asset was fetched but is not what it claims to be.
    #[error("{asset}: {reason}")]
    Malformed {
        /// The asset's file name.
        asset: String,
        /// What was wrong with it.
        reason: String,
    },
    /// The forge could not be reached, or answered something unusable.
    #[error("the release source could not be reached: {0}")]
    Unavailable(String),
}

/// Where the cloud fetches a signed release pair from.
///
/// A seam rather than a concrete client so the `/admin` route can be driven in a test without a
/// socket — the same shape [`crate::webhook::WebhookTransport`] takes, and for the same reason.
pub trait ReleaseSource {
    /// Reads every OTA pair the forge holds for `release`.
    ///
    /// `release` is spelled the way the registry keys it and a rollout's `target_version` names it —
    /// bare, without a leading `v` (ADR-0088 Amendment 2). Which tag that corresponds to on the
    /// forge is [`tag_candidates`]'s business.
    ///
    /// # Errors
    ///
    /// [`ReleaseSourceError`] if the release does not exist, the credentials are refused, an asset
    /// is unusable, or the forge cannot be reached.
    fn fetch(
        &self,
        release: &str,
    ) -> impl Future<Output = Result<FetchedRelease, ReleaseSourceError>> + Send;
}

// ---------------------------------------------------------------------------------------------
// The pure parts
// ---------------------------------------------------------------------------------------------

/// The tags a forge might have published `release` under, most likely first.
///
/// This is **not** the mapping function ADR-0088 Amendment 2 forbids. It writes nothing down and
/// decides nothing: it is two guesses, and if neither is a release the refusal names both, so a fork
/// tagging some third way learns what was looked for instead of reading a config key's documentation.
#[must_use]
pub fn tag_candidates(release: &str) -> Vec<String> {
    let bare = release.strip_prefix('v').unwrap_or(release);
    let prefixed = format!("v{bare}");
    if prefixed == release {
        vec![prefixed]
    } else {
        vec![prefixed, release.to_owned()]
    }
}

/// One asset as the forge lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeAsset {
    /// The file name, e.g. `pos-edge-v1.2.3-x86_64-unknown-linux-gnu.bin`.
    pub name: String,
    /// Where to download it.
    pub url: String,
    /// Its size, as the forge reported it — checked before a byte is read.
    pub size_bytes: u64,
}

/// A bare executable and the detached signature named after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetPair {
    /// The target parsed out of the binary's name.
    pub target: TargetTriple,
    /// The bare executable.
    pub binary: ForgeAsset,
    /// Its `.minisig`.
    pub signature: ForgeAsset,
}

/// Which of a release's assets are OTA pairs, and for which target.
///
/// The rule, stated once: an asset is a pair when its name is `pos-edge-<tag>-<target>` with a
/// [`BINARY_EXTENSIONS`] extension, `<target>` parses as a [`TargetTriple`], and an asset named
/// exactly `<that name>.minisig` is also present. Everything else in a release — the `.tar.gz`, the
/// `.zip`, `SHA256SUMS`, and every signature beside them — is skipped rather than guessed at.
///
/// A binary with no signature is **not** a pair. Hosting bytes the edge would have nothing to judge
/// by would produce a store that fetches, fails verification and stays put, which is the failure
/// mode hardest to read from a shop.
#[must_use]
pub fn select_pairs(tag: &str, assets: &[ForgeAsset]) -> Vec<AssetPair> {
    let prefix = format!("{ARTIFACT_STEM}-{tag}-");
    let mut pairs: Vec<AssetPair> = assets
        .iter()
        .filter_map(|asset| {
            let rest = asset.name.strip_prefix(&prefix)?;
            let triple = BINARY_EXTENSIONS
                .iter()
                .find_map(|extension| rest.strip_suffix(extension))?;
            let target = TargetTriple::parse(triple).ok()?;
            let signature_name = format!("{}{SIGNATURE_EXTENSION}", asset.name);
            let signature = assets.iter().find(|other| other.name == signature_name)?;
            Some(AssetPair {
                target,
                binary: asset.clone(),
                signature: signature.clone(),
            })
        })
        .collect();
    pairs.sort_by(|left, right| left.target.cmp(&right.target));
    pairs
}

/// Decodes the raw signature out of a `.minisig` file.
///
/// Minisign writes four lines — an untrusted comment, the signature, a trusted comment, and the
/// global signature — and the second is the one every part of this tree carries as raw bytes. That
/// is the same line `docs/release-runbook.md` tells an operator to pull out with `sed -n 2p` for the
/// upload route's header; reading it here is that step, done by the cloud.
///
/// # Errors
///
/// A human-readable reason: the file is not text, has no second line, that line is not base64, or it
/// does not decode to a minisign signature's length.
pub fn decode_minisig_file(file: &[u8]) -> Result<Vec<u8>, String> {
    let text = core::str::from_utf8(file).map_err(|_| "not a text file".to_owned())?;
    let line = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .nth(1)
        .ok_or_else(|| "has no signature line — minisign writes it second".to_owned())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(line)
        .map_err(|_| "its signature line is not valid base64".to_owned())?;
    if bytes.len() != MINISIGN_SIGNATURE_LEN {
        return Err(format!(
            "its signature line decodes to {} bytes, and a minisign signature is {MINISIGN_SIGNATURE_LEN}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------------------------
// The HTTPS client
// ---------------------------------------------------------------------------------------------

/// The shape of a release, as the forge's REST API answers it. Only the three fields this needs.
#[derive(Debug, Deserialize)]
struct ReleaseDocument {
    tag_name: String,
    #[serde(default)]
    assets: Vec<AssetDocument>,
}

/// One asset inside a [`ReleaseDocument`].
#[derive(Debug, Deserialize)]
struct AssetDocument {
    name: String,
    /// The API's own download URL, which serves a private repository's asset with the cloud's token
    /// and a public one's without.
    url: String,
    #[serde(default)]
    size: u64,
}

/// One answer from the forge: enough to follow a redirect or read a body.
#[derive(Debug)]
struct Answer {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
}

/// A [`ReleaseSource`] over a GitHub-shaped REST API.
///
/// Cheap to clone — a shared rustls config and three short strings — so the router holds one.
#[derive(Clone)]
pub struct GithubReleases {
    api_base: String,
    repository: String,
    token: Option<Arc<str>>,
    tls: Arc<ClientConfig>,
}

// Hand-written so a token cannot reach a log through a `?state` in some future tracing call.
impl fmt::Debug for GithubReleases {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GithubReleases")
            .field("api_base", &self.api_base)
            .field("repository", &self.repository)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl GithubReleases {
    /// Builds a client for `repository` (`owner/name`) against `api_base`.
    ///
    /// The `ring` crypto provider is selected explicitly rather than via the ambient default, so no
    /// `aws-lc-rs` provider can slip in through feature unification — the same care
    /// [ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md) took for the webhook sender.
    ///
    /// # Panics
    ///
    /// If the `ring` provider cannot supply the safe default TLS protocol versions — which it always
    /// can, so this is a build-time impossibility, not a runtime path.
    #[must_use]
    pub fn new(api_base: &str, repository: &str, token: Option<&str>) -> Self {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supplies the safe default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Self {
            api_base: api_base.trim_end_matches('/').to_owned(),
            repository: repository.trim_matches('/').to_owned(),
            token: token.map(Into::into),
            tls: Arc::new(tls),
        }
    }

    /// One GET over TLS, to the address the SSRF vet approved for this URL.
    ///
    /// `authorize` is the caller's call rather than a property of the client: the token goes to the
    /// forge's own host and to nowhere the forge redirects to.
    async fn get_once(
        &self,
        url: &str,
        accept: &str,
        authorize: bool,
        max_bytes: usize,
    ) -> Result<Answer, ReleaseSourceError> {
        let vetted = vet_blocking(url)
            .await
            .map_err(|reason| ReleaseSourceError::Unavailable(format!("{url}: {reason}")))?;
        let parsed = url::Url::parse(&vetted.url)
            .map_err(|error| ReleaseSourceError::Unavailable(format!("{url}: {error}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| ReleaseSourceError::Unavailable(format!("{url}: no host")))?
            .to_owned();
        let port = parsed.port_or_known_default().unwrap_or(443);
        let mut target = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            target.push('?');
            target.push_str(query);
        }

        let stream = connect(&vetted.addresses, port).await?;
        // SNI and certificate verification bind to the hostname, never the vetted IP we dialed.
        let server_name = ServerName::try_from(host.clone()).map_err(|_| {
            ReleaseSourceError::Unavailable(format!("{host} is not a valid TLS server name"))
        })?;
        let tls = TlsConnector::from(Arc::clone(&self.tls))
            .connect(server_name, stream)
            .await
            .map_err(|error| {
                ReleaseSourceError::Unavailable(format!(
                    "the TLS handshake with {host} failed: {error}"
                ))
            })?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|error| {
                ReleaseSourceError::Unavailable(format!(
                    "the HTTP handshake with {host} failed: {error}"
                ))
            })?;
        // The connection future must be driven for the request to make progress; it ends when the
        // response completes and `sender` is dropped.
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut request = Request::builder()
            .method("GET")
            .uri(&target)
            .header(HOST, host.as_str())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(ACCEPT, accept);
        if let (true, Some(token)) = (authorize, self.token.as_ref()) {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = request.body(Empty::<Bytes>::new()).map_err(|error| {
            ReleaseSourceError::Unavailable(format!("building the request failed: {error}"))
        })?;

        let response = sender.send_request(request).await.map_err(|error| {
            ReleaseSourceError::Unavailable(format!("the request to {host} failed: {error}"))
        })?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = Limited::new(response.into_body(), max_bytes)
            .collect()
            .await
            .map_err(|_over| {
                ReleaseSourceError::Unavailable(format!(
                    "{host} answered with more than {max_bytes} bytes"
                ))
            })?
            .to_bytes()
            .to_vec();
        Ok(Answer {
            status,
            location,
            body,
        })
    }

    /// A GET that follows the forge's redirects, dropping the token the moment the host changes.
    async fn get(
        &self,
        url: &str,
        accept: &str,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Answer, ReleaseSourceError> {
        match tokio::time::timeout(timeout, self.get_following(url, accept, max_bytes)).await {
            Ok(answer) => answer,
            Err(_elapsed) => Err(ReleaseSourceError::Unavailable(format!(
                "{url} did not answer within {} seconds",
                timeout.as_secs()
            ))),
        }
    }

    /// The redirect loop itself, bounded by [`MAX_REDIRECTS`].
    async fn get_following(
        &self,
        url: &str,
        accept: &str,
        max_bytes: usize,
    ) -> Result<Answer, ReleaseSourceError> {
        let mut current = url.to_owned();
        // The token is for the forge's own API host. A redirect points at an object host the forge
        // chose, and handing it a credential it never needs is how a token leaks to a third party.
        let mut authorize = true;
        for _hop in 0..=MAX_REDIRECTS {
            let answer = self
                .get_once(&current, accept, authorize, max_bytes)
                .await?;
            if !matches!(answer.status, 301 | 302 | 303 | 307 | 308) {
                return Ok(answer);
            }
            let Some(location) = answer.location.as_deref() else {
                return Err(ReleaseSourceError::Unavailable(format!(
                    "{current} answered {} with no Location",
                    answer.status
                )));
            };
            let base = url::Url::parse(&current)
                .map_err(|error| ReleaseSourceError::Unavailable(format!("{current}: {error}")))?;
            let next = base.join(location).map_err(|error| {
                ReleaseSourceError::Unavailable(format!(
                    "{current} redirected somewhere unparseable: {error}"
                ))
            })?;
            authorize = authorize && next.host_str() == base.host_str();
            current = next.to_string();
        }
        Err(ReleaseSourceError::Unavailable(format!(
            "{url} redirected more than {MAX_REDIRECTS} times"
        )))
    }

    /// Reads the release document for one candidate tag, or `None` if the forge has no such release.
    async fn read_release(&self, tag: &str) -> Result<Option<ReleaseDocument>, ReleaseSourceError> {
        let url = format!(
            "{}/repos/{}/releases/tags/{}",
            self.api_base,
            self.repository,
            urlencode_segment(tag)
        );
        let answer = self
            .get(
                &url,
                "application/vnd.github+json",
                MAX_METADATA_BYTES,
                METADATA_TIMEOUT,
            )
            .await?;
        match answer.status {
            200 => serde_json::from_slice::<ReleaseDocument>(&answer.body)
                .map(Some)
                .map_err(|error| {
                    ReleaseSourceError::Unavailable(format!(
                        "the release source answered something that is not a release: {error}"
                    ))
                }),
            // A private repository answers `404` to an unauthenticated caller, so a missing token and
            // a missing release look alike from here. The refusal the route builds says both.
            404 => Ok(None),
            401 | 403 => Err(ReleaseSourceError::Unauthorized),
            other => Err(ReleaseSourceError::Unavailable(format!(
                "the release source answered HTTP {other}"
            ))),
        }
    }

    /// Downloads one asset's bytes.
    async fn download(
        &self,
        asset: &ForgeAsset,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, ReleaseSourceError> {
        let answer = self
            .get(&asset.url, "application/octet-stream", max_bytes, timeout)
            .await?;
        match answer.status {
            200 => Ok(answer.body),
            401 | 403 => Err(ReleaseSourceError::Unauthorized),
            other => Err(ReleaseSourceError::Unavailable(format!(
                "downloading {} answered HTTP {other}",
                asset.name
            ))),
        }
    }
}

impl ReleaseSource for GithubReleases {
    async fn fetch(&self, release: &str) -> Result<FetchedRelease, ReleaseSourceError> {
        let candidates = tag_candidates(release);
        let mut document = None;
        for candidate in &candidates {
            if let Some(found) = self.read_release(candidate).await? {
                document = Some(found);
                break;
            }
        }
        let Some(document) = document else {
            return Err(ReleaseSourceError::NoSuchRelease { candidates });
        };

        let assets: Vec<ForgeAsset> = document
            .assets
            .into_iter()
            .map(|asset| ForgeAsset {
                name: asset.name,
                url: asset.url,
                size_bytes: asset.size,
            })
            .collect();
        // The forge's own spelling of the tag, not the candidate that matched: the asset names carry
        // whatever the workflow was run with.
        let pairs = select_pairs(&document.tag_name, &assets);

        let mut artifacts = Vec::with_capacity(pairs.len());
        for pair in pairs {
            if pair.binary.size_bytes > MAX_ARTIFACT_BYTES as u64 {
                return Err(ReleaseSourceError::TooLarge {
                    asset: pair.binary.name,
                    size_bytes: pair.binary.size_bytes,
                });
            }
            let signature_file = self
                .download(&pair.signature, MAX_SIGNATURE_FILE_BYTES, METADATA_TIMEOUT)
                .await?;
            let signature = decode_minisig_file(&signature_file).map_err(|reason| {
                ReleaseSourceError::Malformed {
                    asset: pair.signature.name.clone(),
                    reason,
                }
            })?;
            // The signature is read first on purpose: a malformed one refuses the release after a few
            // kilobytes rather than after a 30 MB download.
            let binary = self
                .download(&pair.binary, MAX_ARTIFACT_BYTES, DOWNLOAD_TIMEOUT)
                .await?;
            artifacts.push(FetchedArtifact {
                target: pair.target,
                binary,
                signature,
                asset_name: pair.binary.name,
            });
        }
        Ok(FetchedRelease {
            tag: document.tag_name,
            artifacts,
        })
    }
}

/// Connects to the first vetted address that accepts a TCP connection.
async fn connect(
    addresses: &[std::net::IpAddr],
    port: u16,
) -> Result<TcpStream, ReleaseSourceError> {
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect(SocketAddr::new(*address, port)).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(ReleaseSourceError::Unavailable(match last_error {
        Some(error) => format!("connecting to the release source failed: {error}"),
        None => "the release source had no vetted address to dial".to_owned(),
    }))
}

/// Percent-encodes one path segment.
///
/// A release tag is validated as a path segment before it reaches here
/// ([`crate::ota::validate_release_tag`]), so this encodes nothing in practice — it is the guard
/// that keeps a future caller from composing a URL out of unvalidated text.
fn urlencode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ForgeAsset, decode_minisig_file, select_pairs, tag_candidates, urlencode_segment};
    use base64::Engine as _;

    fn asset(name: &str) -> ForgeAsset {
        ForgeAsset {
            name: name.to_owned(),
            url: format!("https://api.example.com/assets/{name}"),
            size_bytes: 1024,
        }
    }

    /// A real `.minisig` file's four lines, with a signature of the right length.
    fn minisig_file(signature: &[u8]) -> Vec<u8> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(signature);
        format!(
            "untrusted comment: signature from minisign secret key\n{encoded}\ntrusted comment: timestamp:1777000000\tfile:pos-edge\nZ2xvYmFs\n"
        )
        .into_bytes()
    }

    #[test]
    fn a_bare_version_is_tried_with_the_v_first_and_then_without() {
        assert_eq!(
            tag_candidates("1.2.3"),
            vec!["v1.2.3".to_owned(), "1.2.3".to_owned()],
            "the workflow tags `v1.2.3` and stamps `1.2.3` into the binary, so the prefixed spelling \
             is the likely one and the bare one is the fallback"
        );
    }

    #[test]
    fn a_version_already_carrying_the_v_is_tried_once() {
        assert_eq!(
            tag_candidates("v1.2.3"),
            vec!["v1.2.3".to_owned()],
            "asking twice for the same tag would double every refusal's wait for nothing"
        );
    }

    #[test]
    fn the_three_shipped_targets_are_selected_across_both_extensions() {
        let assets = [
            asset("pos-edge-v1.2.3-x86_64-unknown-linux-gnu.bin"),
            asset("pos-edge-v1.2.3-x86_64-unknown-linux-gnu.bin.minisig"),
            asset("pos-edge-v1.2.3-aarch64-unknown-linux-gnu.bin"),
            asset("pos-edge-v1.2.3-aarch64-unknown-linux-gnu.bin.minisig"),
            asset("pos-edge-v1.2.3-x86_64-pc-windows-msvc.exe"),
            asset("pos-edge-v1.2.3-x86_64-pc-windows-msvc.exe.minisig"),
        ];
        let pairs = select_pairs("v1.2.3", &assets);
        let targets: Vec<String> = pairs
            .iter()
            .map(|pair| pair.target.as_str().to_owned())
            .collect();
        assert_eq!(
            targets,
            vec![
                "aarch64-unknown-linux-gnu".to_owned(),
                "x86_64-pc-windows-msvc".to_owned(),
                "x86_64-unknown-linux-gnu".to_owned(),
            ],
            "Windows publishes its bare executable as `.exe` and the Linux targets as `.bin`; a \
             composed file name would have had to know that, which is why the asset list is read"
        );
    }

    #[test]
    fn the_archives_the_checksums_and_their_signatures_are_not_pairs() {
        let assets = [
            asset("pos-edge-v1.2.3-x86_64-unknown-linux-gnu.tar.gz"),
            asset("pos-edge-v1.2.3-x86_64-unknown-linux-gnu.tar.gz.minisig"),
            asset("pos-edge-v1.2.3-x86_64-pc-windows-msvc.zip"),
            asset("pos-edge-v1.2.3-x86_64-pc-windows-msvc.zip.minisig"),
            asset("SHA256SUMS"),
            asset("SHA256SUMS.minisig"),
        ];
        assert!(
            select_pairs("v1.2.3", &assets).is_empty(),
            "the tarball has its own signature and its own consumer — a human, and the installer — \
             and is emphatically not what OTA installs (ADR-0088 Amendment 2)"
        );
    }

    #[test]
    fn a_binary_with_no_signature_beside_it_is_not_a_pair() {
        let assets = [asset("pos-edge-v1.2.3-x86_64-unknown-linux-gnu.bin")];
        assert!(
            select_pairs("v1.2.3", &assets).is_empty(),
            "hosting bytes the edge has nothing to judge by produces a store that fetches, fails \
             verification and stays put — the failure hardest to read from a shop floor"
        );
    }

    #[test]
    fn an_asset_from_another_release_is_not_selected() {
        let assets = [
            asset("pos-edge-v1.2.2-x86_64-unknown-linux-gnu.bin"),
            asset("pos-edge-v1.2.2-x86_64-unknown-linux-gnu.bin.minisig"),
        ];
        assert!(
            select_pairs("v1.2.3", &assets).is_empty(),
            "the tag is part of the name, so a release page carrying a stray asset cannot host it \
             under the wrong version"
        );
    }

    #[test]
    fn a_target_that_is_not_a_triple_is_skipped_rather_than_guessed_at() {
        let assets = [
            asset("pos-edge-v1.2.3-Windows Server 2022.bin"),
            asset("pos-edge-v1.2.3-Windows Server 2022.bin.minisig"),
        ];
        assert!(
            select_pairs("v1.2.3", &assets).is_empty(),
            "a triple is validated before it becomes part of an object-store key"
        );
    }

    #[test]
    fn the_second_line_of_a_minisig_file_is_the_signature() {
        let signature = vec![7_u8; 74];
        assert_eq!(
            decode_minisig_file(&minisig_file(&signature)).expect("a well-formed file"),
            signature,
            "line one is the untrusted comment and lines three and four are the trusted comment and \
             its global signature; the tree carries the second line, raw"
        );
    }

    #[test]
    fn a_signature_of_the_wrong_length_is_refused_with_its_length() {
        let file = minisig_file(&[7_u8; 12]);
        let refusal = decode_minisig_file(&file).expect_err("twelve bytes is not a signature");
        assert!(
            refusal.contains("12") && refusal.contains("74"),
            "a shape check earns its keep by saying what it found and what it wanted: {refusal}"
        );
    }

    #[test]
    fn a_file_with_no_second_line_is_refused() {
        let refusal = decode_minisig_file(b"untrusted comment: nothing follows\n")
            .expect_err("a comment alone is not a signature");
        assert!(
            refusal.contains("second"),
            "the refusal names where the signature should have been: {refusal}"
        );
    }

    #[test]
    fn a_tag_becomes_one_path_segment() {
        assert_eq!(urlencode_segment("v1.2.3-rc.4"), "v1.2.3-rc.4");
        assert_eq!(
            urlencode_segment("a/b"),
            "a%2Fb",
            "a separator is encoded rather than traversing the API's path"
        );
    }
}
