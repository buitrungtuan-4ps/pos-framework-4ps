// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The relay's wake: how a waiter learns that the row it is parked on now exists
//! ([ADR-0062](../../../docs/adr/0062-the-relay-wake.md)).
//!
//! # Why this exists
//!
//! [ADR-0061](../../../docs/adr/0061-order-relay.md) built the order relay on two loops that each
//! re-read PostgreSQL every 100 ms — the caller's park waiting for the store's outcome, and the
//! store's long-poll waiting for an order. The second one costs about **ten queries a second per
//! store, for as long as the store is switched on**, whether it sells anything or not.
//!
//! The cloud does not need to ask. It is the process that writes the row, so it already knows. This
//! module is that knowledge, made explicit and testable: a writer calls [`RelayWake::queued`] or
//! [`RelayWake::reported`] after its write commits, and a waiter parks on [`RelayWake::wait_queued`]
//! or [`RelayWake::wait_reported`] instead of on a timer.
//!
//! # The wake is never the correctness argument
//!
//! The row in PostgreSQL stays the only source of truth. Every waiter re-reads after it wakes, and
//! every waiter also has a **fallback timer**, so a signal that is lost, coalesced or never sent
//! degrades to exactly the behaviour ADR-0061 shipped — slower, never wrong. That is why
//! [`Woke::Timeout`] is not an error: it is the ordinary path when nothing happened.
//!
//! # Subscribe before you read
//!
//! A waiter takes its subscription *first*, then reads, then waits. The window between "I read and
//! found nothing" and "I began waiting" is exactly where a signal goes missing, and on the submit leg
//! a missed signal is a `503` for an order the store did accept. [`SharedWake::subscribe_queued`]
//! and [`SharedWake::subscribe_reported`] exist to make that order the natural one to write.
//!
//! # Why in-process, and what a second instance would need
//!
//! `deploy/compose.yml` runs one container per service and every deployment in `k8s/` declares
//! `replicas: 1`, so every writer and every waiter are in the same process and an in-process
//! broadcast is exactly right. A multi-instance cloud needs the signal to cross processes — PostgreSQL `LISTEN`/`NOTIFY`
//! is the obvious implementation — and that is a second implementor of this trait, not a change to
//! the relay. Whoever writes it inherits an obligation ADR-0062 records: a cross-process listener can
//! die quietly and move the whole fleet to the fallback timer, so it needs a tick on the
//! background-task health surface and an alert. An in-process notify cannot fail without the process
//! failing, which is the main reason it is what ships today.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pos_proto::ids::{StoreId, TenantId};
use tokio::sync::broadcast;

use crate::relay::OrderQueueId;

/// Why a waiter stopped waiting.
///
/// Both are ordinary. A caller re-reads the row either way, so this exists for tests and for the
/// tracing that tells an operator whether the wake or the timer is doing the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    /// A writer signalled.
    Signalled,
    /// The fallback timer expired first — the ADR-0061 behaviour, and not a failure.
    Timeout,
}

/// The signal a relay waiter parks on, and the writers that raise it.
///
/// **Two waiter classes, two signals.** A parked `submit` waits for an *outcome* on one specific
/// order; a long-polling store waits for an *enqueue* on any of its orders. A single shared
/// notification would wake the wrong one and leave the right one asleep until its fallback fired, so
/// they are kept apart at the trait.
pub trait RelayWake: Send + Sync {
    /// The subscription a store's long-poll holds while it reads and waits.
    type QueuedWait: Send;
    /// The subscription a parked `submit` holds while it reads and waits.
    type ReportedWait: Send;

    /// Begins listening for the next enqueue against `store`, before reading.
    fn subscribe_queued(&self, tenant: TenantId, store: StoreId) -> Self::QueuedWait;

    /// Begins listening for the next outcome against `queued_id`, before reading.
    fn subscribe_reported(&self, tenant: TenantId, queued_id: OrderQueueId) -> Self::ReportedWait;

    /// Signals that `store` has at least one newly queued order. Called after the write commits.
    fn queued(&self, tenant: TenantId, store: StoreId);

    /// Signals that a store reported an outcome for `queued_id`. Called after the write commits.
    fn reported(&self, tenant: TenantId, queued_id: OrderQueueId);

    /// Waits on a subscription taken earlier, giving up after `timeout`.
    fn wait_queued(
        &self,
        subscription: Self::QueuedWait,
        timeout: Duration,
    ) -> impl Future<Output = Woke> + Send;

    /// Waits on a subscription taken earlier, giving up after `timeout`.
    fn wait_reported(
        &self,
        subscription: Self::ReportedWait,
        timeout: Duration,
    ) -> impl Future<Output = Woke> + Send;
}

/// The key a waiter is registered under. Per-store for enqueues, per-order for outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WakeKey {
    Queued(TenantId, StoreId),
    Reported(TenantId, OrderQueueId),
}

/// A live subscription. Dropping it releases the map entry when it was the last holder.
///
/// It is a `broadcast::Receiver` rather than a `Notify` future for one reason, and it is the whole
/// point of the type: a broadcast receiver buffers from **the moment it subscribes**, whereas
/// `Notify::notify_waiters` stores no permit and wakes only waiters already parked. With `Notify` a
/// signal landing between `subscribe` and the first poll of the wait would be lost — exactly the
/// window "subscribe before you read" exists to close.
#[derive(Debug)]
pub struct Subscription {
    key: WakeKey,
    receiver: broadcast::Receiver<()>,
    sender: Arc<broadcast::Sender<()>>,
    owner: Arc<InProcessWakeInner>,
}

/// The shared half, so a [`Subscription`] can clean up after itself without borrowing the wake.
#[derive(Debug, Default)]
pub struct InProcessWakeInner {
    waiters: Mutex<HashMap<WakeKey, Arc<broadcast::Sender<()>>>>,
}

/// How many signals a subscription buffers before it starts reporting a lag.
///
/// One is enough: every value is `()`, so "you missed some" and "there is one" mean the same thing to
/// a waiter that re-reads the row anyway. A lagged receiver is therefore treated as a wake, not an
/// error.
const WAKE_BUFFER: usize = 1;

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut waiters = match self.owner.waiters.lock() {
            Ok(guard) => guard,
            // A poisoned lock means another thread panicked holding it. Leaking one map entry is a
            // better outcome than panicking in a destructor.
            Err(poisoned) => poisoned.into_inner(),
        };
        // Two references remain when this is the last subscription: the map's and ours.
        if Arc::strong_count(&self.sender) <= 2 {
            waiters.remove(&self.key);
        }
    }
}

/// The shipped [`RelayWake`]: one broadcast channel per key, in this process.
///
/// Shared by the two halves of the relay — `OrderRelay` parks on it, the store-facing router waits
/// on it — so it holds its state behind an `Arc` and clones cheaply.
///
/// # The map is bounded by waiters, not by orders
///
/// A per-order key would otherwise accumulate one dead entry per order forever. Entries are
/// reference-counted by the subscriptions holding them and removed when the last one is dropped, so
/// the map's size tracks *waiters in flight*, never orders processed. A [`Subscription`] therefore
/// owns its `Arc` for its whole life — that is what the associated `Wait` types are for.
#[derive(Debug, Default)]
pub struct SharedWake {
    inner: Arc<InProcessWakeInner>,
}

impl SharedWake {
    /// A wake with no waiters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&self, key: WakeKey) -> Subscription {
        let mut waiters = match self.inner.waiters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let sender = Arc::clone(
            waiters
                .entry(key)
                .or_insert_with(|| Arc::new(broadcast::Sender::new(WAKE_BUFFER))),
        );
        Subscription {
            key,
            receiver: sender.subscribe(),
            sender,
            owner: Arc::clone(&self.inner),
        }
    }

    fn signal(&self, key: WakeKey) {
        let waiters = match self.inner.waiters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(sender) = waiters.get(&key) {
            // Every waiter on this key is woken: several devices can long-poll the same store, and
            // several callers can park on the same order. `send` fails only when nobody is
            // subscribed, which is the ordinary case for a store with no one waiting.
            let _woken = sender.send(());
        }
    }
}

impl Clone for SharedWake {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl RelayWake for SharedWake {
    type QueuedWait = Subscription;
    type ReportedWait = Subscription;

    fn subscribe_queued(&self, tenant: TenantId, store: StoreId) -> Subscription {
        self.entry(WakeKey::Queued(tenant, store))
    }

    fn subscribe_reported(&self, tenant: TenantId, queued_id: OrderQueueId) -> Subscription {
        self.entry(WakeKey::Reported(tenant, queued_id))
    }

    fn queued(&self, tenant: TenantId, store: StoreId) {
        self.signal(WakeKey::Queued(tenant, store));
    }

    fn reported(&self, tenant: TenantId, queued_id: OrderQueueId) {
        self.signal(WakeKey::Reported(tenant, queued_id));
    }

    async fn wait_queued(&self, subscription: Subscription, timeout: Duration) -> Woke {
        wait_on(subscription, timeout).await
    }

    async fn wait_reported(&self, subscription: Subscription, timeout: Duration) -> Woke {
        wait_on(subscription, timeout).await
    }
}

/// Waits for the subscription's notification or the timer, whichever lands first.
///
/// The subscription is held across the await and dropped here, which is what keeps its map entry
/// alive for exactly as long as someone is waiting on it.
async fn wait_on(mut subscription: Subscription, timeout: Duration) -> Woke {
    match tokio::time::timeout(timeout, subscription.receiver.recv()).await {
        // Lagged means more signals arrived than the buffer holds. Every value is `()`, so that is
        // still "something happened" — and the caller re-reads the row either way.
        Ok(Ok(()) | Err(broadcast::error::RecvError::Lagged(_))) => Woke::Signalled,
        // Closed cannot happen while this subscription holds an `Arc` of the sender, but treating it
        // as a timeout keeps the fallback path the answer to anything unexpected.
        Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => Woke::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::{RelayWake, SharedWake, Woke};
    use core::time::Duration;
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::ulid::Ulid;

    use crate::relay::OrderQueueId;

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(2))
    }

    fn order() -> OrderQueueId {
        OrderQueueId::new(Ulid::from_u128(3))
    }

    #[tokio::test]
    async fn a_signal_after_the_subscription_wakes_the_waiter() {
        let wake = SharedWake::new();
        let subscription = wake.subscribe_queued(tenant(), store());
        let signaller = wake.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            signaller.queued(tenant(), store());
        });
        assert_eq!(
            wake.wait_queued(subscription, Duration::from_secs(5)).await,
            Woke::Signalled
        );
    }

    #[tokio::test]
    async fn nothing_happening_times_out_rather_than_failing() {
        // The ADR-0061 path, and the reason a lost signal is slow rather than wrong.
        let wake = SharedWake::new();
        let subscription = wake.subscribe_queued(tenant(), store());
        assert_eq!(
            wake.wait_queued(subscription, Duration::from_millis(10))
                .await,
            Woke::Timeout
        );
    }

    #[tokio::test]
    async fn the_two_waiter_classes_do_not_wake_each_other() {
        // The whole reason `queued` and `reported` are separate signals: one shared notification
        // would wake the long-poll and leave the parked submit asleep until its fallback fired.
        let wake = SharedWake::new();
        let parked = wake.subscribe_reported(tenant(), order());
        wake.queued(tenant(), store());
        assert_eq!(
            wake.wait_reported(parked, Duration::from_millis(10)).await,
            Woke::Timeout,
            "an enqueue on the store must not resolve a park waiting on an outcome"
        );
    }

    #[tokio::test]
    async fn a_signal_for_another_store_does_not_wake_this_one() {
        let wake = SharedWake::new();
        let other = StoreId::new(Ulid::from_u128(99));
        let subscription = wake.subscribe_queued(tenant(), store());
        wake.queued(tenant(), other);
        assert_eq!(
            wake.wait_queued(subscription, Duration::from_millis(10))
                .await,
            Woke::Timeout
        );
    }

    #[tokio::test]
    async fn every_waiter_on_a_key_is_woken_not_just_one() {
        // Several devices can long-poll one store, and several callers can park on one order.
        let wake = SharedWake::new();
        let first = wake.subscribe_queued(tenant(), store());
        let second = wake.subscribe_queued(tenant(), store());
        let signaller = wake.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            signaller.queued(tenant(), store());
        });
        let (one, two) = tokio::join!(
            wake.wait_queued(first, Duration::from_secs(5)),
            wake.wait_queued(second, Duration::from_secs(5)),
        );
        assert_eq!((one, two), (Woke::Signalled, Woke::Signalled));
    }

    #[test]
    fn a_dropped_subscription_releases_its_map_entry() {
        // The per-order key would otherwise accumulate one dead entry per order, forever.
        let wake = SharedWake::new();
        {
            let _subscription = wake.subscribe_reported(tenant(), order());
            assert_eq!(wake.inner.waiters.lock().expect("lock").len(), 1);
        }
        assert_eq!(
            wake.inner.waiters.lock().expect("lock").len(),
            0,
            "the map tracks waiters in flight, never orders processed"
        );
    }

    #[test]
    fn two_subscriptions_on_one_key_share_an_entry_and_the_last_drop_clears_it() {
        let wake = SharedWake::new();
        let first = wake.subscribe_queued(tenant(), store());
        let second = wake.subscribe_queued(tenant(), store());
        assert_eq!(wake.inner.waiters.lock().expect("lock").len(), 1);
        drop(first);
        assert_eq!(
            wake.inner.waiters.lock().expect("lock").len(),
            1,
            "a waiter is still parked on this key"
        );
        drop(second);
        assert_eq!(wake.inner.waiters.lock().expect("lock").len(), 0);
    }
}
