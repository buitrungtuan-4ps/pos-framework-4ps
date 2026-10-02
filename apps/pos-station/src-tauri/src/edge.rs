// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The six edge routes the app calls, and nothing else.
//!
//! | Route | Gate | Used for |
//! |---|---|---|
//! | `GET /healthz` | none | Station-mode detection, "is the edge up", its version |
//! | `POST /api/pair` | none | redeeming the six-digit code for a device token |
//! | `GET /api/pair/this_device` | paired device | "is this token still accepted" — touches no sign-in |
//! | `GET /api/pair/devices` | paired device | the same question, only of an edge older than `this_device` |
//! | `GET /api/sync` | paired + signed in | cloud link and outbox, only while a till is open |
//! | `GET /api/printers` | paired + signed in | the published printers, only while a till is open |
//!
//! Synchronous on purpose: every call runs on one of the app's own threads (the monitor, the start-up
//! thread) or inside `spawn_blocking`, never on an async task.
//!
//! **No proxy**, whatever the environment says. The edge is on the shop's LAN or at a hosted origin
//! the till itself reaches directly; a corporate proxy variable on a store PC would otherwise send a
//! LAN request out to the internet.

use std::time::Duration;

use serde::Deserialize;

use crate::address::{EdgeOrigin, PairingCode};

/// How long any one call may take. A LAN edge answers in milliseconds; a hosted one in well under a
/// second. The pairing POST is the only call an operator waits on.
const TIMEOUT: Duration = Duration::from_secs(8);

/// How long connecting may take — short, because "nothing on 127.0.0.1:8787" decides the mode.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// The token probe: the edge answers for the calling token alone.
const THIS_DEVICE: &str = "/api/pair/this_device";

/// The device list, which answered the probe's question before the probe existed.
const DEVICES: &str = "/api/pair/devices";

/// A call that did not produce an answer this build can use.
#[derive(Debug)]
pub(crate) enum EdgeError {
    /// No answer: refused, timed out, or the connection broke.
    Unreachable(String),
    /// An answer with a status or a body this build does not expect.
    Unexpected(String),
}

impl core::fmt::Display for EdgeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreachable(detail) => write!(f, "the edge did not answer: {detail}"),
            Self::Unexpected(detail) => write!(f, "the edge answered unexpectedly: {detail}"),
        }
    }
}

/// Why a pairing code was not redeemed.
#[derive(Debug)]
pub(crate) enum PairRefusal {
    /// `400`: the edge did not accept the code's shape.
    BadCode,
    /// `403`: unknown or expired — the edge deliberately does not say which.
    Rejected,
    /// `429`: too many wrong codes; retry after this many seconds, if the edge said.
    TooManyAttempts(Option<u64>),
    /// `503`: the edge could not issue or record a token.
    Unavailable,
    /// No answer, or one this build cannot read.
    Failed(EdgeError),
}

/// A signed-in read's outcome.
#[derive(Debug)]
pub(crate) enum Gated<T> {
    /// `200`, with the body.
    Read(T),
    /// `403`: the device is paired and nobody is signed in on it (or it went idle).
    SignInNeeded,
    /// `401`: the token is not accepted.
    NotPaired,
}

/// `GET /healthz`, the fields the app reads.
#[derive(Debug, Deserialize)]
pub(crate) struct Health {
    /// The edge binary's version.
    #[serde(default)]
    pub(crate) version: String,
}

/// `GET /api/sync`.
#[derive(Debug, Deserialize)]
pub(crate) struct SyncBody {
    /// Events waiting for the cloud.
    pub(crate) outbox_depth: u64,
    /// The depth the level is graded against.
    pub(crate) outbox_planned_depth: u64,
    /// `OUTBOX_LEVEL_*`.
    pub(crate) outbox_level: String,
    /// `CLOUD_LINK_*`.
    pub(crate) cloud_link: String,
}

/// One entry of `GET /api/printers`.
#[derive(Debug, Deserialize)]
pub(crate) struct PrinterBody {
    /// The printer's device id.
    pub(crate) device_id: String,
    /// Its published name.
    pub(crate) name: String,
}

#[derive(Deserialize)]
struct PairAccepted {
    device_token: String,
}

/// `GET /api/pair/this_device`, the one field the app reads. The edge also names the device and when
/// it paired; the app needs neither.
#[derive(Deserialize)]
struct ThisDevice {
    accepted: bool,
}

/// What `GET /api/pair/this_device` settled.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// The edge has the route, and said whether the token is accepted.
    Answered(bool),
    /// The edge is older than the route, so the device list has to answer.
    NoSuchRoute,
}

/// A client for one or more edges.
#[derive(Debug, Clone)]
pub(crate) struct EdgeClient {
    agent: ureq::Agent,
}

impl EdgeClient {
    /// A client with the app's timeouts, no proxy, and statuses returned rather than raised.
    pub(crate) fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .http_status_as_error(false)
            .proxy(None)
            .user_agent(concat!("pos-station/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// `GET /healthz`.
    pub(crate) fn health(&self, origin: &EdgeOrigin) -> Result<Health, EdgeError> {
        let mut response = self
            .agent
            .get(&origin.url("/healthz"))
            .call()
            .map_err(|error| no_answer(&error))?;
        match response.status().as_u16() {
            200 => response
                .body_mut()
                .read_json::<Health>()
                .map_err(|error| bad_answer(&error)),
            status => Err(EdgeError::Unexpected(format!("/healthz answered {status}"))),
        }
    }

    /// `POST /api/pair`, returning the device token.
    pub(crate) fn pair(
        &self,
        origin: &EdgeOrigin,
        code: &PairingCode,
    ) -> Result<String, PairRefusal> {
        let mut response = self
            .agent
            .post(&origin.url("/api/pair"))
            .send_json(serde_json::json!({ "code": code.as_str() }))
            .map_err(|error| PairRefusal::Failed(no_answer(&error)))?;
        match response.status().as_u16() {
            200 => {
                let accepted = response
                    .body_mut()
                    .read_json::<PairAccepted>()
                    .map_err(|error| PairRefusal::Failed(bad_answer(&error)))?;
                if plausible_token(&accepted.device_token) {
                    Ok(accepted.device_token)
                } else {
                    Err(PairRefusal::Failed(EdgeError::Unexpected(
                        "the device token is not a token".to_owned(),
                    )))
                }
            }
            400 => Err(PairRefusal::BadCode),
            403 => Err(PairRefusal::Rejected),
            429 => Err(PairRefusal::TooManyAttempts(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok()),
            )),
            503 => Err(PairRefusal::Unavailable),
            status => Err(PairRefusal::Failed(EdgeError::Unexpected(format!(
                "/api/pair answered {status}"
            )))),
        }
    }

    /// `GET /api/pair/this_device`: `true` if the token is accepted, `false` if refused.
    ///
    /// Behind the paired-device gate only, so it answers without anyone signed in and resets nobody's
    /// idle window, and it answers for this token alone. An edge older than the route cannot answer
    /// it, and then the device list is asked instead ([`Self::listed`]), as before the route existed.
    /// That costs an edge not yet updated a second request each round, and nothing once it is.
    pub(crate) fn still_paired(&self, origin: &EdgeOrigin, token: &str) -> Result<bool, EdgeError> {
        let mut response = self
            .agent
            .get(&origin.url(THIS_DEVICE))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .map_err(|error| no_answer(&error))?;
        let status = response.status().as_u16();
        // Only a `200` has a body worth reading. One that cannot be read is no answer either, and
        // the list decides.
        let body = if status == 200 {
            response.body_mut().read_to_string().unwrap_or_default()
        } else {
            String::new()
        };
        match probe_answer(status, &body)? {
            Probe::Answered(accepted) => Ok(accepted),
            Probe::NoSuchRoute => self.listed(origin, token),
        }
    }

    /// `GET /api/pair/devices`, for an edge older than `GET /api/pair/this_device`: `true` if the
    /// token is accepted, `false` if refused.
    ///
    /// Behind the paired-device gate only, as the probe is. The body — every paired device's id — is
    /// not read.
    fn listed(&self, origin: &EdgeOrigin, token: &str) -> Result<bool, EdgeError> {
        let response = self
            .agent
            .get(&origin.url(DEVICES))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .map_err(|error| no_answer(&error))?;
        match response.status().as_u16() {
            200 => Ok(true),
            401 => Ok(false),
            status => Err(EdgeError::Unexpected(format!(
                "{DEVICES} answered {status}"
            ))),
        }
    }

    /// `GET /api/sync`.
    pub(crate) fn sync(
        &self,
        origin: &EdgeOrigin,
        token: &str,
    ) -> Result<Gated<SyncBody>, EdgeError> {
        self.gated(origin, "/api/sync", token)
    }

    /// `GET /api/printers`.
    pub(crate) fn printers(
        &self,
        origin: &EdgeOrigin,
        token: &str,
    ) -> Result<Gated<Vec<PrinterBody>>, EdgeError> {
        self.gated(origin, "/api/printers", token)
    }

    fn gated<T: serde::de::DeserializeOwned>(
        &self,
        origin: &EdgeOrigin,
        path: &str,
        token: &str,
    ) -> Result<Gated<T>, EdgeError> {
        let mut response = self
            .agent
            .get(&origin.url(path))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .map_err(|error| no_answer(&error))?;
        match response.status().as_u16() {
            200 => response
                .body_mut()
                .read_json::<T>()
                .map(Gated::Read)
                .map_err(|error| bad_answer(&error)),
            401 => Ok(Gated::NotPaired),
            403 => Ok(Gated::SignInNeeded),
            status => Err(EdgeError::Unexpected(format!("{path} answered {status}"))),
        }
    }
}

/// Whether an edge's token is sane enough to store and embed: printable ASCII, no spaces, bounded.
///
/// Deliberately looser than the edge's own check (32 lowercase hex characters): the app should not
/// break the day the edge lengthens its tokens. The init script escapes it either way.
pub(crate) fn plausible_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 256 && token.bytes().all(|b| b.is_ascii_graphic())
}

/// Reads `GET /api/pair/this_device`'s answer from its status and, for a `200`, its body.
///
/// An edge older than the route seldom answers `404`. For a path it does not know it serves the
/// till's `index.html` with a `200`, and only an edge built without the till answers `404`. So a
/// `200` is an answer only when its body is the probe's own JSON. Read as "accepted", that
/// `index.html` would tell a Station whose token was retired that it is still paired, for as long as
/// its edge is not updated.
fn probe_answer(status: u16, body: &str) -> Result<Probe, EdgeError> {
    match status {
        200 => match serde_json::from_str::<ThisDevice>(body) {
            Ok(answer) => Ok(Probe::Answered(answer.accepted)),
            Err(_) => Ok(Probe::NoSuchRoute),
        },
        404 => Ok(Probe::NoSuchRoute),
        401 => Ok(Probe::Answered(false)),
        status => Err(EdgeError::Unexpected(format!(
            "{THIS_DEVICE} answered {status}"
        ))),
    }
}

fn no_answer(error: &ureq::Error) -> EdgeError {
    EdgeError::Unreachable(error.to_string())
}

fn bad_answer(error: &ureq::Error) -> EdgeError {
    EdgeError::Unexpected(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{Probe, plausible_token, probe_answer};

    #[test]
    fn a_token_is_printable_ascii_without_spaces() {
        assert!(plausible_token("0123456789abcdef0123456789abcdef"));
        assert!(!plausible_token(""));
        assert!(!plausible_token("abc def"));
        assert!(!plausible_token("abc\"\n"));
        assert!(!plausible_token(&"a".repeat(257)));
    }

    #[test]
    fn the_probe_is_accepted_refused_or_absent() {
        let accepted = r#"{"accepted":true,"device_id":"01K6H0000000000000000000AA","pair_time":"2026-10-02T03:00:00Z"}"#;
        assert_eq!(
            probe_answer(200, accepted).ok(),
            Some(Probe::Answered(true))
        );
        let refused = "pair this device to reach the edge";
        assert_eq!(
            probe_answer(401, refused).ok(),
            Some(Probe::Answered(false))
        );
        // An edge older than the route: `404` built without the till, and the till's page with a
        // `200` built with it. Either way the list decides; that page never reads as "accepted".
        let no_ui = "no UI is embedded in this build";
        assert_eq!(probe_answer(404, no_ui).ok(), Some(Probe::NoSuchRoute));
        let index_html = "<!doctype html><html><body><div id=\"root\"></div></body></html>";
        assert_eq!(probe_answer(200, index_html).ok(), Some(Probe::NoSuchRoute));
        assert_eq!(probe_answer(200, "").ok(), Some(Probe::NoSuchRoute));
        // Anything else is unexpected, and the app keeps the token rather than guess.
        assert!(probe_answer(403, "sign in to act on this device").is_err());
        assert!(probe_answer(500, "").is_err());
    }
}
