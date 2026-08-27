// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge liveness heartbeat loop ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 2).
//!
//! A store's config-pull loop ([`config_client`](crate::config_client)) already tells the cloud the
//! store is alive on every pull, but a store that is up and simply not pulling — a parked long-poll,
//! a quiet period between publishes — would otherwise fall silent and read as offline. This is the
//! lightweight ping that keeps `last_seen_at` fresh regardless: a background loop that POSTs the
//! store's heartbeat on a fixed interval.
//!
//! The HTTP is a seam ([`HeartbeatTransport`]), so the loop is tested with no socket, exactly as the
//! config-pull loop is; the field implementation is an HTTPS `POST /sync/stores/{id}/heartbeat`
//! authenticated with the store's scoped API key. A missed heartbeat is never fatal — the store keeps
//! trading locally — so a transport error is logged and the next tick simply retries.

use core::future::Future;
use core::time::Duration;

/// A failure of the heartbeat transport itself — the cloud is unreachable or refused the ping.
#[derive(Debug, thiserror::Error)]
#[error("the heartbeat transport failed: {0}")]
pub struct HeartbeatError(String);

impl HeartbeatError {
    /// Wraps a reason (for the store's log — a heartbeat carries no personal data).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The heartbeat ping the loop rides: POST the store's liveness to the cloud. A seam, so the loop is
/// tested without a socket; the field implementation is an HTTPS POST authenticated with the store's
/// scoped API key.
pub trait HeartbeatTransport: Send + Sync {
    /// Sends one heartbeat.
    ///
    /// # Errors
    ///
    /// [`HeartbeatError`] if the cloud could not be reached or refused the ping.
    fn beat(&self) -> impl Future<Output = Result<(), HeartbeatError>> + Send;
}

/// The heartbeat client: a transport to the cloud and the interval between pings.
#[derive(Debug)]
pub struct HeartbeatClient<T> {
    transport: T,
    interval: Duration,
}

impl<T> HeartbeatClient<T>
where
    T: HeartbeatTransport,
{
    /// Builds a client that pings every `interval`.
    pub fn new(transport: T, interval: Duration) -> Self {
        Self {
            transport,
            interval,
        }
    }

    /// Sends one heartbeat now.
    ///
    /// # Errors
    ///
    /// [`HeartbeatError`] for a transport failure.
    pub async fn beat_once(&self) -> Result<(), HeartbeatError> {
        self.transport.beat().await
    }

    /// Runs the heartbeat loop until `shutdown` resolves: wait `interval`, ping, repeat. A transport
    /// error is logged and the loop continues — the next tick retries, and the store keeps trading on
    /// its last-known-good session while the cloud link is down.
    pub async fn run<F>(self, shutdown: F)
    where
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                () = tokio::time::sleep(self.interval) => {
                    if let Err(error) = self.transport.beat().await {
                        tracing::warn!(%error, "heartbeat failed; will retry next tick");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HeartbeatClient, HeartbeatError, HeartbeatTransport};
    use core::time::Duration;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A transport that counts pings, optionally failing every one.
    struct CountingBeat {
        beats: Arc<AtomicUsize>,
        fail: bool,
    }

    impl HeartbeatTransport for CountingBeat {
        async fn beat(&self) -> Result<(), HeartbeatError> {
            if self.fail {
                return Err(HeartbeatError::new("cloud unreachable"));
            }
            self.beats.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn beat_once_pings_the_transport() {
        let beats = Arc::new(AtomicUsize::new(0));
        let client = HeartbeatClient::new(
            CountingBeat {
                beats: Arc::clone(&beats),
                fail: false,
            },
            Duration::from_secs(60),
        );
        client.beat_once().await.expect("the ping succeeds");
        assert_eq!(
            beats.load(Ordering::SeqCst),
            1,
            "one ping reached the transport"
        );
    }

    #[tokio::test]
    async fn beat_once_surfaces_a_transport_failure() {
        let client = HeartbeatClient::new(
            CountingBeat {
                beats: Arc::new(AtomicUsize::new(0)),
                fail: true,
            },
            Duration::from_secs(60),
        );
        assert!(
            client.beat_once().await.is_err(),
            "a transport failure surfaces rather than being swallowed"
        );
    }

    #[tokio::test]
    async fn run_honours_shutdown_before_the_first_tick() {
        // An already-resolved shutdown must end the loop at once, before the first interval elapses —
        // so the test never waits on a real timer.
        let beats = Arc::new(AtomicUsize::new(0));
        let client = HeartbeatClient::new(
            CountingBeat {
                beats: Arc::clone(&beats),
                fail: false,
            },
            Duration::from_secs(3600),
        );
        client.run(async {}).await;
        assert_eq!(
            beats.load(Ordering::SeqCst),
            0,
            "shutdown wins over the first tick, so no ping is sent"
        );
    }
}
