// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`MessageLink`] adapter over an [`HttpTransport`]: a store publishes its events to the cloud's
//! `/sync` surface
//! ([ADR-0143](../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)).
//!
//! The second way events leave a store, beside `link-nats`. It needs no broker port open to the
//! internet and no fleet-wide token: the transport carries the box's own credential, and the cloud
//! refuses a batch naming any store but that one.
//!
//! # What an acknowledgement means here
//!
//! The cloud ingests a batch in one transaction before it answers, so `200 { accepted: n }` means the
//! events are in the cloud's log, not merely queued. An answer that never arrives is ambiguous, and
//! is reported as a retryable failure: the outbox holds, the retry is safe because ingest is
//! idempotent by `event_id`, and nothing is ever reported accepted that the cloud did not say it
//! holds.
//!
//! # Capacity
//!
//! There is no stream whose depth the store could read. What it can observe is the cloud refusing a
//! batch for back-pressure (`429`), so the link reports itself full from such a refusal until the
//! next batch is accepted. That is the whole of what [`MessageLink::capacity`] can honestly say over
//! this rail, and it is enough for the 80% alert to fire while the cloud is pushing back.

use core::num::NonZeroU32;
use core::sync::atomic::{AtomicBool, Ordering};

use pos_ports::message_link::{LinkCapacity, MessageLink, PublishOutcome};
use pos_ports::{PortError, PortName};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::StoreId;
use pos_proto::protocol::{Hello, HelloOutcome};

use crate::wire::{HttpResponse, HttpTransport};

/// The port this adapter serves, for every [`PortError`] it raises.
const PORT: PortName = PortName::MessageLink;

/// The most envelopes one publish carries: the cloud's own limit (`pos_cloud::http::MAX_PUBLISHED_EVENTS`).
pub const MAX_BATCH: NonZeroU32 = match NonZeroU32::new(256) {
    Some(limit) => limit,
    None => NonZeroU32::MIN,
};

/// The largest body one publish sends: the cloud reads at most 4 MiB
/// (`pos_cloud::http::MAX_PUBLISHED_EVENTS_BYTES`). A batch that would be larger is cut to the prefix
/// that fits, and the rest goes next time.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// What the body opens with, before the first envelope.
const OPEN: &[u8] = br#"{"events":["#;

/// What the body closes with, after the last.
const CLOSE: &[u8] = b"]}";

/// The route a batch is published to.
fn events_path(store_id: StoreId) -> String {
    format!("/sync/stores/{store_id}/events")
}

/// The route the protocol is negotiated on.
fn hello_path(store_id: StoreId) -> String {
    format!("/sync/stores/{store_id}/events/hello")
}

/// The cloud's answer to a publish.
#[derive(serde::Deserialize)]
struct Accepted {
    accepted: u32,
}

/// Events published to the cloud over HTTPS, as the store the transport's credential belongs to.
#[derive(Debug)]
pub struct HttpLink<T> {
    transport: T,
    store_id: StoreId,
    /// Whether a handshake has been accepted: the contract refuses a publish before one.
    handshook: AtomicBool,
    /// Whether the cloud's last word on a publish was back-pressure. See the module docs.
    pushed_back: AtomicBool,
}

impl<T: HttpTransport> HttpLink<T> {
    /// A link for `store_id` over `transport`, which must carry that store's credential.
    #[must_use]
    pub const fn new(transport: T, store_id: StoreId) -> Self {
        Self {
            transport,
            store_id,
            handshook: AtomicBool::new(false),
            pushed_back: AtomicBool::new(false),
        }
    }
}

impl<T: HttpTransport> MessageLink for HttpLink<T> {
    async fn handshake(&self, hello: &Hello) -> Result<HelloOutcome, PortError> {
        let body = serde_json::to_vec(hello).map_err(|error| {
            PortError::internal(PORT, format!("the hello could not be encoded: {error}"))
        })?;
        let response = self
            .transport
            .post_json(&hello_path(self.store_id), body)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        if response.status != 200 {
            return Err(refusal(&response, "the hello"));
        }
        let outcome: HelloOutcome = serde_json::from_slice(&response.body).map_err(|error| {
            PortError::unavailable(
                PORT,
                format!("the cloud's hello answer did not parse: {error}"),
            )
        })?;
        if matches!(outcome, HelloOutcome::Accepted { .. }) {
            self.handshook.store(true, Ordering::SeqCst);
        }
        Ok(outcome)
    }

    async fn publish(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> Result<PublishOutcome, PortError> {
        if !self.handshook.load(Ordering::SeqCst) {
            return Err(PortError::failed_precondition(
                PORT,
                "no handshake has succeeded on this link",
            ));
        }
        if events.is_empty() {
            return Ok(PublishOutcome::all(0));
        }
        let (sent, body) = framed_prefix(events)?;
        let response = self
            .transport
            .post_json(&events_path(self.store_id), body)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        if response.status == 429 {
            self.pushed_back.store(true, Ordering::SeqCst);
        }
        if response.status != 200 {
            return Err(refusal(&response, "the batch"));
        }
        self.pushed_back.store(false, Ordering::SeqCst);
        let answer: Accepted = serde_json::from_slice(&response.body).map_err(|error| {
            PortError::unavailable(
                PORT,
                format!("the cloud's publish answer did not parse: {error}"),
            )
        })?;
        // Never more than was sent: acknowledging an event the cloud was never given would lose it.
        Ok(PublishOutcome {
            accepted: answer.accepted.min(sent),
        })
    }

    async fn capacity(&self) -> Result<LinkCapacity, PortError> {
        Ok(LinkCapacity {
            messages: u64::from(self.pushed_back.load(Ordering::SeqCst)),
            message_limit: Some(1),
            bytes: 0,
            byte_limit: None,
        })
    }

    fn max_batch_size(&self) -> NonZeroU32 {
        MAX_BATCH
    }
}

/// The longest prefix of `events` that fits one publish, as the JSON body `{"events":[…]}` and how
/// many envelopes it carries.
///
/// # Errors
///
/// [`PortError::internal`] if an envelope will not encode, and [`PortError::invalid_argument`] if the
/// first envelope alone is larger than the cloud reads: sending it could never succeed, and saying so
/// is better than a retry loop that cannot end.
fn framed_prefix(events: &[EventEnvelope<RawPayload>]) -> Result<(u32, Vec<u8>), PortError> {
    let mut body = OPEN.to_vec();
    let mut sent: u32 = 0;
    for event in events {
        if sent >= MAX_BATCH.get() {
            break;
        }
        let encoded = serde_json::to_vec(event).map_err(|error| {
            PortError::internal(PORT, format!("an event could not be encoded: {error}"))
        })?;
        let separator = usize::from(sent > 0);
        if body.len() + separator + encoded.len() + CLOSE.len() > MAX_BODY_BYTES {
            if sent == 0 {
                return Err(PortError::invalid_argument(
                    PORT,
                    "one event is larger than the cloud accepts in a publish",
                ));
            }
            break;
        }
        if sent > 0 {
            body.push(b',');
        }
        body.extend_from_slice(&encoded);
        sent += 1;
    }
    body.extend_from_slice(CLOSE);
    Ok((sent, body))
}

/// The [`PortError`] for a status that is not success, in the terms the publisher retries on.
fn refusal(response: &HttpResponse, what: &str) -> PortError {
    match response.status {
        400 | 413 => PortError::invalid_argument(
            PORT,
            format!(
                "the cloud refused {what} as malformed ({})",
                response.status
            ),
        ),
        401 | 403 => PortError::permission_denied(
            PORT,
            format!(
                "the cloud refused this box's credential for {what} ({}); an archived device or a \
                 key without publish_events",
                response.status
            ),
        ),
        429 => PortError::resource_exhausted(PORT, format!("the cloud pushed back on {what}")),
        status => {
            PortError::unavailable(PORT, format!("the cloud could not take {what} ({status})"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CLOSE, MAX_BODY_BYTES, OPEN, framed_prefix};

    /// An empty slice frames as an empty batch, which is exactly the open and the close.
    #[test]
    fn no_events_frame_as_an_empty_batch() {
        let (sent, body) = framed_prefix(&[]).expect("frames");
        assert_eq!(sent, 0);
        assert_eq!(body, br#"{"events":[]}"#);
        assert_eq!(OPEN.len() + CLOSE.len(), body.len());
        assert!(MAX_BODY_BYTES > body.len());
    }
}
