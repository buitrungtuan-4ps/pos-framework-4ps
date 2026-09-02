// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's durable OTA state, and the [`DeviceState`] a rollout decision is made against
//! ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md),
//! [ADR-0052](../../../docs/adr/0052-ota-rollout-config.md)).
//!
//! `pos_core::ota::decide_rollout` puts one rule above every other — above even the kill switch: a
//! device whose *running* version failed its self-test must revert. The fact that rule reads,
//! `DeviceState.last_self_test`, was process memory, and an install **deliberately restarts the
//! edge** ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md)). So the fleet's
//! highest-precedence safety rule depended on the one fact a reboot destroys — the very reboot it
//! exists to recover from. A box that installed a bad build, failed its self-test and restarted came
//! back with no memory of failing, weighed itself against the same rollout, and was eligible to
//! install the same bad build again.
//!
//! # Why this is not a port
//!
//! [`OtaStateAuthority`] is a trait in `pos-edge`, not in `pos-ports`, and that follows the category
//! [`QueueNumberAuthority`](crate::queue::QueueNumberAuthority) and
//! [`ReceiptAuthority`](crate::receipt::ReceiptAuthority) already occupy: durable *edge-local*
//! bookkeeping, implemented for [`store_sqlite::SqliteStore`] over its public API, with an
//! in-memory twin for tests and its own additive migration.
//!
//! A port ([ADR-0026](../../../docs/adr/0026-port-shapes.md),
//! [ADR-0021](../../../docs/adr/0021-corrected-port-list.md)) is a boundary the *domain* crosses and
//! a vendor could sit behind — it earns a `PortName`, a contract suite, and a `pos-fakes`
//! implementation. Nothing swaps in a different self-test store, and `store-postgres` has no reason
//! to hold one: the cloud learns a store's self-test through
//! [`CloudSync::report`](pos_ports::CloudSync::report)
//! ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)), not by sharing a table.
//!
//! [ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md) argues that a trait `store-sqlite`
//! implements *must* live in `pos-ports`, because every adapter depends on exactly `pos-proto` and
//! `pos-ports`. That is true of a trait the **adapter** implements. It does not bind a trait
//! `pos-edge` defines and implements **for** `SqliteStore` using that store's public methods, which
//! is what `queue.rs` has done since PR-1c — the dependency runs `pos-edge` → `store-sqlite`, so the
//! impl is legal where the trait is local. The narrower reading is recorded there.
//!
//! The cost of staying out of `pos-ports` is that no contract suite binds an implementation, and
//! this state drives the highest-precedence rule in the fleet. [`InMemoryOtaState`] is tested against
//! the same expectations as the SQLite path for exactly that reason, as `queue.rs` does; and
//! promoting the trait to a port later is mechanical if a second implementation ever needs binding.

use core::future::Future;

use pos_core::ota::{DeviceOtaAssignment, DeviceState, ReleaseVersion, SelfTest};
use pos_ports::PortError;
use pos_proto::ids::StoreId;
use store_sqlite::SqliteStore;

/// Where the store keeps its last OTA self-test across the restart an install performs.
///
/// One result, not a history: the rollback rule reads only the most recent verdict, and the fleet's
/// trail is the cloud's through [`CloudSync::report`](pos_ports::CloudSync::report). Two local
/// answers to "did it pass" would need a rule for which wins.
pub trait OtaStateAuthority: Send + Sync {
    /// Records the store's latest self-test, replacing any earlier one.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the write fails.
    fn record_self_test(
        &self,
        store_id: StoreId,
        self_test: SelfTest,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// The store's last self-test, or `None` if it has never recorded one — a box that has never
    /// installed anything, which the rollback rule reads as nothing to revert from.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the read fails.
    fn last_self_test(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<SelfTest>, PortError>> + Send;
}

impl OtaStateAuthority for SqliteStore {
    /// Forwards to the store's single-writer upsert, so the verdict is on disk before the caller
    /// proceeds to a restart.
    async fn record_self_test(
        &self,
        store_id: StoreId,
        self_test: SelfTest,
    ) -> Result<(), PortError> {
        self.record_ota_self_test(store_id, self_test.version.to_string(), self_test.passed)
            .await
    }

    /// Reads the row back, parsing the stored version with `ReleaseVersion::parse` — the pair of the
    /// `Display` the write used.
    ///
    /// A row whose version does not parse is treated as **absent** rather than surfaced as an error.
    /// The alternative is worse in the direction that matters: a rollout decision that fails because
    /// one stored string is malformed is a store that stops taking updates at all, where reading it
    /// as "no self-test recorded" only forgoes a rollback the box could not have identified a target
    /// version for anyway.
    async fn last_self_test(&self, store_id: StoreId) -> Result<Option<SelfTest>, PortError> {
        let row = self.last_ota_self_test(store_id).await?;
        Ok(row.and_then(|(version, passed)| {
            ReleaseVersion::parse(&version).map(|version| SelfTest { version, passed })
        }))
    }
}

/// An in-memory OTA-state authority: the last self-test per store, and nothing else.
///
/// What the fakes-backed example and the edge tests record against. **Not durable** — a restart
/// forgets it, which is precisely the defect the SQLite path exists to fix; the contract it honours
/// (one result per store, last write wins, absent until first recorded) is identical, so a decision
/// proven here behaves the same in production.
#[derive(Debug, Default)]
pub struct InMemoryOtaState {
    inner: std::sync::Mutex<std::collections::HashMap<StoreId, SelfTest>>,
}

impl InMemoryOtaState {
    /// A fresh authority with no self-test recorded for any store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl OtaStateAuthority for InMemoryOtaState {
    async fn record_self_test(
        &self,
        store_id: StoreId,
        self_test: SelfTest,
    ) -> Result<(), PortError> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(store_id, self_test);
        Ok(())
    }

    async fn last_self_test(&self, store_id: StoreId) -> Result<Option<SelfTest>, PortError> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&store_id)
            .copied())
    }
}

/// Assembles the [`DeviceState`] a rollout decision is made against, from the three places its
/// facts actually live.
///
/// - **`current`** is this binary's own version ([`crate::version::released`]), not a stored value.
///   What the box is running is a property of the running binary; a stored copy could disagree with
///   it after a rollback, and the rollback rule compares the two.
/// - **`ring` and `canary_bucket`** come from the cloud-published `device_ota` placement, which is
///   what makes the bucket stable across reboots (ADR-0052).
/// - **`last_self_test`** comes from `authority`, the one fact the box must remember for itself.
///
/// Returns `None` when the device cannot be weighed at all, which is deliberately **not** an error:
///
/// - **No placement.** The cloud has not put this store in a ring, so it is eligible for no rollout.
///   There is no safe ring to invent — see [`EdgeSession::device_ota`](crate::app::EdgeSession).
/// - **An unparseable own version.** A developer build stamps `0.0.0`, which parses; a fork that
///   breaks the release workflow's expression is caught by `version::tests::version_parses` at build
///   time. If it somehow still fails here, refusing to weigh an update is the recoverable outcome.
///
/// # Errors
///
/// [`PortError`] if the self-test could not be read. A store that cannot read its own self-test must
/// not be weighed as though it had none: that would silently drop the rollback rule, which is the one
/// case the whole precedence order exists to protect.
pub async fn device_state<A>(
    authority: &A,
    store_id: StoreId,
    placement: Option<DeviceOtaAssignment>,
) -> Result<Option<DeviceState>, PortError>
where
    A: OtaStateAuthority,
{
    let Some(placement) = placement else {
        return Ok(None);
    };
    let Some(current) = crate::version::released() else {
        return Ok(None);
    };
    let last_self_test = authority.last_self_test(store_id).await?;
    Ok(Some(DeviceState {
        current,
        ring: placement.ring,
        canary_bucket: placement.canary_bucket,
        last_self_test,
    }))
}

#[cfg(test)]
mod tests {
    use super::{InMemoryOtaState, OtaStateAuthority, device_state};
    use pos_core::ota::{DeviceOtaAssignment, ReleaseVersion, Ring, SelfTest};
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(7))
    }

    fn version(major: u16, minor: u16, patch: u16) -> ReleaseVersion {
        ReleaseVersion {
            major,
            minor,
            patch,
        }
    }

    #[tokio::test]
    async fn a_store_has_no_self_test_until_it_records_one_and_then_the_latest_wins() {
        let authority = InMemoryOtaState::new();
        assert!(
            authority
                .last_self_test(store())
                .await
                .expect("read")
                .is_none(),
            "a box that has never installed anything has nothing to revert from"
        );

        let failed = SelfTest {
            version: version(1, 4, 0),
            passed: false,
        };
        authority
            .record_self_test(store(), failed)
            .await
            .expect("record");
        assert_eq!(
            authority.last_self_test(store()).await.expect("read"),
            Some(failed)
        );

        // One result per store, last write wins: a later self-test replaces the earlier verdict
        // rather than adding to a history the decision would then have to choose between.
        let passed = SelfTest {
            version: version(1, 4, 1),
            passed: true,
        };
        authority
            .record_self_test(store(), passed)
            .await
            .expect("record");
        assert_eq!(
            authority.last_self_test(store()).await.expect("read"),
            Some(passed)
        );

        // Keyed by store, so one store's verdict is not another's.
        let other = StoreId::new(Ulid::from_u128(8));
        assert!(
            authority
                .last_self_test(other)
                .await
                .expect("read")
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_unplaced_store_assembles_no_device_state() {
        let authority = InMemoryOtaState::new();
        assert!(
            device_state(&authority, store(), None)
                .await
                .expect("assemble")
                .is_none(),
            "no placement means the device cannot be weighed, which is not an error"
        );
    }

    #[tokio::test]
    async fn a_placed_store_assembles_its_state_from_all_three_sources() {
        let authority = InMemoryOtaState::new();
        let recorded = SelfTest {
            version: version(1, 3, 0),
            passed: false,
        };
        authority
            .record_self_test(store(), recorded)
            .await
            .expect("record");

        let placement = DeviceOtaAssignment {
            ring: Ring::Fleet,
            canary_bucket: 42,
        };
        let state = device_state(&authority, store(), Some(placement))
            .await
            .expect("assemble")
            .expect("a placed store has a state");

        // The placement supplies the ring and the bucket …
        assert_eq!(state.ring, Ring::Fleet);
        assert_eq!(state.canary_bucket, 42);
        // … the authority supplies the self-test …
        assert_eq!(state.last_self_test, Some(recorded));
        // … and `current` is the running binary's own version, never a stored one, so a rollback
        // cannot leave the state claiming a version the box is not on.
        assert_eq!(state.current, crate::version::released().expect("parses"));
    }

    #[tokio::test]
    async fn a_recorded_failure_on_the_running_version_is_what_makes_the_rollback_rule_fire() {
        use pos_core::ota::{PublishedUpdate, RolloutDecision, decide_rollout};

        let authority = InMemoryOtaState::new();
        let placement = DeviceOtaAssignment {
            ring: Ring::Fleet,
            canary_bucket: 0,
        };
        let running = crate::version::released().expect("parses");
        let update = PublishedUpdate {
            target: version(running.major.saturating_add(1), 0, 0),
            min_ring: Ring::Lab,
            fleet_rollout_percent: 100,
            signing_key_id: [0xA1; 8],
            halted: false,
        };

        // With no self-test on record the device is simply eligible — this is the state every box is
        // in today, and the rollback arm can never fire from it.
        let eligible = device_state(&authority, store(), Some(placement))
            .await
            .expect("assemble")
            .expect("state");
        assert_eq!(
            decide_rollout(&eligible, &update, &[]),
            RolloutDecision::Install
        );

        // Record a failure against the version the box is *running*, restart-safe on the real store,
        // and the highest-precedence rule takes over — outranking an otherwise-eligible rollout.
        authority
            .record_self_test(
                store(),
                SelfTest {
                    version: running,
                    passed: false,
                },
            )
            .await
            .expect("record");
        let failed = device_state(&authority, store(), Some(placement))
            .await
            .expect("assemble")
            .expect("state");
        assert_eq!(
            decide_rollout(&failed, &update, &[]),
            RolloutDecision::RollBack,
            "a failure on the running version must revert, whatever the rollout says"
        );

        // A failure against a version the box has since left is history, not a reason to revert.
        authority
            .record_self_test(
                store(),
                SelfTest {
                    version: version(running.major.saturating_add(9), 0, 0),
                    passed: false,
                },
            )
            .await
            .expect("record");
        let stale = device_state(&authority, store(), Some(placement))
            .await
            .expect("assemble")
            .expect("state");
        assert_eq!(
            decide_rollout(&stale, &update, &[]),
            RolloutDecision::Install,
            "a failure on a version the box is not running does not block an update"
        );
    }
}
