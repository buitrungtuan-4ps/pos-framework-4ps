// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The box's half of claiming
//! ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)):
//! open a claim, show its code, and poll until someone at the console binds it.
//!
//! Both calls are unauthenticated, like the activation exchange: a box being claimed has no
//! credential yet, which is the point. The secret the cloud hands back at opening is the only thing
//! that can collect the credential, so it stays inside [`OpenedClaim`], out of `Debug`, and is sent
//! back only to the cloud that issued it.

use core::fmt;

use pos_ports::{PortError, PortName, Secret};
use pos_proto::ids::{DeviceId, StoreId, TenantId};

use crate::wire::{HttpResponse, HttpTransport};

/// Errors raised here are about reaching the cloud, which is the `CloudSync` channel's business.
const PORT: PortName = PortName::CloudSync;

/// The route a claim is opened on.
const OPEN_PATH: &str = "/claim";

/// The route a claim is collected on.
fn collect_path(claim_id: &str) -> String {
    format!("/claim/{claim_id}/collect")
}

/// A claim the cloud opened for this box.
#[derive(Clone, serde::Deserialize)]
pub struct OpenedClaim {
    /// The claim's id, which names it when collecting.
    pub claim_id: String,
    /// The code to show, `XXXX-XXXX`.
    pub user_code: String,
    /// How long the claim lasts from now, in seconds.
    pub expires_in_secs: u64,
    /// How often to ask whether it has been bound, in seconds.
    pub poll_interval_secs: u64,
    secret: String,
}

impl fmt::Debug for OpenedClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedClaim")
            .field("claim_id", &self.claim_id)
            .field("user_code", &self.user_code)
            .field("secret", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// What one collection attempt came to.
#[derive(Debug)]
pub enum Collection {
    /// Bound and collected: which store and device this box now is, and its device credential.
    Collected {
        /// The tenant the box belongs to.
        tenant_id: TenantId,
        /// The store it was claimed for.
        store_id: StoreId,
        /// The device slot it fills.
        device_id: DeviceId,
        /// The `posdev_…` credential, for the key vault. Shown once by the cloud.
        credential: Secret,
    },
    /// Not bound yet: ask again after the poll interval.
    Pending,
    /// The claim's hour is over: open a new one.
    Expired,
    /// The cloud will not hand this claim over (it was collected, or is unknown): open a new one.
    Refused,
}

/// The collected answer, on the wire.
#[derive(serde::Deserialize)]
struct CollectedWire {
    tenant_id: String,
    store_id: String,
    device_id: String,
    credential: String,
}

/// The box's claim client over an [`HttpTransport`] to its cloud.
#[derive(Debug, Clone)]
pub struct HttpClaim<T> {
    transport: T,
}

impl<T: HttpTransport> HttpClaim<T> {
    /// A client over `transport`, which is pointed at the cloud the box will belong to.
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Opens a claim.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the cloud cannot be reached or answers anything but `201` with
    /// a claim, [`PortError::resource_exhausted`] if it is rate-limiting this box.
    pub async fn open(&self) -> Result<OpenedClaim, PortError> {
        let response = self
            .transport
            .post_json(OPEN_PATH, b"{}".to_vec())
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        match response.status {
            201 => serde_json::from_slice(&response.body).map_err(|error| {
                PortError::unavailable(PORT, format!("the claim answer did not parse: {error}"))
            }),
            429 => Err(PortError::resource_exhausted(
                PORT,
                "the cloud is limiting claims from this address; try again later",
            )),
            status => Err(PortError::unavailable(
                PORT,
                format!("the cloud could not open a claim ({status})"),
            )),
        }
    }

    /// Asks whether `claim` has been bound, and collects the credential if it has.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the cloud cannot be reached or answers something this client
    /// cannot read, [`PortError::resource_exhausted`] if it is rate-limiting this box. Either way the
    /// claim may still be collected by asking again.
    pub async fn collect(&self, claim: &OpenedClaim) -> Result<Collection, PortError> {
        let body = serde_json::to_vec(&serde_json::json!({ "secret": claim.secret }))
            .map_err(|error| PortError::internal(PORT, error.to_string()))?;
        let response = self
            .transport
            .post_json(&collect_path(&claim.claim_id), body)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        match response.status {
            200 => collected(&response),
            202 => Ok(Collection::Pending),
            409 => Ok(Collection::Expired),
            403 | 404 => Ok(Collection::Refused),
            429 => Err(PortError::resource_exhausted(
                PORT,
                "the cloud is limiting claim polls from this address",
            )),
            status => Err(PortError::unavailable(
                PORT,
                format!("the cloud could not answer the claim ({status})"),
            )),
        }
    }
}

/// Reads a `200` collection into its parts. A malformed id is the cloud's fault and is reported as
/// such; the credential in it is then lost, and the box opens a new claim.
fn collected(response: &HttpResponse) -> Result<Collection, PortError> {
    let wire: CollectedWire = serde_json::from_slice(&response.body).map_err(|error| {
        PortError::unavailable(PORT, format!("the collected claim did not parse: {error}"))
    })?;
    let unreadable = |what: &str| PortError::unavailable(PORT, format!("the {what} is not a ULID"));
    Ok(Collection::Collected {
        tenant_id: wire
            .tenant_id
            .parse()
            .map_err(|_| unreadable("tenant id"))?,
        store_id: wire.store_id.parse().map_err(|_| unreadable("store id"))?,
        device_id: wire
            .device_id
            .parse()
            .map_err(|_| unreadable("device id"))?,
        credential: Secret::new(wire.credential.into_bytes()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::needless_pass_by_value,
        reason = "test scaffolding: a stub cloud's own JSON failing to encode is a setup fault, and \
                  the transport trait takes its body by value"
    )]

    use std::sync::Mutex;

    use super::{Collection, HttpClaim, OpenedClaim};
    use crate::wire::{HttpResponse, HttpTransport, TransportError};

    /// A cloud that answers with a queue of `(status, body)` and records what it was sent.
    struct Scripted {
        answers: Mutex<Vec<(u16, serde_json::Value)>>,
        sent: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl Scripted {
        fn new(answers: Vec<(u16, serde_json::Value)>) -> Self {
            Self {
                answers: Mutex::new(answers),
                sent: Mutex::new(Vec::new()),
            }
        }
    }

    impl HttpTransport for Scripted {
        async fn post_json(
            &self,
            path: &str,
            body: Vec<u8>,
        ) -> Result<HttpResponse, TransportError> {
            let request: serde_json::Value = serde_json::from_slice(&body).expect("a JSON request");
            self.sent
                .lock()
                .expect("lock")
                .push((path.to_owned(), request));
            let (status, body) = self.answers.lock().expect("lock").remove(0);
            Ok(HttpResponse {
                status,
                body: serde_json::to_vec(&body).expect("encode"),
                ..HttpResponse::default()
            })
        }

        async fn post_bytes(
            &self,
            _path: &str,
            _content_type: &str,
            _body: Vec<u8>,
        ) -> Result<HttpResponse, TransportError> {
            Err(TransportError::new("not used"))
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    fn opened() -> serde_json::Value {
        serde_json::json!({
            "claim_id": "01J9ZQ3M6V4Q1ZB2Y7H8K5N0PX",
            "user_code": "AB10-CD0Z",
            "secret": "ab".repeat(32),
            "expires_in_secs": 3600,
            "poll_interval_secs": 5,
        })
    }

    /// A claim opens, waits, and is collected with the secret the cloud gave, which never shows in
    /// `Debug`.
    #[test]
    fn a_claim_opens_waits_and_is_collected_with_its_own_secret() {
        let cloud = Scripted::new(vec![
            (201, opened()),
            (202, serde_json::json!({ "status": "PENDING" })),
            (
                200,
                serde_json::json!({
                    "tenant_id": "01J9ZQ3M6V4Q1ZB2Y7H8K5N0P1",
                    "store_id": "01J9ZQ3M6V4Q1ZB2Y7H8K5N0P2",
                    "device_id": "01J9ZQ3M6V4Q1ZB2Y7H8K5N0P3",
                    "credential": "posdev_id_secret",
                }),
            ),
        ]);
        let client = HttpClaim::new(cloud);
        let claim: OpenedClaim = block_on(client.open()).expect("opens");
        assert_eq!(claim.user_code, "AB10-CD0Z");
        assert!(!format!("{claim:?}").contains(&"ab".repeat(32)));

        assert!(matches!(
            block_on(client.collect(&claim)).expect("asks"),
            Collection::Pending
        ));
        let collected = block_on(client.collect(&claim)).expect("collects");
        assert!(
            matches!(
                &collected,
                Collection::Collected { store_id, credential, .. }
                    if store_id.to_string() == "01J9ZQ3M6V4Q1ZB2Y7H8K5N0P2"
                        && credential.expose() == b"posdev_id_secret"
            ),
            "the third answer is the credential: {collected:?}"
        );

        let sent = client.transport.sent.lock().expect("lock");
        let paths: Vec<&str> = sent.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/claim",
                "/claim/01J9ZQ3M6V4Q1ZB2Y7H8K5N0PX/collect",
                "/claim/01J9ZQ3M6V4Q1ZB2Y7H8K5N0PX/collect"
            ]
        );
        assert!(
            sent.iter()
                .skip(1)
                .all(|(_, body)| body["secret"] == "ab".repeat(32)),
            "every poll presents the secret the cloud gave"
        );
    }

    /// An expired claim and a refused one each tell the box to open a new claim; a limited box is
    /// told to wait.
    #[test]
    fn the_cloud_s_refusals_read_as_what_to_do_next() {
        let cloud = Scripted::new(vec![
            (201, opened()),
            (409, serde_json::json!({})),
            (403, serde_json::json!({})),
            (429, serde_json::json!({})),
        ]);
        let client = HttpClaim::new(cloud);
        let claim = block_on(client.open()).expect("opens");
        assert!(matches!(
            block_on(client.collect(&claim)).expect("asks"),
            Collection::Expired
        ));
        assert!(matches!(
            block_on(client.collect(&claim)).expect("asks"),
            Collection::Refused
        ));
        let limited = block_on(client.collect(&claim)).expect_err("limited");
        assert!(limited.is_retryable());
    }
}
