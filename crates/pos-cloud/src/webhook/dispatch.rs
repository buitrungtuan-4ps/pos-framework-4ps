// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The webhook delivery engine: a per-endpoint cursor over the event log.
//!
//! A webhook subscription is **a cursor over the store's event log**, not a queue of pending
//! deliveries (`docs/roadmap.md` P7). Each [`WebhookEndpoint`] holds only its position; the events
//! themselves stay in the durable log. [`deliver_next`] reads one bounded page after the cursor,
//! signs and delivers it, and advances the cursor **only on success**. That single design decision
//! is what makes the exit criterion true: *a dead endpoint falls behind without any memory growth*,
//! because a failed delivery leaves the cursor where it was and buffers nothing — the backlog lives
//! in PostgreSQL, which was going to hold it anyway.
//!
//! Around that spine sit the safety rails, each its own tested module: the [`super::breaker`] that
//! stops a dead endpoint being hammered and disables it after a day, the [`super::sign`] signature
//! and replay window, and the [`super::ssrf`] vetting that ran before the endpoint was ever
//! registered. Endpoints are fully isolated: one owns one cursor and one breaker, so a slow or
//! hostile receiver cannot touch another's delivery.
//!
//! The actual network POST is a [`WebhookTransport`], so the engine is proven here against a fake
//! that records or refuses deliveries; the concrete TLS sender is [ADR-0032](../../../docs/adr/0032-webhooks.md)'s
//! separate, later piece.

use core::future::Future;
use core::num::NonZeroU32;

use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ids::{EventId, StoreId};
use pos_proto::time::Timestamp;

use super::breaker::{BreakerConfig, BreakerState, CircuitBreaker};
use super::sign::{Signature, SigningSecret, delivery_id, sign};
use super::ssrf::VettedUrl;

/// The default page size: how many events one delivery carries at most.
const DEFAULT_PAGE: u32 = 100;

/// Sends a signed webhook body to a vetted destination.
///
/// The one seam between the engine and the network. An implementation POSTs `body` to `target` with
/// the [`Signature`] as headers ([`super::sign`]), connecting only to `target`'s pre-vetted
/// addresses. `Ok` means the receiver accepted it (a 2xx); any other outcome is an `Err` and the
/// delivery is treated as failed.
pub trait WebhookTransport {
    /// Delivers one signed body.
    ///
    /// `delivery_id` is the receiver's **idempotency key** ([`super::sign::DELIVERY_ID_HEADER`],
    /// production-readiness **R6**), sent as a header when there is one. `None` is for a body that
    /// is not a cursor page and therefore has no stable identity to offer — the alert channel — and
    /// the header is then simply absent rather than carrying a value a receiver could not dedupe on.
    fn deliver(
        &self,
        target: &VettedUrl,
        signature: &Signature,
        delivery_id: Option<&str>,
        body: &[u8],
    ) -> impl Future<Output = Result<(), DeliveryError>> + Send;
}

/// A delivery that did not succeed. Opaque and always retryable — the breaker decides how long to
/// wait, and the cursor does not advance, so a retry re-sends the same page.
#[derive(Debug, Clone, thiserror::Error)]
#[error("webhook delivery failed: {message}")]
pub struct DeliveryError {
    message: String,
}

impl DeliveryError {
    /// Builds a failure with a human-readable reason (a status code, a transport error).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Why a delivery attempt could not even be made into a network call.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The event log could not be read.
    #[error("reading the event log failed: {0}")]
    Store(#[from] PortError),
    /// The batch could not be serialized — an internal invariant failure, since every stored
    /// envelope is serializable.
    #[error("encoding the webhook body failed: {0}")]
    Encode(#[source] serde_json::Error),
}

/// What one [`deliver_next`] attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// A page was delivered and acknowledged; the cursor advanced.
    Delivered {
        /// How many events were in the page.
        events: u32,
    },
    /// Nothing to deliver: the endpoint is caught up.
    Idle,
    /// The breaker is open or the endpoint disabled, so no attempt was made.
    Suppressed,
    /// The delivery was attempted and failed; the cursor did not advance.
    Failed,
}

/// One webhook subscription: where it points, how it signs, and where its cursor sits.
///
/// Holds no delivery buffer by construction — only the cursor — which is what bounds its memory to
/// one page regardless of how far behind it falls.
#[derive(Debug)]
pub struct WebhookEndpoint {
    store_id: StoreId,
    destination: VettedUrl,
    secret: SigningSecret,
    cursor: Option<EventId>,
    page: NonZeroU32,
    breaker: CircuitBreaker,
}

impl WebhookEndpoint {
    /// Registers an endpoint following `store_id`'s log, delivering to an **already vetted**
    /// `destination` ([`super::ssrf::vet`]).
    #[must_use]
    pub fn register(store_id: StoreId, destination: VettedUrl, secret: SigningSecret) -> Self {
        Self::rehydrate(store_id, destination, secret, None)
    }

    /// Rehydrates an endpoint at a persisted `cursor`, for the dispatch task to resume a subscription
    /// after a restart without replaying from the start of the log. The breaker starts closed — it is
    /// ephemeral in-memory state, and a disabled endpoint is not loaded at all (`load_enabled`).
    #[must_use]
    pub fn rehydrate(
        store_id: StoreId,
        destination: VettedUrl,
        secret: SigningSecret,
        cursor: Option<EventId>,
    ) -> Self {
        Self {
            store_id,
            destination,
            secret,
            cursor,
            page: NonZeroU32::new(DEFAULT_PAGE).unwrap_or(NonZeroU32::MIN),
            breaker: CircuitBreaker::new(BreakerConfig::default()),
        }
    }

    /// Points the endpoint at a freshly re-vetted `destination`, leaving the cursor and breaker
    /// untouched. The dispatch task re-vets before each delivery batch (so DNS rebinding cannot slip a
    /// stale address through), then hands the new addresses here.
    pub fn retarget(&mut self, destination: VettedUrl) {
        self.destination = destination;
    }

    /// The last event this endpoint has delivered, or `None` if it has delivered nothing.
    #[must_use]
    pub fn cursor(&self) -> Option<EventId> {
        self.cursor
    }

    /// Shrinks the page, so a test can exercise *two* pages without seeding a hundred events.
    ///
    /// Test-only: production reads [`DEFAULT_PAGE`] and nothing configures it, so a public setter
    /// would be API nobody calls.
    #[cfg(test)]
    fn set_page(&mut self, page: NonZeroU32) {
        self.page = page;
    }

    /// The breaker's current state, for the admin view.
    #[must_use]
    pub fn breaker_state(&self) -> BreakerState {
        self.breaker.state()
    }

    /// Whether the endpoint has been auto-disabled.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.breaker.is_disabled()
    }

    /// Re-enables a disabled endpoint (an operator action) and leaves the cursor untouched, so it
    /// resumes where it fell behind rather than replaying from the start.
    pub fn enable(&mut self) {
        self.breaker.enable();
    }
}

/// Attempts to deliver the next page for one endpoint.
///
/// Consults the breaker, reads one page after the cursor, signs it, delivers it, and advances the
/// cursor only if the delivery succeeded. Never buffers: a failure leaves the cursor and the backlog
/// exactly where they were.
///
/// # Errors
///
/// [`WebhookError`] only if the log cannot be read or the batch cannot be encoded — a delivery that
/// the receiver rejects is a normal [`DeliveryOutcome::Failed`], not an error.
pub async fn deliver_next<S, T>(
    store: &S,
    endpoint: &mut WebhookEndpoint,
    transport: &T,
    now: Timestamp,
) -> Result<DeliveryOutcome, WebhookError>
where
    S: EventStore,
    T: WebhookTransport,
{
    if !endpoint.breaker.allow(now) {
        return Ok(DeliveryOutcome::Suppressed);
    }

    let mut query = EventQuery::first(endpoint.store_id, endpoint.page);
    if let Some(after) = endpoint.cursor {
        query = query.after(after);
    }
    let batch = store.read(&query).await?;
    if batch.is_empty() {
        return Ok(DeliveryOutcome::Idle);
    }

    let body = serde_json::to_vec(&batch).map_err(WebhookError::Encode)?;
    let signature = sign(&endpoint.secret, now, &body);
    // The page's idempotency key (production-readiness R6). A failure below leaves the cursor where
    // it is, so the next attempt re-reads this same page and mints this same id — which is exactly
    // what lets a receiver that answered late recognise the retry instead of processing it twice.
    let delivery_id = batch
        .first()
        .map(|event| delivery_id(endpoint.store_id, event.event_id));

    if transport
        .deliver(
            &endpoint.destination,
            &signature,
            delivery_id.as_deref(),
            &body,
        )
        .await
        .is_ok()
    {
        // Advance only on success. The last event's id is the new high-water mark.
        endpoint.cursor = batch.last().map(|event| event.event_id).or(endpoint.cursor);
        endpoint.breaker.record_success();
        let events = u32::try_from(batch.len()).unwrap_or(u32::MAX);
        Ok(DeliveryOutcome::Delivered { events })
    } else {
        // The receiver refused it. Do not advance — the same page is re-sent next time — and let the
        // breaker decide whether to keep trying.
        endpoint.breaker.record_failure(now);
        Ok(DeliveryOutcome::Failed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use core::num::NonZeroU32;

    use super::{DeliveryError, DeliveryOutcome, WebhookEndpoint, WebhookTransport, deliver_next};

    use pos_contract_tests::fixtures;
    use pos_fakes::FakeStore;
    use pos_ports::event_store::EventStore;
    use pos_ports::{Transactional as _, TxContext as _};
    use pos_proto::envelope::{EventEnvelope, RawPayload};
    use pos_proto::ids::StoreId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use crate::webhook::sign::{SigningSecret, verify};
    use crate::webhook::ssrf::VettedUrl;

    fn store_id() -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }

    fn now() -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(1_700_000_000_000).expect("valid")
    }

    fn destination() -> VettedUrl {
        VettedUrl {
            url: "https://hooks.example.com/pos".to_owned(),
            addresses: vec!["93.184.216.34".parse().expect("addr")],
        }
    }

    async fn seed(store: &FakeStore, count: u32) -> Vec<EventEnvelope<RawPayload>> {
        let events = fixtures::activations(store_id(), 1, count);
        let mut tx = store.begin().await.expect("begin");
        store.append(&mut tx, &events).await.expect("append");
        tx.commit().await.expect("commit");
        events
    }

    /// Records the bodies it accepts, and can be flipped to refuse every delivery.
    ///
    /// `delivery_ids` records **every** attempt, refused ones included — the whole point of R6 is
    /// what a retry carries, and a retry is by definition an attempt the receiver did not accept.
    struct FakeTransport {
        accepted: Mutex<Vec<(super::Signature, Vec<u8>)>>,
        delivery_ids: Mutex<Vec<Option<String>>>,
        down: AtomicBool,
    }

    impl FakeTransport {
        fn up() -> Self {
            Self {
                accepted: Mutex::new(Vec::new()),
                delivery_ids: Mutex::new(Vec::new()),
                down: AtomicBool::new(false),
            }
        }

        fn down() -> Self {
            Self {
                accepted: Mutex::new(Vec::new()),
                delivery_ids: Mutex::new(Vec::new()),
                down: AtomicBool::new(true),
            }
        }

        fn calls(&self) -> usize {
            self.accepted.lock().expect("lock").len()
        }

        fn delivery_ids(&self) -> Vec<Option<String>> {
            self.delivery_ids.lock().expect("lock").clone()
        }
    }

    impl WebhookTransport for FakeTransport {
        async fn deliver(
            &self,
            _target: &VettedUrl,
            signature: &super::Signature,
            delivery_id: Option<&str>,
            body: &[u8],
        ) -> Result<(), DeliveryError> {
            self.delivery_ids
                .lock()
                .expect("lock")
                .push(delivery_id.map(ToOwned::to_owned));
            if self.down.load(Ordering::SeqCst) {
                return Err(DeliveryError::new("endpoint down"));
            }
            self.accepted
                .lock()
                .expect("lock")
                .push((signature.clone(), body.to_vec()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn it_delivers_a_page_signs_it_and_advances_the_cursor() {
        let store = FakeStore::new();
        let events = seed(&store, 3).await;
        let secret = SigningSecret::new("shhh");
        let mut endpoint = WebhookEndpoint::register(store_id(), destination(), secret.clone());
        let transport = FakeTransport::up();

        let outcome = deliver_next(&store, &mut endpoint, &transport, now())
            .await
            .expect("deliver");
        assert_eq!(outcome, DeliveryOutcome::Delivered { events: 3 });
        assert_eq!(
            endpoint.cursor(),
            Some(events[2].event_id),
            "cursor advanced to the last event"
        );

        // The receiver got a genuinely signed, verifiable body. Scope the guard so it is released
        // well before the next await.
        {
            let accepted = transport.accepted.lock().expect("lock");
            let (signature, body) = accepted.first().expect("one delivery");
            assert_eq!(
                verify(
                    &secret,
                    signature.timestamp,
                    body,
                    &signature.signature,
                    now()
                ),
                Ok(()),
                "the delivered body carries a valid signature"
            );
        }

        // Caught up now: nothing more to send.
        let idle = deliver_next(&store, &mut endpoint, &transport, now())
            .await
            .expect("deliver");
        assert_eq!(idle, DeliveryOutcome::Idle);
    }

    #[tokio::test]
    async fn a_dead_endpoint_falls_behind_without_advancing_or_buffering() {
        let store = FakeStore::new();
        seed(&store, 50).await;
        let mut endpoint =
            WebhookEndpoint::register(store_id(), destination(), SigningSecret::new("shhh"));
        let transport = FakeTransport::down();

        // Hammer it well past the failure threshold.
        let mut failures = 0;
        let mut suppressions = 0;
        for tick in 0..20 {
            let outcome = deliver_next(&store, &mut endpoint, &transport, now())
                .await
                .expect("deliver");
            match outcome {
                DeliveryOutcome::Failed => failures += 1,
                DeliveryOutcome::Suppressed => suppressions += 1,
                other => panic!("unexpected outcome on tick {tick}: {other:?}"),
            }
            // The cursor never moves: the endpoint holds a position, not a backlog.
            assert_eq!(
                endpoint.cursor(),
                None,
                "a dead endpoint never advances its cursor"
            );
        }
        assert!(failures >= 1, "it tried at least once");
        assert!(
            suppressions >= 1,
            "the breaker opened and suppressed further attempts, so a dead endpoint is not hammered"
        );
        assert_eq!(transport.calls(), 0, "nothing was ever accepted");
    }

    #[tokio::test]
    async fn recovery_after_failure_resumes_from_the_cursor_and_catches_up() {
        let store = FakeStore::new();
        let events = seed(&store, 4).await;
        let mut endpoint =
            WebhookEndpoint::register(store_id(), destination(), SigningSecret::new("shhh"));

        // First, the endpoint is down: it fails once and does not advance.
        let down = FakeTransport::down();
        assert_eq!(
            deliver_next(&store, &mut endpoint, &down, now())
                .await
                .expect("deliver"),
            DeliveryOutcome::Failed
        );
        assert_eq!(endpoint.cursor(), None);

        // It comes back: the same page delivers and the cursor catches up to the end.
        let up = FakeTransport::up();
        assert_eq!(
            deliver_next(&store, &mut endpoint, &up, now())
                .await
                .expect("deliver"),
            DeliveryOutcome::Delivered { events: 4 }
        );
        assert_eq!(endpoint.cursor(), Some(events[3].event_id));
    }

    #[tokio::test]
    async fn a_retry_carries_the_same_delivery_id_and_the_next_page_a_different_one() {
        // Production-readiness R6. A failed delivery leaves the cursor where it was, so the retry
        // re-sends the same page with a fresh timestamp and therefore a fresh signature. Without a
        // stable idempotency key the two are indistinguishable to a receiver except by hashing the
        // body — so a receiver that processed the page and then answered late would process it twice.
        let store = FakeStore::new();
        let events = seed(&store, 4).await;
        let mut endpoint =
            WebhookEndpoint::register(store_id(), destination(), SigningSecret::new("shhh"));
        endpoint.set_page(NonZeroU32::new(2).expect("a page of two"));

        // Two refused attempts at the first page, then one that lands.
        let down = FakeTransport::down();
        for _ in 0..2 {
            assert_eq!(
                deliver_next(&store, &mut endpoint, &down, now())
                    .await
                    .expect("deliver"),
                DeliveryOutcome::Failed
            );
        }
        let refused = down.delivery_ids();
        assert_eq!(refused.len(), 2, "both attempts reached the transport");
        assert_eq!(
            refused[0], refused[1],
            "a retry of the same page carries the same idempotency key"
        );

        let up = FakeTransport::up();
        assert_eq!(
            deliver_next(&store, &mut endpoint, &up, now())
                .await
                .expect("deliver"),
            DeliveryOutcome::Delivered { events: 2 }
        );
        let first_page = up.delivery_ids();
        assert_eq!(
            first_page[0], refused[0],
            "and the attempt that finally lands carries it too — the page never changed"
        );
        assert_eq!(
            first_page[0].as_deref(),
            Some(format!("{}.{}", store_id(), events[0].event_id).as_str()),
            "the key names the store and the page's first event, so an operator can read it"
        );

        // The cursor has moved, so the next page is a different delivery.
        let second = FakeTransport::up();
        assert_eq!(
            deliver_next(&store, &mut endpoint, &second, now())
                .await
                .expect("deliver"),
            DeliveryOutcome::Delivered { events: 2 }
        );
        assert_ne!(
            second.delivery_ids()[0],
            first_page[0],
            "a different page is a different delivery, never a repeat of the last key"
        );
    }
}
