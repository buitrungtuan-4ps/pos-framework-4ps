// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`NatsConsumer`]: the cloud's durable cursor over a store's JetStream stream.
//!
//! This is the read counterpart of [`NatsLink`](crate::NatsLink). The edge publishes each event to
//! the stream; the cloud consumes them here and feeds them to idempotent ingest. Two properties make
//! that safe:
//!
//!  * **The cursor is durable.** A JetStream *durable pull consumer* tracks its own delivery position
//!    server-side, so a cloud restart resumes exactly where it left off — this is the "cursor over the
//!    event log" `docs/roadmap.md` P7 asks for, and what a later slice resets to replay the log.
//!  * **Nothing is acknowledged before it is stored.** [`NatsConsumer::pull`] hands the caller the
//!    decoded batch *and* the message handles; the caller acknowledges only after ingest has
//!    committed ([`ConsumedBatch::ack`]), and returns the batch for redelivery otherwise
//!    ([`ConsumedBatch::nak`]). Combined with ingest being idempotent by `event_id`
//!    ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §4), that is at-least-once with exactly-once
//!    effect: a redelivered batch is stored once and reported as duplicates.
//!
//! A message whose bytes are not a valid envelope can never be ingested, so it would wedge the cursor
//! forever. [`NatsConsumer::pull`] **terminates** such a message (removing it from redelivery) and
//! counts it in [`ConsumedBatch::poison`], rather than letting one corrupt frame halt the whole feed;
//! the nightly reconciliation (a later slice) re-pushes anything the cursor dropped. This is the only
//! place the consumer discards, and it is never silent — the caller logs the count.

use core::time::Duration;

use async_nats::jetstream::consumer::{AckPolicy, Consumer, pull};
use async_nats::jetstream::{self, AckKind, Message};
use futures_util::StreamExt as _;

use pos_ports::{PortError, PortName};
use pos_proto::envelope::{EventEnvelope, RawPayload};

/// How the cloud binds its durable cursor to a stream.
#[derive(Debug, Clone)]
pub struct ConsumerConfig {
    /// The stream to consume — the same stream the edge publishes to. In this tree that is the one
    /// fleet stream `POS_FLEET`, because a `NatsConsumer` binds one stream and this is the only
    /// cursor the cloud runs ([ADR-0087](../../../../docs/adr/0087-edge-relay-and-event-publish.md)
    /// Amendment 1).
    pub stream: String,
    /// The durable consumer name. This is what makes the cursor survive a restart, so it must be
    /// stable across the process's lifetime; changing it starts a fresh cursor.
    pub durable: String,
    /// Restrict the cursor to this subject, or `""` for every subject the stream captures.
    pub filter_subject: String,
    /// The most messages one [`pull`](NatsConsumer::pull) gathers before returning.
    pub batch: usize,
    /// How long one [`pull`](NatsConsumer::pull) waits for a full batch before returning what it has
    /// (possibly nothing). This is the idle poll interval, so it trades latency against wake-ups.
    pub expires: Duration,
}

/// A durable [`MessageLink`](pos_ports::message_link::MessageLink) *reader* over NATS JetStream.
///
/// Holds a bound durable pull consumer. Cheap to hold across the process's life; one per stream.
#[derive(Debug)]
pub struct NatsConsumer {
    consumer: Consumer<pull::Config>,
    config: ConsumerConfig,
}

impl NatsConsumer {
    /// Connects to NATS at `url` and binds the durable cursor described by `config`.
    ///
    /// Credentials in `url` are presented at connect time via [`crate::endpoint::split`], because
    /// `async-nats` reads them from its options and never from the address. `bootstrap.sh` documents
    /// arming this cursor with `url = "nats://:THE_NATS_TOKEN@nats:4222"`, and until that lift
    /// existed the token was dropped and the generated broker config refused the connection.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if NATS cannot be reached or the stream does not yet exist — the
    /// stream is created by the edge's handshake, so a cloud that starts first retries until a store
    /// has connected.
    pub async fn connect(url: &str, config: ConsumerConfig) -> Result<Self, PortError> {
        let endpoint = crate::endpoint::split(url);
        let client = async_nats::connect_with_options(endpoint.address(), endpoint.options())
            .await
            .map_err(unavailable)?;
        Self::from_client(client, config).await
    }

    /// Binds the cursor over an existing client — how a binary shares one connection with
    /// [`NatsLink`](crate::NatsLink), and how a test keeps a handle to the same server.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the stream does not exist or the consumer cannot be created.
    pub async fn from_client(
        client: async_nats::Client,
        config: ConsumerConfig,
    ) -> Result<Self, PortError> {
        let context = jetstream::new(client);
        let stream = context
            .get_stream(&config.stream)
            .await
            .map_err(unavailable)?;
        // Explicit ack: the server holds a message as pending until we acknowledge it, and
        // redelivers it if we do not — the guarantee the "store before you ack" flow rests on.
        let consumer = stream
            .get_or_create_consumer(
                &config.durable,
                pull::Config {
                    durable_name: Some(config.durable.clone()),
                    ack_policy: AckPolicy::Explicit,
                    filter_subject: config.filter_subject.clone(),
                    ..Default::default()
                },
            )
            .await
            .map_err(unavailable)?;
        Ok(Self { consumer, config })
    }

    /// Pulls the next batch: up to `config.batch` messages, waiting at most `config.expires`.
    ///
    /// Decodes each message into an [`EventEnvelope`]. Undecodable messages are terminated (they can
    /// never be ingested) and counted in [`ConsumedBatch::poison`]; the decoded ones are returned with
    /// their message handles so the caller can [`ack`](ConsumedBatch::ack) after storing them.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the connection fails while gathering or terminating a message.
    pub async fn pull(&self) -> Result<ConsumedBatch, PortError> {
        let mut messages = self
            .consumer
            .batch()
            .max_messages(self.config.batch)
            .expires(self.config.expires)
            .messages()
            .await
            .map_err(unavailable)?;

        let mut events = Vec::new();
        let mut acks: Vec<Message> = Vec::new();
        let mut poison: u32 = 0;
        while let Some(next) = messages.next().await {
            let message = next.map_err(unavailable)?;
            if let Ok(event) = decode(&message.payload) {
                events.push(event);
                acks.push(message);
            } else {
                // Unprocessable by definition: terminate it so redelivery stops, rather than let one
                // corrupt frame wedge the cursor. Loud, not silent — the caller logs `poison`.
                message.ack_with(AckKind::Term).await.map_err(unavailable)?;
                poison = poison.saturating_add(1);
            }
        }
        Ok(ConsumedBatch {
            events,
            acks,
            poison,
        })
    }
}

/// A pulled batch: the decoded events, the handles to acknowledge them, and how many messages were
/// discarded as undecodable.
///
/// The caller ingests [`events`](ConsumedBatch::events), then calls [`ack`](ConsumedBatch::ack) to
/// advance the cursor or [`nak`](ConsumedBatch::nak) to return the batch for redelivery. Dropping it
/// without either leaves the messages pending until the server's ack-wait elapses, which then
/// redelivers them — safe, because ingest is idempotent, but slower.
#[derive(Debug)]
pub struct ConsumedBatch {
    events: Vec<EventEnvelope<RawPayload>>,
    acks: Vec<Message>,
    poison: u32,
}

impl ConsumedBatch {
    /// The decoded events, oldest first.
    #[must_use]
    pub fn events(&self) -> &[EventEnvelope<RawPayload>] {
        &self.events
    }

    /// How many decoded events the batch carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the batch decoded no events (the idle case, or a batch that was all poison).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// How many messages were discarded as undecodable. Already terminated; reported so the caller
    /// can log and alert.
    #[must_use]
    pub fn poison(&self) -> u32 {
        self.poison
    }

    /// Advances the cursor past this batch. Call only after ingest has durably committed it.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if an acknowledgement cannot be sent; the un-acked messages are
    /// redelivered, which idempotent ingest absorbs.
    pub async fn ack(self) -> Result<(), PortError> {
        for message in &self.acks {
            message.ack().await.map_err(unavailable)?;
        }
        Ok(())
    }

    /// Returns the batch to the stream for redelivery — the retryable path when ingest could not
    /// store it.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the negative acknowledgement cannot be sent; the ack-wait then
    /// redelivers anyway.
    pub async fn nak(self) -> Result<(), PortError> {
        for message in &self.acks {
            message
                .ack_with(AckKind::Nak(None))
                .await
                .map_err(unavailable)?;
        }
        Ok(())
    }
}

/// Decodes one message body into an event envelope. Factored out so the poison boundary is unit-
/// tested without a broker.
fn decode(bytes: &[u8]) -> Result<EventEnvelope<RawPayload>, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Maps any NATS error to the port's retryable unavailable status, so the ingest loop backs off and
/// retries rather than dropping the feed.
///
/// Accepts both async-nats's concrete errors and its boxed [`async_nats::Error`]; the boxed form is
/// wrapped in [`NatsError`] so it satisfies `with_source`'s sized bound.
fn unavailable(error: impl Into<async_nats::Error>) -> PortError {
    PortError::unavailable(PortName::MessageLink, "the cloud consumer is unavailable")
        .with_source(NatsError(error.into()))
}

/// A sized wrapper over async-nats's boxed error, so it can be a [`PortError`] source.
#[derive(Debug)]
struct NatsError(async_nats::Error);

impl core::fmt::Display for NatsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl core::error::Error for NatsError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.0.source()
    }
}

#[cfg(test)]
mod tests {
    use super::decode;

    use pos_contract_tests::fixtures;
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    #[test]
    fn a_real_envelope_round_trips_through_the_wire() {
        // Serialise exactly as the edge publishes, then decode as the consumer will — proving the two
        // ends agree on the JSON without hand-writing (and mis-writing) the schema.
        let store = StoreId::new(Ulid::from_u128(0x0ADA));
        let event = fixtures::activations(store, 1, 1)
            .pop()
            .expect("one activation");
        let bytes = serde_json::to_vec(&event).expect("serialise the envelope");
        let decoded = decode(&bytes).expect("a well-formed envelope decodes");
        assert_eq!(decoded, event);
    }

    #[test]
    fn a_corrupt_body_is_a_decode_error_not_a_panic() {
        // The poison boundary: `pull` terminates whatever this rejects, so it must reject cleanly.
        assert!(decode(b"not json at all").is_err());
        assert!(
            decode(b"{}").is_err(),
            "a JSON object missing every field is still poison"
        );
        assert!(decode(b"").is_err());
    }
}
