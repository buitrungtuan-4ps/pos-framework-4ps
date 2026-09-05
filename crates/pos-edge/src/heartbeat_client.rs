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
//!
//! # The ping carries the store's own backlog
//!
//! A heartbeat is the one rail that reaches the cloud on a fixed interval whether or not the store
//! has anything else to say, so it is where the store reports how far behind its event publishing
//! is ([`EventStore::outbox_depth`]). Without it the cloud can see how many orders it is holding
//! *for* a store and nothing at all about how many sales the store is holding *from* it — a box
//! whose NATS link has been down for a day looks identical to one that is perfectly current. The
//! depth is a count, never an event body, so it carries no personal data (`docs/pos-spec.md` §13).

use core::future::Future;
use core::time::Duration;
use std::sync::Arc;

use pos_ports::event_store::EventStore;

use crate::app::Edge;

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

/// What one heartbeat says about the store beyond "I am here".
///
/// Every field is optional and the whole report may be empty, because the cloud's route is older
/// than the report: a store that cannot answer a question says nothing about it rather than
/// guessing, and the cloud leaves whatever it last recorded alone.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatReport {
    /// How many events the store has committed and not yet published, or `None` if its log could not
    /// be read. Deliberately not "0 on failure": zero is the good answer, and reporting it for a
    /// store whose log is unreadable would report the healthiest possible state for the least
    /// healthy one.
    pub outbox_depth: Option<u64>,
}

/// The heartbeat ping the loop rides: POST the store's liveness to the cloud. A seam, so the loop is
/// tested without a socket; the field implementation is an HTTPS POST authenticated with the store's
/// scoped API key.
pub trait HeartbeatTransport: Send + Sync {
    /// Sends one heartbeat carrying `report`.
    ///
    /// # Errors
    ///
    /// [`HeartbeatError`] if the cloud could not be reached or refused the ping.
    fn beat(
        &self,
        report: HeartbeatReport,
    ) -> impl Future<Output = Result<(), HeartbeatError>> + Send;
}

/// Where the loop reads what a heartbeat reports. A seam for the same reason the transport is: the
/// loop is exercised with no store behind it, and the shipped binary passes its [`Edge`].
pub trait HeartbeatSource: Send + Sync {
    /// Gathers what this tick has to say.
    fn report(&self) -> impl Future<Output = HeartbeatReport> + Send;
}

/// The source for a loop with nothing to report — every ping is a bare "I am here". The bootstrap
/// shape, and what the tests that only count pings use.
#[derive(Debug, Default, Clone, Copy)]
pub struct NothingToReport;

impl HeartbeatSource for NothingToReport {
    async fn report(&self) -> HeartbeatReport {
        HeartbeatReport::default()
    }
}

/// The shipped source: the store's own event log, read through the [`Edge`] that owns it.
///
/// A log that cannot be read logs a warning and reports `None` rather than failing the heartbeat —
/// liveness is the ping's first job, and a box that stops saying "I am here" because it could not
/// count its outbox would read as offline, which is a worse lie than an unknown depth.
impl<S> HeartbeatSource for Arc<Edge<S>>
where
    S: EventStore + Send + Sync,
{
    async fn report(&self) -> HeartbeatReport {
        let outbox_depth = match self.store().outbox_depth(self.store_id()).await {
            Ok(depth) => Some(depth),
            Err(error) => {
                tracing::warn!(%error, "could not read the outbox depth; the heartbeat reports none");
                None
            }
        };
        HeartbeatReport { outbox_depth }
    }
}

/// The heartbeat client: a transport to the cloud, where to read the report, and the interval
/// between pings.
#[derive(Debug)]
pub struct HeartbeatClient<T, R = NothingToReport> {
    transport: T,
    source: R,
    interval: Duration,
}

impl<T> HeartbeatClient<T, NothingToReport>
where
    T: HeartbeatTransport,
{
    /// Builds a client that pings every `interval` and reports nothing but its liveness.
    pub fn new(transport: T, interval: Duration) -> Self {
        Self {
            transport,
            source: NothingToReport,
            interval,
        }
    }
}

impl<T, R> HeartbeatClient<T, R>
where
    T: HeartbeatTransport,
    R: HeartbeatSource,
{
    /// Builds a client that pings every `interval`, reading each ping's report from `source`.
    pub fn reporting(transport: T, source: R, interval: Duration) -> Self {
        Self {
            transport,
            source,
            interval,
        }
    }

    /// Sends one heartbeat now.
    ///
    /// # Errors
    ///
    /// [`HeartbeatError`] for a transport failure.
    pub async fn beat_once(&self) -> Result<(), HeartbeatError> {
        let report = self.source.report().await;
        self.transport.beat(report).await
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
                    let report = self.source.report().await;
                    if let Err(error) = self.transport.beat(report).await {
                        tracing::warn!(%error, "heartbeat failed; will retry next tick");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HeartbeatClient, HeartbeatError, HeartbeatReport, HeartbeatSource, HeartbeatTransport,
    };
    use core::time::Duration;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A transport that counts pings and keeps the last report, optionally failing every one.
    #[derive(Default)]
    struct CountingBeat {
        beats: Arc<AtomicUsize>,
        last: Arc<Mutex<Option<HeartbeatReport>>>,
        fail: bool,
    }

    impl HeartbeatTransport for CountingBeat {
        async fn beat(&self, report: HeartbeatReport) -> Result<(), HeartbeatError> {
            if self.fail {
                return Err(HeartbeatError::new("cloud unreachable"));
            }
            *self.last.lock().expect("lock") = Some(report);
            self.beats.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A source that reports a fixed depth, so a test states what the wire should carry.
    struct FixedDepth(Option<u64>);

    impl HeartbeatSource for FixedDepth {
        async fn report(&self) -> HeartbeatReport {
            HeartbeatReport {
                outbox_depth: self.0,
            }
        }
    }

    #[tokio::test]
    async fn beat_once_pings_the_transport() {
        let beats = Arc::new(AtomicUsize::new(0));
        let client = HeartbeatClient::new(
            CountingBeat {
                beats: Arc::clone(&beats),
                ..CountingBeat::default()
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
                fail: true,
                ..CountingBeat::default()
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
                ..CountingBeat::default()
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

    #[tokio::test]
    async fn a_ping_carries_the_reported_outbox_depth() {
        let last = Arc::new(Mutex::new(None));
        let client = HeartbeatClient::reporting(
            CountingBeat {
                last: Arc::clone(&last),
                ..CountingBeat::default()
            },
            FixedDepth(Some(42)),
            Duration::from_secs(60),
        );
        client.beat_once().await.expect("the ping succeeds");
        assert_eq!(
            *last.lock().expect("lock"),
            Some(HeartbeatReport {
                outbox_depth: Some(42)
            }),
            "the depth the source read is the depth the transport sent"
        );
    }

    #[tokio::test]
    async fn a_store_that_cannot_read_its_log_still_pings() {
        // The distinction the whole `Option` exists for: an unreadable log must not become a
        // reported zero (the healthiest possible answer) and must not cost the store its liveness.
        let beats = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(None));
        let client = HeartbeatClient::reporting(
            CountingBeat {
                beats: Arc::clone(&beats),
                last: Arc::clone(&last),
                ..CountingBeat::default()
            },
            FixedDepth(None),
            Duration::from_secs(60),
        );
        client.beat_once().await.expect("the ping succeeds");
        assert_eq!(
            beats.load(Ordering::SeqCst),
            1,
            "the ping still reached the cloud"
        );
        assert_eq!(
            last.lock()
                .expect("lock")
                .expect("a report was sent")
                .outbox_depth,
            None,
            "an unreadable log reports nothing rather than zero"
        );
    }

    #[tokio::test]
    async fn a_client_built_without_a_source_reports_nothing() {
        let last = Arc::new(Mutex::new(None));
        let client = HeartbeatClient::new(
            CountingBeat {
                last: Arc::clone(&last),
                ..CountingBeat::default()
            },
            Duration::from_secs(60),
        );
        client.beat_once().await.expect("the ping succeeds");
        assert_eq!(
            *last.lock().expect("lock"),
            Some(HeartbeatReport::default()),
            "the bootstrap shape is a bare liveness ping"
        );
    }
}
