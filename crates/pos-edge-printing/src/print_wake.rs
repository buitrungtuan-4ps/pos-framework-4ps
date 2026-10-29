// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How a parked agent learns that a job it may claim now exists
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! # The same shape [ADR-0062](../../../docs/adr/0062-the-relay-wake.md) settled, one tier down
//!
//! The process that writes the queue row is the process the agent is parked in — a store runs one
//! edge, and [ADR-0049](../../../docs/adr/0049-single-active-lease.md) is the mechanism that
//! guarantees it — so nothing needs to poll to discover a write it performed itself. The dispatcher
//! enqueues, commits, and signals; the parked `GET /api/print/jobs` wakes and re-reads.
//!
//! # The wake is never the correctness argument
//!
//! The row in SQLite is the only source of truth. The parked request also returns on a timer, and
//! re-reads after either, so a signal that is lost or coalesced degrades to a slower answer and
//! never a wrong one. [`Woke::Timeout`] is the ordinary path when nothing happened, not an error.
//!
//! # Subscribe before you read, which is why this is a broadcast and not a `Notify`
//!
//! A waiter takes its subscription *first*, then reads the queue, then waits. The window between "I
//! read and found nothing" and "I began waiting" is exactly where a signal goes missing, and here a
//! missed signal is a ticket sitting in the queue for the whole park while a guest waits at a table.
//!
//! ADR-0112 sketches this as `tokio::sync::Notify`. A bare `Notify` cannot honour the ADR's own
//! subscribe-before-you-read rule: `notify_waiters` stores no permit and wakes only waiters already
//! parked, so a signal landing between subscribing and the first poll is lost. A
//! `broadcast::Receiver` buffers from the moment it subscribes, which is the property that rule
//! needs — the same reasoning [`pos_cloud::wake`] wrote down when it made the identical choice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pos_proto::ids::DeviceId;
use tokio::sync::broadcast;

/// Why a waiter stopped waiting.
///
/// Both are ordinary: the caller re-reads either way. This exists for tests, and for the tracing
/// that tells an operator whether the wake or the timer is doing the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    /// The dispatcher signalled after an enqueue committed.
    Signalled,
    /// The fallback timer expired first — not a failure.
    Timeout,
}

/// How many signals a subscription buffers before it reports a lag.
///
/// One is enough: every value is `()`, so "you missed some" and "there is one" mean the same to a
/// waiter that re-reads the queue anyway. A lagged receiver is therefore treated as a wake.
const WAKE_BUFFER: usize = 1;

/// The signal a parked agent waits on, and the enqueue that raises it.
///
/// One class of waiter and one signal, unlike the relay's two: an acknowledgement is a direct call
/// and needs no notification, so there is nothing else to wake.
pub trait PrintWake: Send + Sync {
    /// The subscription a parked agent holds while it reads and waits.
    type Wait: Send;

    /// Begins listening for the next enqueue against `agent`, **before** reading the queue.
    fn subscribe(&self, agent: DeviceId) -> Self::Wait;

    /// Signals that `agent` has at least one newly queued job. Called after the write commits.
    fn queued(&self, agent: DeviceId);

    /// Waits on a subscription taken earlier, giving up after `timeout`.
    fn wait(
        &self,
        subscription: Self::Wait,
        timeout: Duration,
    ) -> impl Future<Output = Woke> + Send;
}

/// The shared half, so a [`Subscription`] can release its map entry without borrowing the wake.
#[derive(Debug, Default)]
pub struct WakeInner {
    waiters: Mutex<HashMap<DeviceId, Arc<broadcast::Sender<()>>>>,
}

/// A live subscription. Dropping it releases the map entry when it was the last holder.
#[derive(Debug)]
pub struct Subscription {
    agent: DeviceId,
    receiver: broadcast::Receiver<()>,
    sender: Arc<broadcast::Sender<()>>,
    owner: Arc<WakeInner>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut waiters = match self.owner.waiters.lock() {
            Ok(guard) => guard,
            // A poisoned lock means another thread panicked holding it. Leaking one map entry beats
            // panicking in a destructor.
            Err(poisoned) => poisoned.into_inner(),
        };
        // Two references remain when this is the last subscription: the map's and ours.
        if Arc::strong_count(&self.sender) <= 2 {
            waiters.remove(&self.agent);
        }
    }
}

/// The shipped [`PrintWake`]: one broadcast channel per agent, in this process.
///
/// The map is bounded by *waiters in flight*, never by agents ever seen: entries are
/// reference-counted by the subscriptions holding them and removed when the last one drops.
#[derive(Debug, Default)]
pub struct SharedPrintWake {
    inner: Arc<WakeInner>,
}

impl SharedPrintWake {
    /// A wake with no waiters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clone for SharedPrintWake {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl PrintWake for SharedPrintWake {
    type Wait = Subscription;

    fn subscribe(&self, agent: DeviceId) -> Subscription {
        let mut waiters = match self.inner.waiters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let sender = Arc::clone(
            waiters
                .entry(agent)
                .or_insert_with(|| Arc::new(broadcast::Sender::new(WAKE_BUFFER))),
        );
        Subscription {
            agent,
            receiver: sender.subscribe(),
            sender,
            owner: Arc::clone(&self.inner),
        }
    }

    fn queued(&self, agent: DeviceId) {
        let waiters = match self.inner.waiters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(sender) = waiters.get(&agent) {
            // `send` fails only when nobody is subscribed, which is the ordinary case for an agent
            // that is not currently parked.
            let _woken = sender.send(());
        }
    }

    async fn wait(&self, mut subscription: Subscription, timeout: Duration) -> Woke {
        match tokio::time::timeout(timeout, subscription.receiver.recv()).await {
            // Lagged means more signals arrived than the buffer holds. Every value is `()`, so that
            // is still "something happened" — and the caller re-reads the queue either way.
            Ok(Ok(()) | Err(broadcast::error::RecvError::Lagged(_))) => Woke::Signalled,
            // Closed cannot happen while this subscription holds an `Arc` of the sender; treating it
            // as a timeout keeps the fallback the answer to anything unexpected.
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => Woke::Timeout,
        }
    }
}

/// Shared by delegation, so the dispatcher that signals and the route that parks hold one wake.
impl<T: PrintWake + ?Sized> PrintWake for Arc<T> {
    type Wait = T::Wait;

    fn subscribe(&self, agent: DeviceId) -> Self::Wait {
        (**self).subscribe(agent)
    }

    fn queued(&self, agent: DeviceId) {
        (**self).queued(agent);
    }

    fn wait(
        &self,
        subscription: Self::Wait,
        timeout: Duration,
    ) -> impl Future<Output = Woke> + Send {
        (**self).wait(subscription, timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::{PrintWake, SharedPrintWake, Woke};
    use core::time::Duration;
    use pos_proto::ids::DeviceId;
    use pos_proto::ulid::Ulid;

    fn agent(seed: u128) -> DeviceId {
        DeviceId::new(Ulid::from_u128(seed))
    }

    #[tokio::test]
    async fn a_signal_after_the_subscription_wakes_the_waiter() {
        let wake = SharedPrintWake::new();
        let subscription = wake.subscribe(agent(1));
        let signaller = wake.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            signaller.queued(agent(1));
        });
        assert_eq!(
            wake.wait(subscription, Duration::from_secs(5)).await,
            Woke::Signalled
        );
    }

    #[tokio::test]
    async fn a_signal_between_subscribing_and_waiting_is_not_lost() {
        // The whole reason this is a broadcast rather than a `Notify`. The dispatcher enqueues in
        // the window after the handler subscribed and before it parked; a `Notify` stores no permit
        // and the ticket would sit in the queue for the whole park while a guest waits.
        let wake = SharedPrintWake::new();
        let subscription = wake.subscribe(agent(1));
        wake.queued(agent(1));
        assert_eq!(
            wake.wait(subscription, Duration::from_millis(10)).await,
            Woke::Signalled
        );
    }

    #[tokio::test]
    async fn nothing_happening_times_out_rather_than_failing() {
        // The fallback path, and the reason a lost signal is slow rather than wrong.
        let wake = SharedPrintWake::new();
        let subscription = wake.subscribe(agent(1));
        assert_eq!(
            wake.wait(subscription, Duration::from_millis(10)).await,
            Woke::Timeout
        );
    }

    #[tokio::test]
    async fn a_signal_for_another_agent_does_not_wake_this_one() {
        // Two terminals in one shop: a receipt queued for the counter must not wake the kitchen's
        // agent into a read that finds nothing.
        let wake = SharedPrintWake::new();
        let subscription = wake.subscribe(agent(1));
        wake.queued(agent(2));
        assert_eq!(
            wake.wait(subscription, Duration::from_millis(10)).await,
            Woke::Timeout
        );
    }

    #[test]
    fn a_dropped_subscription_releases_its_map_entry() {
        // The map tracks waiters in flight, never agents ever seen.
        let wake = SharedPrintWake::new();
        {
            let _subscription = wake.subscribe(agent(1));
            assert_eq!(wake.inner.waiters.lock().expect("lock").len(), 1);
        }
        assert_eq!(wake.inner.waiters.lock().expect("lock").len(), 0);
    }
}
