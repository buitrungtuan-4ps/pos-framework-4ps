// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud wakes a store once its configuration is saved
//! ([ADR-0195](../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md) decision 1).
//!
//! Every path that publishes to a store ends in one committed config-tree save, and the save tells
//! [`ConfigWakes::hook`] the version the tree now ends at. The hook never waits: it hands the wake
//! to a queue of [`CONFIG_WAKE_QUEUE`] and returns, so a slow or absent broker never slows a publish.
//! [`run`] drains the queue onto the bus. A wake that does not fit, or that the bus refuses, is
//! logged and counted, and the store still gets its version on the 30-second poll.
//!
//! The counts are the `config_wake` task's health detail: `sent_total`, `dropped_total` (the queue
//! was full) and `failed_total` (the bus refused), each since the cloud started. A tick that
//! dropped or failed a wake reads `ok: false`.

use core::future::Future;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::sync::Arc;

use pos_ports::PortError;
use pos_proto::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId};
use pos_proto::wake::ConfigWake;
use store_postgres::ConfigSaved;
use tokio::sync::mpsc;

/// The most wakes waiting for the bus.
pub const CONFIG_WAKE_QUEUE: usize = 1024;

/// How often the sender records its health.
pub const CONFIG_WAKE_TICK: Duration = Duration::from_secs(30);

/// One wake to send.
type Wake = (StoreId, ConfigVersionId);

/// The running totals the health detail reports.
#[derive(Debug, Default)]
pub struct WakeCounts {
    sent: AtomicU64,
    dropped: AtomicU64,
    failed: AtomicU64,
}

impl WakeCounts {
    /// `(sent, dropped, failed)` since the cloud started.
    #[must_use]
    pub fn totals(&self) -> (u64, u64, u64) {
        (
            self.sent.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            self.failed.load(Ordering::Relaxed),
        )
    }
}

/// The saving side: hands each committed save's wake to the queue.
#[derive(Debug, Clone)]
pub struct ConfigWakes {
    sender: mpsc::Sender<Wake>,
    counts: Arc<WakeCounts>,
}

/// The sending side: the queue [`run`] drains.
#[derive(Debug)]
pub struct WakeQueue {
    receiver: mpsc::Receiver<Wake>,
    counts: Arc<WakeCounts>,
}

impl WakeQueue {
    /// The totals, shared with the saving side.
    #[must_use]
    pub fn counts(&self) -> Arc<WakeCounts> {
        Arc::clone(&self.counts)
    }
}

impl ConfigWakes {
    /// A queue of [`CONFIG_WAKE_QUEUE`] and its two ends.
    #[must_use]
    pub fn channel() -> (Self, WakeQueue) {
        let (sender, receiver) = mpsc::channel(CONFIG_WAKE_QUEUE);
        let counts = Arc::new(WakeCounts::default());
        (
            Self {
                sender,
                counts: Arc::clone(&counts),
            },
            WakeQueue { receiver, counts },
        )
    }

    /// Queues a wake for `store_id` at `version`. Never waits: a full or closed queue drops it.
    pub fn queue(&self, store_id: StoreId, version: ConfigVersionId) {
        if self.sender.try_send((store_id, version)).is_err() {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                %store_id,
                "the config wake queue is full or closed; the store gets its version on the poll"
            );
        }
    }

    /// The hook the config-tree store calls after each committed save.
    #[must_use]
    pub fn hook(&self) -> ConfigSaved {
        let wakes = self.clone();
        ConfigSaved::new(move |_tenant, store_id, version| wakes.queue(store_id, version))
    }
}

/// Sends one wake on the bus.
pub trait WakeSender: Send + Sync {
    /// Sends `wake` to `store_id`.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the bus would not take it.
    fn send(
        &self,
        store_id: StoreId,
        wake: ConfigWake,
    ) -> impl Future<Output = Result<(), PortError>> + Send;
}

impl<F, Fut> WakeSender for F
where
    F: Fn(StoreId, ConfigWake) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), PortError>> + Send,
{
    fn send(
        &self,
        store_id: StoreId,
        wake: ConfigWake,
    ) -> impl Future<Output = Result<(), PortError>> + Send {
        self(store_id, wake)
    }
}

/// Sends queued wakes until `shutdown`, recording the `config_wake` task's health every `tick`
/// ([`CONFIG_WAKE_TICK`] in production).
pub async fn run<W, H, C>(
    mut queue: WakeQueue,
    sender: W,
    (health, clock, tick): (H, C, Duration),
    shutdown: impl Future<Output = ()>,
) where
    W: WakeSender,
    H: crate::health::TaskHealthStore,
    C: ClockSource,
{
    tokio::pin!(shutdown);
    let interval_secs = tick.as_secs();
    let mut tick = tokio::time::interval(tick);
    let mut reported = (0, 0);
    loop {
        tokio::select! {
            () = &mut shutdown => return,
            Some((store_id, config_version_id)) = queue.receiver.recv() => {
                match sender.send(store_id, ConfigWake { config_version_id }).await {
                    Ok(()) => queue.counts.sent.fetch_add(1, Ordering::Relaxed),
                    Err(error) => {
                        tracing::warn!(%store_id, %error, "a config wake was not sent; the store gets its version on the poll");
                        queue.counts.failed.fetch_add(1, Ordering::Relaxed)
                    }
                };
            }
            _ = tick.tick() => {
                let (sent, dropped, failed) = queue.counts.totals();
                let detail = crate::health::tick_detail(
                    (dropped, failed) == reported,
                    interval_secs,
                    serde_json::json!({
                        "sent_total": sent,
                        "dropped_total": dropped,
                        "failed_total": failed,
                    }),
                );
                reported = (dropped, failed);
                if let Err(error) = health
                    .record_tick(crate::health::CONFIG_WAKE, clock.now(), &detail)
                    .await
                {
                    tracing::warn!(%error, "recording config wake task health failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_cloud_api::health::{TaskHealth, TaskHealthError, TaskHealthStore};
    use pos_ports::{PortError, PortName};
    use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use pos_proto::wake::ConfigWake;

    use super::{CONFIG_WAKE_QUEUE, ConfigWakes, run};

    fn store(n: u128) -> StoreId {
        StoreId::new(Ulid::from_u128(n))
    }

    fn version(n: u128) -> ConfigVersionId {
        ConfigVersionId::new(Ulid::from_u128(n))
    }

    #[derive(Default)]
    struct Health(Mutex<Vec<serde_json::Value>>);

    impl TaskHealthStore for &Health {
        async fn record_tick(
            &self,
            task: &str,
            _at: Timestamp,
            detail: &serde_json::Value,
        ) -> Result<(), TaskHealthError> {
            assert_eq!(task, crate::health::CONFIG_WAKE);
            self.0.lock().expect("health").push(detail.clone());
            Ok(())
        }

        async fn list_health(&self) -> Result<Vec<TaskHealth>, TaskHealthError> {
            Ok(Vec::new())
        }
    }

    struct Clock;

    impl pos_proto::ClockSource for Clock {
        fn now(&self) -> Timestamp {
            Timestamp::EPOCH
        }
    }

    /// The hook never waits: past the queue's bound a wake is dropped and counted.
    #[test]
    fn the_hook_queues_and_a_full_queue_drops_and_counts() {
        let (wakes, queue) = ConfigWakes::channel();
        let hook = wakes.hook();
        for n in 0..=CONFIG_WAKE_QUEUE {
            hook.notify(
                TenantId::new(Ulid::from_u128(1)),
                store(2),
                version(u128::try_from(n).expect("small")),
            );
        }
        assert_eq!(queue.counts().totals(), (0, 1, 0), "one past the bound");
    }

    /// Each queued wake goes out naming its version; one the bus refuses is counted, and the health
    /// tick says so.
    #[tokio::test]
    async fn the_sender_sends_each_wake_and_counts_a_refusal() {
        let (wakes, queue) = ConfigWakes::channel();
        let counts = queue.counts();
        let sent: Mutex<Vec<(StoreId, ConfigWake)>> = Mutex::default();
        let health = Health::default();
        wakes.queue(store(2), version(10));
        wakes.queue(store(3), version(11));
        let sender = |store_id: StoreId, wake: ConfigWake| {
            let refused = store_id == store(3);
            sent.lock().expect("sent").push((store_id, wake));
            async move {
                if refused {
                    Err(PortError::unavailable(PortName::StoreWake, "refused"))
                } else {
                    Ok(())
                }
            }
        };
        let tick = core::time::Duration::from_millis(50);
        let stop = tokio::time::sleep(tick * 6);
        run(queue, sender, (&health, Clock, tick), stop).await;

        let sent = sent.lock().expect("sent").clone();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent.first().map(|(_, wake)| wake.config_version_id),
            Some(version(10))
        );
        assert_eq!(counts.totals(), (1, 0, 1));
        let ticks = health.0.lock().expect("health").clone();
        let last = ticks.last().expect("a tick");
        assert_eq!(last["failed_total"], 1);
        assert_eq!(last["sent_total"], 1);
        assert!(
            ticks.iter().any(|tick| tick["ok"] == false),
            "a tick that saw a refusal is not ok: {ticks:?}"
        );
    }
}
