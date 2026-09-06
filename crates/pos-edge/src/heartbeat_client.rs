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
//!
//! # …and the lease generation it holds
//!
//! For the same reason, the ping is where the store says which lease generation it holds
//! ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)). The cloud knows the
//! *authoritative* generation — it issued it — but until the box reports its own, a **split** is
//! invisible: a store that has been replaced and a store that has simply not pulled config yet look
//! identical from the console. Reporting it is what turns "this box refuses to update" into
//! "this box holds 3, the store is on 4", which is the difference between a mystery and a diagnosis.
//! A generation is a counter, not a person, so it carries no personal data either.

use core::future::Future;
use core::time::Duration;
use std::sync::Arc;

use pos_core::lease::LeaseGeneration;
use pos_ports::event_store::EventStore;

use crate::app::Edge;
use crate::lease_state::LeaseAuthority;

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
    /// The lease generation this box holds, or `None` if it has never taken one — a store the cloud
    /// has never issued a lease to, which is every store until an operator does
    /// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)).
    ///
    /// `None` is also what an unreadable authority reports, for the same reason the depth does not
    /// report `0`: the cloud must not be told a generation the box did not actually say.
    pub lease_generation: Option<u64>,
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

/// The store's own event log, read through the [`Edge`] that owns it.
///
/// A log that cannot be read logs a warning and reports `None` rather than failing the heartbeat —
/// liveness is the ping's first job, and a box that stops saying "I am here" because it could not
/// count its outbox would read as offline, which is a worse lie than an unknown depth. The same rule
/// governs every field a report carries.
///
/// This impl reports no lease generation, because an [`Edge`] alone does not hold one; the shipped
/// binary uses [`StoreReport`], which pairs it with the lease authority.
impl<S> HeartbeatSource for Arc<Edge<S>>
where
    S: EventStore + Send + Sync,
{
    async fn report(&self) -> HeartbeatReport {
        HeartbeatReport {
            outbox_depth: outbox_depth(self).await,
            lease_generation: None,
        }
    }
}

/// The shipped source: the store's event log **and** the lease generation it holds.
///
/// Two facts from two places, because they live in two places — the outbox depth is a count over the
/// event log, the held generation is a row the box wrote once and never rewrites
/// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)). Pairing them here
/// keeps the heartbeat one ping rather than two.
#[derive(Debug, Clone)]
pub struct StoreReport<S, L> {
    edge: Arc<Edge<S>>,
    lease: L,
}

impl<S, L> StoreReport<S, L> {
    /// Pairs the store's log with the authority that holds its lease.
    pub const fn new(edge: Arc<Edge<S>>, lease: L) -> Self {
        Self { edge, lease }
    }
}

impl<S, L> HeartbeatSource for StoreReport<S, L>
where
    S: EventStore + Send + Sync,
    L: LeaseAuthority,
{
    async fn report(&self) -> HeartbeatReport {
        let lease_generation = match self.lease.held(self.edge.store_id()).await {
            Ok(held) => held.map(LeaseGeneration::value),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not read the held lease generation; the heartbeat reports none"
                );
                None
            }
        };
        HeartbeatReport {
            outbox_depth: outbox_depth(&self.edge).await,
            lease_generation,
        }
    }
}

/// How far behind the store's event publishing is, or `None` if its log could not be read.
///
/// Deliberately not "0 on failure": zero is the *good* answer, so reporting it for a store whose log
/// is unreadable would report the healthiest possible state for the least healthy one.
async fn outbox_depth<S>(edge: &Arc<Edge<S>>) -> Option<u64>
where
    S: EventStore + Send + Sync,
{
    match edge.store().outbox_depth(edge.store_id()).await {
        Ok(depth) => Some(depth),
        Err(error) => {
            tracing::warn!(%error, "could not read the outbox depth; the heartbeat reports none");
            None
        }
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
    ///
    /// When `shutdown` resolves the loop stops ticking but the task does not end: it waits for
    /// `drained` and then sends one last beat. See [`Self::farewell`] for why that beat is the whole
    /// point of the parameter.
    pub async fn run<F, D>(self, shutdown: F, drained: D)
    where
        F: Future<Output = ()> + Send,
        D: Future<Output = ()> + Send,
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
        self.farewell(drained).await;
    }

    /// The one beat a stopping store sends *after* its last drain, released by `drained`.
    ///
    /// # Why a stop needs a beat of its own
    ///
    /// The loop above breaks the instant shutdown resolves, and the publish loop's last drain
    /// (`event_publish::drain_before_stop`) runs *afterwards*. So without this, the tick that
    /// reported a backlog was always the last thing a cleanly-stopping machine said, and the zero it
    /// went on to achieve was never reported to anyone.
    ///
    /// That zero is not cosmetic. It is the signal the cloud waits for to release a handover: a
    /// superseded generation clears only on a beat carrying **both** a drained outbox and the
    /// generation being superseded
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)). With no beat
    /// after the drain the automatic clear could never fire at all, and every replaced machine would
    /// need a human to confirm what the machine itself already knew.
    ///
    /// # It reports the outbox, not the drain's opinion of the outbox
    ///
    /// `drain_before_stop` returns `()`, and this does not ask it to change. The report is read from
    /// the store the same way every other beat reads it, so a drain that ran out of budget reports
    /// the events it is actually leaving behind rather than a claim about them — and the cloud's
    /// clear, which is guarded on that depth being zero, refuses itself with no error path to plumb.
    /// The durable outbox is the fact; a return value would only be a second, weaker account of it.
    ///
    /// A failure here is logged, never propagated: the beat is the courtesy a stop pays the console,
    /// and a store that cannot pay it simply falls silent, which is what the console already knows
    /// how to read.
    async fn farewell<D>(&self, drained: D)
    where
        D: Future<Output = ()> + Send,
    {
        drained.await;
        let report = self.source.report().await;
        match self.transport.beat(report).await {
            Ok(()) => tracing::info!(
                outbox_depth = report.outbox_depth,
                lease_generation = report.lease_generation,
                "heartbeat: the stopping store reported its final state"
            ),
            Err(error) => tracing::warn!(
                %error,
                "heartbeat: the stopping store could not report its final state; the console will \
                 see it fall silent rather than stop cleanly, and a handover waiting on this beat \
                 stays open for an operator to close"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HeartbeatClient, HeartbeatError, HeartbeatReport, HeartbeatSource, HeartbeatTransport,
    };
    use core::time::Duration;
    use futures_util::poll;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

    /// A source whose depth a test can change between reads — the outbox as the drain sees it.
    struct LiveDepth(Arc<AtomicU64>);

    impl HeartbeatSource for LiveDepth {
        async fn report(&self) -> HeartbeatReport {
            HeartbeatReport {
                outbox_depth: Some(self.0.load(Ordering::SeqCst)),
                lease_generation: Some(4),
            }
        }
    }

    /// A source that reports a fixed depth, so a test states what the wire should carry.
    struct FixedDepth(Option<u64>);

    impl HeartbeatSource for FixedDepth {
        async fn report(&self) -> HeartbeatReport {
            HeartbeatReport {
                outbox_depth: self.0,
                lease_generation: None,
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
    async fn a_stop_ends_the_loop_at_once_and_sends_exactly_one_last_beat() {
        // An already-resolved shutdown must end the loop at once, before the first interval elapses —
        // so the test never waits on a real timer. The single beat that follows is the farewell, not
        // a tick: an hour-long interval cannot have elapsed in a test that returns immediately.
        let beats = Arc::new(AtomicUsize::new(0));
        let client = HeartbeatClient::new(
            CountingBeat {
                beats: Arc::clone(&beats),
                ..CountingBeat::default()
            },
            Duration::from_secs(3600),
        );
        client.run(async {}, async {}).await;
        assert_eq!(
            beats.load(Ordering::SeqCst),
            1,
            "shutdown wins over the first tick, and the only ping is the one a stop owes the console"
        );
    }

    #[tokio::test]
    async fn the_last_beat_is_sent_after_the_drain_and_reports_what_the_drain_achieved() {
        // The bug this guards, and the reason the drain gets its own signal: beating at the stop
        // reports the backlog the drain is about to clear, and the cloud releases a handover only on
        // a beat carrying a *zero* (ADR-0110). A beat one moment too early keeps every replaced
        // machine's handover open forever.
        let beats = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(None));
        let depth = Arc::new(AtomicU64::new(7));
        let client = HeartbeatClient::reporting(
            CountingBeat {
                beats: Arc::clone(&beats),
                last: Arc::clone(&last),
                fail: false,
            },
            LiveDepth(Arc::clone(&depth)),
            Duration::from_secs(3600),
        );

        let (drained_tx, drained_rx) = tokio::sync::oneshot::channel::<()>();
        let run = client.run(async {}, async move {
            let _ignored = drained_rx.await;
        });
        tokio::pin!(run);

        // One poll takes the loop all the way to waiting on the drain. The stop has landed; the
        // outbox still holds seven events; nothing may have been said yet.
        assert!(
            poll!(&mut run).is_pending(),
            "the loop waits for the drain rather than ending at the stop"
        );
        assert_eq!(
            beats.load(Ordering::SeqCst),
            0,
            "no beat reported the backlog the drain was still working on"
        );

        // The drain empties the outbox and then releases the beat, in that order.
        depth.store(0, Ordering::SeqCst);
        drained_tx
            .send(())
            .expect("the loop still holds the receiver");
        run.await;

        assert_eq!(beats.load(Ordering::SeqCst), 1, "exactly one last beat");
        assert_eq!(
            *last.lock().expect("lock"),
            Some(HeartbeatReport {
                outbox_depth: Some(0),
                lease_generation: Some(4),
            }),
            "the last beat carries the drained zero and the generation this box held — the two \
             facts the cloud's clear is keyed on"
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
                outbox_depth: Some(42),
                lease_generation: None,
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
