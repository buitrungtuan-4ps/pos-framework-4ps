// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The pull on a wake ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)
//! decision 2).
//!
//! The store pulls when the bus says its configuration changed, when the bus (re)connects, and on
//! the poll, which is unchanged. The pull is the one [`ConfigClient::pump_once`] the poll runs:
//!
//! - **One at a time.** The loop pulls, then waits for the next reason, so two pulls never overlap.
//!   Wakes heard while a pull runs wait in the subscription and are read after it; one naming the
//!   version the store holds is skipped.
//! - **At most one wake-driven pull per [`WAKE_PULL_SPACING`].** A wake inside the spacing waits it
//!   out, and every signal heard while it waits is answered by the same pull, so a burst of wakes
//!   costs one more pull, and a flood, forged or not, one cheap `up_to_date` answer a second.
//! - **A dead bus keeps the poll.** A wait that fails is retried after [`WAKE_RETRY`] within the
//!   poll's own schedule, said once when the bus goes and once when it comes back.
//!
//! A wake-driven apply logs `wake_to_apply_ms`, the time from hearing the wake to applying its
//! version (decision 6): a version id and a duration, nothing personal. The time is also kept in
//! [`Edge::config_wake_standing`](crate::app::Edge::config_wake_standing), with whether the store
//! hears wakes at all (`Connected` says it does, a failing bus that it is on the poll), for the
//! heartbeat.

use pos_edge_app::app::ConfigDelivery;
use pos_ports::config_store::ConfigStore;
use pos_ports::event_store::EventStore;
use pos_ports::store_wake::{StoreWake, WakeSignal};
use tokio::time::{Duration, Instant};

use super::{ConfigClient, ConfigTransport, RETRY_BACKOFF};

/// The shortest gap between two wake-driven pulls.
pub const WAKE_PULL_SPACING: Duration = Duration::from_secs(1);

/// How long a failed wait on the bus rests before it is tried again.
pub const WAKE_RETRY: Duration = Duration::from_secs(5);

/// Why the loop pulls next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// The poll interval passed.
    Poll,
    /// The bus connected, or connected again.
    Connected,
    /// A wake named a version this store does not hold, heard at this instant.
    Wake(Instant),
}

/// Whether the bus answered the last wait; kept so a dead bus is said once, not every retry.
#[derive(Debug, Default)]
struct Bus {
    down: bool,
}

impl<T, S> ConfigClient<T, S>
where
    T: ConfigTransport,
    S: EventStore + ConfigStore,
{
    /// Runs the config-pull loop with `wake` until `shutdown` resolves: [`Self::run`]'s poll, and a
    /// pull as soon as the cloud wakes this store or the bus (re)connects.
    pub async fn run_waking<W, F>(self, wake: W, poll_interval: Duration, shutdown: F)
    where
        W: StoreWake,
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        let mut bus = Bus::default();
        let mut last_wake_pull: Option<Instant> = None;
        let mut heard_at: Option<Instant> = None;
        loop {
            let pulled = tokio::select! {
                () = &mut shutdown => break,
                pulled = self.pump_once() => pulled,
            };
            let wait = match pulled {
                Ok(applied) => {
                    if let (Some(heard), Some(version)) = (heard_at, applied) {
                        let wake_to_apply_ms =
                            u64::try_from(heard.elapsed().as_millis()).unwrap_or(u64::MAX);
                        self.edge
                            .config_wake_standing()
                            .record_apply_ms(wake_to_apply_ms);
                        tracing::info!(
                            config_version_id = %version,
                            wake_to_apply_ms,
                            "applied the configuration a wake announced"
                        );
                    }
                    poll_interval
                }
                Err(error) => {
                    tracing::warn!(%error, "config pull failed; backing off");
                    RETRY_BACKOFF
                }
            };
            heard_at = None;
            let deadline = Instant::now() + wait;
            let reason = tokio::select! {
                () = &mut shutdown => break,
                reason = self.next_reason(&wake, deadline, &mut bus) => reason,
            };
            if reason == Reason::Poll {
                continue;
            }
            if let Some(ready) = last_wake_pull.map(|last| last + WAKE_PULL_SPACING) {
                tokio::select! {
                    () = &mut shutdown => break,
                    () = absorb_until(&wake, ready) => {}
                }
            }
            last_wake_pull = Some(Instant::now());
            if let Reason::Wake(at) = reason {
                heard_at = Some(at);
            }
        }
    }

    /// Waits on `wake` until a reason to pull, or `deadline` for the poll.
    async fn next_reason<W: StoreWake>(
        &self,
        wake: &W,
        deadline: Instant,
        bus: &mut Bus,
    ) -> Reason {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Reason::Poll;
            }
            match wake.next_signal(left).await {
                Ok(Some(WakeSignal::Connected)) => {
                    self.edge
                        .config_wake_standing()
                        .set_delivery(ConfigDelivery::Wake);
                    if std::mem::take(&mut bus.down) {
                        tracing::info!("the config wake bus is back; pulling to catch up");
                    }
                    return Reason::Connected;
                }
                Ok(Some(WakeSignal::Wake(Some(heard))))
                    if self.held().as_deref()
                        == Some(heard.config_version_id.to_string().as_str()) => {}
                Ok(Some(WakeSignal::Wake(_))) => return Reason::Wake(Instant::now()),
                // A wait that ends early has nothing to say before the poll.
                Ok(None) => {
                    tokio::time::sleep_until(deadline).await;
                    return Reason::Poll;
                }
                Err(error) => {
                    self.edge
                        .config_wake_standing()
                        .set_delivery(ConfigDelivery::Poll);
                    if !std::mem::replace(&mut bus.down, true) {
                        tracing::warn!(%error, "the config wake bus is unavailable; the poll carries on");
                    }
                    tokio::time::sleep(left.min(WAKE_RETRY)).await;
                }
            }
        }
    }
}

/// Reads and drops every signal until `ready`: the pull about to run answers them all, so a burst
/// of wakes waiting out the spacing becomes one pull.
async fn absorb_until<W: StoreWake>(wake: &W, ready: Instant) {
    loop {
        let left = ready.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        if !matches!(wake.next_signal(left).await, Ok(Some(_))) {
            tokio::time::sleep_until(ready).await;
            return;
        }
    }
}
