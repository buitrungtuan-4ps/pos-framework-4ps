// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The config wake over core NATS
//! ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)): the store's
//! [`NatsStoreWake`], and the cloud's [`publish_config_wake`].
//!
//! Core NATS, not JetStream: the subject `pos.store.<store_id>.config` is captured by no stream, so a
//! wake sent while a store is away is gone. That is the design. The store pulls on every
//! (re)connect, reported here as [`WakeSignal::Connected`], and the poll covers the rest.
//!
//! async-nats re-subscribes on its own after a reconnect and says so only in its connection
//! statistics, so the wait is cut into [`RECONNECT_CHECK`] slices and the connect count is read
//! between them: a reconnect during a wait is reported within one slice.

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use async_nats::connection::State;
use futures_util::StreamExt;
use futures_util::lock::Mutex;

use pos_ports::store_wake::{StoreWake, WakeSignal};
use pos_ports::{PortError, PortName};
use pos_proto::ids::StoreId;
use pos_proto::wake::ConfigWake;

/// How often a wait looks for a reconnect.
pub const RECONNECT_CHECK: Duration = Duration::from_millis(100);

/// A [`StoreWake`] on the connection the edge already holds.
#[derive(Debug)]
pub struct NatsStoreWake {
    client: async_nats::Client,
    subscriber: Mutex<async_nats::Subscriber>,
    /// The connect count last reported as [`WakeSignal::Connected`]; zero before the first.
    reported_connects: AtomicU64,
}

impl NatsStoreWake {
    /// Subscribes `client` to `store_id`'s wakes. Never waits on the broker: a client still
    /// connecting queues the subscription, and its first connect is reported as
    /// [`WakeSignal::Connected`].
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the client is closed.
    pub async fn subscribe(
        client: async_nats::Client,
        store_id: StoreId,
    ) -> Result<Self, PortError> {
        let subscriber = client
            .subscribe(ConfigWake::subject(store_id))
            .await
            .map_err(unavailable)?;
        Ok(Self {
            client,
            subscriber: Mutex::new(subscriber),
            reported_connects: AtomicU64::new(0),
        })
    }

    /// The underlying client, for connection observation (and, in tests, a forced reconnect).
    #[must_use]
    pub const fn client(&self) -> &async_nats::Client {
        &self.client
    }

    /// `Connected` once per connect not yet reported, while connected.
    fn connected_since_last(&self) -> Option<WakeSignal> {
        if self.client.connection_state() != State::Connected {
            return None;
        }
        let connects = self.client.statistics().connects.load(Ordering::Relaxed);
        let reported = self.reported_connects.swap(connects, Ordering::Relaxed);
        (connects > reported).then_some(WakeSignal::Connected)
    }
}

impl StoreWake for NatsStoreWake {
    async fn next_signal(&self, within: Duration) -> Result<Option<WakeSignal>, PortError> {
        let mut subscriber = self.subscriber.lock().await;
        let mut left = within;
        loop {
            if let Some(connected) = self.connected_since_last() {
                return Ok(Some(connected));
            }
            if left.is_zero() {
                return Ok(None);
            }
            let slice = left.min(RECONNECT_CHECK);
            left = left.saturating_sub(slice);
            // Cancel-safe: an unfinished `next` leaves the message in the subscription's queue.
            match tokio::time::timeout(slice, subscriber.next()).await {
                Ok(Some(message)) => {
                    return Ok(Some(WakeSignal::Wake(ConfigWake::decode(&message.payload))));
                }
                Ok(None) => {
                    return Err(PortError::unavailable(
                        PortName::StoreWake,
                        "the wake subscription is closed",
                    ));
                }
                Err(_elapsed) => {}
            }
        }
    }
}

/// Wakes `store_id`: a core publish of `wake` on its subject. The cloud calls it after a publish
/// has committed, once per store the publish changed (ADR-0195 decision 1).
///
/// # Errors
///
/// [`PortError::unavailable`] if the client cannot take the message. The caller logs and counts
/// it and never fails the configuration publish over it: the poll still delivers.
pub async fn publish_config_wake(
    client: &async_nats::Client,
    store_id: StoreId,
    wake: &ConfigWake,
) -> Result<(), PortError> {
    let payload = serde_json::to_vec(wake).map_err(|error| {
        PortError::internal(PortName::StoreWake, "could not serialise a wake").with_source(error)
    })?;
    client
        .publish(ConfigWake::subject(store_id), payload.into())
        .await
        .map_err(unavailable)
}

fn unavailable<E: core::error::Error + Send + Sync + 'static>(error: E) -> PortError {
    PortError::unavailable(PortName::StoreWake, "the wake bus is unavailable").with_source(error)
}
