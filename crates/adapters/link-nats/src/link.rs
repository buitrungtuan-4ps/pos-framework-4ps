// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`NatsLink`]: the JetStream context, the handshake, and the `MessageLink` implementation.

use core::num::NonZeroU32;
use core::sync::atomic::{AtomicBool, Ordering};

use async_nats::jetstream;
use async_nats::jetstream::stream::{Config as StreamConfig, DiscardPolicy};

use pos_ports::message_link::{LinkCapacity, MessageLink, PublishOutcome};
use pos_ports::{PortError, PortName};
use pos_proto::PROTOCOL_VERSION;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::protocol::{Hello, HelloOutcome, MIN_SUPPORTED_PROTOCOL_VERSION, negotiate};

/// The largest batch this link accepts in one `publish`.
const MAX_BATCH_SIZE: u32 = 256;

/// Which JetStream stream a link publishes to, and the limits the 80% alert watches.
#[derive(Debug, Clone)]
pub struct NatsConfig {
    /// The stream name (one per store, e.g. `POS_STORE_<id>`).
    pub stream: String,
    /// The subject every event is published to; the stream captures exactly this subject.
    pub subject: String,
    /// The stream's message cap, or `-1` for unlimited.
    pub max_messages: i64,
    /// The stream's byte cap, or `-1` for unlimited.
    pub max_bytes: i64,
}

/// A [`MessageLink`] over NATS JetStream.
///
/// Holds the JetStream context and the client (kept so a caller can observe or drain the
/// connection), plus whether a handshake has succeeded on this connection.
#[derive(Debug)]
pub struct NatsLink {
    jetstream: jetstream::Context,
    client: async_nats::Client,
    config: NatsConfig,
    handshook: AtomicBool,
}

impl NatsLink {
    /// Connects to NATS at `url` and prepares a link for `config`. No stream is created until the
    /// handshake.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if NATS cannot be reached.
    pub async fn connect(url: &str, config: NatsConfig) -> Result<Self, PortError> {
        let client = async_nats::connect(url).await.map_err(unavailable)?;
        Ok(Self::from_client(client, config))
    }

    /// Builds a link over an existing client — how a binary shares one connection across adapters,
    /// and how a test keeps a handle to sever it.
    #[must_use]
    pub fn from_client(client: async_nats::Client, config: NatsConfig) -> Self {
        Self {
            jetstream: jetstream::new(client.clone()),
            client,
            config,
            handshook: AtomicBool::new(false),
        }
    }

    /// The underlying client, for connection observation (and, in tests, draining).
    #[must_use]
    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    /// Ensures this store's stream exists, `discard: new` so a full stream refuses rather than
    /// silently dropping the oldest event.
    async fn ensure_stream(&self) -> Result<(), PortError> {
        self.jetstream
            .get_or_create_stream(StreamConfig {
                name: self.config.stream.clone(),
                subjects: vec![self.config.subject.clone()],
                max_messages: self.config.max_messages,
                max_bytes: self.config.max_bytes,
                discard: DiscardPolicy::New,
                ..Default::default()
            })
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// The stream's current fill against its limits.
    async fn stream_capacity(&self) -> Result<LinkCapacity, PortError> {
        let mut stream = self
            .jetstream
            .get_stream(&self.config.stream)
            .await
            .map_err(unavailable)?;
        let info = stream.info().await.map_err(unavailable)?;
        Ok(LinkCapacity {
            messages: info.state.messages,
            message_limit: positive_limit(info.config.max_messages),
            bytes: info.state.bytes,
            byte_limit: positive_limit(info.config.max_bytes),
        })
    }
}

impl MessageLink for NatsLink {
    async fn handshake(&self, hello: &Hello) -> Result<HelloOutcome, PortError> {
        // Reachability + stream existence, then the real negotiation from pos-proto — no cloud
        // responder, because the link is outbound only.
        self.ensure_stream().await?;
        let outcome = negotiate(hello, MIN_SUPPORTED_PROTOCOL_VERSION, PROTOCOL_VERSION);
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
                PortName::MessageLink,
                "no handshake has succeeded on this connection",
            ));
        }
        // A full stream is back-pressure, not loss: report resource_exhausted (retryable) so the
        // events stay in the outbox. Checked before publishing, so a full stream never accepts a
        // partial prefix it cannot hold. This call also fails if the connection is gone, which is
        // the retryable unavailable the "never at-most-once" obligation wants.
        if self.stream_capacity().await?.is_at_least(100) {
            return Err(PortError::resource_exhausted(
                PortName::MessageLink,
                "the stream is at capacity",
            ));
        }

        let limit = usize::try_from(MAX_BATCH_SIZE).unwrap_or(usize::MAX);
        let mut accepted: u32 = 0;
        for event in events.iter().take(limit) {
            let payload = serde_json::to_vec(event).map_err(encode)?;
            let published = self
                .jetstream
                .publish(self.config.subject.clone(), payload.into())
                .await;
            let acked = match published {
                Ok(ack) => ack.await.map(|_| ()),
                Err(error) => Err(error),
            };
            match acked {
                Ok(()) => accepted = accepted.saturating_add(1),
                // The first failure with nothing yet accepted is retryable back to the caller; a
                // failure after a prefix landed is reported as that prefix, so the outbox retries
                // only the tail.
                Err(error) => {
                    if accepted == 0 {
                        return Err(unavailable(error));
                    }
                    break;
                }
            }
        }
        Ok(PublishOutcome { accepted })
    }

    async fn capacity(&self) -> Result<LinkCapacity, PortError> {
        self.stream_capacity().await
    }

    fn max_batch_size(&self) -> NonZeroU32 {
        NonZeroU32::new(MAX_BATCH_SIZE).unwrap_or(NonZeroU32::MIN)
    }
}

/// Maps a JetStream `-1` (unlimited) to `None`, any positive cap to `Some`.
fn positive_limit(limit: i64) -> Option<u64> {
    if limit <= 0 {
        None
    } else {
        u64::try_from(limit).ok()
    }
}

/// Maps any NATS error to the port's unavailable status — the retryable classification the outbox
/// relies on.
fn unavailable<E: core::error::Error + Send + Sync + 'static>(error: E) -> PortError {
    PortError::unavailable(PortName::MessageLink, "the cloud link is unavailable")
        .with_source(error)
}

/// Maps an envelope serialisation failure to the port's internal status.
fn encode(error: serde_json::Error) -> PortError {
    PortError::internal(
        PortName::MessageLink,
        "could not serialise an event envelope",
    )
    .with_source(error)
}
