// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The loop that finally constructs an [`OtaUpdater`] in the shipped binary (roadmap v3 **R5-d**).
//!
//! Everything this drives has existed and been tested for four merged slices — the rollout decision
//! ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)), signature verification
//! ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)), the orchestration
//! ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md)) — and none of it ran, because nothing
//! outside `crates/pos-edge/tests/ota.rs` ever built the updater. This module is that caller.
//!
//! # What one tick is
//!
//! The two facts a rollout is weighed against arrive as published configuration and are already read
//! into the live session by the config-pull loop
//! ([`crate::config_client::session_from_config`]):
//! `fleet_update` (the rollout, plus the revoked keys) and `device_ota` (this box's ring and canary
//! bucket). [`device_state`](crate::ota_state::device_state) turns the placement, the running
//! binary's own version and the durable self-test into the
//! [`DeviceState`](pos_core::ota::DeviceState) the decision compares.
//! Then [`OtaUpdater::run`] decides, and this loop reports the outcome and asks the process to
//! restart when the binary on disk changed.
//!
//! A tick with nothing published, no placement, or a decision of halt/refuse/skip does nothing at
//! all — no fetch, no disk write, no report. That is the state of every store until an operator
//! promotes a release, so it must be quiet.
//!
//! # The boot half, and why it is not part of the tick
//!
//! [ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1 splits the self-test in two: a
//! pre-commit smoke test that gates the swap, and a **boot confirmation** that is the verdict
//! [`decide_rollout`](pos_core::ota::decide_rollout) actually reads, because that rule compares
//! against the version the box is *running* and can therefore only be settled after the restart.
//! [`confirm_boot`] is that half: it clears the unconfirmed marker, records the pass, and reports.
//! It runs once, at start-up, not on a tick.

use core::future::Future;
use core::time::Duration;
use std::sync::Arc;

use pos_core::ota::SelfTest;
use pos_ports::cloud_sync::{CloudSync, UpdateReport};
use pos_ports::signer::Signer;
use pos_proto::ids::StoreId;
use pos_proto::text::ReleaseTag;

use crate::app::Edge;
use crate::ota::{InstallError, OtaUpdater, UpdateInstaller, UpdateOutcome, UpdatePlan};
use crate::lease_state::{LeaseAuthority, standing as lease_standing_for};
use crate::ota_state::{OtaStateAuthority, device_state};

/// How often the loop weighs the published rollout.
///
/// Ten minutes, not the config-pull's thirty seconds. A tick that decides to install is rare — at
/// most once per release per store — and every other tick is pure local arithmetic over a session
/// the config loop refreshed anyway, so a faster cadence buys nothing and a store that has just been
/// added to a ring is not waiting on a deadline.
pub const OTA_POLL_INTERVAL: Duration = Duration::from_secs(600);

/// How the loop asks the process to end so the service manager restarts it into the binary that is
/// now on disk.
///
/// A seam for the same reason every other loop in this crate has one: the restart is the single step
/// a test cannot take, and a loop that called `std::process::exit` directly could not be tested at
/// all. The field implementation flips the shared shutdown watch, which drains in-flight requests
/// exactly as a `SIGTERM` does — so a restart for an update loses no committed sale.
pub trait RestartRequest: Send + Sync {
    /// Asks for a graceful stop. Must be safe to call more than once.
    fn request_restart(&self);
}

impl RestartRequest for tokio::sync::watch::Sender<bool> {
    fn request_restart(&self) {
        let _ignored = self.send(true);
    }
}

impl<T: RestartRequest> RestartRequest for Arc<T> {
    fn request_restart(&self) {
        (**self).request_restart();
    }
}

/// The field [`RestartRequest`], and the one thing the process has to be able to say out loud once
/// the server has drained: **this stop was a restart** (roadmap v3 **E4**).
///
/// A `SIGTERM` and an installed update end the process through the same shutdown watch, because both
/// have to drain in-flight requests the same way — a store must not lose a committed sale to either.
/// On `systemd` that is the whole story: `Restart=always` starts the binary again whatever the exit
/// code was, so the two stops need not be told apart.
///
/// Windows' Service Control Manager is the opposite. A service that reports `SERVICE_STOPPED` with
/// exit code zero has, as far as SCM is concerned, been stopped **on purpose**, and nothing starts
/// it again. A store that installed an update, exited cleanly and never came back would sit dark
/// until somebody drove to the shop — during trading hours, on the strength of a release nobody
/// asked the box to take that minute.
///
/// So the intent is recorded beside the watch rather than inferred from it: the OTA loop flips both,
/// and the process reads [`Self::wanted`] after the drain to decide what to tell the operating
/// system. The flag only ever goes from `false` to `true`, so asking twice is asking once.
#[derive(Debug, Clone)]
pub struct RestartIntent {
    /// Whether a restart was asked for. `Arc` because the OTA loop holds one clone and the process
    /// that reads it after the drain holds another.
    wanted: Arc<core::sync::atomic::AtomicBool>,
    /// The shutdown the server and every background loop drain on.
    shutdown: Arc<tokio::sync::watch::Sender<bool>>,
}

impl RestartIntent {
    /// An intent over the shutdown `shutdown` — nothing asked for yet.
    #[must_use]
    pub fn new(shutdown: Arc<tokio::sync::watch::Sender<bool>>) -> Self {
        Self {
            wanted: Arc::new(core::sync::atomic::AtomicBool::new(false)),
            shutdown,
        }
    }

    /// Whether the stop that is now happening was asked for so the binary on disk can be started.
    ///
    /// `false` for an operator's stop, a `SIGTERM`, or a machine shutdown.
    #[must_use]
    pub fn wanted(&self) -> bool {
        self.wanted.load(core::sync::atomic::Ordering::Relaxed)
    }
}

impl RestartRequest for RestartIntent {
    fn request_restart(&self) {
        // Record the intent *before* asking for the drain. The reader runs after the server stops,
        // so either order is correct today; this order stays correct if a future reader ever looks
        // while the drain is in flight.
        self.wanted
            .store(true, core::sync::atomic::Ordering::Relaxed);
        let _ignored = self.shutdown.send(true);
    }
}

/// What the unconfirmed-boot marker said about this boot.
///
/// Produced by the install seam — on Linux by
/// [`SystemdInstaller::begin_boot`](crate::installer::SystemdInstaller::begin_boot) — and lives here
/// rather than beside it because it describes what a *boot* is, which is this loop's concern; the
/// seam that writes the marker is operating-system code and the Windows one is roadmap E4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootStanding {
    /// No marker: this version has already proved itself, or was never installed over the air.
    /// Nothing to confirm and nothing to revert.
    Settled,
    /// A committed version is on its `attempt`-th boot and has not been healthy yet. The caller
    /// should call [`confirm_boot`] once the store is serving.
    Unconfirmed {
        /// Which attempt this boot is, counting from one.
        attempt: u32,
    },
    /// The allowance was spent. The install seam has already pointed the running binary back at the
    /// previous version and restored the database, so the caller must exit and let the service
    /// manager start it.
    Reverted,
}

/// The confirmation seam for a box with **no update layout** — no `bin/current` to retarget, so
/// nothing here ever installs (production-readiness **R1**).
///
/// It exists so the *boot report* does not have to be conditional on a layout the report does not
/// use. Reporting which binary a store runs is worth doing whether or not that store can install a
/// new one — in fact it is worth *more* there, because those are exactly the boxes an upgrade
/// campaign has to find and lay out by hand, and until this they were the ones the fleet view held
/// `NULL` for.
///
/// Its [`confirm_boot`](BootConfirmation::confirm_boot) is unreachable rather than a no-op that
/// papers over something: the unconfirmed marker is the installer's own file, so a box with no
/// installer settles as [`BootStanding::Settled`] and [`confirm_boot`]'s only marker-clearing branch
/// never runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoUpdateLayout;

impl BootConfirmation for NoUpdateLayout {
    fn confirm_boot(&self) -> Result<(), InstallError> {
        Ok(())
    }
}

/// Clearing the unconfirmed-boot marker: the one thing [`confirm_boot`] needs from the install seam.
///
/// Separate from [`UpdateInstaller`] because it is not part of an install cycle — it is what the
/// *next* process does — and separate from the concrete installer so this module stays free of
/// operating-system code.
pub trait BootConfirmation: Send + Sync {
    /// Records that the running version came up and is trusted from now on. Idempotent.
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the marker could not be cleared. Leaving it would count a healthy
    /// version's ordinary restarts towards a revert.
    fn confirm_boot(&self) -> Result<(), InstallError>;
}

/// What one tick did, for the log and for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// No `fleet_update` node is published: there is no rollout to weigh.
    NoRollout,
    /// No `device_ota` placement, or this binary's own version does not parse — either way the box
    /// cannot be weighed, which [`device_state`] treats as an absence rather than a fault.
    Unweighable,
    /// The durable self-test could not be read, so the box was deliberately not weighed: doing so
    /// would silently drop the rollback rule.
    StateUnreadable,
    /// The lease standing could not be read, so the box was deliberately not weighed
    /// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)). A box that cannot
    /// read its own lease has not established that it is still the store, and weighing it as
    /// `Active` is the exact failure the lease exists to remove — so an unreadable lease refuses,
    /// the same way an unreadable self-test does.
    LeaseUnreadable,
    /// The updater ran and reached a decision.
    Decided(UpdateOutcome),
    /// The updater ran and failed — a fetch, a verification, or an install step.
    Failed,
}

/// The edge OTA loop: an updater over the three seams, the durable self-test authority, the live
/// session the rollout is read from, and the restart the swap needs.
pub struct OtaClient<C, S, I, A, L, St, R> {
    cloud: C,
    updater: OtaUpdater<C, S, I>,
    authority: A,
    lease: L,
    edge: Arc<Edge<St>>,
    store_id: StoreId,
    restart: R,
}

impl<C, S, I, A, L, St, R> core::fmt::Debug for OtaClient<C, S, I, A, L, St, R> {
    /// Names the store and nothing else. The seams hold a socket, a signing key list and a path
    /// layout; none of them belongs in a log line, and the store id is the one field that helps.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OtaClient")
            .field("store_id", &self.store_id)
            .finish_non_exhaustive()
    }
}

impl<C, S, I, A, L, St, R> OtaClient<C, S, I, A, L, St, R>
where
    C: CloudSync + Clone,
    S: Signer,
    I: UpdateInstaller,
    A: OtaStateAuthority,
    L: LeaseAuthority,
    R: RestartRequest,
{
    /// Composes the loop. The `cloud` is held twice — once by the updater for the artifact fetch and
    /// once here for the report — because [`OtaUpdater`] owns its channel and a report is not part
    /// of an update cycle; both halves are the same client, so there is no second connection.
    pub fn new(
        cloud: C,
        signer: S,
        installer: I,
        trusted_keys: Vec<pos_ports::signer::PublicKey>,
        authority: A,
        lease: L,
        edge: Arc<Edge<St>>,
        store_id: StoreId,
        restart: R,
    ) -> Self {
        Self {
            updater: OtaUpdater::new(cloud.clone(), signer, installer, trusted_keys),
            cloud,
            authority,
            lease,
            edge,
            store_id,
            restart,
        }
    }

    /// The durable self-test authority, so [`confirm_boot`] settles the boot through the same one
    /// the loop weighs against rather than a second handle that could disagree.
    pub const fn authority(&self) -> &A {
        &self.authority
    }

    /// Weighs the published rollout once.
    pub async fn tick(&self) -> TickOutcome {
        let session = self.edge.session();
        let Some(rollout) = session.fleet_update.as_ref() else {
            return TickOutcome::NoRollout;
        };
        let device = match device_state(&self.authority, self.store_id, session.device_ota).await {
            Ok(Some(device)) => device,
            Ok(None) => return TickOutcome::Unweighable,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not read this store's last self-test; not weighing the rollout, because \
                     weighing it as though there were none would drop the rollback rule"
                );
                return TickOutcome::StateUnreadable;
            }
        };
        // The release is named by the bare version the rollout publishes — the same string the
        // binary reports and the cloud's artifact registry is keyed by (ADR-0088 Amendment 2).
        let release = ReleaseTag::new(rollout.update.target.to_string());
        let plan = UpdatePlan {
            published: &rollout.update,
            release: &release,
            revoked_keys: &rollout.revoked_keys,
        };
        // The box's real standing (ADR-0108): its own durable held generation, weighed against the
        // authoritative one the cloud published in the `lease` node. `Superseded` and `Invalid` both
        // make the updater refuse — a machine a replacement has taken over must not install
        // anything. A store the cloud has never issued a lease to reads `Active`, unchanged.
        let lease = match lease_standing_for(&self.lease, self.store_id, session.lease_generation)
            .await
        {
            Ok(standing) => standing,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not read this store's lease standing; not weighing the rollout, because \
                     weighing it as active would let a superseded box install"
                );
                return TickOutcome::LeaseUnreadable;
            }
        };
        match self.updater.run(&device, &plan, lease).await {
            Ok(outcome) => {
                self.after(outcome).await;
                TickOutcome::Decided(outcome)
            }
            Err(error) => {
                tracing::warn!(%error, "the update cycle failed; the store keeps running what it has");
                TickOutcome::Failed
            }
        }
    }

    /// Reports an outcome that changed the binary on disk, then asks for the restart that makes it
    /// the running one. The other four outcomes changed nothing and are logged only.
    async fn after(&self, outcome: UpdateOutcome) {
        let report = match outcome {
            // The version is committed and its pre-commit smoke test passed. If it then fails to
            // come up, the boot marker reverts the box and `confirm_boot` reports the correction —
            // so an optimistic report here is self-healing rather than sticky.
            UpdateOutcome::Installed { version } => UpdateReport {
                store: self.store_id,
                installed: ReleaseTag::new(version.to_string()),
                self_test_passed: Some(true),
            },
            // A rollback reverted the symlink, so the version still running is this binary's own —
            // and it stays that way until the restart takes effect.
            UpdateOutcome::RolledBack => UpdateReport {
                store: self.store_id,
                installed: crate::version::tag(),
                self_test_passed: Some(false),
            },
            UpdateOutcome::Halted
            | UpdateOutcome::Refused
            | UpdateOutcome::Skipped(_)
            | UpdateOutcome::ReadOnly => {
                tracing::debug!(?outcome, "nothing to install this tick");
                return;
            }
        };
        if let Err(error) = self.cloud.report(&report).await {
            // A report the cloud never saw is dropped, never a reason to undo an install: the
            // binary on disk has already changed, and the next boot reports again.
            tracing::warn!(%error, "the cloud did not accept the update report");
        }
        tracing::info!(
            ?outcome,
            "restarting into the binary this cycle put on disk"
        );
        self.restart.request_restart();
    }

    /// Ticks every `interval` until `shutdown` resolves.
    pub async fn run(self, interval: Duration, shutdown: impl Future<Output = ()>) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let shutdown = core::pin::pin!(shutdown);
        let mut stopping = shutdown;
        loop {
            tokio::select! {
                _instant = ticker.tick() => {
                    let outcome = self.tick().await;
                    tracing::debug!(?outcome, "ota tick");
                }
                () = &mut stopping => {
                    tracing::info!("ota loop draining");
                    return;
                }
            }
        }
    }
}

/// Settles the boot half: clears the unconfirmed marker, records the durable verdict, and tells the
/// cloud which binary this store is running
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md),
/// [ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1).
///
/// Called once, after the store is serving — reaching that point *is* the confirmation, because a
/// binary that parsed its config, migrated its database and bound its socket is a binary that came
/// up. `standing` is what the install seam said earlier in start-up (on Linux,
/// [`SystemdInstaller::begin_boot`](crate::installer::SystemdInstaller::begin_boot)):
///
/// * [`BootStanding::Unconfirmed`] — an over-the-air install is on trial and has now proved itself.
///   The marker is cleared and `{running version, passed}` is recorded, which is the value the
///   rollback rule reads on every later tick.
/// * [`BootStanding::Reverted`] — the box gave up on a version and put the previous one back. The
///   failure is recorded against the version that failed, which is the one this process is *not*
///   running, so the report carries the bad news without arming a rollback of the binary that works.
/// * [`BootStanding::Settled`] — an ordinary restart. Nothing is recorded, deliberately: overwriting
///   a stored verdict with a pass earned by no install would erase the one fact the console has
///   about a store that failed a release.
///
/// The report is sent in every case, because a report exists chiefly to say which binary a store is
/// running (ADR-0078 Amendment 1) and that is worth knowing from a store's first boot.
pub async fn confirm_boot<C, A, B>(
    cloud: &C,
    authority: &A,
    installer: &B,
    store_id: StoreId,
    standing: BootStanding,
) where
    C: CloudSync,
    A: OtaStateAuthority,
    B: BootConfirmation,
{
    let running = crate::version::released();
    if matches!(standing, BootStanding::Unconfirmed { .. })
        && let Some(version) = running
    {
        if let Err(error) = installer.confirm_boot() {
            tracing::warn!(%error, "could not clear the unconfirmed-boot marker");
        }
        let passed = SelfTest {
            version,
            passed: true,
        };
        if let Err(error) = authority.record_self_test(store_id, passed).await {
            tracing::warn!(%error, "could not record this version's self-test");
        }
        tracing::info!(%version, "this version came up; it is confirmed");
    }

    let self_test_passed = match authority.last_self_test(store_id).await {
        Ok(recorded) => recorded.map(|self_test| self_test.passed),
        Err(error) => {
            tracing::warn!(%error, "could not read the last self-test for the boot report");
            None
        }
    };
    let report = UpdateReport {
        store: store_id,
        installed: crate::version::tag(),
        self_test_passed,
    };
    if let Err(error) = cloud.report(&report).await {
        tracing::warn!(%error, "the cloud did not accept the boot report");
    }
}

#[cfg(test)]
mod tests {
    use super::{OTA_POLL_INTERVAL, RestartRequest, TickOutcome};
    use core::time::Duration;

    #[derive(Debug, Default)]
    struct CountingRestart {
        asked: std::sync::atomic::AtomicUsize,
    }

    impl RestartRequest for CountingRestart {
        fn request_restart(&self) {
            self.asked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn an_untouched_intent_asks_for_nothing() {
        // The ordinary case, and the one that matters most on Windows: an operator's stop must not
        // look like an update's restart, or a service told to stop would come straight back up.
        let (sender, _receiver) = tokio::sync::watch::channel(false);
        let intent = super::RestartIntent::new(std::sync::Arc::new(sender));
        assert!(!intent.wanted(), "nobody has asked for a restart");
    }

    #[test]
    fn a_restart_request_both_drains_and_says_it_was_a_restart() {
        // The two halves have to happen together: the drain is what keeps a committed sale, and the
        // recorded intent is what the process tells the service manager once the drain is done.
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let intent = super::RestartIntent::new(std::sync::Arc::new(sender));
        intent.request_restart();
        assert!(*receiver.borrow(), "the drain was asked for");
        assert!(intent.wanted(), "and it was asked for as a restart");
    }

    #[test]
    fn asking_a_second_time_says_the_same_thing() {
        // The loop reports and then asks; a retry or a second tick must not turn one restart into
        // something else. The flag only ever goes one way.
        let (sender, _receiver) = tokio::sync::watch::channel(false);
        let intent = super::RestartIntent::new(std::sync::Arc::new(sender));
        intent.request_restart();
        intent.request_restart();
        assert!(intent.wanted(), "still exactly one answer: restart me");
    }

    #[test]
    fn a_clone_reads_the_same_intent() {
        // The OTA loop owns one clone and the process that reads the answer after the drain owns
        // another. If they were separate flags the reader would always see `false` and a Windows box
        // would install an update and stay dark.
        let (sender, _receiver) = tokio::sync::watch::channel(false);
        let intent = super::RestartIntent::new(std::sync::Arc::new(sender));
        let held_by_the_loop = intent.clone();
        held_by_the_loop.request_restart();
        assert!(intent.wanted(), "one intent, two handles");
    }

    #[test]
    fn a_watch_sender_is_a_restart_request_and_flipping_it_is_idempotent() {
        // The field implementation: the loop reuses the shutdown the server already drains on, so a
        // restart for an update takes the same graceful path a SIGTERM does.
        let (sender, receiver) = tokio::sync::watch::channel(false);
        sender.request_restart();
        sender.request_restart();
        assert!(*receiver.borrow(), "the drain is asked for, once or twice");
    }

    #[test]
    fn an_arc_wrapped_requester_forwards() {
        let counting = std::sync::Arc::new(CountingRestart::default());
        std::sync::Arc::clone(&counting).request_restart();
        assert_eq!(
            counting.asked.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the loop takes its requester by value, so it must work behind an Arc"
        );
    }

    #[test]
    fn the_loop_ticks_far_slower_than_the_config_pull() {
        // Not a style preference: every tick but the rare installing one is local arithmetic over a
        // session another loop refreshes, so a fast cadence would be pure wake-ups.
        assert!(OTA_POLL_INTERVAL >= Duration::from_secs(300));
    }

    #[test]
    fn nothing_published_and_nothing_weighable_are_distinct_quiet_outcomes() {
        // Both are silent, and the log has to be able to tell an operator which one a store is in:
        // "no release promoted" and "this store is in no ring" need different fixes.
        assert_ne!(TickOutcome::NoRollout, TickOutcome::Unweighable);
        assert_ne!(TickOutcome::Unweighable, TickOutcome::StateUnreadable);
    }
}
