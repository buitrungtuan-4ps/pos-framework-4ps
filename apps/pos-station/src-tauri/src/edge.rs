// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The five edge routes the app calls, and nothing else.
//!
//! | Route | Gate | Used for |
//! |---|---|---|
//! | `GET /healthz` | none | Station-mode detection, "is the edge up", its version |
//! | `POST /api/pair` | none | redeeming the six-digit code for a device token |
//! | `GET /api/pair/devices` | paired device | "is this token still accepted" — touches no sign-in |
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

    /// `GET /api/pair/devices`: `true` if the token is accepted, `false` if refused.
    ///
    /// Behind the paired-device gate only, so it answers without anyone signed in and resets nobody's
    /// idle window. The body — every paired device's id — is not read.
    pub(crate) fn still_paired(&self, origin: &EdgeOrigin, token: &str) -> Result<bool, EdgeError> {
        let response = self
            .agent
            .get(&origin.url("/api/pair/devices"))
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .map_err(|error| no_answer(&error))?;
        match response.status().as_u16() {
            200 => Ok(true),
            401 => Ok(false),
            status => Err(EdgeError::Unexpected(format!(
                "/api/pair/devices answered {status}"
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

fn no_answer(error: &ureq::Error) -> EdgeError {
    EdgeError::Unreachable(error.to_string())
}

fn bad_answer(error: &ureq::Error) -> EdgeError {
    EdgeError::Unexpected(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::plausible_token;

    #[test]
    fn a_token_is_printable_ascii_without_spaces() {
        assert!(plausible_token("0123456789abcdef0123456789abcdef"));
        assert!(!plausible_token(""));
        assert!(!plausible_token("abc def"));
        assert!(!plausible_token("abc\"\n"));
        assert!(!plausible_token(&"a".repeat(257)));
    }
}
